//! What a run's sources were: each captured file's bytes and declarations,
//! the per-test execution record written when the run is published, and the
//! change model that says which tests a later edit could have reached.
//! `tests affected`, `runs source` and assertions coverage all read these.

use crate::{
    coverage_report::{
        ArchiveReportRequest, CoverageReport, ExitCodeInput, analyze_coverage_archive,
    },
    evidence_archive::read_archive_selected,
    lifecycle::atomic_write,
    run_store::{RunMetadata, StoredRun},
    source_units::{Code, Diff, named},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::Path,
};
pub type Files = BTreeMap<String, String>;
pub fn digest(value: &impl Serialize) -> String {
    format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(value).expect("serializable map"))
    )
}

/// One-based lines and UTF-8 byte columns, for every language. Text is exact.
pub fn local_path(file: &str) -> bool {
    !file.is_empty()
        && !file.contains(['\\', ':'])
        && file.split('/').all(|p| !matches!(p, "" | "." | ".."))
}
pub fn line_starts(source: &str) -> Vec<usize> {
    std::iter::once(0)
        .chain(source.match_indices('\n').map(|(at, _)| at + 1))
        .collect()
}

/// The text of 1-based `line`, exactly as `str::lines` would yield it: without
/// its line ending, with a carriage return stripped only where a line feed
/// follows it, and with no empty line after a final line feed.
pub fn line_text<'s>(source: &'s str, starts: &[usize], line: usize) -> Option<&'s str> {
    let start = *starts.get(line.checked_sub(1)?)?;
    if start >= source.len() {
        return None;
    }
    match starts.get(line) {
        Some(next) => {
            let text = &source[start..next - 1];
            Some(text.strip_suffix('\r').unwrap_or(text))
        }
        None => Some(&source[start..]),
    }
}

/// Source text held in memory for capture or a verified current-checkout query.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Inputs {
    pub language: String,
    pub files: Files,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FileFingerprint {
    pub sha256: String,
    pub bytes: usize,
    /// The parser's view of the file: what it declares, each declaration
    /// digested with comments blanked. Absent for a file no parser reads and
    /// in manifests written before this existed; such a file is compared by
    /// its bytes, as every file once was.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<Code>,
}
impl FileFingerprint {
    pub fn of(source: &str) -> Self {
        Self {
            sha256: format!("{:x}", Sha256::digest(source.as_bytes())),
            bytes: source.len(),
            code: None,
        }
    }
    /// Bytes and, where Supercov has a parser for the file, its declarations.
    pub fn read(path: &str, source: &str) -> Self {
        let mut fingerprint = Self::of(source);
        fingerprint.code = crate::source_units::code(path, source);
        fingerprint
    }
    pub fn same_bytes(&self, other: &Self) -> bool {
        self.sha256 == other.sha256
    }
}
pub type FileManifest = BTreeMap<String, FileFingerprint>;

/// The run stores identities and hashes, never complete source files.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InputManifest {
    pub schema_version: u32,
    pub language: String,
    pub files: FileManifest,
}
/// The manifest's schema: files, their bytes and their declarations.
pub const MANIFEST_SCHEMA: u32 = 3;
impl Inputs {
    pub fn manifest(&self) -> InputManifest {
        InputManifest {
            schema_version: MANIFEST_SCHEMA,
            language: self.language.clone(),
            files: self
                .files
                .iter()
                .map(|(p, s)| (p.clone(), FileFingerprint::read(p, s)))
                .collect(),
        }
    }
}
impl InputManifest {
    pub fn with_sources(&self, files: Files) -> Inputs {
        Inputs {
            language: self.language.clone(),
            files,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TestSelector {
    pub file: String,
    pub name: String,
}

/// What each test of a run executed, in the units of that run's manifest.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Executions {
    pub tests: Vec<Execution>,
    /// Per file, the units that hold a probe of their own. A change confined
    /// to these can reach a test only by being run.
    pub probed: BTreeMap<String, Vec<usize>>,
    /// Per file, the units the run actually reached -- every record's, the one
    /// holding execution no test could be credited with included.
    ///
    /// This is the bound on a test whose own reach nothing recorded. It cannot
    /// have run more than the run did, so a change outside this reached no
    /// test at all and a change inside it might have reached any of them.
    /// Without it the only sound answer for such a test would be "affected by
    /// everything", which is true and useless.
    #[serde(default)]
    pub covered: BTreeMap<String, Vec<usize>>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Execution {
    pub test: TestSelector,
    pub passed: bool,
    /// Per file, the innermost unit of every probe this test fired.
    pub files: BTreeMap<String, Vec<usize>>,
    /// How completely `files` describes what this test ran: `exact`, `partial`
    /// where it is a lower bound, or `run-wide` where nothing was recorded.
    ///
    /// Only under `exact` does an absence here mean the test did not run the
    /// code. A Go test that called `t.Parallel()` records nothing of its own,
    /// and a Ruby test records only the lines no earlier test had reached --
    /// so reading either silence as proof is how a change to code a test
    /// exercised comes back as "this test is unaffected".
    #[serde(default = "exact_by_default")]
    pub attribution: String,
}

/// Every execution recorded before attribution was a question was an exact
/// one, so a record that does not say is one.
fn exact_by_default() -> String {
    crate::coverage_report::ATTRIBUTION_EXACT.to_owned()
}
/// How one captured file moved between two runs, judged once and read for
/// every flow.
pub enum FileChange<'a> {
    Same,
    /// Only comments changed: no program can tell.
    CommentsOnly,
    /// Not among the previous run's inputs.
    Added,
    Removed,
    /// No parser reads the file on one side or the other; its bytes moved.
    Bytes,
    Code {
        before: &'a Code,
        after: &'a Code,
        diff: Diff,
        /// The change is confined to declaration bodies that only run: it
        /// reaches a test only if the test ran one of them.
        narrow: bool,
    },
}
pub fn file_change<'a>(
    before: Option<&'a FileFingerprint>,
    after: Option<&'a FileFingerprint>,
    probed: Option<&[usize]>,
) -> Option<FileChange<'a>> {
    let Some(before) = before else {
        return after.map(|_| FileChange::Added);
    };
    let Some(after) = after else {
        return Some(FileChange::Removed);
    };
    if before.same_bytes(after) {
        return Some(FileChange::Same);
    }
    let (Some(old), Some(new)) = (&before.code, &after.code) else {
        return Some(FileChange::Bytes);
    };
    if old.semantic == new.semantic {
        return Some(FileChange::CommentsOnly);
    }
    let diff = old.diff(new);
    let narrow = probed.is_some_and(|probed| diff.narrow(old, probed));
    Some(FileChange::Code {
        before: old,
        after: new,
        diff,
        narrow,
    })
}
/// Every unit that moved, by name: what changed, what arrived, what went.
pub fn describe(before: &Code, after: &Code, diff: &Diff) -> String {
    let mut parts = Vec::new();
    if !diff.changed.is_empty() {
        parts.push(named(diff.changed.iter().map(|i| &before.units[*i])));
    }
    if !diff.added.is_empty() {
        parts.push(format!(
            "added {}",
            named(diff.added.iter().map(|i| &after.units[*i]))
        ));
    }
    if !diff.removed.is_empty() {
        parts.push(format!(
            "removed {}",
            named(diff.removed.iter().map(|i| &before.units[*i]))
        ));
    }
    if parts.is_empty() {
        "declarations".to_owned()
    } else {
        parts.join("; ")
    }
}

/// The run's manifest, written beside the archive at publication so a query
/// need not decompress the archive for one entry. Used only while the
/// archive's digest is the one it records.
const MANIFEST_CACHE_FILE: &str = "source-manifest.cache.json";
/// What each test executed, written at publication; `tests affected` reads it.
pub const EXECUTIONS_FILE: &str = "test-executions.json";
/// The archive entry holding the run's source manifest.
pub use crate::source_capture::ARCHIVE_PATH;

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ManifestCache {
    evidence_digest: String,
    manifest: InputManifest,
    #[serde(default)]
    statement_exclusions: Vec<Value>,
}
pub struct RunManifest {
    pub manifest: InputManifest,
    pub evidence_digest: String,
    pub statement_exclusions: Vec<Value>,
}
pub struct RunInputs {
    pub inputs: Inputs,
    pub stored: RunManifest,
}
impl std::ops::Deref for RunInputs {
    type Target = RunManifest;
    fn deref(&self) -> &RunManifest {
        &self.stored
    }
}
/// The run's manifest with the current checkout's sources, which must be the
/// run's own byte for byte.
pub fn load_inputs(root: &Path, run: &StoredRun) -> Result<RunInputs, String> {
    let stored = load_manifest(run)?;
    let inputs = crate::source_capture::current_sources(root, &stored.manifest)?;
    Ok(RunInputs { inputs, stored })
}
pub fn load_manifest(run: &StoredRun) -> Result<RunManifest, String> {
    load_optional_manifest(run)?.ok_or_else(|| {
        "This run has no source manifest. Run tests once with this version of Supercov".into()
    })
}
fn load_optional_manifest(run: &StoredRun) -> Result<Option<RunManifest>, String> {
    if run.metadata.merged == Some(true) {
        return Err("Use a single run; merged runs have multiple source manifests".into());
    }
    let bytes = fs::read(&run.evidence_path).map_err(|e| e.to_string())?;
    let evidence_digest = format!("{:x}", Sha256::digest(&bytes));
    if let Some(cached) = fs::read(run.directory.join(MANIFEST_CACHE_FILE))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<ManifestCache>(&bytes).ok())
        .filter(|cached| cached.evidence_digest == evidence_digest)
        .filter(|cached| validate_manifest(&cached.manifest).is_ok())
    {
        return Ok(Some(RunManifest {
            manifest: cached.manifest,
            evidence_digest,
            statement_exclusions: cached.statement_exclusions,
        }));
    }
    let entries = read_archive_selected(&run.evidence_path, |path| {
        path == ARCHIVE_PATH || path == "statement-exclusions.json"
    })
    .map_err(|e| e.to_string())?;
    let Some(input) = entries.iter().find(|e| e.path == ARCHIVE_PATH) else {
        return Ok(None);
    };
    let value: Value = serde_json::from_slice(&input.contents).map_err(|e| e.to_string())?;
    if value["schemaVersion"].as_u64() != Some(u64::from(MANIFEST_SCHEMA)) {
        return Err(
            "Unsupported source manifest schema; rerun tests with this version of Supercov".into(),
        );
    }
    let manifest = serde_json::from_value::<InputManifest>(value).map_err(|e| e.to_string())?;
    validate_manifest(&manifest)?;
    if fs::read(&run.evidence_path).map_err(|e| e.to_string())? != bytes {
        return Err("Run archive changed during read".into());
    }
    Ok(Some(RunManifest {
        manifest,
        evidence_digest,
        statement_exclusions: entries
            .iter()
            .find(|e| e.path == "statement-exclusions.json")
            .map(|entry| serde_json::from_slice(&entry.contents).map_err(|e| e.to_string()))
            .transpose()?
            .unwrap_or_default(),
    }))
}
fn validate_manifest(manifest: &InputManifest) -> Result<(), String> {
    if manifest.files.iter().any(|(p, f)| {
        !local_path(p) || f.sha256.len() != 64 || !f.sha256.bytes().all(|b| b.is_ascii_hexdigit())
    }) {
        return Err("Invalid source manifest".into());
    }
    Ok(())
}

/// The manifest a run archived, from the inputs the frontend archived it
/// from and the digest publication took of the archive: what reading the
/// archive back would give, without decompressing it to find one entry.
pub(crate) fn archived_manifest(
    manifest: InputManifest,
    evidence_digest: String,
) -> Result<RunManifest, String> {
    validate_manifest(&manifest)?;
    Ok(RunManifest {
        manifest,
        evidence_digest,
        statement_exclusions: Vec::new(),
    })
}
fn write_json(
    root: &Path,
    run: &StoredRun,
    name: &str,
    value: &impl serde::Serialize,
) -> Result<(), String> {
    let mut bytes = serde_json::to_vec_pretty(value).map_err(|e| e.to_string())?;
    bytes.push(b'\n');
    atomic_write(root, &run.directory.join(name), &bytes).map_err(|e| e.to_string())
}
/// Write the run's manifest cache and execution record into the unpublished
/// run directory; the lifecycle publishes them with the run in one rename.
///
/// `analysed` is the run's coverage when the caller has already analysed its
/// evidence. Merged runs and archives without a manifest have neither.
pub(crate) fn prepare_publication_with(
    root: &Path,
    directory: &Path,
    metadata: &RunMetadata,
    analysed: Option<&CoverageReport>,
    archived: Option<RunManifest>,
) -> Result<(), String> {
    if metadata.merged == Some(true) {
        return Ok(());
    }
    let run = StoredRun {
        id: metadata.id.clone(),
        directory: directory.into(),
        evidence_path: directory.join("evidence.raw.gz"),
        metadata_path: directory.join("run.json"),
        query_index_path: directory.join(crate::run_store::RUST_QUERY_INDEX_FILE),
        metadata: metadata.clone(),
    };
    let input = match archived {
        Some(input) => input,
        None => match load_optional_manifest(&run)? {
            Some(input) => input,
            None => return Ok(()),
        },
    };
    let current = crate::source_capture::current_sources(root, &input.manifest);
    let owned;
    let analysed = match analysed {
        Some(report) => Some(report),
        None => {
            owned = coverage(&run).ok();
            owned.as_ref()
        }
    };
    // What each test ran, so a later edit can be told apart from code the test
    // never reached. Evidence that will not analyse leaves the record out.
    if let Some(executions) = analysed.and_then(|report| {
        executions(
            report,
            &input.manifest,
            current.as_ref().ok().map(|inputs| &inputs.files),
        )
    }) {
        write_json(root, &run, EXECUTIONS_FILE, &executions)?;
    }
    // A cache: a run that cannot write it is read from its archive instead.
    let _ = write_json(
        root,
        &run,
        MANIFEST_CACHE_FILE,
        &ManifestCache {
            evidence_digest: input.evidence_digest.clone(),
            manifest: input.manifest.clone(),
            statement_exclusions: input.statement_exclusions.clone(),
        },
    );
    Ok(())
}
/// Each test's execution, placed in the declarations of the run's own
/// manifest: for every probe a test fired, the innermost unit holding it.
/// Also which units hold a probe at all, per file, since only a change
/// confined to such units can be said to have missed a test. Sources are
/// needed to place JavaScript columns; without them there is no record.
pub fn executions(
    coverage: &CoverageReport,
    manifest: &InputManifest,
    sources: Option<&Files>,
) -> Option<Executions> {
    // Each point's file, by index into `files`, and unit. Hits are looked up
    // millions of times on a large run, so the lookup hashes with Fx and a
    // test's hits are grouped by file index before any name is copied.
    let mut located: crate::interned::FastMap<&str, (usize, usize)> = Default::default();
    let mut files: Vec<&str> = Vec::new();
    let mut file_indexes: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    let mut probed: BTreeMap<String, BTreeSet<usize>> = BTreeMap::new();
    for point in &coverage.view.points {
        let meta = &point.meta;
        let Some(code) = manifest
            .files
            .get(&meta.file)
            .and_then(|fingerprint| fingerprint.code.as_ref())
        else {
            continue;
        };
        let column = if manifest.language == "javascript" {
            let source = sources?.get(&meta.file)?;
            let Some(column) = byte_column(source, meta.line, meta.column, &manifest.language)
            else {
                continue;
            };
            column
        } else {
            meta.column + 1
        };
        let unit = code.unit_at(meta.line, column);
        let file = *file_indexes.entry(meta.file.as_str()).or_insert_with(|| {
            files.push(meta.file.as_str());
            files.len() - 1
        });
        located.insert(meta.id.as_str(), (file, unit));
        if code.units[unit].is_code() {
            probed.entry(meta.file.clone()).or_default().insert(unit);
        }
    }
    // Everything the run reached, whoever reached it. Collected before the
    // per-test loop because the record that holds what no test could be
    // credited with has no test file, and the loop below skips it -- which is
    // exactly the record a parallel suite's coverage lives in.
    let mut covered_by_file = vec![Vec::new(); files.len()];
    for test in &coverage.view.tests {
        for hit in &test.hits {
            if let Some((file, unit)) = located.get(hit.as_str()) {
                covered_by_file[*file].push(*unit);
            }
        }
    }
    let covered = files
        .iter()
        .zip(covered_by_file)
        .filter(|(_, units)| !units.is_empty())
        .map(|(file, units)| {
            (
                (*file).to_owned(),
                units.into_iter().collect::<BTreeSet<_>>(),
            )
        })
        .collect::<BTreeMap<_, _>>();

    let mut tests: BTreeMap<TestSelector, Execution> = BTreeMap::new();
    for test in &coverage.view.tests {
        let Some(file) = &test.file else {
            continue;
        };
        let selector = TestSelector {
            file: file.clone(),
            name: test.name.clone(),
        };
        let record = tests.entry(selector.clone()).or_insert_with(|| Execution {
            test: selector,
            passed: false,
            files: BTreeMap::new(),
            attribution: crate::coverage_report::ATTRIBUTION_EXACT.to_owned(),
        });
        record.passed |= test.outcome == "passed";
        // One name can be recorded more than once -- retries, a test declared
        // in two files -- and a single incomplete appearance is enough to make
        // the whole record's file list a lower bound.
        if !crate::coverage_report::coverage_is_complete(&record.attribution) {
            // already the weaker claim
        } else if !crate::coverage_report::coverage_is_complete(&test.attribution) {
            record.attribution = test.attribution.clone();
        }
        let mut reached = test
            .hits
            .iter()
            .filter_map(|hit| located.get(hit.as_str()).copied())
            .collect::<Vec<_>>();
        reached.sort_unstable();
        for group in reached.chunk_by(|left, right| left.0 == right.0) {
            record
                .files
                .entry(files[group[0].0].to_owned())
                .or_default()
                .extend(group.iter().map(|(_, unit)| *unit));
        }
    }
    let mut tests = tests.into_values().collect::<Vec<_>>();
    for record in &mut tests {
        for units in record.files.values_mut() {
            units.sort_unstable();
            units.dedup();
        }
    }
    Some(Executions {
        tests,
        probed: probed
            .into_iter()
            .map(|(file, units)| (file, units.into_iter().collect()))
            .collect(),
        covered: covered
            .into_iter()
            .map(|(file, units)| (file, units.into_iter().collect()))
            .collect(),
    })
}
/// Which of a run's tests the current checkout's changes could have reached,
/// from what each test executed and what has changed since, declaration by
/// declaration. A test is affected by a change in code it ran, in its test
/// file, or in a file it ran code in whose declarations changed shape; not by
/// a change confined to code it never ran; not by comments or blank lines. A
/// test that did not pass is listed as affected regardless: it has a result to
/// establish. What this cannot see is a file the run never captured -- a file
/// added since -- and the run's dependencies and configuration, which the
/// working-tree check answers for.
pub fn affected_tests(root: &Path, run: &StoredRun) -> Result<Value, String> {
    let stored = load_manifest(run)?;
    let executions: Executions = fs::read(run.directory.join(EXECUTIONS_FILE))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .ok_or(
            "This run has no per-test execution record; rerun tests with this version of Supercov",
        )?;
    let executions = &executions;
    let root = crate::workspace::canonicalize_simplified(root).map_err(|e| e.to_string())?;
    let manifest = &stored.manifest;
    let now = manifest
        .files
        .keys()
        .filter_map(|file| {
            let path = root.join(file);
            let canonical = crate::workspace::canonicalize_simplified(&path).ok()?;
            if !canonical.starts_with(&root) || !canonical.is_file() {
                return None;
            }
            let text = fs::read_to_string(canonical).ok()?;
            Some((file.clone(), FileFingerprint::read(file, &text)))
        })
        .collect::<FileManifest>();
    let changes = manifest
        .files
        .keys()
        .filter_map(|file| {
            file_change(
                manifest.files.get(file),
                now.get(file),
                executions.probed.get(file).map(Vec::as_slice),
            )
            .map(|change| (file.as_str(), change))
        })
        .collect::<BTreeMap<_, _>>();
    let files = changes
        .iter()
        .filter_map(|(file, change)| {
            let (kind, detail) = match change {
                FileChange::Same | FileChange::Added => return None,
                FileChange::CommentsOnly => ("formatting", None),
                FileChange::Removed => ("removed", None),
                FileChange::Bytes => ("changed", None),
                FileChange::Code {
                    before,
                    after,
                    diff,
                    narrow,
                } => (
                    if *narrow { "bodies" } else { "declarations" },
                    Some(describe(before, after, diff)),
                ),
            };
            Some(json!({"file":file,"change":kind,"detail":detail}))
        })
        .collect::<Vec<_>>();
    // What a test whose own reach is unknown could have reached.
    //
    // Nothing can narrow such a test below the run it ran in, and nothing
    // needs to go wider: its reach is bounded above by what the run covered.
    // So a change inside that bound could have reached it, and a change
    // outside it -- code no test in the run ever ran, a file added since --
    // could not have reached any test, this one included.
    //
    // That is a real narrowing rather than "always affected". It is also the
    // only safe direction: reading an empty file list as "this change missed
    // it" is what let a change to code every test exercised report `0 of 2
    // tests affected` and exit 0, which an agent running only affected tests
    // would act on by running nothing.
    let beyond_the_run = changes
        .iter()
        .filter_map(|(file, change)| match change {
            FileChange::Same | FileChange::CommentsOnly | FileChange::Added => None,
            FileChange::Removed => Some(format!("{file} removed (the run covered code in it)")),
            FileChange::Bytes => Some(format!("{file} changed (the run covered code in it)")),
            FileChange::Code { before, diff, .. } => {
                // Unit by unit rather than file by file: a function no test
                // ran sits in the same file as the ones they did, so asking
                // whether the file was covered would make every change to it
                // reach every test.
                let code = manifest.files.get(*file).and_then(|f| f.code.as_ref());
                let reached = executions
                    .covered
                    .get(*file)
                    .map(Vec::as_slice)
                    .unwrap_or_default()
                    .iter()
                    .flat_map(|unit| match code {
                        Some(code) if *unit < code.units.len() => {
                            code.ancestors(*unit).collect::<Vec<_>>()
                        }
                        _ => vec![*unit],
                    })
                    .collect::<BTreeSet<_>>();
                let hit = diff
                    .changed
                    .iter()
                    .filter(|unit| reached.contains(unit))
                    .filter_map(|unit| before.units.get(*unit))
                    .collect::<Vec<_>>();
                (!hit.is_empty())
                    .then(|| format!("{file}: {} changed (the run covered it)", named(hit)))
            }
        })
        .collect::<Vec<_>>();

    // A JVM class's static initializer runs once per process and is credited
    // to the first test to load the class, so a test that only reads a static
    // field it set ran nothing recorded in that file. For a change to a
    // class's declarations, a test whose own file names the class is affected
    // too. (A module's top level in Python, Ruby, JavaScript or Go runs once
    // as well, but replaying their histories missed no failing test there and
    // the same rule grew dry-inflector's selection from 73% to 90% of its
    // tests, so it is the JVM's alone.)
    let named_by_tests = named_by_tests(&changes);
    let mut test_sources = BTreeMap::<&str, Option<String>>::new();

    let mut affected = Vec::new();
    let mut undetermined = Vec::new();
    let mut unaffected = Vec::new();
    for record in &executions.tests {
        let mut reasons = Vec::new();
        // The changed declarations this test ran, where: what they held in
        // the run (their own lines, nested declarations aside) and where
        // they are now, so a caller can say which of their statements the
        // test is known to check.
        let mut changed_code = Vec::new();
        if !record.passed {
            reasons.push("did not pass in the run".to_owned());
        }
        match changes.get(record.test.file.as_str()) {
            None | Some(FileChange::Same | FileChange::CommentsOnly | FileChange::Added) => {}
            Some(FileChange::Removed) => reasons.push("test file removed".to_owned()),
            Some(FileChange::Bytes) => reasons.push("test file changed".to_owned()),
            Some(FileChange::Code {
                before,
                after,
                diff,
                ..
            }) => reasons.push(format!(
                "test file changed: {}",
                describe(before, after, diff)
            )),
        }
        for (file, units) in &record.files {
            let code = manifest.files.get(file).and_then(|f| f.code.as_ref());
            let ran = units
                .iter()
                .flat_map(|unit| match code {
                    Some(code) if *unit < code.units.len() => {
                        code.ancestors(*unit).collect::<Vec<_>>()
                    }
                    _ => vec![*unit],
                })
                .collect::<BTreeSet<_>>();
            match changes.get(file.as_str()) {
                None | Some(FileChange::Same | FileChange::CommentsOnly | FileChange::Added) => {}
                Some(FileChange::Removed) => {
                    reasons.push(format!("{file} removed (this test ran code in it)"));
                }
                Some(FileChange::Bytes) => {
                    reasons.push(format!("{file} changed (this test ran code in it)"));
                }
                Some(FileChange::Code {
                    before,
                    after,
                    diff,
                    narrow,
                }) => {
                    if *narrow {
                        let hit = diff
                            .changed
                            .iter()
                            .filter(|i| ran.contains(i))
                            .map(|i| &before.units[*i])
                            .collect::<Vec<_>>();
                        let now = after.by_path();
                        for index in diff.changed.iter().filter(|i| ran.contains(i)) {
                            let unit = &before.units[*index];
                            changed_code.push(json!({
                                "file": file,
                                "declaration": unit.path,
                                "own": own_lines(before, *index),
                                "now": now.get(unit.path.as_str()).map(|u| [u.line, u.end_line]),
                            }));
                        }
                        if !hit.is_empty() {
                            reasons
                                .push(format!("{file}: {} changed (this test ran it)", named(hit)));
                        }
                    } else {
                        reasons.push(format!(
                            "{file}: {} changed (this test ran code in this file)",
                            describe(before, after, diff)
                        ));
                    }
                }
            }
        }
        for (file, names) in &named_by_tests {
            if record.files.contains_key(*file) || *file == record.test.file.as_str() {
                continue; // what it ran there already decided
            }
            let source = test_sources
                .entry(record.test.file.as_str())
                .or_insert_with(|| fs::read_to_string(root.join(&record.test.file)).ok());
            if let Some(name) = source
                .as_deref()
                .and_then(|text| names.iter().find(|name| names_word(text, name)))
            {
                reasons.push(format!(
                    "{file}: declarations changed, and the test file names {name} (code that runs once per process is credited to the first test to reach it)"
                ));
            }
        }
        if !crate::coverage_report::coverage_is_complete(&record.attribution) {
            // What its own record proves is the strong claim, and it keeps it:
            // a Ruby test credited with the changed line ran the changed line,
            // whatever else it also ran unrecorded.
            let proven = !reasons.is_empty();
            // The run's bound is what stands in for a record this test does
            // not have. Where it has one that already proves the change
            // reached it, saying so twice says nothing twice.
            if !proven {
                reasons.extend(beyond_the_run.iter().cloned());
                reasons.sort();
                reasons.dedup();
            }
            let entry = json!({
                "file": record.test.file,
                "name": record.test.name,
                "reasons": reasons,
                "attribution": record.attribution,
            });
            if proven {
                affected.push(entry);
            } else if reasons.is_empty() {
                unaffected.push(entry);
            } else {
                undetermined.push(entry);
            }
            continue;
        }
        let entry = json!({"file":record.test.file,"name":record.test.name,"reasons":reasons,"changedCode":changed_code});
        if reasons.is_empty() {
            unaffected.push(entry);
        } else {
            affected.push(entry);
        }
    }
    // Most likely to fail first: a test whose own file or name shares words
    // with a changed file (`test_receivebuffer.py` for `_receivebuffer.py`).
    // Replaying real commits in nine projects, this put a failing test in the
    // first three for 23 of 57 breaking commits where file order managed 13.
    let changed_words = changes
        .iter()
        .filter(|(_, change)| !matches!(change, FileChange::Same | FileChange::CommentsOnly))
        .flat_map(|(file, _)| words_of(file))
        .collect::<BTreeSet<_>>();
    for bucket in [&mut affected, &mut undetermined] {
        for entry in bucket.iter_mut() {
            entry["nameMatch"] = json!(name_match(entry, &changed_words));
        }
        bucket.sort_by_key(|entry| std::cmp::Reverse(entry["nameMatch"].as_u64().unwrap_or(0)));
    }
    Ok(json!({
        "run": run.id,
        "affected": affected,
        // Kept apart from `affected` because they are a different claim. A
        // test is affected when a change reached code it is recorded as having
        // run; it is undetermined when nothing recorded what it ran and the
        // change is inside what the run as a whole covered. Merging them would
        // report a precision the run does not have -- but both have to be run,
        // so both are in `--names`.
        "undetermined": undetermined,
        "unaffected": unaffected,
        "changedFiles": files,
        "summary": {
            "tests": executions.tests.len(),
            "affected": affected.len(),
            "undetermined": undetermined.len(),
            "unaffected": unaffected.len(),
            "changedFiles": files.len(),
        },
        "meaning": "Tests whose recorded execution a change since the run could have reached, and those whose execution nothing recorded: a test that ran alongside others has no coverage of its own, so it is undetermined whenever a change reaches anything the run covered. Run both. A file the run never captured, a dependency or a configuration change is not seen here; see workingTree."
    }))
}
/// For each changed JVM file whose declarations changed, the names a test
/// file may use to reach it: its top-level classes and its own name.
fn named_by_tests<'a>(
    changes: &BTreeMap<&'a str, FileChange<'_>>,
) -> BTreeMap<&'a str, BTreeSet<String>> {
    changes
        .iter()
        .filter(|(file, _)| {
            [".java", ".kt", ".kts", ".groovy", ".scala"]
                .iter()
                .any(|extension| file.ends_with(extension))
        })
        .filter_map(|(file, change)| match change {
            FileChange::Code {
                before,
                narrow: false,
                ..
            } => {
                let stem = file.rsplit('/').next()?.split('.').next()?.to_owned();
                let mut names = before
                    .units
                    .iter()
                    .filter(|unit| unit.parent.is_none_or(|p| before.units[p].path.is_empty()))
                    .filter_map(|unit| unit.path.rsplit('.').next().map(str::to_owned))
                    .filter(|name| name.len() > 2)
                    .collect::<BTreeSet<_>>();
                if stem.len() > 2
                    && !matches!(stem.as_str(), "mod" | "lib" | "main" | "index" | "__init__")
                {
                    names.insert(stem);
                }
                Some((*file, names))
            }
            _ => None,
        })
        .collect::<BTreeMap<_, _>>()
}

/// The words a file's name is made of, test markers aside:
/// `tests/test_receive_buffer.py` -> `receive`, `buffer`;
/// `ReceiveBufferTest.java` -> `receive`, `buffer`.
fn words_of(path: &str) -> BTreeSet<String> {
    let stem = path.rsplit(['/', '\\']).next().unwrap_or(path);
    let stem = stem.split('.').next().unwrap_or(stem);
    split_words(stem)
        .into_iter()
        .filter(|w| !matches!(w.as_str(), "test" | "tests" | "spec" | "specs"))
        .collect()
}

/// `receiveBuffer_v2` -> `receive`, `buffer`, `v2`: camel and snake case
/// split, lowercased, single letters dropped.
fn split_words(text: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut previous_lower = false;
    for c in text.chars() {
        if !c.is_alphanumeric() {
            if !current.is_empty() {
                words.push(std::mem::take(&mut current));
            }
            previous_lower = false;
            continue;
        }
        if c.is_uppercase() && previous_lower && !current.is_empty() {
            words.push(std::mem::take(&mut current));
        }
        previous_lower = c.is_lowercase() || c.is_ascii_digit();
        current.extend(c.to_lowercase());
    }
    if !current.is_empty() {
        words.push(current);
    }
    words.into_iter().filter(|w| w.len() > 1).collect()
}

/// How many of the changed files' words a test's file or name shares.
fn name_match(entry: &Value, changed: &BTreeSet<String>) -> usize {
    let file = entry["file"].as_str().unwrap_or("");
    let name = entry["name"].as_str().unwrap_or("");
    // The test's own name: after the last separator a runner puts between a
    // file or class and the test (`::`, `#`, `>`, `.`), parameters aside.
    let own = name
        .rsplit(['#', ':', '>', '.', '/'])
        .next()
        .unwrap_or(name)
        .split(['[', '('])
        .next()
        .unwrap_or("");
    let mut words = words_of(file);
    words.extend(split_words(own));
    words.intersection(changed).count()
}

/// Whether `text` holds `word` as a whole identifier.
fn names_word(text: &str, word: &str) -> bool {
    text.match_indices(word).any(|(at, _)| {
        let before = text[..at].chars().last();
        let after = text[at + word.len()..].chars().next();
        let part = |c: Option<char>| c.is_some_and(|c| c.is_alphanumeric() || c == '_');
        !part(before) && !part(after)
    })
}

/// A declaration's own lines: its span without the spans of the
/// declarations nested in it, as inclusive `[first, last]` ranges.
fn own_lines(code: &crate::source_units::Code, index: usize) -> Vec<[usize; 2]> {
    let unit = &code.units[index];
    let mut children = code
        .units
        .iter()
        .filter(|u| u.parent == Some(index))
        .map(|u| (u.line, u.end_line))
        .collect::<Vec<_>>();
    children.sort_unstable();
    let mut out = Vec::new();
    let mut next = unit.line;
    for (start, end) in children {
        if start > next {
            out.push([next, start - 1]);
        }
        next = next.max(end + 1);
    }
    if next <= unit.end_line {
        out.push([next, unit.end_line]);
    }
    out
}

pub fn coverage(run: &StoredRun) -> Result<CoverageReport, String> {
    analyze_coverage_archive(&ArchiveReportRequest {
        archive_path: run.evidence_path.clone(),
        run_id: run.id.clone(),
        generated_at: run.metadata.started_at.clone(),
        integrity: None,
        test_exit_code: ExitCodeInput::Present(run.metadata.test_exit_code),
    })
    .map_err(|e| format!("{e:?}"))
}
fn byte_column(source: &str, line: usize, column: usize, language: &str) -> Option<usize> {
    byte_column_in(source, &line_starts(source), line, column, language)
}

/// `byte_column`, given where each line starts, so that resolving a whole
/// file's statements looks each line up instead of walking the file to it.
fn byte_column_in(
    source: &str,
    starts: &[usize],
    line: usize,
    column: usize,
    language: &str,
) -> Option<usize> {
    if line == 0 {
        return None;
    }
    let line = line_text(source, starts, line)?;
    if language != "javascript" {
        // Native Rust, Python and Ruby manifests use zero-based byte columns.
        return column.checked_add(1);
    }
    if column == 0 {
        return None;
    }
    let mut units = 0;
    for (byte, ch) in line.char_indices() {
        if units == column - 1 {
            return Some(byte + 1);
        }
        units += ch.len_utf16();
    }
    (units == column - 1).then_some(line.len() + 1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::evidence_archive::{read_archive, write_archive};
    use crate::run_store::discover_runs;

    fn prepare_publication(
        root: &Path,
        directory: &Path,
        metadata: &RunMetadata,
        analysed: Option<&CoverageReport>,
    ) -> Result<(), String> {
        prepare_publication_with(root, directory, metadata, analysed, None)
    }

    fn recorded(run: &StoredRun) -> Executions {
        serde_json::from_slice(&fs::read(run.directory.join(EXECUTIONS_FILE)).expect("a record"))
            .unwrap()
    }

    /// `byte_column` as it was before it took line starts.
    fn reference_byte_column(
        source: &str,
        line: usize,
        column: usize,
        language: &str,
    ) -> Option<usize> {
        if line == 0 {
            return None;
        }
        let line = source.lines().nth(line - 1)?;
        if language != "javascript" {
            return column.checked_add(1);
        }
        if column == 0 {
            return None;
        }
        let mut units = 0;
        for (byte, ch) in line.char_indices() {
            if units == column - 1 {
                return Some(byte + 1);
            }
            units += ch.len_utf16();
        }
        (units == column - 1).then_some(line.len() + 1)
    }

    #[test]
    fn a_column_from_line_starts_agrees_with_walking_the_file() {
        // JavaScript columns count UTF-16 units, so text whose widths differ --
        // accents, and an emoji that is two units -- is where they could part.
        for source in [
            "",
            "a",
            "a\n",
            "a\r\nb\r\n",
            "x\r",
            "héllo\nwörld\n",
            "😀x\ny😀z\n",
            "const a = 1;\n  b();\n",
        ] {
            let starts = line_starts(source);
            for language in ["javascript", "python"] {
                for line in 0..source.len() + 3 {
                    for column in 0..source.len() + 4 {
                        assert_eq!(
                            byte_column_in(source, &starts, line, column, language),
                            reference_byte_column(source, line, column, language),
                            "{language} {line}:{column} in {source:?}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn publication_records_what_each_test_ran_and_affected_tests_reads_it() {
        let root = std::env::temp_dir().join(format!("supercov-affected-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("src")).unwrap();
        fs::create_dir_all(root.join("tests")).unwrap();
        // The fixture's one point sits at line 1, column 0, so `work` has to
        // start the file: an `export` keyword there would belong to the top
        // level, not to the function.
        let app = "function work() {\n  return 1;\n}\nfunction idle() {\n  return 2;\n}\n";
        let test = "import assert from 'node:assert/strict';\nimport { LIMIT } from '../src/config.js';\nassert.equal(work(), 1);\n";
        let config = "export const LIMIT = 5;\nfunction check(n) {\n  return n < LIMIT;\n}\n";
        fs::write(root.join("src/app.js"), app).unwrap();
        fs::write(root.join("src/config.js"), config).unwrap();
        fs::write(root.join("tests/app.test.js"), test).unwrap();
        // The fixture run's one point sits at line 1, column 0 of src/app.js
        // with a zero-based byte column, which is how every non-JavaScript
        // frontend reports; the language is named for that.
        let inputs = crate::source_capture::capture(
            &root,
            "python",
            [
                "src/app.js".into(),
                "src/config.js".into(),
                "tests/app.test.js".into(),
            ],
        )
        .unwrap();
        let directory = crate::run_store::create_analyzable_test_run(&root, "first");
        let path = directory.join("evidence.raw.gz");
        let entries = crate::source_capture::append(read_archive(&path).unwrap(), &inputs).unwrap();
        let archive = write_archive(entries, &path).unwrap();
        let metadata_path = directory.join("run.json");
        let mut metadata: RunMetadata =
            serde_json::from_slice(&fs::read(&metadata_path).unwrap()).unwrap();
        metadata.raw_evidence.files = archive.files;
        metadata.raw_evidence.compressed_bytes = archive.compressed_bytes;
        metadata.raw_evidence.uncompressed_bytes = archive.uncompressed_bytes;
        fs::write(&metadata_path, serde_json::to_vec(&metadata).unwrap()).unwrap();
        prepare_publication(&root, &directory, &metadata, None).unwrap();
        let run = discover_runs(&root).unwrap().runs.remove(0);
        let stored = load_manifest(&run).unwrap();
        let executions = &recorded(&run);
        let code = stored.manifest.files["src/app.js"].code.as_ref().unwrap();
        let work = code.units.iter().position(|u| u.path == "work").unwrap();
        let idle = code.units.iter().position(|u| u.path == "idle").unwrap();
        assert_eq!(executions.tests.len(), 1);
        let record = &executions.tests[0];
        assert_eq!(record.test.file, "tests/app.test.js");
        assert_eq!(record.test.name, "test");
        assert!(record.passed);
        assert_eq!(record.files["src/app.js"], vec![work]);
        assert_eq!(
            executions.probed["src/app.js"],
            vec![work],
            "idle holds no probe in this run"
        );
        let _ = idle;

        let names = |value: &Value, key: &str| {
            value[key]
                .as_array()
                .unwrap()
                .iter()
                .map(|t| t["name"].as_str().unwrap().to_owned())
                .collect::<Vec<_>>()
        };
        // Nothing changed.
        let report = affected_tests(&root, &run).unwrap();
        assert!(names(&report, "affected").is_empty(), "{report}");
        assert_eq!(names(&report, "unaffected"), ["test"]);
        assert_eq!(report["summary"]["changedFiles"], 0);
        // A comment: still nothing.
        fs::write(root.join("src/app.js"), format!("// about\n{app}")).unwrap();
        let report = affected_tests(&root, &run).unwrap();
        assert!(names(&report, "affected").is_empty(), "{report}");
        assert_eq!(report["changedFiles"][0]["change"], "formatting");
        // A body the test ran: affected, and it says which.
        fs::write(
            root.join("src/app.js"),
            app.replace("return 1;", "return 1 + 0;"),
        )
        .unwrap();
        let report = affected_tests(&root, &run).unwrap();
        assert_eq!(names(&report, "affected"), ["test"]);
        assert_eq!(
            report["affected"][0]["reasons"][0],
            "src/app.js: work (line 1) changed (this test ran it)"
        );
        // A body the test did not run, once idle has a probe of its own: not
        // affected. Here idle holds none, so the change is not one that can
        // be said to have missed the test, and it counts.
        fs::write(
            root.join("src/app.js"),
            app.replace("return 2;", "return 2 + 0;"),
        )
        .unwrap();
        let report = affected_tests(&root, &run).unwrap();
        assert_eq!(names(&report, "affected"), ["test"], "{report}");
        assert!(
            report["affected"][0]["reasons"][0]
                .as_str()
                .unwrap()
                .contains("this test ran code in this file"),
            "{report}"
        );
        // A declaration added: affected, the file's shape changed.
        fs::write(
            root.join("src/app.js"),
            format!("{app}function more() {{}}\n"),
        )
        .unwrap();
        let report = affected_tests(&root, &run).unwrap();
        assert_eq!(names(&report, "affected"), ["test"]);
        assert_eq!(report["changedFiles"][0]["change"], "declarations");
        // The test file itself.
        fs::write(root.join("src/app.js"), app).unwrap();
        fs::write(root.join("tests/app.test.js"), test.replace("1)", "2)")).unwrap();
        let report = affected_tests(&root, &run).unwrap();
        assert_eq!(names(&report, "affected"), ["test"]);
        assert!(
            report["affected"][0]["reasons"][0]
                .as_str()
                .unwrap()
                .starts_with("test file changed"),
            "{report}"
        );
        // Top-level code in a file the test names but never ran a statement
        // of: outside the JVM, not affected (a JVM class's static initializer
        // is the case the naming rule is for).
        fs::write(root.join("tests/app.test.js"), test).unwrap();
        fs::write(root.join("src/config.js"), config.replace("= 5", "= 6")).unwrap();
        let report = affected_tests(&root, &run).unwrap();
        assert!(names(&report, "affected").is_empty(), "{report}");
        fs::write(root.join("src/config.js"), config).unwrap();
        // A source file removed.
        fs::remove_file(root.join("src/app.js")).unwrap();
        let report = affected_tests(&root, &run).unwrap();
        assert_eq!(
            report["affected"][0]["reasons"][0],
            "src/app.js removed (this test ran code in it)"
        );
        fs::remove_dir_all(root).unwrap();
    }

    /// h11's `test_receivebuffer.py` for `_receivebuffer.py`, commons-cli's
    /// `ConverterTests#testClassDoesNotInitialize` for `Converter.java`.
    #[test]
    fn a_test_named_like_the_changed_file_comes_first() {
        assert_eq!(
            words_of("h11/tests/test_receive_buffer.py"),
            BTreeSet::from(["receive".to_owned(), "buffer".to_owned()])
        );
        assert_eq!(
            words_of("src/test/java/org/apache/commons/cli/ConverterTests.java"),
            BTreeSet::from(["converter".to_owned()])
        );
        assert_eq!(
            split_words("parseHTTPVersion_v2"),
            ["parse", "httpversion", "v2"]
        );
        let changed = words_of("src/main/java/org/apache/commons/cli/Converter.java");
        let entry = |file: &str, name: &str| json!({"file": file, "name": name});
        assert_eq!(
            name_match(
                &entry(
                    "src/test/java/x/ConverterTests.java",
                    "x.ConverterTests#testClass()"
                ),
                &changed
            ),
            1
        );
        assert_eq!(
            name_match(
                &entry(
                    "src/test/java/x/OptionTests.java",
                    "x.OptionTests#testConverter()"
                ),
                &changed
            ),
            1,
            "the test's own name counts too"
        );
        assert_eq!(
            name_match(
                &entry(
                    "src/test/java/x/OptionTests.java",
                    "x.OptionTests#testBuilder()"
                ),
                &changed
            ),
            0
        );
    }

    /// apache/commons-cli's `Converter.CLASS`: a static field set in a
    /// static initializer credited to whichever test loaded the class first.
    /// Its tests are reached by the class's name; a JavaScript module's top
    /// level is not.
    #[test]
    fn a_jvm_class_changed_in_its_declarations_is_reached_by_its_name() {
        let java = "package app;\nclass Converter {\n    static final Object CLASS = null;\n    static int size(int n) {\n        return n;\n    }\n}\n";
        let before = FileFingerprint::read("src/app/Converter.java", java);
        let after = FileFingerprint::read(
            "src/app/Converter.java",
            &java.replace("CLASS = null", "CLASS = new Object()"),
        );
        let js = "export const LIMIT = 5;\n";
        let js_before = FileFingerprint::read("src/config.js", js);
        let js_after = FileFingerprint::read("src/config.js", &js.replace('5', "6"));
        let changes = BTreeMap::from([
            (
                "src/app/Converter.java",
                file_change(Some(&before), Some(&after), Some(&[])).unwrap(),
            ),
            (
                "src/config.js",
                file_change(Some(&js_before), Some(&js_after), Some(&[])).unwrap(),
            ),
        ]);
        let named = named_by_tests(&changes);
        assert_eq!(
            named.get("src/app/Converter.java"),
            Some(&BTreeSet::from(["Converter".to_owned()]))
        );
        assert!(!named.contains_key("src/config.js"));
        assert!(names_word("assertEquals(Converter.CLASS, x)", "Converter"));
        assert!(!names_word("ConverterTests", "Converter"));
    }

    #[test]
    fn a_test_that_ran_beside_others_is_undetermined_rather_than_unaffected() {
        // A parallel suite's coverage sits in the record nobody could claim,
        // and the tests beside it hold nothing of their own. Reading "this
        // test's record does not mention the changed file" as "this test is
        // unaffected" would answer `0 of 1 tests affected` for a change to the
        // only code the run executed -- and an agent running only the affected
        // tests would run nothing. The run's own bound is what stands in.
        let root = std::env::temp_dir().join(format!(
            "supercov-undetermined-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("src")).unwrap();
        fs::create_dir_all(root.join("tests")).unwrap();
        let app = "function work() {\n  return 1;\n}\nfunction idle() {\n  return 2;\n}\n";
        let test = "import assert from 'node:assert/strict';\nimport { LIMIT } from '../src/config.js';\nassert.equal(work(), 1);\n";
        let config = "export const LIMIT = 5;\nfunction check(n) {\n  return n < LIMIT;\n}\n";
        fs::write(root.join("src/app.js"), app).unwrap();
        fs::write(root.join("src/config.js"), config).unwrap();
        fs::write(root.join("tests/app.test.js"), test).unwrap();
        let inputs = crate::source_capture::capture(
            &root,
            "python",
            ["src/app.js".into(), "tests/app.test.js".into()],
        )
        .unwrap();
        let directory = crate::run_store::create_run_wide_test_run(&root, "parallel");
        let path = directory.join("evidence.raw.gz");
        let entries = crate::source_capture::append(read_archive(&path).unwrap(), &inputs).unwrap();
        let archive = write_archive(entries, &path).unwrap();
        let metadata_path = directory.join("run.json");
        let mut metadata: RunMetadata =
            serde_json::from_slice(&fs::read(&metadata_path).unwrap()).unwrap();
        metadata.raw_evidence.files = archive.files;
        metadata.raw_evidence.compressed_bytes = archive.compressed_bytes;
        metadata.raw_evidence.uncompressed_bytes = archive.uncompressed_bytes;
        fs::write(&metadata_path, serde_json::to_vec(&metadata).unwrap()).unwrap();
        prepare_publication(&root, &directory, &metadata, None).unwrap();
        let run = discover_runs(&root).unwrap().runs.remove(0);

        let executions = &recorded(&run);
        assert_eq!(executions.tests.len(), 1, "the named test, not the record");
        let record = &executions.tests[0];
        assert_eq!(
            record.attribution,
            crate::coverage_report::ATTRIBUTION_RUN_WIDE
        );
        assert!(
            record.files.is_empty(),
            "it is credited with nothing: {:?}",
            record.files
        );
        assert!(
            !executions.covered["src/app.js"].is_empty(),
            "and the run is credited with what it reached"
        );

        let bucket = |value: &Value, key: &str| {
            value[key]
                .as_array()
                .unwrap()
                .iter()
                .map(|t| t["name"].as_str().unwrap().to_owned())
                .collect::<Vec<_>>()
        };

        // Nothing changed: nothing to rerun, and the run's bound says nothing
        // either.
        let report = affected_tests(&root, &run).unwrap();
        assert_eq!(bucket(&report, "unaffected"), ["test"], "{report}");
        assert!(bucket(&report, "undetermined").is_empty(), "{report}");

        // The one function the run executed changed. The test's own record
        // cannot say whether it reached it, and the run's can.
        fs::write(
            root.join("src/app.js"),
            app.replace("return 1;", "return 3;"),
        )
        .unwrap();
        let report = affected_tests(&root, &run).unwrap();
        assert!(
            bucket(&report, "affected").is_empty(),
            "nothing proves it ran the change: {report}"
        );
        assert_eq!(
            bucket(&report, "undetermined"),
            ["test"],
            "the run reached it, so this test might have: {report}"
        );
        assert_eq!(
            report["undetermined"][0]["attribution"],
            crate::coverage_report::ATTRIBUTION_RUN_WIDE,
            "{report}"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_lower_bound_still_proves_what_it_recorded() {
        // Ruby's is the other incomplete attribution: the test is credited
        // with coverage, and that coverage is a floor rather than the whole of
        // what it ran. The floor is still evidence -- a test credited with the
        // changed line ran the changed line -- so it is affected outright,
        // with its own reason, rather than swept into the bucket for tests
        // nothing can say anything about.
        let root = std::env::temp_dir().join(format!(
            "supercov-partial-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("src")).unwrap();
        fs::create_dir_all(root.join("tests")).unwrap();
        let app = "function work() {\n  return 1;\n}\nfunction idle() {\n  return 2;\n}\n";
        let test = "import assert from 'node:assert/strict';\nimport { LIMIT } from '../src/config.js';\nassert.equal(work(), 1);\n";
        let config = "export const LIMIT = 5;\nfunction check(n) {\n  return n < LIMIT;\n}\n";
        fs::write(root.join("src/app.js"), app).unwrap();
        fs::write(root.join("src/config.js"), config).unwrap();
        fs::write(root.join("tests/app.test.js"), test).unwrap();
        let inputs = crate::source_capture::capture(
            &root,
            "python",
            ["src/app.js".into(), "tests/app.test.js".into()],
        )
        .unwrap();
        let directory = crate::run_store::create_partial_test_run(&root, "partial");
        let path = directory.join("evidence.raw.gz");
        let entries = crate::source_capture::append(read_archive(&path).unwrap(), &inputs).unwrap();
        let archive = write_archive(entries, &path).unwrap();
        let metadata_path = directory.join("run.json");
        let mut metadata: RunMetadata =
            serde_json::from_slice(&fs::read(&metadata_path).unwrap()).unwrap();
        metadata.raw_evidence.files = archive.files;
        metadata.raw_evidence.compressed_bytes = archive.compressed_bytes;
        metadata.raw_evidence.uncompressed_bytes = archive.uncompressed_bytes;
        fs::write(&metadata_path, serde_json::to_vec(&metadata).unwrap()).unwrap();
        prepare_publication(&root, &directory, &metadata, None).unwrap();
        let run = discover_runs(&root).unwrap().runs.remove(0);

        let executions = recorded(&run);
        let record = &executions.tests[0];
        assert_eq!(
            record.attribution,
            crate::coverage_report::ATTRIBUTION_PARTIAL
        );
        assert!(
            !record.files.is_empty(),
            "a lower bound is coverage it was credited with"
        );

        let names = |value: &Value, key: &str| {
            value[key]
                .as_array()
                .unwrap()
                .iter()
                .map(|t| t["name"].as_str().unwrap().to_owned())
                .collect::<Vec<_>>()
        };
        fs::write(
            root.join("src/app.js"),
            app.replace("return 1;", "return 3;"),
        )
        .unwrap();
        let report = affected_tests(&root, &run).unwrap();
        assert_eq!(
            names(&report, "affected"),
            ["test"],
            "what its own record proves, it proves: {report}"
        );
        assert!(names(&report, "undetermined").is_empty(), "{report}");
        // And the reason is its own, not the run's stand-in.
        let reasons = report["affected"][0]["reasons"].as_array().unwrap();
        assert_eq!(reasons.len(), 1, "{report}");
        let reason = reasons[0].as_str().unwrap();
        assert!(
            reason.contains("this test ran it"),
            "the reason is its own: {report}"
        );
        assert!(
            !reason.contains("the run covered"),
            "and not the run's stand-in: {report}"
        );
        fs::remove_dir_all(root).unwrap();
    }
}
