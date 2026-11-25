//! Atomic, integrity-checked merging of independently executed run shards.

use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

use serde_json::Value;
use supercov_contracts::FrontendRunDeclaration;

use crate::{
    evidence_archive::{EvidenceArchiveEntry, read_archive, read_archive_selected, write_archive},
    lifecycle::{
        Archived, ProjectLock, finalize_published_run, publish_archived_run,
        remove_stored_tree_deferred,
    },
    run_store::{RawEvidenceMetadata, RunInventory, RunMetadata, StoredRun, discover_runs},
    strong_plan::StrongVerdicts,
};

/// A record's top-level entries as they were written, in their order: the
/// scopes a merge rewrites are almost all at the top, and the browser events
/// beside them, most of a record, need not be parsed and written again.
struct TopLevel<'a>(Vec<(String, &'a serde_json::value::RawValue)>);

impl<'de> serde::Deserialize<'de> for TopLevel<'de> {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Entries;
        impl<'de> serde::de::Visitor<'de> for Entries {
            type Value = TopLevel<'de>;
            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                formatter.write_str("an object")
            }
            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                mut map: A,
            ) -> Result<Self::Value, A::Error> {
                let mut entries: Vec<(String, &'de serde_json::value::RawValue)> = Vec::new();
                while let Some((key, value)) =
                    map.next_entry::<String, &serde_json::value::RawValue>()?
                {
                    // As a parsed object keeps a repeated key: where it first
                    // was, with the last value.
                    match entries.iter_mut().find(|(seen, _)| *seen == key) {
                        Some(entry) => entry.1 = value,
                        None => entries.push((key, value)),
                    }
                }
                Ok(TopLevel(entries))
            }
        }
        deserializer.deserialize_map(Entries)
    }
}

/// A record with every scope's run id rewritten, as `rewrite_scope_run_ids`
/// rewrites its parsed value; each top-level entry that holds no scope is
/// written as it was.
///
/// Parsing a scope again can fail where reading the record did not: the
/// record's entries are read as raw text, which nests without a limit, and a
/// value parsed from them stops at serde_json's recursion limit.
fn rewritten_record(
    entries: &TopLevel<'_>,
    merged_run_id: &str,
) -> Result<Vec<u8>, serde_json::Error> {
    const WRITES: &str = "JSON writes to memory";
    let mut written =
        Vec::with_capacity(entries.0.iter().map(|(_, raw)| raw.get().len() + 16).sum());
    written.push(b'{');
    for (index, (key, raw)) in entries.0.iter().enumerate() {
        if index > 0 {
            written.push(b',');
        }
        serde_json::to_writer(&mut written, key).expect(WRITES);
        written.push(b':');
        let text = raw.get();
        if key == "scope" && text.starts_with('{') {
            let mut scope = serde_json::from_str::<serde_json::Map<String, Value>>(text)?;
            scope.insert("runId".into(), Value::String(merged_run_id.into()));
            serde_json::to_writer(&mut written, &scope).expect(WRITES);
        } else if text.contains("\"scope\"") {
            let mut value = serde_json::from_str::<Value>(text)?;
            rewrite_scope_run_ids(&mut value, merged_run_id);
            serde_json::to_writer(&mut written, &value).expect(WRITES);
        } else {
            written.extend_from_slice(text.as_bytes());
        }
    }
    written.push(b'}');
    Ok(written)
}

fn rewrite_scope_run_ids(value: &mut Value, merged_run_id: &str) {
    match value {
        Value::Array(values) => {
            for value in values {
                rewrite_scope_run_ids(value, merged_run_id);
            }
        }
        Value::Object(object) => {
            for (key, value) in object {
                if key == "scope" && value.is_object() {
                    value
                        .as_object_mut()
                        .expect("scope was checked as an object")
                        .insert("runId".into(), Value::String(merged_run_id.into()));
                } else {
                    rewrite_scope_run_ids(value, merged_run_id);
                }
            }
        }
        _ => {}
    }
}

/// The tests a run's archive holds a record of.
///
/// Each entry is read on its own, on every core: a merge reads every run's,
/// and a run of a whole browser suite holds hundreds of megabytes of them.
fn measured_tests(archive: &[EvidenceArchiveEntry]) -> BTreeMap<String, String> {
    let named = crate::threads::parallel_map(archive, |entry| {
        let mut named = Vec::new();
        let Ok(text) = std::str::from_utf8(&entry.contents) else {
            return named;
        };
        for line in text.lines() {
            if !line.contains("\"testId\"") {
                continue;
            }
            if let Ok(value) = serde_json::from_str::<Value>(line)
                && let (Some(id), Some(name)) = (value["testId"].as_str(), value["test"].as_str())
            {
                named.push((id.to_owned(), name.to_owned()));
            }
        }
        named
    });
    // In the archive's order, so a test named twice keeps its last name.
    named.into_iter().flatten().collect()
}

/// An entry's records for the merge, without those of the tests measured
/// again in another run. None when nothing of the entry is left.
pub(crate) fn rewritten_contents(
    contents: &[u8],
    merged_run_id: &str,
    superseded: Option<&BTreeSet<String>>,
) -> Result<Option<Vec<u8>>, String> {
    Ok(rewritten_records(contents, merged_run_id, superseded)?.contents(|_| true))
}

/// An entry's records rewritten for the merge, each with the test it is of:
/// what `rewritten_contents` writes, before the merge leaves out a run's
/// copy of a test another run holds alike.
struct Rewritten {
    /// Every record kept, one after another.
    text: Vec<u8>,
    /// Each record's test, an index into `tests`, and its bytes in `text`.
    records: Vec<(Option<usize>, std::ops::Range<usize>)>,
    tests: Vec<String>,
    /// Whether a superseded test's records were left out.
    left_out: bool,
    /// Whether the entry ended with a newline.
    ended: bool,
}

impl Rewritten {
    /// The entry with the records `keep` keeps, given each one's test, or
    /// None for a record of no test. None when nothing of the entry is left.
    fn contents(&self, keep: impl Fn(Option<&str>) -> bool) -> Option<Vec<u8>> {
        let mut written = Vec::with_capacity(self.text.len());
        let mut kept = false;
        let mut left_out = self.left_out;
        for (test, bytes) in &self.records {
            if !keep(test.map(|test| self.tests[test].as_str())) {
                left_out = true;
                continue;
            }
            if kept {
                written.push(b'\n');
            }
            kept = true;
            written.extend_from_slice(&self.text[bytes.clone()]);
        }
        if self.ended && kept {
            written.push(b'\n');
        }
        (kept || !left_out).then_some(written)
    }
}

fn rewritten_records(
    contents: &[u8],
    merged_run_id: &str,
    superseded: Option<&BTreeSet<String>>,
) -> Result<Rewritten, String> {
    let contents = std::str::from_utf8(contents)
        .map_err(|_| "coverage evidence contains non-UTF-8 data".to_owned())?;
    let mut rewritten = Rewritten {
        text: Vec::with_capacity(contents.len()),
        records: Vec::new(),
        tests: Vec::new(),
        left_out: false,
        ended: contents.ends_with('\n'),
    };
    let mut indexes = std::collections::HashMap::<String, usize>::new();
    // A compact attempt file names its scope on a writer's first line only:
    // a line without one is of the test of the line before it.
    let mut current_test: Option<String> = None;
    for line in contents.split('\n') {
        if line.is_empty() {
            continue;
        }
        // An object, nearly every record: its entries as written, its scope
        // and test read from them alone. Anything else holds no scope of its
        // own and is of the test of the line before it.
        let (test, record) = match serde_json::from_str::<TopLevel<'_>>(line) {
            Ok(entries) => {
                let entry = |name: &str| {
                    entries
                        .0
                        .iter()
                        .find(|(key, _)| key == name)
                        .map(|(_, raw)| raw.get())
                };
                let scope_test = entry("scope").and_then(|scope| {
                    serde_json::from_str::<Value>(scope)
                        .ok()
                        .and_then(|scope| scope["testId"].as_str().map(str::to_owned))
                });
                let own_test = entry("testId")
                    .and_then(|test| serde_json::from_str::<Value>(test).ok())
                    .and_then(|test| test.as_str().map(str::to_owned));
                if entry("scope").is_some() {
                    current_test = own_test.clone().or_else(|| scope_test.clone());
                }
                let test = own_test.or(scope_test).or_else(|| current_test.clone());
                (test, Ok(entries))
            }
            Err(_) => {
                let value = serde_json::from_str::<Value>(line).map_err(|error| {
                    format!("cannot merge malformed recognized JSON evidence: {error}")
                })?;
                (current_test.clone(), Err(value))
            }
        };
        if superseded.is_some_and(|tests| test.as_ref().is_some_and(|test| tests.contains(test))) {
            rewritten.left_out = true;
            continue;
        }
        let start = rewritten.text.len();
        match record {
            Ok(entries) => {
                rewritten
                    .text
                    .extend(rewritten_record(&entries, merged_run_id).map_err(|error| {
                        format!("cannot merge malformed recognized JSON evidence: {error}")
                    })?);
            }
            Err(mut value) => {
                rewrite_scope_run_ids(&mut value, merged_run_id);
                rewritten
                    .text
                    .extend(serde_json::to_vec(&value).expect("plain data serializes"));
            }
        }
        let test = test.map(|test| {
            *indexes.entry(test).or_insert_with_key(|test| {
                rewritten.tests.push(test.clone());
                rewritten.tests.len() - 1
            })
        });
        rewritten.records.push((test, start..rewritten.text.len()));
    }
    Ok(rewritten)
}

/// Of one run, what another run earlier in the merge holds byte for byte:
/// a test's records, or the records of no test in one of its entries.
#[derive(Default)]
struct HeldAlike {
    tests: BTreeSet<String>,
    /// Entries, by path, whose records of no test are alike.
    untested: BTreeSet<String>,
}

impl HeldAlike {
    fn len(&self) -> usize {
        self.tests.len() + self.untested.len()
    }
}

/// Of each run, the records another run, earlier in the merge, holds byte
/// for byte (`jobs` and `rewritten` in the merge's order, each entry's
/// records as the merge writes them): a run made from another without
/// measuring, or a test measured alike twice. That copy is left out, and
/// every copy that differs in anything is kept: a test whose path depends on
/// the time keeps each path it took. A run's copy of a test is its records
/// in its entries' order, with the entry each is in; the records of no test
/// (a server's background) are a copy of their entry's.
fn copies_held_alike(
    jobs: &[(usize, &EvidenceArchiveEntry)],
    rewritten: &[Option<&Rewritten>],
    runs: usize,
) -> Vec<HeldAlike> {
    use sha2::Digest;
    // Of a test (`true`) or of an entry's records of no test.
    type Key<'a> = (bool, &'a str);
    // Each run's copies, hashed on every core, a run at a time.
    let hashed = crate::threads::parallel_map(&(0..runs).collect::<Vec<_>>(), |run| {
        let mut copies = BTreeMap::<Key<'_>, sha2::Sha256>::new();
        for ((shard, entry), rewritten) in jobs.iter().zip(rewritten) {
            let Some(rewritten) = rewritten.filter(|_| shard == run) else {
                continue;
            };
            for (test, bytes) in &rewritten.records {
                let key = match test {
                    Some(test) => (true, rewritten.tests[*test].as_str()),
                    None => (false, entry.path.as_str()),
                };
                let copy = copies.entry(key).or_default();
                // Lengths first, so no two different sequences of paths and
                // records hash alike.
                for part in [entry.path.as_bytes(), &rewritten.text[bytes.clone()]] {
                    copy.update((part.len() as u64).to_le_bytes());
                    copy.update(part);
                }
            }
        }
        copies
            .into_iter()
            .map(|(key, copy)| (key, <[u8; 32]>::from(copy.finalize())))
            .collect::<Vec<_>>()
    });
    let mut held = BTreeMap::<Key<'_>, BTreeSet<[u8; 32]>>::new();
    hashed
        .into_iter()
        .map(|copies| {
            let mut alike = HeldAlike::default();
            for (key, copy) in copies {
                if held.entry(key).or_default().insert(copy) {
                    continue;
                }
                match key {
                    (true, test) => alike.tests.insert(test.to_owned()),
                    (false, path) => alike.untested.insert(path.to_owned()),
                };
            }
            alike
        })
        .collect()
}

fn merged_path(path: &str, shard: usize) -> String {
    if path.starts_with("server/background/") {
        return format!(
            "server/background/shard-{shard}-{}",
            path.rsplit('/').next().unwrap_or(path)
        );
    }
    if let Some(path) = path.strip_prefix("server/") {
        return format!("server/shard-{shard}/{path}");
    }
    format!("shards/{shard}/{path}")
}

fn incompatible_dimensions(first: &StoredRun, candidate: &StoredRun) -> Vec<&'static str> {
    let first = &first.metadata.integrity;
    let candidate = &candidate.metadata.integrity;
    let mut dimensions = Vec::new();
    if candidate.schema_version != first.schema_version {
        dimensions.push("coverage schema");
    }
    if candidate.instrumenter_version != first.instrumenter_version
        || candidate.fingerprint.instrumenter != first.fingerprint.instrumenter
    {
        dimensions.push("instrumenter");
    }
    if candidate.fingerprint.source != first.fingerprint.source {
        dimensions.push("source");
    }
    if candidate.fingerprint.tests != first.fingerprint.tests {
        dimensions.push("tests");
    }
    if candidate.fingerprint.dependencies != first.fingerprint.dependencies {
        dimensions.push("dependencies");
    }
    if candidate.fingerprint.configuration != first.fingerprint.configuration {
        dimensions.push("configuration");
    }
    // With every domain alike, what is left is how each was run: the test
    // command, for the languages whose fingerprint holds it. Commands that
    // differ are what shards are.
    if dimensions.is_empty()
        && candidate.fingerprint.combined != first.fingerprint.combined
        && candidate.fingerprint.execution == first.fingerprint.execution
    {
        dimensions.push("integrity fingerprint");
    }
    dimensions
}

/// The files a run was recorded with, from its archive.
fn recorded_files(
    archive: &[EvidenceArchiveEntry],
) -> Option<(&[u8], crate::source_manifest::InputManifest)> {
    let entry = archive
        .iter()
        .find(|entry| entry.path == crate::source_capture::ARCHIVE_PATH)?;
    Some((
        entry.contents.as_slice(),
        serde_json::from_slice(&entry.contents).expect("a run's captured sources are JSON"),
    ))
}

/// Each file's bytes as each run recorded them, where it did.
fn recorded_hashes(archives: &[Vec<EvidenceArchiveEntry>]) -> Vec<BTreeMap<String, String>> {
    archives
        .iter()
        .map(|archive| {
            recorded_files(archive)
                .map(|(_, manifest)| {
                    manifest
                        .files
                        .into_iter()
                        .map(|(file, fingerprint)| (file, fingerprint.sha256))
                        .collect::<BTreeMap<_, _>>()
                })
                .unwrap_or_default()
        })
        .collect()
}

/// Of each file the merged run lists mutants of, the run whose list of it
/// the merge took.
type Owners = BTreeMap<String, usize>;

/// The merged run's one `mutants.json`, from each run's (`base` the run
/// whose manifest the merge takes), and of each file listed, the run whose
/// list of it the merge took: the mutants of every file a run listed,
/// each file's from the first run that listed it, `base` first. Runs that
/// recorded a file with the same bytes and listed it differently, or where
/// one listed it and the other did not, did not enumerate the same mutants,
/// and are not merged. A run that listed none (its probes off, or recorded
/// before they were) lends its tests, whose hits say what ran and whose
/// values were not recorded: `testsWithoutValues`.
fn merged_mutants(
    inputs: &[&StoredRun],
    archives: &[Vec<EvidenceArchiveEntry>],
    hashes: &[BTreeMap<String, String>],
    base: usize,
) -> Result<(Option<Vec<u8>>, Owners), String> {
    use crate::mutant_list::MutantsFile;
    const NAME: &str = "mutants.json";
    let mut listed = Vec::new();
    let mut unlisted = Vec::new();
    let mut first_file = None;
    for index in std::iter::once(base).chain((0..archives.len()).filter(|index| *index != base)) {
        let entry = archives[index].iter().find(|entry| entry.path == NAME);
        let file = entry
            .map(|entry| {
                serde_json::from_slice::<MutantsFile>(&entry.contents).map_err(|error| {
                    format!(
                        "Cannot merge coverage run {}: its {NAME} cannot be read: {error}",
                        inputs[index].id
                    )
                })
            })
            .transpose()?;
        if first_file.is_none() {
            first_file = entry.map(|entry| entry.contents.clone());
        }
        match file {
            Some(file) if file.mutant_set.is_some() => listed.push((index, file)),
            _ => unlisted.push(index),
        }
    }
    let Some((first, others)) = listed.split_first() else {
        // No run listed mutants: the merge lists none either, as a run
        // measured with them off says.
        return Ok((first_file, BTreeMap::new()));
    };
    fn ids(mutants: &[crate::js_mutant_links::CandidateMutant]) -> Vec<&str> {
        mutants.iter().map(|mutant| mutant.id.as_str()).collect()
    }
    let mut files = BTreeMap::<String, (usize, Vec<_>)>::new();
    let mut without_values = BTreeSet::new();
    // Each language's mutant set, from the first run that listed it: a run
    // that listed the same language with another set did not enumerate the
    // same mutants.
    let mut sets = BTreeMap::<String, (usize, String)>::new();
    let mut languages = BTreeMap::<String, String>::new();
    for (index, file) in std::iter::once(first).chain(others) {
        // A listed run lists at least one language's set.
        file.sets().into_iter().try_for_each(|(language, set)| {
            match sets.get(&language) {
                Some((kept, kept_set)) if *kept_set != set => {
                    return Err(format!(
                        "Cannot merge coverage run {}: it listed the mutants of {set}, and run {} those of {kept_set}",
                        inputs[*index].id, inputs[*kept].id,
                    ));
                }
                Some(_) => {}
                None => {
                    sets.insert(language, (*index, set));
                }
            }
            Ok(())
        })?;
        for (path, language) in &file.languages {
            languages
                .entry(path.clone())
                .or_insert_with(|| language.clone());
        }
        without_values.extend(file.tests_without_values.iter().cloned());
        for (path, mutants) in &file.files {
            match files.get(path) {
                Some((kept, kept_mutants))
                    if hashes[*kept].get(path) == hashes[*index].get(path)
                        && ids(kept_mutants) != ids(mutants) =>
                {
                    return Err(format!(
                        "Cannot merge coverage runs {} and {}: they listed different mutants of {path}, recorded with the same bytes",
                        inputs[*kept].id, inputs[*index].id
                    ));
                }
                Some(_) => {}
                None => {
                    files.insert(path.clone(), (*index, mutants.clone()));
                }
            }
        }
    }
    // A file one run listed and another, which recorded it with the same
    // bytes, did not.
    for (index, file) in std::iter::once(first).chain(others) {
        for (path, (kept, _)) in &files {
            let same = hashes[*index]
                .get(path)
                .is_some_and(|hash| hashes[*kept].get(path) == Some(hash));
            if same && !file.files.contains_key(path) {
                let (kept, missing) = (&inputs[*kept].id, &inputs[*index].id);
                return Err(format!(
                    "Cannot merge coverage runs {kept} and {missing}: {kept} listed the mutants of {path}, and {missing}, which recorded it with the same bytes, did not"
                ));
            }
        }
    }
    for index in unlisted {
        without_values.extend(measured_tests(&archives[index]).into_keys());
    }
    // Each language's files under its set; one run listed, so one set at
    // least.
    let mut merged = sets
        .into_iter()
        .map(|(language, (_, set))| {
            let files = files
                .iter()
                .filter(|(path, _)| {
                    languages
                        .get(*path)
                        .map_or(crate::mutant_list::JAVASCRIPT, String::as_str)
                        == language
                })
                .map(|(path, (_, mutants))| (path.clone(), mutants.clone()))
                .collect::<BTreeMap<_, _>>();
            MutantsFile::listed(&language, &set, files)
        })
        .fold(MutantsFile::unlisted(), |merged, one| {
            merged.merge(one).expect("each language is merged once")
        });
    merged.tests_without_values = without_values.into_iter().collect();
    let owners = files
        .into_iter()
        .map(|(path, (owner, _))| (path, owner))
        .collect();
    Ok((
        Some(serde_json::to_vec(&merged).expect("plain data serializes")),
        owners,
    ))
}

/// Runs merged although their tests or configuration differ: the latest,
/// whose record of the files and integrity the merge takes, and what differed
/// that does not reach the others.
struct Grown {
    latest: usize,
    files: Option<Vec<u8>>,
    across: Vec<String>,
    /// Of each run, the tests whose record a changed support file made out
    /// of date and that another of the runs measured again: their old
    /// records are left out.
    superseded: BTreeMap<usize, BTreeSet<String>>,
}

/// Which of the runs the others can be added to, where their tests or test
/// configuration differ: the newest, when nothing that changed between each
/// of the others and it reaches what that run's tests did. That is a suite
/// that has gained tests since, with helpers they need and settings of
/// theirs, and what an earlier run recorded of its tests is still what they
/// do. Each file that differs is judged by `run_currency::judge`.
fn judged_against_latest(
    root: &Path,
    inputs: &[&StoredRun],
    archives: &[Vec<EvidenceArchiveEntry>],
    known: &[StoredRun],
    allow_changed: bool,
) -> Result<Option<Grown>, String> {
    let first = inputs[0];
    let fingerprint = |input: &StoredRun| input.metadata.integrity.fingerprint.clone();
    // Files no fingerprint covers -- a mock's data, read by a server the
    // tests reached -- are in each run's record of its files.
    let recorded =
        |index: usize| recorded_files(&archives[index]).map(|(_, manifest)| manifest.files);
    let differ = inputs.iter().any(|input| {
        fingerprint(input).tests != fingerprint(first).tests
            || fingerprint(input).configuration != fingerprint(first).configuration
            || fingerprint(input).source != fingerprint(first).source
    }) || (1..inputs.len()).any(|index| recorded(index) != recorded(0));
    if !differ {
        return Ok(None);
    }
    let latest = (0..inputs.len())
        .max_by(|left, right| {
            inputs[*left]
                .metadata
                .started_at
                .cmp(&inputs[*right].metadata.started_at)
        })
        .expect("at least two runs");
    let files = archives
        .iter()
        .map(|archive| recorded_files(archive))
        .collect::<Vec<_>>();
    // Without a record of the files, the domains are all there is to say;
    // where another run holds one, the record itself is one of them.
    let unrecorded = |run: &StoredRun| {
        let other = inputs
            .iter()
            .find(|other| other.id != run.id)
            .expect("at least two runs");
        let mut dimensions = incompatible_dimensions(other, run);
        if files.iter().any(Option::is_some) {
            dimensions.push("recorded files");
        }
        format!(
            "Cannot merge incompatible run {}: differing domains: {}",
            run.id,
            dimensions.join(", ")
        )
    };
    let Some((_, into)) = &files[latest] else {
        return Err(unrecorded(inputs[latest]));
    };
    // A file only an earlier run recorded -- a mock no test of the latest
    // reached -- is not gone: it is judged by what the checkout holds now,
    // and the merge's record keeps it.
    let mut into = into.clone();
    // The latest run's record is one of them.
    files.iter().flatten().for_each(|(_, recorded)| {
        for file in recorded.files.keys() {
            if into.files.contains_key(file) {
                continue;
            }
            if let Ok(text) = crate::fault_fs::read_to_string(root.join(file)) {
                into.files.insert(
                    file.clone(),
                    crate::source_manifest::FileFingerprint::of(&text),
                );
            }
        }
    });
    let into = &into;
    // Every file known to hold tests, from every run there is: a file of some
    // other test changing takes nothing from this run.
    let tests = known
        .iter()
        .filter_map(crate::source_manifest::test_files)
        .flatten()
        .collect::<BTreeSet<_>>();
    let checkout = crate::run_currency::Checkout::new(root, &tests);
    let merged = |index: usize| inputs[index].metadata.merged == Some(true);
    let reference = crate::run_currency::Ran::from_entries(&archives[latest], merged(latest));
    // A file's text in the checkout; judged only where it is what the
    // latest run recorded.
    let text = |file: &str| crate::fault_fs::read_to_string(root.join(file)).ok();
    let mut across = Vec::new();
    let mut superseded = BTreeMap::new();
    // A merge is of two runs at least: one is not the latest.
    inputs.iter().enumerate().try_for_each(|(index, input)| -> Result<(), String> {
        if index == latest {
            return Ok(());
        }
        let Some((_, recorded)) = &files[index] else {
            return Err(unrecorded(input));
        };
        let judgment = crate::run_currency::judge(
            &checkout,
            &input.metadata.command.join(" "),
            &recorded.files,
            &into.files,
            &text,
            crate::source_manifest::test_files(input).as_ref(),
            &crate::run_currency::Ran::from_entries(&archives[index], merged(index)),
            reference.playwright.as_ref(),
            allow_changed,
        );
        // Tests a changed support file reached: kept out of this run's part
        // when another run measured them again, else the run cannot be
        // merged until they are.
        if !judgment.reached.is_empty() {
            let reached = judgment
                .reached
                .values()
                .flatten()
                .cloned()
                .collect::<BTreeSet<_>>();
            let own = measured_tests(&archives[index]);
            let again = archives
                .iter()
                .enumerate()
                .filter(|(other, _)| *other != index)
                .flat_map(|(_, archive)| measured_tests(archive).into_keys())
                .collect::<BTreeSet<_>>();
            let files = judgment.reached.keys().cloned().collect::<Vec<_>>();
            let missing = reached
                .iter()
                .filter(|test| !again.contains(*test))
                .map(|test| own.get(test).cloned().unwrap_or_else(|| test.clone()))
                .collect::<Vec<_>>();
            if !missing.is_empty() {
                return Err(format!(
                    "Cannot merge run {} with run {}: {} changed, and {} of its tests reached {} through a server that served them and were not measured again: {}. Measure them again (`supercov runs {} tests affected --names`), then merge.",
                    input.id,
                    inputs[latest].id,
                    listed(&files),
                    missing.len(),
                    if files.len() == 1 { "it" } else { "them" },
                    listed(&missing),
                    input.id,
                ));
            }
            across.extend(files.iter().map(|file| {
                format!(
                    "{file} (the {} test(s) of `{}` that reached it were measured again)",
                    judgment.reached[file].len(),
                    input.metadata.command.join(" ")
                )
            }));
            superseded.insert(index, reached);
        }
        if !judgment.outdated.is_empty() {
            return Err(format!(
                "Cannot merge run {} with run {}: {}. {}",
                input.id,
                inputs[latest].id,
                listed(&judgment.outdated),
                if judgment.own_tests {
                    "Its own tests changed; measure them again."
                } else if judgment.settled {
                    "Measure it again."
                } else {
                    "Measure it again, or pass --allow-changed if the change alters nothing its tests ran."
                },
            ));
        }
        across.extend(judgment.across);
        Ok(())
    })?;
    Ok(Some(Grown {
        latest,
        files: Some(serde_json::to_vec(into).expect("plain data serializes")),
        across: crate::run_currency::across(across),
        superseded,
    }))
}

/// A Rust Worker no test of a run started is noted on each of its files, in
/// that run alone: a run whose test started it measured the same files under
/// the same denominator. So runs that started different Workers merge, and
/// the note stands in the merge only where no run started the Worker.
const UNRUN_WORKER: &str = "rust-worker-not-run#";

/// Each manifest with the unrun-Worker notes no run shares dropped, and the
/// notes every run shares, which stand.
fn settle_unrun_workers(manifests: &[Option<&[u8]>]) -> (Vec<Option<Vec<u8>>>, BTreeSet<String>) {
    let parsed = manifests
        .iter()
        .map(|manifest| manifest.and_then(|bytes| serde_json::from_slice::<Value>(bytes).ok()))
        .collect::<Vec<_>>();
    let unrun = |manifest: &Value| {
        manifest["limitations"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|limitation| limitation["id"].as_str())
            .filter(|id| id.starts_with(UNRUN_WORKER))
            .map(str::to_owned)
            .collect::<BTreeSet<_>>()
    };
    let notes = parsed.iter().flatten().map(unrun).collect::<Vec<_>>();
    let shared = notes.iter().skip(1).fold(
        notes.first().cloned().unwrap_or_default(),
        |shared, next| shared.intersection(next).cloned().collect(),
    );
    let raw = manifests
        .iter()
        .map(|manifest| manifest.map(<[u8]>::to_vec));
    if notes.iter().all(|note| *note == shared) {
        return (raw.collect(), shared);
    }
    let settled = raw
        .zip(parsed)
        .map(|(raw, parsed)| match parsed {
            Some(mut manifest) => {
                if let Some(limitations) = manifest
                    .get_mut("limitations")
                    .and_then(Value::as_array_mut)
                {
                    limitations.retain(|limitation| {
                        limitation["id"]
                            .as_str()
                            .is_none_or(|id| !id.starts_with(UNRUN_WORKER) || shared.contains(id))
                    });
                    // As a run with none writes it: no list at all.
                    if limitations.is_empty() {
                        manifest
                            .as_object_mut()
                            .map(|fields| fields.remove("limitations"));
                    }
                }
                serde_json::to_vec(&manifest).ok()
            }
            None => raw,
        })
        .collect();
    (settled, shared)
}

/// The language a run measured, from its frontend declaration.
fn measured_language(run: &StoredRun) -> Option<String> {
    let entries = read_archive(&run.evidence_path).ok()?;
    let entry = entries
        .iter()
        .find(|entry| entry.path == "frontend.json")
        .expect("a stored run's archive holds its frontend declaration");
    serde_json::from_slice::<FrontendRunDeclaration>(&entry.contents)
        .ok()
        .map(|declaration| declaration.language)
}

/// Why a run of another Supercov release cannot be merged with this one's.
pub fn recorded_by_another_release(run: &StoredRun) -> String {
    match run.metadata.recorded_by.as_deref() {
        Some(version) if version != env!("CARGO_PKG_VERSION") => format!(
            "it was recorded by Supercov {version}, which instruments differently from Supercov {}",
            env!("CARGO_PKG_VERSION")
        ),
        // Runs say which release recorded them from 4.7.0 on, so one that
        // does not is older; its date is what else it can say.
        None => format!(
            "it was recorded{} by a Supercov older than 4.7.0, which instruments differently from Supercov {}",
            run.metadata
                .started_at
                .get(..10)
                .map(|day| format!(" on {day}"))
                .unwrap_or_default(),
            env!("CARGO_PKG_VERSION")
        ),
        // The same release number instrumenting differently: a build of
        // Supercov's own source between releases.
        Some(version) => format!(
            "it was recorded by another build of Supercov {version}, which instruments differently from this one"
        ),
    }
}

/// A few of what moved, and how many more there are.
pub fn listed(items: &[String]) -> String {
    match items.len() {
        0..=3 => items.join("; "),
        n => format!("{}; and {} more", items[..3].join("; "), n - 3),
    }
}

/// Merge two or more complete, compatible stored runs into one immutable run.
pub fn merge_coverage_runs(
    root: &Path,
    run_ids: &[String],
    merged_run_id: &str,
    started_at: &str,
    allow_changed: bool,
) -> Result<String, String> {
    merge_runs(root, run_ids, merged_run_id, started_at, allow_changed).map(|merged| merged.id)
}

/// A merged run, and what its merge has to say beside it: which strong
/// verdicts it kept of the runs merged, and whose it did not.
pub struct MergedRun {
    pub id: String,
    pub notes: Vec<String>,
}

/// `merge_coverage_runs`, with what the merge has to say.
pub fn merge_runs(
    root: &Path,
    run_ids: &[String],
    merged_run_id: &str,
    started_at: &str,
    allow_changed: bool,
) -> Result<MergedRun, String> {
    if run_ids.len() < 2 {
        return Err("Usage: supercov merge <run-id> <run-id> [...]".into());
    }
    // Copies of one run, each with verdicts another job saved into it, are
    // one directory here: the store holds the copy written last, and the
    // others' verdicts are not in it to add up.
    let mut named = BTreeSet::new();
    if let Some(twice) = run_ids.iter().find(|id| !named.insert(*id)) {
        return Err(format!(
            "Each merged run must be unique: {twice} is named more than once. Copies of one run that several jobs saved verdicts into share its id, and .supercov/runs holds one of them, so they cannot be added up. Each job of a split strong pass needs a run of its own: `supercov mutants --shard k/n -- <command>` measures one (up to 4.29.5, on a checkout unchanged since the runs it restored, it saved into the latest of them instead). Run that round again and merge its jobs' runs"
        ));
    }
    let inventory =
        discover_runs(root).map_err(|error| format!("Cannot read local coverage runs: {error}"))?;
    let inputs = run_ids
        .iter()
        .map(|id| {
            inventory
                .runs
                .iter()
                .find(|run| run.id == *id)
                .ok_or_else(|| {
                    // A run that never finished publishing is named as such;
                    // anything else is simply not here.
                    match inventory.rejected.iter().find(|run| run.entry == *id) {
                        Some(rejected) => {
                            format!("Cannot merge coverage run {id}: {}", rejected.reason)
                        }
                        None => format!("Cannot merge coverage run {id}: no such local run"),
                    }
                })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let first = inputs[0];
    // A merge is of two runs at least.
    inputs.iter().skip(1).try_for_each(|input| -> Result<(), String> {
        let dimensions = incompatible_dimensions(first, input);
        // Another release instruments differently, and nothing else is worth
        // saying about the difference: the run has to be measured again.
        if dimensions
            .iter()
            .any(|dimension| matches!(*dimension, "instrumenter" | "coverage schema"))
        {
            // Each language's instrumenter is its own, so runs of two
            // languages always differ here. Said as another build, a
            // JavaScript run and a Python run of one project were "measured
            // by another build of Supercov" -- the same build, minutes apart.
            if let (Some(first_language), Some(language)) =
                (measured_language(first), measured_language(input))
                && first_language != language
            {
                return Err(format!(
                    "Cannot merge run {}: it measured {language}, and run {} measured {first_language}; runs of different languages are not merged",
                    input.id, first.id
                ));
            }
            let older = [first, *input]
                .into_iter()
                .find(|run| run.metadata.recorded_by.as_deref() != Some(env!("CARGO_PKG_VERSION")))
                .unwrap_or(input);
            return Err(format!(
                "Cannot merge run {}: {}. Measure it again to merge it.",
                older.id,
                recorded_by_another_release(older)
            ));
        }
        // Tests and their configuration may differ; what that takes is
        // looked at file by file.
        // So may the source, where what changed is outside what is measured.
        // A different number of source files is a different source, even
        // where the digest says otherwise.
        let mut dimensions = dimensions;
        if input.metadata.integrity.fingerprint.source_files
            != first.metadata.integrity.fingerprint.source_files
            && !dimensions.contains(&"source")
        {
            dimensions.push("source files");
        }
        if dimensions
            .iter()
            .any(|dimension| !matches!(*dimension, "tests" | "configuration" | "source"))
        {
            return Err(format!(
                "Cannot merge incompatible run {}: differing domains: {}",
                input.id,
                dimensions.join(", ")
            ));
        }
        Ok(())
    })?;

    let archives = inputs
        .iter()
        .map(|input| {
            read_archive(&input.evidence_path)
                .map_err(|error| format!("Cannot merge coverage run {}: {error}", input.id))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let grown = judged_against_latest(root, &inputs, &archives, &inventory.runs, allow_changed)?;
    let (manifests, unrun_workers) = settle_unrun_workers(
        &archives
            .iter()
            .map(|entries| {
                entries
                    .iter()
                    .find(|entry| entry.path == "manifest.json")
                    .map(|entry| entry.contents.as_slice())
            })
            .collect::<Vec<_>>(),
    );
    // Every archive Supercov writes holds its manifest, model and frontend.
    const HELD: &str = "a stored run's archive holds its manifest and coverage model";
    let manifests = manifests
        .iter()
        .map(|manifest| manifest.as_deref().expect(HELD))
        .collect::<Vec<_>>();
    let mut manifest = manifests[0];
    if let Some(Grown { latest, .. }) = grown {
        // A test file added since is one more entry in the scope's list of
        // what is not source. What is measured has to be the same.
        let measured = |manifest: &[u8]| {
            let mut value =
                serde_json::from_slice::<Value>(manifest).expect("a stored manifest is JSON");
            value
                .as_object_mut()
                .expect("a stored manifest is an object")
                .remove("scope");
            value
        };
        let first = measured(manifests[0]);
        if manifests
            .iter()
            .any(|candidate| measured(candidate) != first)
        {
            return Err("Cannot merge runs with different coverage denominators".into());
        }
        manifest = manifests[latest];
    } else if manifests.iter().any(|candidate| *candidate != manifest) {
        return Err("Cannot merge runs with different coverage denominators".into());
    }
    let models = archives
        .iter()
        .map(|entries| {
            entries
                .iter()
                .find(|entry| entry.path == "coverage-model.json")
                .map(|entry| entry.contents.as_slice())
        })
        .collect::<Vec<_>>();
    let model = models[0].expect(HELD);
    if models.iter().any(|candidate| *candidate != Some(model)) {
        return Err("Cannot merge runs with different coverage models".into());
    }
    let declarations = archives
        .iter()
        .map(|entries| {
            let contents = entries
                .iter()
                .find(|entry| entry.path == "frontend.json")
                .expect("a stored run's archive was read with its frontend declaration");
            let mut declaration = serde_json::from_slice::<FrontendRunDeclaration>(
                &contents.contents,
            )
            .map_err(|error| format!("Cannot merge invalid frontend declaration: {error}"))?;
            declaration
                .structural_limitations
                .retain(|id| !id.starts_with(UNRUN_WORKER) || unrun_workers.contains(id));
            Ok::<_, String>(declaration)
        })
        .collect::<Result<Vec<_>, _>>()?;
    let mut merged_declaration = declarations[0].clone();
    let mut runners = BTreeMap::new();
    // A declaration for each run, with a runner at least each.
    declarations.into_iter().try_for_each(|declaration| {
        if declaration.protocol_version != merged_declaration.protocol_version
            || declaration.frontend_id != merged_declaration.frontend_id
            || declaration.frontend_version != merged_declaration.frontend_version
            || declaration.language != merged_declaration.language
            || declaration.structural_source != merged_declaration.structural_source
            || declaration.structural_limitations != merged_declaration.structural_limitations
        {
            return Err("Cannot merge runs with different frontend declarations".to_owned());
        }
        declaration.runners.into_iter().try_for_each(|runner| {
            if let Some(existing) = runners.get(&runner.runner) {
                if existing != &runner {
                    return Err(format!(
                        "Cannot merge incompatible declarations for runner {}",
                        runner.runner
                    ));
                }
            } else {
                runners.insert(runner.runner.clone(), runner);
            }
            Ok(())
        })
    })?;
    merged_declaration.runners = runners.into_values().collect();

    let hashes = recorded_hashes(&archives);
    let (mutants, owners) = merged_mutants(
        &inputs,
        &archives,
        &hashes,
        grown.as_ref().map_or(0, |grown| grown.latest),
    )?;
    // The strong verdicts each run saved, where they read. A run some of
    // whose tests another measured again, after a support file changed,
    // lends none: a kill there may be its old test's.
    let held = inputs
        .iter()
        .enumerate()
        .filter(|(index, _)| {
            grown
                .as_ref()
                .is_none_or(|grown| !grown.superseded.contains_key(index))
        })
        .filter_map(|(index, input)| {
            let saved = std::fs::read(
                input
                    .directory
                    .join(crate::strong_plan::STRONG_VERDICTS_FILE),
            );
            let document = serde_json::from_slice::<StrongVerdicts>(&saved.ok()?).ok()?;
            Some((index, document))
        })
        .collect::<Vec<_>>()
        .into_iter()
        .map(|(index, document)| {
            let tests = measured_tests(&archives[index]).into_keys().collect();
            (index, document, tests)
        })
        .collect::<Vec<_>>();

    let mut lock = ProjectLock::acquire(root, merged_run_id, started_at)
        .map_err(|error| format!("Cannot lock coverage store for merge: {error}"))?;
    let work = root.join(".supercov/work").join(merged_run_id);
    // Owned before the merge's work, which lets the runs' evidence go
    // once it is written into the merged run's.
    let mut entries = vec![
        EvidenceArchiveEntry {
            path: "coverage-model.json".into(),
            contents: model.to_vec(),
        },
        EvidenceArchiveEntry {
            path: "frontend.json".into(),
            contents: serde_json::to_vec(&merged_declaration).expect("plain data serializes"),
        },
        EvidenceArchiveEntry {
            path: "manifest.json".into(),
            contents: manifest.to_vec(),
        },
    ];
    let result = (|| {
        entries.extend(mutants.map(|contents| EvidenceArchiveEntry {
            path: "mutants.json".into(),
            contents,
        }));
        // Two runs at least, each with evidence besides its files of the
        // whole run. Each entry is rewritten on its own, side by side,
        // and they are kept in order: one after another, parsing and writing
        // every record again was most of merging a large run.
        let jobs = archives
            .iter()
            .enumerate()
            .flat_map(|(shard, archive)| {
                archive
                    .iter()
                    .filter(|entry| {
                        !matches!(
                            entry.path.as_str(),
                            "coverage-model.json"
                                | "frontend.json"
                                | "manifest.json"
                                | "mutants.json"
                        )
                    })
                    .map(move |entry| (shard, entry))
            })
            .collect::<Vec<_>>();
        let rewrite = |(shard, entry): &(usize, &EvidenceArchiveEntry)| {
            // One record of the files for the whole merge: the latest
            // run's, which holds every file the others were recorded with.
            let superseded = grown.as_ref().and_then(|grown| grown.superseded.get(shard));
            match grown.as_ref().and_then(|grown| grown.files.as_ref()) {
                Some(files) if entry.path == crate::source_capture::ARCHIVE_PATH => {
                    Ok(Err(files.clone()))
                }
                _ => rewritten_records(&entry.contents, merged_run_id, superseded).map(Ok),
            }
        };
        let threads = std::thread::available_parallelism()
            .map_or(1, std::num::NonZeroUsize::get)
            .min(jobs.len().max(1));
        let next = std::sync::atomic::AtomicUsize::new(0);
        let mut rewritten = std::thread::scope(|scope| {
            let workers = (0..threads)
                .map(|_| {
                    scope.spawn(|| {
                        let mut done = Vec::new();
                        loop {
                            let index = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                            let Some(job) = jobs.get(index) else {
                                break done;
                            };
                            done.push((index, rewrite(job)));
                        }
                    })
                })
                .collect::<Vec<_>>();
            workers
                .into_iter()
                .flat_map(|worker| worker.join().unwrap_or_else(crate::threads::rethrow))
                .collect::<Vec<_>>()
        });
        rewritten.sort_unstable_by_key(|(index, _)| *index);
        // Each entry's records, or the merge's one record of the files.
        let rewritten = rewritten
            .into_iter()
            .map(|(_, rewritten)| rewritten)
            .collect::<Result<Vec<_>, _>>()?;
        let records = rewritten
            .iter()
            .map(|rewritten| rewritten.as_ref().ok())
            .collect::<Vec<_>>();
        // A test two runs hold alike is kept once. Runs of one checkout that
        // each measured the suite (the jobs of a sharded pass) hold it once
        // each, and a run made from another without measuring holds it as
        // that one does.
        let alike = copies_held_alike(&jobs, &records, archives.len());
        let copies_left_out = alike.iter().map(HeldAlike::len).sum::<usize>();
        // An entry with nothing left had only records of tests measured
        // again elsewhere, or held alike by another run.
        for ((shard, entry), rewritten) in jobs.iter().zip(rewritten) {
            let contents = match rewritten {
                Err(files) => Some(files),
                Ok(rewritten) => {
                    let alike = &alike[*shard];
                    let untested = alike.untested.contains(&entry.path);
                    rewritten.contents(|test| match test {
                        Some(test) => !alike.tests.contains(test),
                        None => !untested,
                    })
                }
            };
            if let Some(contents) = contents {
                entries.push(EvidenceArchiveEntry {
                    path: merged_path(&entry.path, *shard),
                    contents,
                });
            }
        }
        // The runs' evidence is all in the merged run's now: let it go
        // before the merged run is read back to plan its mutants, which
        // holds every record parsed.
        drop(jobs);
        drop(archives);
        let archive_path = work.join("evidence.raw.gz");
        let raw = write_archive(entries, &archive_path)
            .map_err(|error| format!("Could not write merged evidence: {error}"))?;
        let metadata = RunMetadata {
            id: merged_run_id.into(),
            started_at: started_at.into(),
            duration_ms: 0.0,
            command: ["supercov", "merge"]
                .into_iter()
                .chain(allow_changed.then_some("--allow-changed"))
                .map(str::to_owned)
                .chain(run_ids.iter().cloned())
                .collect(),
            test_exit_code: Some(
                if inputs
                    .iter()
                    .all(|input| input.metadata.test_exit_code == Some(0))
                {
                    0
                } else {
                    1
                },
            ),
            // Of the latest run where the tests grew: it is the one the
            // checkout is compared with.
            integrity: inputs[grown.as_ref().map_or(0, |grown| grown.latest)]
                .metadata
                .integrity
                .clone(),
            raw_evidence: RawEvidenceMetadata {
                schema_version: raw.schema_version,
                format: raw.format.into(),
                file: raw.file.into(),
                files: raw.files,
                uncompressed_bytes: raw.uncompressed_bytes,
                compressed_bytes: raw.compressed_bytes,
            },
            isolated_build: None,
            instrumented_build_cache: None,
            timings: None,
            merged: Some(true),
            parents: Some(run_ids.to_vec()),
            source_roots: None,
            copy: None,
            recorded_by: None,
            companions: None,
            supersedes: None,
            measured_as: None,
            merged_across: grown
                .as_ref()
                .map(|grown| grown.across.clone())
                .filter(|across| !across.is_empty()),
        };
        // What each run answers with, kept in the merged run: it is answered
        // from them, and stands alone once they are gone.
        let parts = work.join(crate::run_store::PARTS_DIRECTORY);
        inputs
            .iter()
            .try_for_each(|input| keep_part(&inventory, input, &parts))
            .map_err(|error| format!("Could not keep the merged runs' records: {error}"))?;
        publish_archived_run(
            root,
            &metadata,
            &archive_path,
            Archived {
                parts: Some(parts),
                ..Archived::default()
            },
        )
        .map_err(|error| format!("Could not publish merged run: {error}"))?;
        finalize_published_run(root, merged_run_id)
            .map_err(|error| format!("Could not finalize merged run: {error}"))?;
        let mut notes = kept_verdicts(root, merged_run_id, &inputs, held, &owners, &hashes);
        if copies_left_out > 0 {
            notes.insert(
                0,
                format!(
                    "{copies_left_out} copy(ies) of records that an earlier run held byte for byte kept once, from that run"
                ),
            );
        }
        Ok(MergedRun {
            id: merged_run_id.to_owned(),
            notes,
        })
    })();
    let _ = remove_stored_tree_deferred(root, &work);
    let release = lock
        .release()
        .map_err(|error| format!("Could not release merge lock: {error}"));
    match (result, release) {
        (Err(error), _) => Err(error),
        (Ok(_), Err(error)) => Err(error),
        (Ok(merged), Ok(())) => Ok(merged),
    }
}

/// The file each test of a run's records is written in, by test id.
fn test_files_by_id(archive: &[EvidenceArchiveEntry]) -> BTreeMap<String, String> {
    let filed = crate::threads::parallel_map(archive, |entry| {
        std::str::from_utf8(&entry.contents)
            .map(|text| {
                text.lines()
                    .filter(|line| line.contains("\"testFile\""))
                    .filter_map(|line| serde_json::from_str::<Value>(line).ok())
                    .filter_map(|value| {
                        value["testId"]
                            .as_str()
                            .zip(value["testFile"].as_str())
                            .map(|(id, file)| (id.to_owned(), file.to_owned()))
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
    });
    filed.into_iter().flatten().collect()
}

/// A run made from `source` without measuring, holding the records of the
/// tests written in `files` alone, under `id`: what measuring only those
/// tests would have recorded of them, as a pass whose command runs only
/// them needs. Every other record -- a server's work no test named, the
/// files and the mutants the run listed -- is kept as it is, and so is what
/// a merged run keeps of its runs. Returns how many of its tests it holds.
pub fn derive_run_of_files(
    root: &Path,
    source: &StoredRun,
    files: &BTreeSet<String>,
    id: &str,
    started_at: &str,
) -> Result<usize, String> {
    let archive = read_archive(&source.evidence_path)
        .map_err(|error| format!("run {}'s evidence could not be read: {error}", source.id))?;
    let filed = test_files_by_id(&archive);
    let left_out = filed
        .iter()
        .filter(|(_, file)| !files.contains(*file))
        .map(|(test, _)| test.clone())
        .collect::<BTreeSet<_>>();
    let kept = filed.len() - left_out.len();
    let evidence = source.evidence_run_id();
    let entries = archive
        .into_iter()
        .map(|entry| {
            if matches!(
                entry.path.as_str(),
                "coverage-model.json" | "frontend.json" | "manifest.json" | "mutants.json"
            ) {
                return Ok(Some(entry));
            }
            rewritten_contents(&entry.contents, evidence, Some(&left_out)).map(|contents| {
                contents.map(|contents| EvidenceArchiveEntry {
                    path: entry.path,
                    contents,
                })
            })
        })
        .filter_map(Result::transpose)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("run {}'s evidence could not be read: {error}", source.id))?;
    let mut lock = ProjectLock::acquire(root, id, started_at)
        .map_err(|error| format!("Cannot lock coverage store: {error}"))?;
    let work = root.join(".supercov/work").join(id);
    let result = (|| {
        let archive_path = work.join("evidence.raw.gz");
        let raw = write_archive(entries, &archive_path)
            .map_err(|error| format!("Could not write the run's evidence: {error}"))?;
        let mut metadata = source.metadata.clone();
        metadata.id = id.to_owned();
        metadata.started_at = started_at.to_owned();
        metadata.measured_as = Some(evidence.to_owned());
        metadata.supersedes = None;
        metadata.raw_evidence = RawEvidenceMetadata {
            schema_version: raw.schema_version,
            format: raw.format.into(),
            file: raw.file.into(),
            files: raw.files,
            uncompressed_bytes: raw.uncompressed_bytes,
            compressed_bytes: raw.compressed_bytes,
        };
        // What a merged run keeps of its runs answers for it here too.
        let parts = (metadata.merged == Some(true)).then(|| {
            let parts = work.join(crate::run_store::PARTS_DIRECTORY);
            let inventory = RunInventory {
                runs: Vec::new(),
                rejected: Vec::new(),
            };
            metadata
                .parents
                .iter()
                .flatten()
                .filter_map(|parent| crate::run_store::merged_part(&inventory, source, parent))
                .try_for_each(|part| keep_part(&inventory, &part, &parts))
                .map(|()| parts)
        });
        let parts = parts
            .transpose()
            .map_err(|error| format!("Could not keep the run's records: {error}"))?;
        publish_archived_run(
            root,
            &metadata,
            &archive_path,
            Archived {
                parts,
                ..Archived::default()
            },
        )
        .map_err(|error| format!("Could not publish the run: {error}"))?;
        // Published: what is left of its work is removed below either way.
        let _ = finalize_published_run(root, id);
        Ok(kept)
    })();
    let _ = remove_stored_tree_deferred(root, &work);
    let release = lock
        .release()
        .map_err(|error| format!("Could not release the lock: {error}"));
    match (result, release) {
        (Err(error), _) | (Ok(_), Err(error)) => Err(error),
        (Ok(kept), Ok(())) => Ok(kept),
    }
}

/// The entries of a run's archive that say what its tests are and what they
/// reached beside the code it measured: what `tests affected` and a check of
/// the run against the checkout read (`source_manifest::affected_tests`,
/// `run_currency::Ran`), with the model and frontend every archive holds.
const ANSWERED_WITH: &[&str] = &[
    "coverage-model.json",
    "frontend.json",
    "manifest.json",
    crate::source_capture::ARCHIVE_PATH,
    "statement-exclusions.json",
    crate::support_reach::ARCHIVE_PATH,
    crate::run_currency::PLAYWRIGHT_CONFIGURATION,
];

/// Keeps under `into` what `run` answers with, as a run of its own: its
/// metadata, its record of what each test ran, and the entries of its
/// archive that `ANSWERED_WITH` names (a merge's own at its top level and
/// its runs' under `shards/`). A merged run's own runs are kept in it the
/// same way, where they are held. A merged run is answered from its runs, so
/// one that keeps them answers with its runs gone: a baseline cached alone.
fn keep_part(inventory: &RunInventory, run: &StoredRun, into: &Path) -> std::io::Result<()> {
    let directory = into.join(&run.id);
    // fault-site: run-merge.keep-part.create-dir-all
    crate::fault_fs::create_dir_all(&directory)?;
    let merged = run.metadata.merged == Some(true);
    let entries = read_archive_selected(&run.evidence_path, |path| {
        let name = if merged {
            path.rsplit('/').next().unwrap_or(path)
        } else {
            path
        };
        ANSWERED_WITH.contains(&name)
    })
    .and_then(|entries| write_archive(entries, &directory.join("evidence.raw.gz")))
    .map_err(std::io::Error::other)?;
    let mut metadata = run.metadata.clone();
    metadata.raw_evidence = RawEvidenceMetadata {
        schema_version: entries.schema_version,
        format: entries.format.into(),
        file: entries.file.into(),
        files: entries.files,
        uncompressed_bytes: entries.uncompressed_bytes,
        compressed_bytes: entries.compressed_bytes,
    };
    // fault-site: run-merge.keep-part.write-metadata
    crate::fault_fs::write(
        directory.join("run.json"),
        serde_json::to_vec_pretty(&metadata).expect("plain data serializes"),
    )?;
    let executions = run.directory.join(crate::source_manifest::EXECUTIONS_FILE);
    if let Ok(record) = crate::fault_fs::read(&executions) {
        // fault-site: run-merge.keep-part.write-executions
        crate::fault_fs::write(
            directory.join(crate::source_manifest::EXECUTIONS_FILE),
            record,
        )?;
    }
    // A merge's own runs, where they are held; a run's have none.
    let parts = directory.join(crate::run_store::PARTS_DIRECTORY);
    run.metadata
        .parents
        .iter()
        .flatten()
        .filter_map(|id| crate::run_store::merged_part(inventory, run, id))
        .try_for_each(|part| keep_part(inventory, &part, &parts))
}

/// The merged run's strong verdicts, from those the runs merged saved
/// (`strong_verdicts::merged_verdicts`), saved into it; what was kept, and
/// each run whose verdicts were not, and why. `held` is each run that saved
/// verdicts, with them and the ids of the tests its records name: a test it
/// recorded only by name lends no survival.
fn kept_verdicts(
    root: &Path,
    merged_run_id: &str,
    inputs: &[&StoredRun],
    held: Vec<(usize, StrongVerdicts, BTreeSet<String>)>,
    owners: &BTreeMap<String, usize>,
    hashes: &[BTreeMap<String, String>],
) -> Vec<String> {
    if held.is_empty() {
        return Vec::new();
    }
    // The merged run's plan. Its archive was published a moment ago, but
    // reading it again can still fail (the file system, a note beside it
    // that cannot be written): then nothing is lent, the merge stands, and a
    // pass over the merged run runs its mutants. A run that lists no mutant
    // and measured no point has no plan: it lists no set, so no run's
    // verdicts are of its set, and none is lent.
    let planned = discover_runs(root)
        .map_err(|error| error.to_string())
        .and_then(|inventory| {
            let run = inventory
                .runs
                .into_iter()
                .find(|run| run.id == merged_run_id)
                .expect("a run published under the project's lock is listed");
            let planned = crate::strong_plan::plan_for_run(&run, root)
                .map_err(|error| format!("{error:?}"))?;
            Ok((run.directory, planned.plan))
        });
    let (directory, plan) = match planned {
        Ok(planned) => planned,
        Err(error) => {
            return vec![format!(
                "the runs' strong verdicts are not kept: the merged run's mutants could not be planned ({error}); a strong pass over it runs them"
            )];
        }
    };
    let mut notes = Vec::new();
    let mut lending = Vec::new();
    // Not empty, as said above: a loop over it never runs zero times.
    held.into_iter().for_each(|(index, document, tests)| {
        let id = &inputs[index].id;
        if plan.as_ref().map(|plan| &plan.mutant_set) != Some(&document.mutant_set) {
            notes.push(format!(
                "run {id}'s strong verdicts are not kept: they are of the mutants of {}, which the merged run does not list",
                document.mutant_set
            ));
            return;
        }
        let other_bytes = owners
            .iter()
            .filter(|(path, owner)| hashes[index].get(*path) != hashes[**owner].get(*path))
            .map(|(path, _)| path.clone())
            .collect::<BTreeSet<_>>();
        lending.push((
            id.clone(),
            document.mutants,
            document.progress,
            document.lived,
            tests,
            other_bytes,
        ));
    });
    if lending.is_empty() {
        return notes;
    }
    let plan = plan.expect("a run lent verdicts of the set the merged run's plan lists");
    let lent = lending
        .iter()
        .map(
            |(_, verdicts, progress, lived, tests, other_bytes)| crate::strong_verdicts::Lent {
                verdicts,
                progress,
                lived,
                tests,
                other_bytes,
            },
        )
        .collect::<Vec<_>>();
    let (mutants, taken) = crate::strong_verdicts::merged_verdicts(&plan, &lent);
    // The candidates each mutant still open passed in any run: a pass over
    // the merged run, or a later round carrying it over, resumes there.
    let progress = crate::strong_verdicts::merged_progress(&plan, &lent, &mutants);
    let lived = crate::strong_verdicts::merged_lived(&lent, &progress);
    let document = StrongVerdicts {
        schema_version: crate::strong_plan::STRONG_VERDICTS_SCHEMA_VERSION,
        run: merged_run_id.to_owned(),
        mutants,
        mutant_set: plan.mutant_set,
        progress,
        lived,
    };
    let from = lending
        .iter()
        .map(|(id, ..)| id.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    // What each run added: shards of one pass that each carried the earlier
    // rounds' verdicts hold those alike, and they count for the first.
    let mut each = lending
        .iter()
        .zip(&taken)
        .map(|((id, ..), taken)| format!("{taken} from {id}"))
        .collect::<Vec<_>>();
    let planned = document.mutants.len() - taken.iter().sum::<usize>();
    if planned > 0 {
        each.push(format!("{planned} the merged run's plan decides"));
    }
    let resumed = crate::strong_verdicts::progress_note(&document.progress);
    notes.push(
        match crate::strong_verdicts::save(
            &directory.join(crate::strong_plan::STRONG_VERDICTS_FILE),
            &document,
        ) {
            Ok(()) => format!(
                "{} strong verdict(s) kept from runs {from}: {}{resumed}",
                document.mutants.len(),
                each.join(", ")
            ),
            Err(error) => format!("the runs' strong verdicts are not kept: {error}"),
        },
    );
    notes
}

#[cfg(test)]
mod tests {
    use std::{
        collections::BTreeSet,
        fs,
        sync::{
            Arc, Barrier,
            atomic::{AtomicU64, Ordering},
        },
        time::{SystemTime, UNIX_EPOCH},
    };

    use super::*;
    use crate::{evidence_archive::read_archive, run_store::create_analyzable_test_run};

    /// The name alone, so the uniqueness this depends on can be tested without
    /// creating a directory for every sample.
    fn temporary_name() -> String {
        // The clock below ticks once per microsecond, so the pid and the nonce
        // do not distinguish two tests that start together -- and the tests in
        // this module rewrite run-b's metadata and delete the root when they
        // finish, which the other test then reads as an incomplete run. The
        // counter is what makes each name its own.
        static UNIQUE: AtomicU64 = AtomicU64::new(0);
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        format!(
            "supercov-run-merge-{}-{nonce}-{}",
            std::process::id(),
            UNIQUE.fetch_add(1, Ordering::Relaxed)
        )
    }

    fn temporary() -> crate::test_directory::TestDirectory {
        let root = std::env::temp_dir().join(temporary_name());
        // `create_dir`, not `create_dir_all`: if a root is ever handed out
        // twice, the second test must fail here saying so, rather than share a
        // directory and fail somewhere else for a reason that reads as a bug in
        // the code under test.
        fs::create_dir(&root).unwrap();
        crate::test_directory::TestDirectory::adopt(root)
    }

    #[test]
    fn temporary_roots_stay_distinct_when_tests_start_together() {
        // What the harness does to adjacent tests: release several at once and
        // require that no two of them are handed the same directory.
        const THREADS: usize = 8;
        const EACH: usize = 500;
        let barrier = Arc::new(Barrier::new(THREADS));
        let names: Vec<String> = (0..THREADS)
            .map(|_| {
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    (0..EACH).map(|_| temporary_name()).collect::<Vec<_>>()
                })
            })
            .collect::<Vec<_>>()
            .into_iter()
            .flat_map(|handle| handle.join().unwrap())
            .collect();
        let distinct = names.iter().collect::<BTreeSet<_>>();
        assert_eq!(
            distinct.len(),
            names.len(),
            "two tests would have shared a temporary root"
        );
    }

    /// A test two runs recorded byte for byte alike (a run made from
    /// another without measuring, under an id of its own) is kept once, from
    /// the first; a copy that differs in anything (a test whose path or
    /// timing differed) is kept beside it. Nothing either measured is lost.
    #[test]
    fn a_test_recorded_alike_by_several_runs_is_kept_once_and_every_other_copy_kept() {
        let root = temporary();
        for id in ["run-a", "run-b", "run-c"] {
            create_analyzable_test_run(&root, id);
        }
        let worker = |id: &str| {
            read_archive(&root.join(".supercov/runs").join(id).join("evidence.raw.gz"))
                .unwrap()
                .into_iter()
                .find(|entry| entry.path == "worker/mcdc.json")
                .unwrap()
                .contents
        };
        let record = serde_json::from_slice::<Value>(&worker("run-a")).unwrap();
        assert!(record["testId"].is_string(), "{record}");
        // Run b's record names its own run: the merge names the merged one
        // in both, so they are alike.
        let mut own_run = record.clone();
        own_run["scope"] = serde_json::json!({"runId": "run-b", "testId": record["testId"]});
        let mut first = record.clone();
        first["scope"] = serde_json::json!({"runId": "run-a", "testId": record["testId"]});
        rewrite_entry(
            &root,
            "run-a",
            "worker/mcdc.json",
            Some(serde_json::to_vec(&first).unwrap()),
        );
        rewrite_entry(
            &root,
            "run-b",
            "worker/mcdc.json",
            Some(serde_json::to_vec(&own_run).unwrap()),
        );
        // Run c's took longer.
        let mut slower = first.clone();
        slower["durationMs"] =
            serde_json::json!(record["durationMs"].as_f64().unwrap_or(0.0) + 1.0);
        rewrite_entry(
            &root,
            "run-c",
            "worker/mcdc.json",
            Some(serde_json::to_vec(&slower).unwrap()),
        );
        assert_ne!(worker("run-a"), worker("run-b"));
        let merged = merge_runs(
            &root,
            &["run-a".into(), "run-b".into(), "run-c".into()],
            "merged",
            "2026-01-01T00:00:00.000Z",
            false,
        )
        .unwrap();
        assert_eq!(
            merged.notes,
            [
                "1 copy(ies) of records that an earlier run held byte for byte kept once, from that run"
            ]
        );
        let archive = read_archive(&root.join(".supercov/runs/merged/evidence.raw.gz")).unwrap();
        let workers = archive
            .iter()
            .filter(|entry| entry.path.ends_with("mcdc.json"))
            .map(|entry| entry.path.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            workers,
            ["shards/0/worker/mcdc.json", "shards/2/worker/mcdc.json"]
        );
        fs::remove_dir_all(root).unwrap();
    }

    /// A run made from another holds the records of the tests of the files
    /// named alone, and what a merged run keeps of its runs; each step that
    /// fails says so, and leaves no run.
    #[test]
    fn a_run_made_from_another_holds_the_tests_of_the_files_named() {
        let root = temporary();
        create_analyzable_test_run(&root, "run-a");
        create_analyzable_test_run(&root, "run-b");
        let stored = |id: &str| {
            discover_runs(&root)
                .unwrap()
                .runs
                .into_iter()
                .find(|run| run.id == id)
                .unwrap()
        };
        let named = BTreeSet::from(["tests/app.test.js".to_owned()]);
        let at = "2026-01-02T00:00:00.000Z";
        assert_eq!(
            derive_run_of_files(&root, &stored("run-a"), &named, "kept", at),
            Ok(1)
        );
        assert_eq!(
            derive_run_of_files(&root, &stored("run-a"), &BTreeSet::new(), "none", at),
            Ok(0)
        );
        let tests = |id: &str| measured_tests(&read_archive(&stored(id).evidence_path).unwrap());
        assert_eq!(tests("kept").len(), 1);
        assert!(tests("none").is_empty());
        assert_eq!(
            stored("kept").metadata.measured_as.as_deref(),
            Some("run-a")
        );
        merge_runs(
            &root,
            &["run-a".into(), "run-b".into()],
            "merged",
            "2026-01-01T00:00:00.000Z",
            false,
        )
        .unwrap();
        assert_eq!(
            derive_run_of_files(&root, &stored("merged"), &named, "from-merge", at),
            Ok(1)
        );
        let derived = stored("from-merge");
        assert_eq!(derived.metadata.merged, Some(true));
        assert!(derived.directory.join("parts/run-a/run.json").is_file());
        let merged = stored("merged");
        for (fault, said) in [
            (
                "fs.read_from#evidence-archive.read.read-from",
                "run merged's evidence could not be read",
            ),
            (
                "fs.create_dir_all#evidence-archive.write-entries.create-dir-all",
                "Could not write the run's evidence",
            ),
            (
                "fs.create_dir_all#run-merge.keep-part.create-dir-all",
                "Could not keep the run's records",
            ),
            (
                "fs.rename#lifecycle.publish-run-with-fault.rename",
                "Could not publish the run",
            ),
            ("copy-release", "Could not release the lock"),
        ] {
            let failed = crate::fault::during(fault, || {
                derive_run_of_files(&root, &merged, &named, "failed", at)
            });
            assert!(
                failed.as_ref().is_err_and(|error| error.starts_with(said)),
                "{fault}: {failed:?}"
            );
            let _ = fs::remove_dir_all(root.join(".supercov/runs/failed"));
        }
        assert!(
            derive_run_of_files(&root, &merged, &named, "../escape", at)
                .is_err_and(|error| error.starts_with("Cannot lock coverage store"))
        );
        // A record that cannot be read again.
        rewrite_entry(
            &root,
            "run-b",
            "worker/torn.json",
            Some(b"{\"scope\":{}}\nnot json".to_vec()),
        );
        assert!(
            derive_run_of_files(&root, &stored("run-b"), &named, "torn", at)
                .is_err_and(|error| error
                    .starts_with("run run-b's evidence could not be read: cannot merge malformed"))
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn merges_compatible_runs_atomically_and_rejects_duplicates() {
        let root = temporary();
        create_analyzable_test_run(&root, "run-a");
        create_analyzable_test_run(&root, "run-b");
        let id = merge_coverage_runs(
            &root,
            &["run-a".into(), "run-b".into()],
            "merged",
            "2026-01-01T00:00:00.000Z",
            false,
        )
        .unwrap();
        assert_eq!(id, "merged");
        let metadata: RunMetadata =
            serde_json::from_slice(&fs::read(root.join(".supercov/runs/merged/run.json")).unwrap())
                .unwrap();
        assert_eq!(metadata.merged, Some(true));
        assert_eq!(metadata.parents, Some(vec!["run-a".into(), "run-b".into()]));
        let archive = read_archive(&root.join(".supercov/runs/merged/evidence.raw.gz")).unwrap();
        assert!(archive.iter().any(|entry| entry.path == "manifest.json"));
        assert!(
            archive
                .iter()
                .any(|entry| entry.path.starts_with("shards/0/"))
        );
        // The two runs recorded their test byte for byte alike: it is kept
        // once, from the first.
        assert!(
            !archive
                .iter()
                .any(|entry| entry.path.starts_with("shards/1/"))
        );
        assert!(matches!(
            merge_coverage_runs(
                &root,
                &["run-a".into(), "run-a".into()],
                "invalid",
                "2026-01-01T00:00:00.000Z",
                false,
            ),
            Err(error) if error.starts_with("Each merged run must be unique: run-a is named more than once.")
                && error.contains("`supercov mutants --shard k/n -- <command>` measures one")
        ));
        fs::remove_dir_all(root).unwrap();
    }

    /// Runs whose archives hold only the files of the whole run, no record
    /// of any test, merge into a run that holds the same.
    #[test]
    fn runs_with_no_records_merge_into_a_run_with_none() {
        let root = temporary();
        for id in ["run-a", "run-b"] {
            create_analyzable_test_run(&root, id);
            let path = root.join(".supercov/runs").join(id).join("evidence.raw.gz");
            let kept = read_archive(&path)
                .unwrap()
                .into_iter()
                .filter(|entry| {
                    matches!(
                        entry.path.as_str(),
                        "coverage-model.json" | "frontend.json" | "manifest.json"
                    )
                })
                .collect::<Vec<_>>();
            assert_eq!(kept.len(), 3);
            let written = write_archive(kept, &path).unwrap();
            let run = root.join(".supercov/runs").join(id).join("run.json");
            let mut metadata: Value = serde_json::from_slice(&fs::read(&run).unwrap()).unwrap();
            metadata["rawEvidence"]["files"] = written.files.into();
            metadata["rawEvidence"]["compressedBytes"] = written.compressed_bytes.into();
            metadata["rawEvidence"]["uncompressedBytes"] = written.uncompressed_bytes.into();
            fs::write(&run, serde_json::to_vec(&metadata).unwrap()).unwrap();
        }
        merge_coverage_runs(
            &root,
            &["run-a".into(), "run-b".into()],
            "merged",
            "2026-01-01T00:00:00.000Z",
            false,
        )
        .unwrap();
        let archive = read_archive(&root.join(".supercov/runs/merged/evidence.raw.gz")).unwrap();
        assert_eq!(
            archive
                .iter()
                .map(|entry| entry.path.as_str())
                .collect::<Vec<_>>(),
            ["coverage-model.json", "frontend.json", "manifest.json"]
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn names_every_incompatible_integrity_dimension() {
        let root = temporary();
        create_analyzable_test_run(&root, "run-a");
        create_analyzable_test_run(&root, "run-b");
        let metadata_path = root.join(".supercov/runs/run-b/run.json");
        let mut metadata: RunMetadata =
            serde_json::from_slice(&fs::read(&metadata_path).unwrap()).unwrap();
        metadata.integrity.fingerprint.source = "1".repeat(64);
        metadata.integrity.fingerprint.tests = "2".repeat(64);
        metadata.integrity.fingerprint.configuration = "3".repeat(64);
        metadata.integrity.fingerprint.combined = "4".repeat(64);
        fs::write(&metadata_path, serde_json::to_vec(&metadata).unwrap()).unwrap();

        let error = merge_coverage_runs(
            &root,
            &["run-a".into(), "run-b".into()],
            "merged",
            "2026-01-01T00:00:00.000Z",
            false,
        )
        .unwrap_err();
        assert_eq!(
            error,
            "Cannot merge incompatible run run-b: differing domains: source, tests, configuration"
        );
        fs::remove_dir_all(root).unwrap();
    }

    /// A server's record torn as it was written is kept in its run, counted
    /// as corrupt; a merge cannot carry it, and says so.
    #[test]
    fn a_run_holding_a_torn_record_is_not_merged() {
        let root = temporary();
        create_analyzable_test_run(&root, "run-a");
        create_analyzable_test_run(&root, "run-b");
        rewrite_entry(
            &root,
            "run-b",
            "server/background/worker.jsonl",
            Some(b"{\"type\":\"hit\",\"id\":\"statement\"}\n{\"type\":\"hi".to_vec()),
        );
        let error = merge_coverage_runs(
            &root,
            &["run-a".into(), "run-b".into()],
            "merged",
            "2026-01-01T00:00:00.000Z",
            false,
        )
        .unwrap_err();
        assert!(
            error.contains("cannot merge malformed recognized JSON evidence"),
            "{error}"
        );
        fs::remove_dir_all(root).unwrap();
    }

    /// Rewrites one entry of a stored run's evidence (None removes it) and
    /// keeps its metadata in step with the archive.
    fn rewrite_entry(root: &Path, id: &str, path: &str, contents: Option<Vec<u8>>) {
        let directory = root.join(".supercov/runs").join(id);
        let mut entries = read_archive(&directory.join("evidence.raw.gz")).unwrap();
        entries.retain(|entry| entry.path != path);
        if let Some(contents) = contents {
            entries.push(crate::evidence_archive::EvidenceArchiveEntry {
                path: path.into(),
                contents,
            });
        }
        let archive =
            crate::evidence_archive::write_archive(entries, &directory.join("evidence.raw.gz"))
                .unwrap();
        edit_metadata(root, id, |metadata| {
            metadata.raw_evidence.files = archive.files;
            metadata.raw_evidence.uncompressed_bytes = archive.uncompressed_bytes;
            metadata.raw_evidence.compressed_bytes = archive.compressed_bytes;
        });
    }

    fn edit_metadata(root: &Path, id: &str, edit: impl FnOnce(&mut RunMetadata)) {
        let path = root.join(".supercov/runs").join(id).join("run.json");
        let mut metadata: RunMetadata = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        edit(&mut metadata);
        fs::write(&path, serde_json::to_vec(&metadata).unwrap()).unwrap();
    }

    /// Gives a stored run a record of the files it was measured with.
    fn record_files(root: &Path, id: &str, files: &[(&str, &str)]) {
        let manifest = crate::source_manifest::InputManifest {
            schema_version: crate::source_manifest::MANIFEST_SCHEMA,
            language: "javascript".into(),
            files: files
                .iter()
                .map(|(file, text)| {
                    (
                        file.to_string(),
                        crate::source_manifest::FileFingerprint::of(text),
                    )
                })
                .collect(),
        };
        rewrite_entry(
            root,
            id,
            crate::source_capture::ARCHIVE_PATH,
            Some(serde_json::to_vec(&manifest).unwrap()),
        );
    }

    /// Run-a recorded a file run-b did not, which the checkout holds as
    /// run-a recorded it: run-b is the run the merge grows into.
    fn grown_from_a_recorded_file(root: &Path) {
        record_files(root, "run-a", &[("test/data.json", "same")]);
        record_files(root, "run-b", &[]);
        fs::create_dir_all(root.join("test")).unwrap();
        fs::write(root.join("test/data.json"), "same").unwrap();
    }

    fn declaration_with(edit: impl FnOnce(&mut Value)) -> Option<Vec<u8>> {
        let root = temporary();
        create_analyzable_test_run(&root, "source");
        let entries = read_archive(&root.join(".supercov/runs/source/evidence.raw.gz")).unwrap();
        let mut value: Value = serde_json::from_slice(
            &entries
                .iter()
                .find(|entry| entry.path == "frontend.json")
                .unwrap()
                .contents,
        )
        .unwrap();
        edit(&mut value);
        Some(serde_json::to_vec(&value).unwrap())
    }

    /// Gives a stored run's manifest and frontend declaration these
    /// limitations, by id; an empty id is a limitation with none.
    fn limit(root: &Path, id: &str, limitations: &[&str]) {
        let entries =
            read_archive(&root.join(".supercov/runs").join(id).join("evidence.raw.gz")).unwrap();
        let read = |path: &str| -> Value {
            serde_json::from_slice(
                &entries
                    .iter()
                    .find(|entry| entry.path == path)
                    .unwrap()
                    .contents,
            )
            .unwrap()
        };
        let mut manifest = read("manifest.json");
        manifest["limitations"] = limitations
            .iter()
            .map(|limitation| match *limitation {
                "" => serde_json::json!({"kind": "source-scope"}),
                limitation => serde_json::json!({"id": limitation, "kind": "source-scope"}),
            })
            .collect();
        let mut declaration = read("frontend.json");
        declaration["structuralLimitations"] = limitations
            .iter()
            .filter(|limitation| !limitation.is_empty())
            .map(|limitation| Value::from(*limitation))
            .collect();
        rewrite_entry(
            root,
            id,
            "manifest.json",
            Some(serde_json::to_vec(&manifest).unwrap()),
        );
        rewrite_entry(
            root,
            id,
            "frontend.json",
            Some(serde_json::to_vec(&declaration).unwrap()),
        );
    }

    /// Runs whose tests started different Rust Workers measured the same
    /// files: they merge, and a Worker stays noted as unrun only where no run
    /// started it.
    #[test]
    fn runs_that_started_different_workers_merge() {
        let root = temporary();
        create_analyzable_test_run(&root, "run-a");
        create_analyzable_test_run(&root, "run-b");
        let unrun = |file: &str| format!("{UNRUN_WORKER}{file}");
        let (plane, runners, shared) = (unrun("plane.rs"), unrun("runners.rs"), unrun("shared.rs"));
        limit(&root, "run-a", &[&runners, &shared, "boundary", ""]);
        limit(&root, "run-b", &[&plane, &shared, "boundary", ""]);
        merge_coverage_runs(
            &root,
            &["run-a".into(), "run-b".into()],
            "merged",
            "2026-01-01T00:00:00.000Z",
            false,
        )
        .unwrap();
        let archive = read_archive(&root.join(".supercov/runs/merged/evidence.raw.gz")).unwrap();
        let read = |path: &str| -> Value {
            serde_json::from_slice(
                &archive
                    .iter()
                    .find(|entry| entry.path == path)
                    .unwrap()
                    .contents,
            )
            .unwrap()
        };
        let ids = read("manifest.json")["limitations"]
            .as_array()
            .unwrap()
            .iter()
            .map(|limitation| limitation["id"].as_str().unwrap_or("").to_owned())
            .collect::<Vec<_>>();
        assert_eq!(ids, [shared.as_str(), "boundary", ""]);
        assert_eq!(
            read("frontend.json")["structuralLimitations"],
            serde_json::json!([shared, "boundary"])
        );

        // Any other difference in what is measured still refuses.
        limit(&root, "run-b", &[&plane, &shared, ""]);
        assert_eq!(
            merge_coverage_runs(
                &root,
                &["run-a".into(), "run-b".into()],
                "refused",
                "2026-01-01T00:00:00.000Z",
                false,
            )
            .unwrap_err(),
            "Cannot merge runs with different coverage denominators"
        );
        fs::remove_dir_all(root).unwrap();
    }

    /// A manifest that cannot be read, or has no limitations, is left for the
    /// comparison to judge; notes all runs share are kept as they were.
    #[test]
    fn unrun_worker_notes_settle_around_unreadable_manifests() {
        let noted = format!(r#"{{"limitations":[{{"id":"{UNRUN_WORKER}a.rs"}}]}}"#);
        let (settled, shared) = settle_unrun_workers(&[
            Some(noted.as_bytes()),
            Some(br#"{"points":[]}"#),
            None,
            Some(b"not json"),
        ]);
        assert!(shared.is_empty());
        assert_eq!(
            settled,
            [
                Some(b"{}".to_vec()),
                Some(br#"{"points":[]}"#.to_vec()),
                None,
                Some(b"not json".to_vec()),
            ]
        );
        let (settled, shared) = settle_unrun_workers(&[Some(noted.as_bytes()); 2]);
        assert_eq!(shared, BTreeSet::from([format!("{UNRUN_WORKER}a.rs")]));
        assert_eq!(
            settled,
            [Some(noted.clone().into_bytes()), Some(noted.into_bytes())]
        );
    }

    /// Every way two runs can fail to merge is said by name, and a second
    /// runner, unlike a changed one, is simply added.
    #[test]
    fn each_refusal_to_merge_says_why() {
        type Setup = Box<dyn Fn(&Path)>;
        let version = env!("CARGO_PKG_VERSION");
        let cases: Vec<(&str, Setup, String)> = vec![
            (
                "another instrumenter, release unknown",
                Box::new(|root| {
                    edit_metadata(root, "run-b", |m| m.integrity.instrumenter_version = "x".into())
                }),
                format!(
                    "Cannot merge run run-a: it was recorded by a Supercov older than 4.7.0, which instruments differently from Supercov {version}. Measure it again to merge it."
                ),
            ),
            (
                "another build of this release",
                Box::new(move |root| {
                    for id in ["run-a", "run-b"] {
                        edit_metadata(root, id, |m| m.recorded_by = Some(version.into()));
                    }
                    edit_metadata(root, "run-b", |m| {
                        m.integrity.fingerprint.instrumenter = "9".repeat(64)
                    });
                }),
                format!(
                    "Cannot merge run run-b: it was recorded by another build of Supercov {version}, which instruments differently from this one. Measure it again to merge it."
                ),
            ),
            (
                "another release",
                Box::new(|root| {
                    edit_metadata(root, "run-b", |m| {
                        m.recorded_by = Some("0.0.1".into());
                        m.integrity.fingerprint.instrumenter = "9".repeat(64);
                    })
                }),
                "Cannot merge run run-a: it was recorded by a Supercov older than 4.7.0".into(),
            ),
            (
                "another language",
                Box::new(|root| {
                    edit_metadata(root, "run-b", |m| m.integrity.instrumenter_version = "x".into());
                    rewrite_entry(
                        root,
                        "run-b",
                        "frontend.json",
                        declaration_with(|v| v["language"] = "python".into()),
                    );
                }),
                "Cannot merge run run-b: it measured python, and run run-a measured javascript; runs of different languages are not merged".into(),
            ),
            (
                "another instrumenter, the first run's language unread",
                Box::new(|root| {
                    edit_metadata(root, "run-b", |m| m.integrity.instrumenter_version = "x".into());
                    rewrite_entry(root, "run-a", "frontend.json", Some(b"[]".to_vec()));
                }),
                "Cannot merge run run-a: it was recorded by a Supercov older than 4.7.0".into(),
            ),
            (
                "the same domains run another way",
                Box::new(|root| {
                    edit_metadata(root, "run-b", |m| {
                        m.integrity.fingerprint.combined = "4".repeat(64)
                    })
                }),
                "Cannot merge incompatible run run-b: differing domains: integrity fingerprint"
                    .into(),
            ),
            (
                "dependencies",
                Box::new(|root| {
                    edit_metadata(root, "run-b", |m| {
                        m.integrity.fingerprint.dependencies = "9".repeat(64)
                    })
                }),
                "Cannot merge incompatible run run-b: differing domains: dependencies".into(),
            ),
            (
                "source file count",
                Box::new(|root| {
                    edit_metadata(root, "run-b", |m| m.integrity.fingerprint.source_files = 2)
                }),
                "Cannot merge incompatible run run-b: differing domains: source files".into(),
            ),
            (
                "denominator",
                Box::new(|root| {
                    rewrite_entry(root, "run-b", "manifest.json", Some(b"{}".to_vec()))
                }),
                "Cannot merge runs with different coverage denominators".into(),
            ),
            (
                "configuration, the files unrecorded",
                Box::new(|root| {
                    edit_metadata(root, "run-b", |m| {
                        m.integrity.fingerprint.configuration = "9".repeat(64)
                    })
                }),
                "Cannot merge incompatible run run-b: differing domains: configuration".into(),
            ),
            (
                "source and its file count, the files unrecorded",
                Box::new(|root| {
                    edit_metadata(root, "run-b", |m| {
                        m.integrity.fingerprint.source = "9".repeat(64);
                        m.integrity.fingerprint.source_files = 2;
                    })
                }),
                "Cannot merge incompatible run run-b: differing domains: source".into(),
            ),
            (
                "a file recorded once and gone from the checkout",
                Box::new(|root| {
                    record_files(root, "run-a", &[("mock/data.json", "{}")]);
                    record_files(root, "run-b", &[]);
                }),
                "Cannot merge run run-a with run run-b: mock/data.json, which it measured, is gone. Measure it again.".into(),
            ),
            (
                "two served files, their test not measured again",
                Box::new(|root| {
                    record_files(root, "run-a", &[("mocks/a.js", "1"), ("mocks/b.js", "1")]);
                    record_files(root, "run-b", &[("mocks/a.js", "2"), ("mocks/b.js", "2")]);
                    rewrite_entry(
                        root,
                        "run-a",
                        crate::support_reach::ARCHIVE_PATH,
                        Some(
                            serde_json::to_vec(&serde_json::json!({"files": {
                                "mocks/a.js": ["t9"],
                                "mocks/b.js": ["t9"],
                            }}))
                            .unwrap(),
                        ),
                    );
                    let entries = read_archive(&root.join(".supercov/runs/run-a/evidence.raw.gz"))
                        .unwrap();
                    let mut manifest: Value = serde_json::from_slice(
                        &entries
                            .iter()
                            .find(|entry| entry.path == "manifest.json")
                            .unwrap()
                            .contents,
                    )
                    .unwrap();
                    manifest["scope"] = serde_json::json!({"entries": [
                        {"file": "src/app.js", "status": "included"}
                    ]});
                    rewrite_entry(
                        root,
                        "run-a",
                        "manifest.json",
                        Some(serde_json::to_vec(&manifest).unwrap()),
                    );
                }),
                "Cannot merge run run-a with run run-b: mocks/a.js; mocks/b.js changed, and 1 of its tests reached them through a server that served them and were not measured again: t9.".into(),
            ),
            (
                "grown, the recorded file gone since",
                Box::new(|root| {
                    grown_from_a_recorded_file(root);
                    fs::remove_file(root.join("test/data.json")).unwrap();
                }),
                "Cannot merge run run-a with run run-b: test/data.json is gone. Measure it again, or pass --allow-changed if the change alters nothing its tests ran.".into(),
            ),
            (
                "grown, the earlier run without a record of its files",
                Box::new(|root| record_files(root, "run-b", &[])),
                "Cannot merge incompatible run run-a: differing domains: recorded files".into(),
            ),
            (
                "grown, the later run's denominator another",
                Box::new(|root| {
                    grown_from_a_recorded_file(root);
                    rewrite_entry(root, "run-b", "manifest.json", Some(b"{}".to_vec()));
                }),
                "Cannot merge runs with different coverage denominators".into(),
            ),
            (
                "model",
                Box::new(|root| {
                    rewrite_entry(root, "run-b", "coverage-model.json", Some(b"{}".to_vec()))
                }),
                "Cannot merge runs with different coverage models".into(),
            ),
            (
                "invalid declaration",
                Box::new(|root| {
                    rewrite_entry(root, "run-b", "frontend.json", Some(b"[]".to_vec()))
                }),
                "Cannot merge invalid frontend declaration".into(),
            ),
            (
                "protocol version",
                Box::new(|root| {
                    rewrite_entry(
                        root,
                        "run-b",
                        "frontend.json",
                        declaration_with(|v| v["protocolVersion"] = 3.into()),
                    )
                }),
                "Cannot merge runs with different frontend declarations".into(),
            ),
            (
                "frontend id",
                Box::new(|root| {
                    rewrite_entry(
                        root,
                        "run-b",
                        "frontend.json",
                        declaration_with(|v| v["frontendId"] = "other".into()),
                    )
                }),
                "Cannot merge runs with different frontend declarations".into(),
            ),
            (
                "declared language",
                Box::new(|root| {
                    rewrite_entry(
                        root,
                        "run-b",
                        "frontend.json",
                        declaration_with(|v| v["language"] = "typescript".into()),
                    )
                }),
                "Cannot merge runs with different frontend declarations".into(),
            ),
            (
                "structural source",
                Box::new(|root| {
                    rewrite_entry(
                        root,
                        "run-b",
                        "frontend.json",
                        declaration_with(|v| v["structuralSource"] = "mixed".into()),
                    )
                }),
                "Cannot merge runs with different frontend declarations".into(),
            ),
            (
                "structural limitations",
                Box::new(|root| {
                    rewrite_entry(
                        root,
                        "run-b",
                        "frontend.json",
                        declaration_with(|v| v["structuralLimitations"] = serde_json::json!(["x"])),
                    )
                }),
                "Cannot merge runs with different frontend declarations".into(),
            ),
            (
                "coverage schema",
                Box::new(|root| edit_metadata(root, "run-b", |m| m.integrity.schema_version = 3)),
                "Cannot merge run run-a: it was recorded by a Supercov older than 4.7.0".into(),
            ),
            (
                "frontend version",
                Box::new(|root| {
                    rewrite_entry(
                        root,
                        "run-b",
                        "frontend.json",
                        declaration_with(|v| v["frontendVersion"] = "fixture-v2".into()),
                    )
                }),
                "Cannot merge runs with different frontend declarations".into(),
            ),
            (
                "runner",
                Box::new(|root| {
                    rewrite_entry(
                        root,
                        "run-b",
                        "frontend.json",
                        declaration_with(|v| {
                            v["runners"][0]["executionModel"] = "process-per-test".into()
                        }),
                    )
                }),
                "Cannot merge incompatible declarations for runner node:test".into(),
            ),
        ];
        for (label, setup, expected) in cases {
            let root = temporary();
            create_analyzable_test_run(&root, "run-a");
            create_analyzable_test_run(&root, "run-b");
            setup(&root);
            let error = merge_coverage_runs(
                &root,
                &["run-a".into(), "run-b".into()],
                "merged",
                "2026-01-01T00:00:00.000Z",
                false,
            )
            .unwrap_err();
            assert!(error.starts_with(&expected), "{label}: {error}");
        }

        let root = temporary();
        create_analyzable_test_run(&root, "run-a");
        let merge = |ids: &[&str]| {
            merge_coverage_runs(
                &root,
                &ids.iter().map(|id| (*id).to_owned()).collect::<Vec<_>>(),
                "merged",
                "2026-01-01T00:00:00.000Z",
                false,
            )
        };
        assert_eq!(
            merge(&["run-a"]).unwrap_err(),
            "Usage: supercov merge <run-id> <run-id> [...]"
        );
        assert_eq!(
            merge(&["run-a", "absent"]).unwrap_err(),
            "Cannot merge coverage run absent: no such local run"
        );
        create_analyzable_test_run(&root, "run-b");
        rewrite_entry(
            &root,
            "run-b",
            "frontend.json",
            declaration_with(|v| {
                let mut second = v["runners"][0].clone();
                second["runner"] = "vitest".into();
                v["runners"].as_array_mut().unwrap().push(second);
            }),
        );
        // One run whose tests failed makes the merged run's tests fail.
        edit_metadata(&root, "run-b", |m| m.test_exit_code = Some(1));
        merge(&["run-a", "run-b"]).unwrap();
        let merged: RunMetadata =
            serde_json::from_slice(&fs::read(root.join(".supercov/runs/merged/run.json")).unwrap())
                .unwrap();
        assert_eq!(merged.test_exit_code, Some(1));
        let entries = read_archive(&root.join(".supercov/runs/merged/evidence.raw.gz")).unwrap();
        let declaration: Value = serde_json::from_slice(
            &entries
                .iter()
                .find(|entry| entry.path == "frontend.json")
                .unwrap()
                .contents,
        )
        .unwrap();
        let runners = declaration["runners"]
            .as_array()
            .unwrap()
            .iter()
            .map(|runner| runner["runner"].as_str().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(runners, ["node:test", "vitest"]);
    }

    #[test]
    fn rewrites_nested_scope_run_ids_and_rejects_non_json_records() {
        let rewritten = rewritten_contents(
            b"{\"scope\":{\"runId\":\"old\",\"testId\":\"test\"},\"nested\":[{\"scope\":{\"runId\":\"old\"}}]}\n",
            "merged",
            None,
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            std::str::from_utf8(&rewritten).unwrap(),
            "{\"scope\":{\"runId\":\"merged\",\"testId\":\"test\"},\"nested\":[{\"scope\":{\"runId\":\"merged\"}}]}\n"
        );
        assert!(rewritten_contents(b"plain\n", "merged", None).is_err());
    }

    /// Record shapes a writer may produce besides the usual: an empty
    /// object, a repeated key, a scope that is not an object, an array of
    /// records, and a line that is not an object following a record whose
    /// test is kept or measured again.
    #[test]
    fn every_record_shape_keeps_its_text_and_takes_the_merged_run() {
        let superseded = BTreeSet::from(["t2".to_owned()]);
        let rewrite = |text: &str| {
            rewritten_contents(text.as_bytes(), "merged", Some(&superseded))
                .map(|kept| kept.map(|bytes| String::from_utf8(bytes).unwrap()))
        };
        assert_eq!(
            rewrite("{}\n{\"a\":1,\"b\":0,\"a\":2}\n{\"a\":{\"scope\":1}}\n[{},{\"scope\":{}}]\n"),
            Ok(Some(
                "{}\n{\"a\":2,\"b\":0}\n{\"a\":{\"scope\":1}}\n[{},{\"scope\":{\"runId\":\"merged\"}}]\n"
                    .to_owned()
            ))
        );
        // A line that is not an object is of the test of the record before it.
        assert_eq!(
            rewrite("{\"scope\":{\"testId\":\"t1\"}}\n[1]\n{\"scope\":{\"testId\":\"t2\"}}\n[2]\n"),
            Ok(Some(
                "{\"scope\":{\"testId\":\"t1\",\"runId\":\"merged\"}}\n[1]\n".to_owned()
            ))
        );
    }

    /// A record's entries are read as raw text, which nests without a limit;
    /// a scope nested deeper than a parsed value may be is refused, whether
    /// it is the record's own or one inside another entry.
    #[test]
    fn a_scope_nested_past_the_parse_limit_is_refused() {
        let deep = format!("{}{}", "[".repeat(200), "]".repeat(200));
        for record in [
            format!("{{\"scope\":{{\"a\":{deep}}}}}"),
            format!("{{\"n\":{{\"scope\":{{}},\"d\":{deep}}}}}"),
        ] {
            let error = rewritten_contents(record.as_bytes(), "merged", None).unwrap_err();
            assert!(error.contains("recursion limit"), "{error}");
        }
    }

    #[test]
    fn a_test_measured_again_elsewhere_leaves_its_records_out() {
        let superseded = BTreeSet::from(["paid".to_owned()]);
        let records = b"{\"testId\":\"paid\",\"test\":\"pays\"}\n{\"testId\":\"mail\",\"test\":\"mails\"}\n{\"scope\":{\"testId\":\"paid\"},\"type\":\"hit\"}\n";
        let kept = rewritten_contents(records, "merged", Some(&superseded))
            .unwrap()
            .unwrap();
        assert_eq!(
            std::str::from_utf8(&kept).unwrap(),
            "{\"testId\":\"mail\",\"test\":\"mails\"}\n"
        );
        // An entry of the superseded test alone is left out whole.
        let only = b"{\"testId\":\"paid\",\"test\":\"pays\"}\n";
        assert_eq!(
            rewritten_contents(only, "merged", Some(&superseded)).unwrap(),
            None
        );
    }

    /// A merged entry keeps its records under the merged run's id, drops
    /// those of tests measured again elsewhere, and is gone when nothing is
    /// left of it; anything but JSON lines is refused.
    #[test]
    fn merged_records_take_the_new_run_and_drop_superseded_tests() {
        let superseded = ["t2".to_owned()].into_iter().collect::<BTreeSet<_>>();
        let rewrite = |text: &str, superseded: Option<&BTreeSet<String>>| {
            rewritten_contents(text.as_bytes(), "merged", superseded)
                .map(|kept| kept.map(|bytes| String::from_utf8(bytes).unwrap()))
        };
        assert_eq!(
            rewrite(
                "{\"testId\":\"t1\",\"scope\":{\"runId\":\"a\"}}\n{\"testId\":\"t2\"}\n{\"scope\":{\"testId\":\"t2\"}}\n{\"scope\":\"text\"}\n",
                Some(&superseded)
            ),
            Ok(Some(
                "{\"testId\":\"t1\",\"scope\":{\"runId\":\"merged\"}}\n{\"scope\":\"text\"}\n"
                    .to_owned()
            ))
        );
        // Every record superseded: nothing of the entry is left.
        assert_eq!(
            rewrite("{\"testId\":\"t2\"}\n", Some(&superseded)),
            Ok(None)
        );
        // An entry with no records stays, empty.
        assert_eq!(rewrite("", None), Ok(Some(String::new())));
        // No newline at the end: none added.
        assert_eq!(
            rewrite("[{\"scope\":{}}]", None),
            Ok(Some("[{\"scope\":{\"runId\":\"merged\"}}]".to_owned()))
        );
        assert!(
            rewrite("not json\n", None)
                .unwrap_err()
                .contains("malformed")
        );
        assert!(
            rewritten_contents(&[0xff, 0xfe], "merged", None)
                .unwrap_err()
                .contains("non-UTF-8")
        );
        let entry = |contents: &[u8]| EvidenceArchiveEntry {
            path: "e.jsonl".into(),
            contents: contents.to_vec(),
        };
        let tests = measured_tests(&[
            entry(b"{\"testId\":\"t1\",\"test\":\"adds\"}\n{\"testId\":\"t3\"}\nnot \"testId\" json\nplain\n"),
            entry(&[0xff, 0xfe]),
            entry(b""),
        ]);
        assert!(measured_tests(&[]).is_empty());
        assert_eq!(
            tests,
            [("t1".to_owned(), "adds".to_owned())].into_iter().collect()
        );
        let items = |n: usize| (1..=n).map(|i| format!("i{i}")).collect::<Vec<_>>();
        assert_eq!(listed(&items(0)), "");
        assert_eq!(listed(&items(3)), "i1; i2; i3");
        assert_eq!(listed(&items(5)), "i1; i2; i3; and 2 more");
    }

    /// A mutants list in the shape instrumentation writes, of `files`, each
    /// with the mutants of `src/app.js`'s fixture decision (all but those
    /// `dropped` names), or none listed at all.
    fn list_mutants(root: &Path, id: &str, files: Option<&[&str]>, dropped: &[&str]) {
        let candidate = |file: &str, mutator: &str, descriptor: &str| {
            serde_json::json!({
                "id": format!("{file}:1:5-1:18:{mutator}:{descriptor}"),
                "mutator": mutator,
                "replacement": descriptor,
                "start": [1, 5],
                "end": [1, 18],
                "original": "left && right",
                "statement": "statement",
                "decision": { "id": "decision" },
            })
        };
        let listed = match files {
            Some(files) => serde_json::json!({
                "schemaVersion": 1,
                "mutantSet": "stryker-js@10.1.1",
                "files": files
                    .iter()
                    .map(|file| {
                        let mutants = ["true", "false"]
                            .into_iter()
                            .filter(|descriptor| !dropped.contains(descriptor))
                            .map(|descriptor| candidate(file, "ConditionalExpression", descriptor))
                            .collect::<Vec<_>>();
                        (file.to_string(), Value::from(mutants))
                    })
                    .collect::<serde_json::Map<_, _>>(),
            }),
            None => serde_json::json!({ "schemaVersion": 1, "mutantSet": null, "files": {} }),
        };
        rewrite_entry(
            root,
            id,
            "mutants.json",
            Some(serde_json::to_vec(&listed).unwrap()),
        );
    }

    fn merge_two(root: &Path, first: &str, second: &str) -> Result<String, String> {
        merge_coverage_runs(
            root,
            &[first.into(), second.into()],
            "merged",
            "2026-01-01T00:00:00.000Z",
            false,
        )
    }

    /// The merged run's archive entries named `mutants.json`, at any depth,
    /// and the one at its top read.
    fn merged_mutants_file(root: &Path) -> (Vec<String>, crate::js_mutant_links::MutantsFile) {
        let archive = read_archive(&root.join(".supercov/runs/merged/evidence.raw.gz")).unwrap();
        let named = archive
            .iter()
            .filter(|entry| entry.path.ends_with("mutants.json"))
            .map(|entry| entry.path.clone())
            .collect();
        let top = archive
            .iter()
            .find(|entry| entry.path == "mutants.json")
            .unwrap();
        (named, serde_json::from_slice(&top.contents).unwrap())
    }

    /// Each mutant of the merged run, with its status, from its index.
    fn merged_verdicts(root: &Path) -> Vec<(String, crate::coverage_index::MutantStatus)> {
        let run = discover_runs(root)
            .unwrap()
            .runs
            .into_iter()
            .find(|run| run.id == "merged")
            .unwrap();
        let container = crate::run_store::open_or_rebuild_query_index(&run).unwrap();
        let index = crate::coverage_index::CoverageIndex::new(&container).unwrap();
        let mutation = index.mutation().unwrap().unwrap();
        mutation
            .files
            .iter()
            .flat_map(|file| index.mutants_of(file).unwrap())
            .map(|mutant| (mutant.id(), mutant.status))
            .collect()
    }

    /// Runs that listed the same mutants merge into a run that lists them
    /// once, not once per run.
    #[test]
    fn a_merged_run_lists_its_mutants_once() {
        let root = temporary();
        create_analyzable_test_run(&root, "run-a");
        create_analyzable_test_run(&root, "run-b");
        list_mutants(&root, "run-a", Some(&["src/app.js"]), &[]);
        list_mutants(&root, "run-b", Some(&["src/app.js"]), &[]);
        merge_two(&root, "run-a", "run-b").unwrap();
        let (named, merged) = merged_mutants_file(&root);
        assert_eq!(named, ["mutants.json"]);
        assert_eq!(merged.mutant_set.as_deref(), Some("stryker-js@10.1.1"));
        assert_eq!(merged.files["src/app.js"].len(), 2);
        assert!(merged.tests_without_values.is_empty());
        use crate::coverage_index::MutantStatus::Infected;
        assert_eq!(
            merged_verdicts(&root),
            [
                (
                    "src/app.js:1:5-1:18:ConditionalExpression:true".to_owned(),
                    Infected
                ),
                (
                    "src/app.js:1:5-1:18:ConditionalExpression:false".to_owned(),
                    Infected
                ),
            ]
        );
        fs::remove_dir_all(root).unwrap();
    }

    /// A run that listed no mutants adds no files to the list, and its
    /// tests, which recorded no values, are named so their silence decides
    /// nothing; whichever run comes first.
    #[test]
    fn a_run_without_mutants_lends_its_tests_and_not_its_files() {
        for (first, second) in [("run-a", "run-b"), ("run-b", "run-a")] {
            let root = temporary();
            create_analyzable_test_run(&root, "run-a");
            create_analyzable_test_run(&root, "run-b");
            list_mutants(&root, "run-a", Some(&["src/app.js"]), &[]);
            list_mutants(&root, "run-b", None, &[]);
            merge_two(&root, first, second).unwrap();
            let (named, merged) = merged_mutants_file(&root);
            assert_eq!(named, ["mutants.json"], "{first} first");
            assert_eq!(
                merged.files.keys().collect::<Vec<_>>(),
                ["src/app.js"],
                "{first} first"
            );
            assert_eq!(merged.tests_without_values, ["test"], "{first} first");
            assert_eq!(merged_verdicts(&root).len(), 2, "{first} first");
            fs::remove_dir_all(root).unwrap();
        }
        // Neither listed any: the merge lists none either.
        let root = temporary();
        create_analyzable_test_run(&root, "run-a");
        create_analyzable_test_run(&root, "run-b");
        list_mutants(&root, "run-a", None, &[]);
        list_mutants(&root, "run-b", None, &[]);
        merge_two(&root, "run-a", "run-b").unwrap();
        let (named, merged) = merged_mutants_file(&root);
        assert_eq!(named, ["mutants.json"]);
        assert_eq!(merged.mutant_set, None);
        fs::remove_dir_all(root).unwrap();
    }

    /// Different files' mutants are kept together; the same file, with the
    /// same bytes, listed differently is refused.
    #[test]
    fn runs_that_listed_a_file_differently_are_not_merged() {
        // Each run listed a file of its own: both are kept.
        let root = temporary();
        create_analyzable_test_run(&root, "run-a");
        create_analyzable_test_run(&root, "run-b");
        list_mutants(&root, "run-a", Some(&["src/app.js"]), &[]);
        list_mutants(&root, "run-b", Some(&["src/other.js"]), &[]);
        merge_two(&root, "run-a", "run-b").unwrap();
        let (_, merged) = merged_mutants_file(&root);
        assert_eq!(
            merged.files.keys().collect::<Vec<_>>(),
            ["src/app.js", "src/other.js"]
        );
        fs::remove_dir_all(root).unwrap();
        // The same file, other mutants.
        let root = temporary();
        create_analyzable_test_run(&root, "run-a");
        create_analyzable_test_run(&root, "run-b");
        list_mutants(&root, "run-a", Some(&["src/app.js"]), &[]);
        list_mutants(&root, "run-b", Some(&["src/app.js"]), &["false"]);
        assert_eq!(
            merge_two(&root, "run-a", "run-b").unwrap_err(),
            "Cannot merge coverage runs run-a and run-b: they listed different mutants of src/app.js, recorded with the same bytes"
        );
        assert!(!root.join(".supercov/runs/merged").exists());
        fs::remove_dir_all(root).unwrap();
        // Both recorded src/app.js with the same bytes, and one did not
        // list its mutants.
        let root = temporary();
        create_analyzable_test_run(&root, "run-a");
        create_analyzable_test_run(&root, "run-b");
        let files = [("src/app.js", "left && right"), ("src/other.js", "other")];
        record_files(&root, "run-a", &files);
        record_files(&root, "run-b", &files);
        list_mutants(&root, "run-a", Some(&["src/app.js"]), &[]);
        list_mutants(&root, "run-b", Some(&["src/other.js"]), &[]);
        assert_eq!(
            merge_two(&root, "run-a", "run-b").unwrap_err(),
            "Cannot merge coverage runs run-b and run-a: run-b listed the mutants of src/other.js, and run-a, which recorded it with the same bytes, did not"
        );
        fs::remove_dir_all(root).unwrap();
    }

    /// A run's mutants list that does not read, or that lists another
    /// version's mutants than the first run's, is not merged.
    #[test]
    fn runs_whose_mutant_lists_do_not_read_or_differ_in_version_are_not_merged() {
        let root = temporary();
        create_analyzable_test_run(&root, "run-a");
        create_analyzable_test_run(&root, "run-b");
        list_mutants(&root, "run-a", Some(&["src/app.js"]), &[]);
        rewrite_entry(&root, "run-b", "mutants.json", Some(b"not json".to_vec()));
        let unread = merge_two(&root, "run-a", "run-b").unwrap_err();
        assert!(
            unread
                .starts_with("Cannot merge coverage run run-b: its mutants.json cannot be read: "),
            "{unread}"
        );
        let other_version = serde_json::json!({
            "schemaVersion": 1, "mutantSet": "stryker-js@9.0.0", "files": {}
        });
        rewrite_entry(
            &root,
            "run-b",
            "mutants.json",
            Some(serde_json::to_vec(&other_version).unwrap()),
        );
        assert_eq!(
            merge_two(&root, "run-a", "run-b").unwrap_err(),
            "Cannot merge coverage run run-b: it listed the mutants of stryker-js@9.0.0, and run run-a those of stryker-js@10.1.1"
        );
        assert!(!root.join(".supercov/runs/merged").exists());
        fs::remove_dir_all(root).unwrap();
    }

    /// A run that listed another language's mutants merges with one that
    /// listed JavaScript's: each file keeps its language and each language
    /// its set.
    #[test]
    fn runs_that_listed_different_languages_merge_each_with_its_set() {
        let root = temporary();
        create_analyzable_test_run(&root, "run-a");
        create_analyzable_test_run(&root, "run-b");
        list_mutants(&root, "run-a", Some(&["src/app.js"]), &[]);
        let python = serde_json::json!({
            "schemaVersion": 1,
            "mutantSet": "mutmut@3",
            "mutantSets": {"python": "mutmut@3"},
            "languages": {"src/app.py": "python"},
            "files": {"src/app.py": [{
                "mutator": "BooleanLiteral", "replacement": "False",
                "start": [1, 1], "end": [1, 5], "original": "True"
            }]}
        });
        rewrite_entry(
            &root,
            "run-b",
            "mutants.json",
            Some(serde_json::to_vec(&python).unwrap()),
        );
        merge_two(&root, "run-a", "run-b").unwrap();
        let (_, merged) = merged_mutants_file(&root);
        assert_eq!(merged.language_of("src/app.py"), "python");
        assert_eq!(merged.language_of("src/app.js"), "javascript");
        assert_eq!(
            merged.mutant_set.as_deref(),
            Some("stryker-js@10.1.1, mutmut@3")
        );
        assert_eq!(merged.files.len(), 2);
        fs::remove_dir_all(root).unwrap();
    }

    /// Runs that listed no file's mutants merge into a run of the same
    /// mutants that lists none; a file only one run recorded is listed as
    /// the run the merge grows into listed it, however the other did.
    #[test]
    fn runs_that_listed_no_file_or_a_file_one_did_not_record_merge() {
        let root = temporary();
        create_analyzable_test_run(&root, "run-a");
        create_analyzable_test_run(&root, "run-b");
        list_mutants(&root, "run-a", Some(&[]), &[]);
        list_mutants(&root, "run-b", Some(&[]), &[]);
        merge_two(&root, "run-a", "run-b").unwrap();
        let (_, merged) = merged_mutants_file(&root);
        assert_eq!(merged.mutant_set.as_deref(), Some("stryker-js@10.1.1"));
        assert!(merged.files.is_empty());
        fs::remove_dir_all(root).unwrap();

        let root = temporary();
        create_analyzable_test_run(&root, "run-a");
        create_analyzable_test_run(&root, "run-b");
        record_files(&root, "run-a", &[("src/other.js", "left && right")]);
        record_files(&root, "run-b", &[]);
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/other.js"), "left && right").unwrap();
        list_mutants(&root, "run-a", Some(&["src/other.js"]), &[]);
        list_mutants(&root, "run-b", Some(&["src/other.js"]), &["false"]);
        merge_two(&root, "run-a", "run-b").unwrap();
        let (_, merged) = merged_mutants_file(&root);
        let ids = merged.files["src/other.js"]
            .iter()
            .map(|mutant| mutant.id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(ids, ["src/other.js:1:5-1:18:ConditionalExpression:true"]);
        fs::remove_dir_all(root).unwrap();
    }

    /// Writes `verdicts` (mutant id, status) into run `id` as its strong
    /// verdicts of `set`.
    fn save_verdicts(root: &Path, id: &str, set: &str, verdicts: &[(&str, &str)]) {
        let mutants = verdicts
            .iter()
            .map(|(mutant, status)| {
                (
                    (*mutant).to_owned(),
                    serde_json::json!({"status": status, "tests": 1}),
                )
            })
            .collect::<serde_json::Map<_, _>>();
        fs::write(
            root.join(".supercov/runs")
                .join(id)
                .join(crate::strong_plan::STRONG_VERDICTS_FILE),
            serde_json::to_vec(&serde_json::json!({
                "schemaVersion": 1, "run": id, "mutantSet": set, "mutants": mutants
            }))
            .unwrap(),
        )
        .unwrap();
    }

    /// The merged run's strong verdicts, by mutant id, as statuses.
    fn kept_statuses(root: &Path) -> Option<Vec<(String, String)>> {
        let saved = fs::read(
            root.join(".supercov/runs/merged")
                .join(crate::strong_plan::STRONG_VERDICTS_FILE),
        )
        .ok()?;
        let document = serde_json::from_slice::<StrongVerdicts>(&saved).unwrap();
        assert_eq!(document.run, "merged");
        Some(
            document
                .mutants
                .into_iter()
                .map(|(id, verdict)| (id, verdict.status))
                .collect(),
        )
    }

    /// The merge's notes on strong verdicts: these runs are made alike, so
    /// the note that their tests' records were kept once is left out.
    fn merge_noted(root: &Path, ids: &[&str]) -> Vec<String> {
        merge_runs(
            root,
            &ids.iter().map(|id| (*id).to_owned()).collect::<Vec<_>>(),
            "merged",
            "2026-01-01T00:00:00.000Z",
            false,
        )
        .unwrap()
        .notes
        .into_iter()
        .filter(|note| !note.ends_with("byte for byte kept once, from that run"))
        .collect()
    }

    const TRUE: &str = "src/app.js:1:5-1:18:ConditionalExpression:true";
    const FALSE: &str = "src/app.js:1:5-1:18:ConditionalExpression:false";

    /// The runs' strong verdicts are kept in the merged run, by mutant: a
    /// kill in one, a survival in the other where it measured every test
    /// that could kill it. A run whose verdicts are of another mutant set
    /// lends none, and says so; one whose verdicts do not read lends none.
    #[test]
    fn merged_runs_keep_their_strong_verdicts() {
        let root = temporary();
        for id in ["run-a", "run-b", "run-c", "run-d"] {
            create_analyzable_test_run(&root, id);
            list_mutants(&root, id, Some(&["src/app.js"]), &[]);
        }
        let set = "stryker-js@10.1.1";
        save_verdicts(&root, "run-a", set, &[(TRUE, "killed")]);
        save_verdicts(
            &root,
            "run-b",
            set,
            &[(TRUE, "survived"), (FALSE, "survived")],
        );
        save_verdicts(&root, "run-c", "stryker-js@9.0.0", &[(FALSE, "killed")]);
        fs::write(
            root.join(".supercov/runs/run-d")
                .join(crate::strong_plan::STRONG_VERDICTS_FILE),
            b"not json",
        )
        .unwrap();
        let notes = merge_noted(&root, &["run-a", "run-b", "run-c", "run-d"]);
        assert_eq!(
            notes,
            [
                "run run-c's strong verdicts are not kept: they are of the mutants of stryker-js@9.0.0, which the merged run does not list",
                "2 strong verdict(s) kept from runs run-a, run-b: 1 from run-a, 1 from run-b",
            ]
        );
        assert_eq!(
            kept_statuses(&root).unwrap(),
            [
                (FALSE.to_owned(), "survived".to_owned()),
                (TRUE.to_owned(), "killed".to_owned())
            ]
        );
        fs::remove_dir_all(root).unwrap();
    }

    /// Where no merged run's verdicts are of the merged list's set, nothing
    /// is saved; where the verdicts cannot be saved, the merge says so and
    /// still stands.
    #[test]
    fn verdicts_of_another_set_or_that_cannot_be_saved_are_not_kept() {
        let root = temporary();
        for id in ["run-a", "run-b"] {
            create_analyzable_test_run(&root, id);
            list_mutants(&root, id, Some(&["src/app.js"]), &[]);
        }
        save_verdicts(&root, "run-a", "stryker-js@9.0.0", &[(TRUE, "killed")]);
        let notes = merge_noted(&root, &["run-a", "run-b"]);
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert!(notes[0].starts_with("run run-a's strong verdicts are not kept"));
        assert_eq!(kept_statuses(&root), None);
        fs::remove_dir_all(root.join(".supercov/runs/merged")).unwrap();
        save_verdicts(&root, "run-a", "stryker-js@10.1.1", &[(TRUE, "killed")]);
        let notes = crate::fault::during("fs.write#strong-verdicts.save", || {
            merge_noted(&root, &["run-a", "run-b"])
        });
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert!(
            notes[0].starts_with("the runs' strong verdicts are not kept: "),
            "{notes:?}"
        );
        assert_eq!(kept_statuses(&root), None);
        // The merged run's archive cannot be read again to plan it (the
        // fifth archive opened: each run's, each run's again for what the
        // merge keeps of it, then the merged run's): nothing is lent, and
        // the merge stands.
        fs::remove_dir_all(root.join(".supercov/runs/merged")).unwrap();
        let notes = crate::fault::during("fs.open#evidence-archive.read-archive-each@5", || {
            merge_noted(&root, &["run-a", "run-b"])
        });
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert!(
            notes[0].starts_with(
                "the runs' strong verdicts are not kept: the merged run's mutants could not be planned ("
            ),
            "{notes:?}"
        );
        assert_eq!(kept_statuses(&root), None);
        assert!(root.join(".supercov/runs/merged/run.json").is_file());
        // The store cannot be listed again to find the merged run (the
        // second listing: the merge's own, then this one): the same.
        fs::remove_dir_all(root.join(".supercov/runs/merged")).unwrap();
        let notes = crate::fault::during("fs.read_dir#run-store.discover-runs@2", || {
            merge_noted(&root, &["run-a", "run-b"])
        });
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert!(
            notes[0].starts_with(
                "the runs' strong verdicts are not kept: the merged run's mutants could not be planned ("
            ),
            "{notes:?}"
        );
        assert_eq!(kept_statuses(&root), None);
        assert!(root.join(".supercov/runs/merged/run.json").is_file());
        fs::remove_dir_all(root).unwrap();
    }

    /// A run that recorded a listed file with other bytes than the run whose
    /// list of it the merge took lends no verdict of that file's mutants:
    /// here the merge grows into run-b, which recorded no file, and run-a's
    /// record of src/app.js is not run-b's.
    #[test]
    fn a_run_that_recorded_a_file_otherwise_lends_none_of_its_verdicts() {
        let root = temporary();
        for id in ["run-a", "run-b"] {
            create_analyzable_test_run(&root, id);
            list_mutants(&root, id, Some(&["src/app.js"]), &[]);
        }
        record_files(&root, "run-a", &[("src/app.js", "left && right;\n")]);
        record_files(&root, "run-b", &[]);
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/app.js"), "left && right;\n").unwrap();
        let set = "stryker-js@10.1.1";
        save_verdicts(&root, "run-a", set, &[(TRUE, "killed")]);
        save_verdicts(&root, "run-b", set, &[(FALSE, "killed")]);
        merge_noted(&root, &["run-a", "run-b"]);
        assert_eq!(
            kept_statuses(&root).unwrap(),
            [(FALSE.to_owned(), "killed".to_owned())]
        );
        fs::remove_dir_all(root).unwrap();
    }
}
