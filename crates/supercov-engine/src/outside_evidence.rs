//! Line evidence measured where Supercov could not run, as an LCOV file,
//! added to a run. Supercov runs no other coverage tool: a profile another
//! tool recorded is exported as LCOV by that tool first.
//!
//! Such evidence says which lines ran and which functions were entered, and
//! nothing finer: not which statement on a line, which way a branch went, or
//! which conditions decided. So it is credited by line, to a test of its own
//! or to the run's background, and every line it credits is visibly its own:
//! the test is of kind `imported`, its runner `lines`. A file the run did not
//! measure joins it with one statement per recorded line, and a limitation
//! that says so. The run's own records are left exactly as they were.
//!
//! The run is not changed: the import publishes a new run, made from it.
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::Path,
};

use serde_json::{Value, json};
use supercov_contracts::{
    AttributionPrecision, ExecutionModel, FrontendAttribution, FrontendLimitation,
    FrontendLimitationScope, FrontendRunDeclaration, FrontendRunnerDeclaration,
};

use crate::{
    coverage_analysis::PointKind,
    coverage_report::{
        ATTRIBUTION_EXACT, CoverageManifest, PointMeta, RawTestResult, RuntimeSnapshot,
        TestProvenance,
    },
    evidence_archive::{EvidenceArchiveEntry, read_archive, write_archive},
    lifecycle::{ProjectLock, finalize_published_run, publish_run, remove_stored_tree_deferred},
    run_store::{RawEvidenceMetadata, RunMetadata, discover_runs, select_run},
    source_manifest::{FileFingerprint, InputManifest, local_path},
};

/// The runner of an imported test: what its evidence can say.
pub const IMPORTED_RUNNER: &str = "lines";
/// The kind of an imported test, so `--kind` can pick or leave it out.
pub const IMPORTED_KIND: &str = "imported";
const BACKGROUND_RUNNER: &str = "background";

/// Who the imported lines are credited to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Credit {
    /// A test of its own, under this name.
    Test(String),
    /// The run's background: work no test can be named for.
    Background,
}

#[derive(Debug, Clone)]
pub struct ImportRequest<'a> {
    pub root: &'a Path,
    /// The run to import into: an id, a prefix of one, or `latest`.
    pub run: Option<&'a str>,
    /// An LCOV tracefile.
    pub evidence: &'a Path,
    pub credit: Credit,
    /// The imported test's outcome, where it is known.
    pub status: Option<&'a str>,
    pub run_id: &'a str,
    pub started_at: &'a str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportOutcome {
    pub run_id: String,
    pub parent: String,
    /// Lines that ran, in files the import credited.
    pub lines_run: usize,
    /// The run's statements and functions those lines credited.
    pub credited: usize,
    /// Files the run did not measure, added with the import's lines.
    pub added_files: Vec<String>,
    /// Files of the evidence left out, each with why.
    pub skipped: Vec<(String, String)>,
}

/// What a recorded file says ran: lines and function starts with their counts.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct RecordedFile {
    lines: BTreeMap<usize, u64>,
    functions: BTreeMap<usize, u64>,
}

/// LCOV's tracefile records, merged by file: a line ran if any record says so.
fn parse_lcov(text: &str) -> Result<BTreeMap<String, RecordedFile>, String> {
    let mut files = BTreeMap::<String, RecordedFile>::new();
    let mut current: Option<String> = None;
    let mut function_lines = BTreeMap::<String, usize>::new();
    let number = |value: &str, what: &str, at: usize| {
        value
            .trim()
            .parse::<u64>()
            .map_err(|_| format!("line {at}: {what} is not a number: {value}"))
    };
    for (index, raw) in text.lines().enumerate() {
        let at = index + 1;
        let line = raw.trim();
        if line.is_empty() || line == "end_of_record" {
            if line == "end_of_record" {
                current = None;
                function_lines.clear();
            }
            continue;
        }
        let Some((tag, value)) = line.split_once(':') else {
            continue;
        };
        match tag {
            "SF" => {
                files.entry(value.to_owned()).or_default();
                current = Some(value.to_owned());
                function_lines.clear();
            }
            "DA" | "FN" | "FNDA" => {
                let Some(file) = current.as_ref() else {
                    return Err(format!("line {at}: {tag} before any SF"));
                };
                let record = files.get_mut(file).expect("SF registered the file");
                let fields = value.split(',').collect::<Vec<_>>();
                match tag {
                    // DA:<line>,<count>[,<checksum>]
                    "DA" => {
                        let [line, count, ..] = fields[..] else {
                            return Err(format!("line {at}: DA needs a line and a count"));
                        };
                        let line = usize::try_from(number(line, "DA line", at)?)
                            .map_err(|_| format!("line {at}: DA line out of range"))?;
                        // Some tools write a negative or fractional count for a
                        // line that ran; only zero says it did not.
                        let count = count.trim().parse::<u64>().unwrap_or_else(|_| {
                            u64::from(count.trim().parse::<f64>().is_ok_and(|v| v != 0.0))
                        });
                        let slot = record.lines.entry(line).or_default();
                        *slot = slot.saturating_add(count);
                    }
                    // FN:<line>,<name> or, in LCOV 2, FN:<line>,<end>,<name>
                    "FN" if fields.len() >= 2 => {
                        let line = usize::try_from(number(fields[0], "FN line", at)?)
                            .map_err(|_| format!("line {at}: FN line out of range"))?;
                        let name = fields[1..].last().expect("two fields").to_string();
                        function_lines.insert(name, line);
                    }
                    // FNDA:<count>,<name>
                    "FNDA" if fields.len() >= 2 => {
                        let count = number(fields[0], "FNDA count", at)?;
                        let name = fields[1..].join(",");
                        if let Some(&line) = function_lines
                            .get(&name)
                            .or_else(|| function_lines.get(fields[fields.len() - 1]))
                        {
                            let slot = record.functions.entry(line).or_default();
                            *slot = slot.saturating_add(count);
                        }
                    }
                    _ => return Err(format!("line {at}: malformed {tag}")),
                }
            }
            _ => {}
        }
    }
    if files.is_empty() {
        return Err("no SF record: this is not an LCOV tracefile".into());
    }
    Ok(files)
}

/// An LLVM profile, raw or indexed, by its magic: `\xfflprofr\x81` and
/// `\xfflprofi\x81`, little-endian as written on every host LLVM profiles.
fn is_llvm_profile(bytes: &[u8]) -> bool {
    let magic = |kind: u8| [0x81, kind, b'f', b'o', b'r', b'p', b'l', 0xff];
    bytes.len() >= 8 && (bytes[..8] == magic(b'r') || bytes[..8] == magic(b'i'))
}

/// The project file a recorded path names: under the project root, or, for
/// evidence recorded elsewhere (a container's `/app`, another checkout), the
/// one measured file its path ends with.
fn project_path(
    root: &Path,
    canonical_root: &Path,
    recorded: &str,
    measured: &BTreeSet<String>,
) -> Result<String, String> {
    let normalized = recorded.replace('\\', "/");
    let path = Path::new(&normalized);
    // Absolute however it was recorded: `/app/...` from a Linux container is
    // absolute on Windows too, though `Path` reads it as relative there.
    let absolute = normalized.starts_with('/') || normalized.as_bytes().get(1) == Some(&b':');
    let mut relative = None;
    if absolute {
        for base in [root, canonical_root] {
            if let Ok(inside) = path.strip_prefix(base) {
                relative = Some(inside.to_string_lossy().replace('\\', "/"));
                break;
            }
        }
    } else {
        relative = Some(normalized.trim_start_matches("./").to_owned());
    }
    if let Some(relative) = relative.filter(|relative| local_path(relative))
        && (measured.contains(&relative) || root.join(&relative).is_file())
    {
        return Ok(relative);
    }
    // The longest measured path the recorded one ends with:
    // `/app/x/src/lib.rs` is `x/src/lib.rs` over `src/lib.rs`.
    let mut matches = measured
        .iter()
        // A path equal to a measured file was returned above; only a tail
        // after a whole segment is left to match.
        .filter(|file| {
            normalized
                .strip_suffix(file.as_str())
                .is_some_and(|prefix| prefix.ends_with('/'))
        })
        .collect::<Vec<_>>();
    matches.sort_by_key(|file| std::cmp::Reverse(file.len()));
    // A file the run did not measure: the longest tail of the recorded path
    // that is a file of the project.
    if matches.is_empty() {
        let parts = normalized
            .split('/')
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>();
        if let Some(tail) = (1..parts.len())
            .map(|from| parts[from..].join("/"))
            .find(|tail| local_path(tail) && root.join(tail).is_file())
        {
            return Ok(tail);
        }
    }
    // Two tails of one path are never the same length, so the longest is
    // the only one that long.
    match matches.first() {
        Some(longest) => Ok((*longest).clone()),
        None => Err(if absolute {
            "outside the project".into()
        } else {
            "no such file in the project".into()
        }),
    }
}

/// The scope entry the run keeps for a file, if any: whether it measured it.
fn scope_status(manifest: &CoverageManifest, file: &str) -> Option<(String, String)> {
    manifest
        .scope
        .as_ref()?
        .get("entries")?
        .as_array()?
        .iter()
        .find(|entry| entry.get("file").and_then(Value::as_str) == Some(file))
        .map(|entry| {
            let field = |name| {
                entry
                    .get(name)
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned()
            };
            (field("status"), field("reason"))
        })
}

fn imported_runner_declaration() -> FrontendRunnerDeclaration {
    let unavailable = |suffix: &str, scope, reason: &str| FrontendLimitation {
        id: format!("imported-lines-{suffix}-unavailable"),
        scopes: vec![scope],
        reason: reason.into(),
    };
    FrontendRunnerDeclaration {
        runner: IMPORTED_RUNNER.into(),
        execution_model: ExecutionModel::ProcessPerTest,
        attribution: FrontendAttribution {
            run: AttributionPrecision::Exact,
            worker: AttributionPrecision::Unavailable,
            test: AttributionPrecision::Exact,
            retry: AttributionPrecision::Unavailable,
            phase: AttributionPrecision::Unavailable,
            action: AttributionPrecision::Unavailable,
            assertion: AttributionPrecision::Unavailable,
        },
        limitations: vec![
            unavailable(
                "worker",
                FrontendLimitationScope::Worker,
                "Imported line evidence names no worker",
            ),
            unavailable(
                "retry",
                FrontendLimitationScope::Retry,
                "Imported line evidence is one record, with no attempts",
            ),
            unavailable(
                "phase",
                FrontendLimitationScope::Phase,
                "Imported line evidence says which lines ran, not when",
            ),
            unavailable(
                "action",
                FrontendLimitationScope::Action,
                "Imported line evidence says which lines ran, not when",
            ),
            unavailable(
                "assertion",
                FrontendLimitationScope::Assertion,
                "Imported line evidence carries no assertions",
            ),
        ],
    }
}

fn background_runner_declaration() -> FrontendRunnerDeclaration {
    FrontendRunnerDeclaration {
        runner: BACKGROUND_RUNNER.into(),
        execution_model: ExecutionModel::ParallelUnattributed,
        attribution: FrontendAttribution {
            run: AttributionPrecision::Exact,
            worker: AttributionPrecision::Unavailable,
            test: AttributionPrecision::Unavailable,
            retry: AttributionPrecision::Unavailable,
            phase: AttributionPrecision::Unavailable,
            action: AttributionPrecision::Unavailable,
            assertion: AttributionPrecision::Unavailable,
        },
        limitations: vec![FrontendLimitation {
            id: "background-unattributed".into(),
            scopes: vec![
                FrontendLimitationScope::Worker,
                FrontendLimitationScope::Test,
                FrontendLimitationScope::Retry,
                FrontendLimitationScope::Phase,
                FrontendLimitationScope::Action,
                FrontendLimitationScope::Assertion,
            ],
            reason: "Background work is not attributed to a test".into(),
        }],
    }
}

/// Add line evidence measured outside Supercov to a run, as a new run.
pub fn import_line_evidence(request: &ImportRequest<'_>) -> Result<ImportOutcome, String> {
    if let Credit::Test(name) = &request.credit
        && name.trim().is_empty()
    {
        return Err("the imported test needs a name".into());
    }
    if let Some(status) = request.status
        && !matches!(status, "passed" | "failed")
    {
        return Err(format!("--status is passed or failed, not {status}"));
    }
    let inventory = discover_runs(request.root)
        .map_err(|error| format!("Cannot read local coverage runs: {error}"))?;
    let parent = select_run(&inventory, request.run)
        .map_err(|error| format!("Cannot import into that run: {error}"))?
        .clone();
    let work = request.root.join(".supercov/work").join(request.run_id);
    let bytes = fs::read(request.evidence)
        .map_err(|error| format!("Cannot read {}: {error}", request.evidence.display()))?;
    if is_llvm_profile(&bytes) {
        return Err(format!(
            "{} is an LLVM profile: export it as LCOV with LLVM's own tools first, as `supercov docs cli` shows",
            request.evidence.display()
        ));
    }
    let text = String::from_utf8(bytes)
        .map_err(|_| format!("{} is not an LCOV tracefile", request.evidence.display()))?;
    let recorded = parse_lcov(&text).map_err(|error| {
        format!(
            "Cannot read {} as LCOV: {error}",
            request.evidence.display()
        )
    })?;

    let mut archive = read_archive(&parent.evidence_path)
        .map_err(|error| format!("Cannot read run {}: {error}", parent.id))?;
    let take = |archive: &mut Vec<EvidenceArchiveEntry>, path: &str| {
        archive
            .iter()
            .position(|entry| entry.path == path)
            .map(|at| archive.remove(at).contents)
    };
    let mut manifest = take(&mut archive, "manifest.json")
        .ok_or("the run has no manifest")
        .and_then(|bytes| {
            serde_json::from_slice::<CoverageManifest>(&bytes)
                .map_err(|_| "its manifest is invalid")
        })
        .map_err(|error| format!("Cannot import into run {}: {error}", parent.id))?;
    let mut declaration = take(&mut archive, "frontend.json")
        .ok_or("the run has no frontend declaration")
        .and_then(|bytes| {
            serde_json::from_slice::<FrontendRunDeclaration>(&bytes)
                .map_err(|_| "its frontend declaration is invalid")
        })
        .map_err(|error| format!("Cannot import into run {}: {error}", parent.id))?;
    let mut sources = take(&mut archive, crate::source_capture::ARCHIVE_PATH)
        .map(|bytes| serde_json::from_slice::<InputManifest>(&bytes))
        .transpose()
        .map_err(|_| {
            format!(
                "Cannot import into run {}: its source record is invalid",
                parent.id
            )
        })?;

    let measured = manifest
        .points
        .iter()
        .map(|point| point.file.clone())
        .chain(manifest.branches.iter().map(|branch| branch.file.clone()))
        .chain(
            manifest
                .decisions
                .iter()
                .map(|decision| decision.file.clone()),
        )
        .collect::<BTreeSet<_>>();
    let canonical_root = crate::workspace::canonicalize_simplified(request.root)
        .unwrap_or_else(|_| request.root.to_owned());
    let mut by_file = BTreeMap::<String, RecordedFile>::new();
    let mut skipped = Vec::new();
    for (path, file) in recorded {
        match project_path(request.root, &canonical_root, &path, &measured) {
            Ok(project) => {
                let merged = by_file.entry(project).or_default();
                for (line, count) in file.lines {
                    *merged.lines.entry(line).or_default() += count;
                }
                for (line, count) in file.functions {
                    *merged.functions.entry(line).or_default() += count;
                }
            }
            Err(reason) => skipped.push((path, reason)),
        }
    }

    let skipped_paths = skipped.len();
    let mut hits = BTreeSet::<String>::new();
    let mut lines_run = 0;
    let mut added_files = Vec::new();
    let evidence_name = request.evidence.file_name().map_or_else(
        || request.evidence.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    );
    for (file, recorded) in &by_file {
        if !measured.contains(file)
            && let Some((status, reason)) = scope_status(&manifest, file)
            && status != "included"
        {
            skipped.push((file.clone(), format!("the run leaves it out: {reason}")));
            continue;
        }
        let text = fs::read_to_string(request.root.join(file)).ok();
        // Evidence of other source is not mixed in: a line past the file's
        // end says the file has changed since it was recorded.
        if let Some(text) = &text {
            let length = text.lines().count();
            if let Some((line, _)) = recorded
                .lines
                .iter()
                .next_back()
                .filter(|(line, _)| **line > length)
            {
                return Err(format!(
                    "{} records line {line} of {file}, which has {length} lines: the evidence is of other source",
                    request.evidence.display()
                ));
            }
        }
        let ran = recorded
            .lines
            .iter()
            .filter(|(_, count)| **count > 0)
            .map(|(line, _)| *line)
            .collect::<BTreeSet<_>>();
        lines_run += ran.len();
        if measured.contains(file) {
            let entered = recorded
                .functions
                .iter()
                .filter(|(_, count)| **count > 0)
                .map(|(line, _)| *line)
                .collect::<BTreeSet<_>>();
            for point in manifest.points.iter().filter(|point| point.file == *file) {
                let credited = match point.kind {
                    PointKind::Statement => ran.contains(&point.line),
                    PointKind::Function => entered.contains(&point.line),
                };
                if credited {
                    hits.insert(point.id.clone());
                }
            }
            continue;
        }
        // A file the run did not measure: the evidence's own lines are its
        // statements, and its source joins the run's record of sources.
        let Some(text) = text else {
            skipped.push((file.clone(), "no such file in the project".into()));
            continue;
        };
        let source_lines = text.lines().collect::<Vec<_>>();
        for line in recorded.lines.keys() {
            let id = format!("imported:{file}:{line}");
            manifest.points.push(PointMeta {
                id: id.clone(),
                kind: PointKind::Statement,
                file: file.clone(),
                line: *line,
                column: 0,
                source: source_lines
                    .get(line.saturating_sub(1))
                    .map_or_else(String::new, |source| source.trim().to_owned()),
                label: None,
            });
            if ran.contains(line) {
                hits.insert(id);
            }
        }
        // A whole-file boundary, anchored as the Rust frontend anchors its own.
        let limitation = format!("imported-lines#{file}");
        manifest.limitations.push(json!({
            "id": limitation,
            "kind": "source-scope",
            "file": file,
            "line": 1,
            "column": 0,
            "source": "",
            "reason": format!("Measured only by the lines {evidence_name} recorded: each line is one statement, and its branches, decisions and functions are not measured"),
            "blocking": false,
        }));
        declaration.structural_limitations.push(limitation);
        if let Some(entries) = manifest
            .scope
            .as_mut()
            .and_then(|scope| scope.get_mut("entries"))
            .and_then(Value::as_array_mut)
        {
            entries.push(json!({
                "file": file,
                "status": "included",
                "reason": "imported line evidence",
            }));
        }
        if let Some(sources) = sources.as_mut() {
            sources
                .files
                .insert(file.clone(), FileFingerprint::read(file, &text));
        }
        added_files.push(file.clone());
    }
    // Every file the run leaves out, or that is gone: a run made of
    // nothing would be a run all the same.
    if !by_file.is_empty() && skipped.len() == by_file.len() + skipped_paths {
        return Err(format!(
            "Nothing in {} is a file the run measures: {}",
            request.evidence.display(),
            crate::run_merge::listed(
                &skipped
                    .iter()
                    .map(|(file, reason)| format!("{file} ({reason})"))
                    .collect::<Vec<_>>()
            )
        ));
    }
    if by_file.is_empty() {
        return Err(format!(
            "Nothing in {} is a file of this project: {}",
            request.evidence.display(),
            crate::run_merge::listed(
                &skipped
                    .iter()
                    .map(|(file, reason)| format!("{file} ({reason})"))
                    .collect::<Vec<_>>()
            )
        ));
    }

    let imports = archive
        .iter()
        .filter(|entry| {
            entry.path.starts_with("imported-")
                || entry.path.starts_with("server/background/imported-")
        })
        .count()
        + 1;
    let hits = hits.into_iter().collect::<Vec<_>>();
    let credited = hits.len();
    match &request.credit {
        Credit::Test(name) => {
            let test_id = format!("imported:{name}");
            let taken = archive.iter().any(|entry| {
                std::str::from_utf8(&entry.contents).is_ok_and(|text| {
                    text.contains(&serde_json::to_string(&test_id).expect("string"))
                })
            });
            if taken {
                return Err(format!(
                    "Run {} already holds an imported test named {name}; import into the original run, or give this one another name",
                    parent.id
                ));
            }
            let record = RawTestResult {
                test_id: Some(test_id),
                scope: None,
                test: name.clone(),
                test_file: None,
                title: Some(name.clone()),
                retry: request.status.map(|_| 0),
                status: Some(request.status.unwrap_or("unknown").into()),
                expected_status: None,
                flaky: false,
                span: None,
                provenance: TestProvenance {
                    runner: IMPORTED_RUNNER.into(),
                    kind: IMPORTED_KIND.into(),
                    project: None,
                    source: format!("imported:{evidence_name}"),
                    tags: Vec::new(),
                },
                role: "test".into(),
                attribution: ATTRIBUTION_EXACT.into(),
                phases: vec![],
                runtime: vec![RuntimeSnapshot {
                    decisions: vec![],
                    hits,
                    events: vec![],
                    logicals: vec![],
                    phase_id: None,
                }],
                browser: vec![],
                server: vec![],
            };
            archive.push(EvidenceArchiveEntry {
                path: format!("imported-{imports}/mcdc.json"),
                contents: serde_json::to_vec(&record).map_err(|error| error.to_string())?,
            });
            if !declaration
                .runners
                .iter()
                .any(|runner| runner.runner == IMPORTED_RUNNER)
            {
                declaration.runners.push(imported_runner_declaration());
            }
        }
        Credit::Background => {
            let mut contents = Vec::new();
            for hit in &hits {
                contents.extend(
                    serde_json::to_vec(&json!({ "type": "hit", "id": hit })).expect("json"),
                );
                contents.push(b'\n');
            }
            archive.push(EvidenceArchiveEntry {
                path: format!("server/background/imported-{imports}-0.jsonl"),
                contents,
            });
            if !declaration
                .runners
                .iter()
                .any(|runner| runner.runner == BACKGROUND_RUNNER)
            {
                declaration.runners.push(background_runner_declaration());
            }
        }
    }
    declaration
        .runners
        .sort_by(|left, right| left.runner.cmp(&right.runner));

    let mut entries = Vec::with_capacity(archive.len() + 4);
    entries.push(EvidenceArchiveEntry {
        path: "frontend.json".into(),
        contents: serde_json::to_vec(&declaration).map_err(|error| error.to_string())?,
    });
    entries.push(EvidenceArchiveEntry {
        path: "manifest.json".into(),
        contents: serde_json::to_vec(&manifest).map_err(|error| error.to_string())?,
    });
    if let Some(sources) = &sources {
        entries.push(EvidenceArchiveEntry {
            path: crate::source_capture::ARCHIVE_PATH.into(),
            contents: serde_json::to_vec(sources).map_err(|error| error.to_string())?,
        });
    }
    for entry in archive {
        // The records name the run they were made in; they are this run's now.
        let contents = if entry.path == "coverage-model.json" {
            entry.contents
        } else {
            crate::run_merge::rewritten_contents(&entry.contents, request.run_id, None)
                .map_err(|error| format!("Cannot import into run {}: {error}", parent.id))?
                .unwrap_or_default()
        };
        entries.push(EvidenceArchiveEntry {
            path: entry.path,
            contents,
        });
    }

    let mut lock = ProjectLock::acquire(request.root, request.run_id, request.started_at)
        .map_err(|error| format!("Cannot lock coverage store for the import: {error}"))?;
    let result = (|| -> Result<(), String> {
        let archive_path = work.join("evidence.raw.gz");
        let raw = write_archive(entries, &archive_path)
            .map_err(|error| format!("Could not write the imported run: {error}"))?;
        let mut command = vec![
            "supercov".to_owned(),
            "import".to_owned(),
            parent.id.clone(),
            request.evidence.display().to_string(),
        ];
        match &request.credit {
            Credit::Test(name) => command.extend(["--test".to_owned(), name.clone()]),
            Credit::Background => command.push("--background".to_owned()),
        }
        if let Some(status) = request.status {
            command.extend(["--status".to_owned(), status.to_owned()]);
        }
        let metadata = RunMetadata {
            id: request.run_id.into(),
            started_at: request.started_at.into(),
            duration_ms: 0.0,
            command,
            test_exit_code: match request.status {
                Some("failed") => Some(1),
                _ => parent.metadata.test_exit_code,
            },
            integrity: parent.metadata.integrity.clone(),
            raw_evidence: RawEvidenceMetadata {
                schema_version: raw.schema_version,
                format: raw.format.into(),
                file: raw.file.into(),
                files: raw.files,
                uncompressed_bytes: raw.uncompressed_bytes,
                compressed_bytes: raw.compressed_bytes,
            },
            isolated_build: parent.metadata.isolated_build,
            instrumented_build_cache: None,
            timings: None,
            // A run made from another, like a merge: its fingerprint is the
            // parent's, checked against the parent's command.
            merged: Some(true),
            parents: Some(vec![parent.id.clone()]),
            source_roots: parent.metadata.source_roots.clone(),
            copy: None,
            merged_across: parent.metadata.merged_across.clone(),
            recorded_by: parent.metadata.recorded_by.clone(),
            companions: None,
        };
        publish_run(request.root, &metadata, &archive_path)
            .map_err(|error| format!("Could not publish the imported run: {error}"))?;
        finalize_published_run(request.root, request.run_id)
            .map_err(|error| format!("Could not finalize the imported run: {error}"))?;
        Ok(())
    })();
    let _ = remove_stored_tree_deferred(request.root, &work);
    let release = lock
        .release()
        .map_err(|error| format!("Could not release the import lock: {error}"));
    result?;
    release?;
    Ok(ImportOutcome {
        run_id: request.run_id.into(),
        parent: parent.id,
        lines_run,
        credited,
        added_files,
        skipped,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lcov_records_merge_by_file_and_name_function_starts() {
        let files = parse_lcov(
            "TN:\nSF:/app/src/lib.rs\nFN:3,grade\nFNDA:2,grade\nFN:9,9,unused\nFNDA:0,unused\nDA:3,2\nDA:4,0\nDA:5,1,abc\nend_of_record\nTN:other\nSF:/app/src/lib.rs\nDA:4,3\nend_of_record\n",
        )
        .unwrap();
        let file = &files["/app/src/lib.rs"];
        assert_eq!(file.lines, BTreeMap::from([(3, 2), (4, 3), (5, 1)]));
        assert_eq!(file.functions, BTreeMap::from([(3, 2), (9, 0)]));
    }

    #[test]
    fn what_is_not_lcov_is_said_to_be() {
        assert!(parse_lcov("hello\n").unwrap_err().contains("not an LCOV"));
        assert!(
            parse_lcov("DA:1,1\n")
                .unwrap_err()
                .contains("before any SF")
        );
        assert!(
            parse_lcov("SF:a\nDA:x,1\n")
                .unwrap_err()
                .contains("not a number")
        );
    }

    #[test]
    fn recorded_paths_find_the_one_project_file_they_name() {
        let root = Path::new("/work/project");
        let measured = BTreeSet::from([
            "src/lib.rs".to_owned(),
            "a/util.rs".to_owned(),
            "b/util.rs".to_owned(),
            "crates/x/src/util.rs".to_owned(),
        ]);
        let path = |recorded| project_path(root, root, recorded, &measured);
        assert_eq!(path("/work/project/src/lib.rs").unwrap(), "src/lib.rs");
        assert_eq!(path("src/lib.rs").unwrap(), "src/lib.rs");
        assert_eq!(path("./src/lib.rs").unwrap(), "src/lib.rs");
        // Recorded in a container that mounted the project at /app.
        assert_eq!(path("/app/src/lib.rs").unwrap(), "src/lib.rs");
        assert_eq!(path("C:\\build\\src\\lib.rs").unwrap(), "src/lib.rs");
        // The longest match where it is the only one that long.
        assert_eq!(
            path("/app/crates/x/src/util.rs").unwrap(),
            "crates/x/src/util.rs"
        );
        assert!(path("/app/util.rs").is_err());
        assert!(
            path("/elsewhere/lib/other.rs")
                .unwrap_err()
                .contains("outside")
        );
        // A suffix that is not a whole path segment is no match.
        assert!(path("/app/xsrc/lib.rs").is_err());
    }

    #[test]
    fn lcov_edges_blank_lines_short_records_and_names_with_commas() {
        let files = parse_lcov(
            "SF:src/a.rs\n\nFN:4,(anonymous,1)\nFNDA:3,(anonymous,1)\nFN:8,b\nFNDA:1,x,b\nDA:4,1\nend_of_record\n",
        )
        .unwrap();
        // A name with a comma, and a count whose name the record splits,
        // are both found by the name's last field.
        assert_eq!(
            files["src/a.rs"].functions,
            BTreeMap::from([(4, 3), (8, 1)])
        );
        assert!(
            parse_lcov("SF:a\nFN:3\n")
                .unwrap_err()
                .contains("malformed FN")
        );
        assert!(
            parse_lcov("SF:a\nFNDA:3\n")
                .unwrap_err()
                .contains("malformed FNDA")
        );
        assert!(parse_lcov("SF:a\nDA:3\n").unwrap_err().contains("DA needs"));
        // A count for a function no FN named is no function's.
        let files = parse_lcov("SF:a\nFNDA:2,ghost\nend_of_record\n").unwrap();
        assert!(files["a"].functions.is_empty());
    }

    #[test]
    fn a_longer_tail_wins_and_a_path_that_climbs_or_names_nothing_is_refused() {
        let root = Path::new("/work/project");
        let measured = BTreeSet::from(["src/util.rs".to_owned(), "x/src/util.rs".to_owned()]);
        let path = |recorded| project_path(root, root, recorded, &measured);
        // Both end the recorded path; the longer one is the file.
        assert_eq!(path("/app/x/src/util.rs").unwrap(), "x/src/util.rs");
        // A relative path naming no project file says so, not "outside".
        assert!(
            path("lib/none.rs")
                .unwrap_err()
                .contains("no such file in the project")
        );
        assert!(path("/app/../../etc/passwd").is_err());
    }

    #[test]
    fn llvm_profiles_are_known_by_their_magic() {
        let raw = [0x81, b'r', b'f', b'o', b'r', b'p', b'l', 0xff, 0];
        let indexed = [0x81, b'i', b'f', b'o', b'r', b'p', b'l', 0xff];
        assert!(is_llvm_profile(&raw));
        assert!(is_llvm_profile(&indexed));
        assert!(!is_llvm_profile(b"SF:a\nDA:1,1\n"));
    }

    /// A recorded path is the project file it names: under the root, the
    /// longest measured file it ends with, or the longest tail that is a
    /// file of the project; otherwise it says why not.
    #[test]
    fn a_recorded_path_is_read_as_the_project_file_it_names() {
        let base = std::env::temp_dir().join(format!(
            "supercov-project-path-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(base.join("src/deep")).unwrap();
        let root = crate::test_directory::TestDirectory::adopt(base);
        std::fs::write(root.join("README.md"), "hi\n").unwrap();
        std::fs::write(root.join("src/lib.rs"), "\n").unwrap();
        let measured = ["src/lib.rs", "src/deep/x.rs", "deep/x.rs"]
            .map(str::to_owned)
            .into_iter()
            .collect::<BTreeSet<_>>();
        let canonical = root.canonicalize().unwrap();
        let read = |recorded: &str| project_path(&root, &canonical, recorded, &measured);
        let inside = format!("{}/src/lib.rs", root.display());
        for (recorded, expected) in [
            ("/app/src/lib.rs", Ok("src/lib.rs")),
            ("./src/lib.rs", Ok("src/lib.rs")),
            (inside.as_str(), Ok("src/lib.rs")),
            ("C:\\proj\\src\\lib.rs", Ok("src/lib.rs")),
            // Both end it; the longer is the file.
            ("/app/src/deep/x.rs", Ok("src/deep/x.rs")),
            // Not measured, but a file of the project.
            ("/elsewhere/README.md", Ok("README.md")),
            ("/nowhere/zzz.rs", Err("outside the project")),
            ("zzz.rs", Err("no such file in the project")),
        ] {
            assert_eq!(
                read(recorded),
                expected.map(str::to_owned).map_err(str::to_owned),
                "{recorded}"
            );
        }
        let magic = |kind: u8| vec![0x81, kind, b'f', b'o', b'r', b'p', b'l', 0xff, 0];
        assert!(is_llvm_profile(&magic(b'r')));
        assert!(is_llvm_profile(&magic(b'i')));
        assert!(!is_llvm_profile(&magic(b'x')));
        assert!(!is_llvm_profile(&magic(b'r')[..7]));
    }
}
