//! Validated local run discovery and immutable run identity.
//!
//! The run store is user data. Discovery never follows links, never mutates a
//! run, and never silently treats malformed metadata as a valid run. Callers
//! receive accepted runs and rejected-entry diagnostics independently.

use crate::fault_fs as fs;
use std::{
    io::{self, Read, Write},
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use supercov_contracts::EVIDENCE_ARCHIVE_SCHEMA_VERSION;

use crate::query_index::{QUERY_INDEX_SCHEMA_VERSION, QueryIndexIdentity};
use crate::{
    coverage_index::{CoverageIndex, CoverageIndexError, coverage_index_sections},
    coverage_report::{
        ArchiveReportRequest, CoverageReport, ExitCodeInput, ReportError, analyze_coverage_archive,
    },
    evidence_archive::read_archive_schema_version,
    query_index::{QueryIndex, QueryIndexError, write_query_index},
};

const MAX_RUN_METADATA_BYTES: u64 = 1024 * 1024;
pub const RUST_ANALYSIS_ABI_VERSION: u32 = 1;
pub const RUST_QUERY_PRODUCER_ABI_VERSION: u32 = 4;
pub const RUST_QUERY_INDEX_FILE: &str = "query-index.v1.bin";
/// The statements that continue onto later lines, written at publication.
pub const STATEMENT_SPANS_FILE: &str = "statement-spans.json";
/// What `assertions assess` reads from the analysis, written at publication.
pub const ASSESSMENT_INPUT_FILE: &str = "assessment-input.json.gz";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RunFingerprint {
    pub algorithm: String,
    pub source: String,
    pub tests: String,
    pub dependencies: String,
    pub configuration: String,
    pub instrumenter: String,
    pub execution: String,
    pub combined: String,
    pub source_files: usize,
    pub test_files: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GitIntegrity {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub revision: Option<String>,
    /// Branch at measurement time. Older runs and detached HEADs may omit it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    pub dirty: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RunIntegrity {
    pub schema_version: u32,
    pub instrumenter_version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub git: Option<GitIntegrity>,
    pub fingerprint: RunFingerprint,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stale: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stale_reasons: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RunTimings {
    #[serde(serialize_with = "crate::coverage_analysis::serialize_javascript_number")]
    pub initialization_ms: f64,
    #[serde(serialize_with = "crate::coverage_analysis::serialize_javascript_number")]
    pub workspace_preparation_ms: f64,
    #[serde(serialize_with = "crate::coverage_analysis::serialize_javascript_number")]
    pub adapter_setup_ms: f64,
    #[serde(serialize_with = "crate::coverage_analysis::serialize_javascript_number")]
    pub instrumented_build_ms: f64,
    #[serde(serialize_with = "crate::coverage_analysis::serialize_javascript_number")]
    pub test_command_ms: f64,
    #[serde(serialize_with = "crate::coverage_analysis::serialize_javascript_number")]
    pub evidence_publication_ms: f64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InstrumentedBuildCache {
    pub key: String,
    pub reused: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RawEvidenceMetadata {
    pub schema_version: u32,
    pub format: String,
    pub file: String,
    pub files: usize,
    pub uncompressed_bytes: u64,
    pub compressed_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RunMetadata {
    pub id: String,
    pub started_at: String,
    #[serde(serialize_with = "crate::coverage_analysis::serialize_javascript_number")]
    pub duration_ms: f64,
    pub command: Vec<String>,
    pub test_exit_code: Option<i32>,
    pub integrity: RunIntegrity,
    pub raw_evidence: RawEvidenceMetadata,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub isolated_build: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instrumented_build_cache: Option<InstrumentedBuildCache>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timings: Option<RunTimings>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub merged: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parents: Option<Vec<String>>,
    /// The `SUPERCOV_SOURCE_ROOTS` the run was measured under. It decides what
    /// the run measures, so it is part of what the run is, replayed like the
    /// command when a later query asks whether the checkout has moved on. The
    /// environment of that later query is no guide: it is usually a different
    /// shell, and a run judged against roots it was not measured under read as
    /// stale the moment it was recorded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_roots: Option<Vec<String>>,
    /// Which copy of the project the command ran in, where it ran in one: 1
    /// for a run alone, 2 for one started while that copy was in use. It is
    /// where what the command wrote is to be found.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub copy: Option<usize>,
    /// For a merge made with `--allow-changed`: what differed between the
    /// runs merged, files the tests load and `configuration`. The merge is
    /// of tests that ran against those as they were then.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub merged_across: Option<Vec<String>>,
    /// The Supercov release that recorded the run. A run of another release
    /// was instrumented differently and cannot be merged with this one's;
    /// the version is what says so. Absent in runs recorded before 4.7.0.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recorded_by: Option<String>,
    /// The runs one command published beside this one: the runs of servers
    /// a browser suite drove, or the browser run a server's run took its
    /// tests from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub companions: Option<Vec<CompanionRun>>,
}

/// A run published from the same command as another.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CompanionRun {
    pub id: String,
    /// `JavaScript`, `Python`, `Ruby`.
    pub language: String,
    /// `server`: a server the run's tests drove. `browser`: the browser run
    /// whose tests this server's run took.
    pub role: String,
}

#[cfg(test)]
pub(crate) fn create_analyzable_test_run(root: &Path, id: &str) -> PathBuf {
    create_analyzable_run(root, id, None)
}

/// The same run as a suite that ran its tests at once: the coverage sits in
/// the record nobody could be credited with, and the test beside it is named,
/// run-wide, and holds nothing of its own. It is the shape Go publishes for a
/// package whose tests all call `t.Parallel()`.
#[cfg(test)]
pub(crate) fn create_run_wide_test_run(root: &Path, id: &str) -> PathBuf {
    create_analyzable_run(root, id, Some(crate::coverage_report::ATTRIBUTION_RUN_WIDE))
}

/// The same run as a Ruby suite: the test keeps the coverage it was credited
/// with, and that coverage is a lower bound rather than the whole of what it
/// ran. What its own record proves, it still proves.
#[cfg(test)]
pub(crate) fn create_partial_test_run(root: &Path, id: &str) -> PathBuf {
    create_analyzable_run(root, id, Some(crate::coverage_report::ATTRIBUTION_PARTIAL))
}

#[cfg(test)]
fn create_analyzable_run(root: &Path, id: &str, attribution: Option<&str>) -> PathBuf {
    use crate::{
        coverage_analysis::{McdcVector, PointKind},
        coverage_report::{
            BranchAlternativeMeta, BranchMeta, CoverageManifest, DecisionMeta, DecisionSnapshot,
            PointMeta, RawTestResult, RuntimeSnapshot, TestProvenance,
        },
        evidence_archive::{EvidenceArchiveEntry, write_archive},
    };

    let directory = root.join(".supercov/runs").join(id);
    fs::create_dir_all(&directory).unwrap();
    let decision = DecisionMeta {
        id: "decision".into(),
        file: "src/app.js".into(),
        line: 1,
        column: 0,
        source: "left && right".into(),
        conditions: vec!["left".into(), "right".into()],
        kind: "if".into(),
    };
    let manifest = CoverageManifest {
        unmeasured: Vec::new(),
        decisions: vec![decision.clone()],
        points: vec![PointMeta {
            id: "statement".into(),
            kind: PointKind::Statement,
            file: "src/app.js".into(),
            line: 1,
            column: 0,
            source: "work();".into(),
            label: None,
        }],
        branches: vec![BranchMeta {
            id: "branch".into(),
            kind: "if".into(),
            file: "src/app.js".into(),
            line: 1,
            column: 0,
            source: "if (left && right)".into(),
            alternatives: vec![
                BranchAlternativeMeta {
                    id: "branch:true".into(),
                    label: "true".into(),
                },
                BranchAlternativeMeta {
                    id: "branch:false".into(),
                    label: "false".into(),
                },
            ],
        }],
        limitations: vec![],
        scope: None,
        assertion_sites: Vec::new(),
    };
    let result = RawTestResult {
        span: None,
        test_id: Some("test".into()),
        scope: None,
        test: "test".into(),
        test_file: Some("tests/app.test.js".into()),
        title: None,
        retry: Some(0),
        status: Some("passed".into()),
        expected_status: None,
        flaky: false,
        provenance: TestProvenance {
            runner: "node:test".into(),
            kind: "unit".into(),
            project: None,
            source: "test-fixture".into(),
            tags: Vec::new(),
        },
        role: "test".into(),
        attribution: crate::coverage_report::ATTRIBUTION_EXACT.into(),
        phases: vec![],
        runtime: vec![RuntimeSnapshot {
            decisions: vec![DecisionSnapshot {
                meta: decision,
                vectors: vec![
                    McdcVector {
                        values: vec![Some(false), Some(false)],
                        outcome: false,
                    },
                    McdcVector {
                        values: vec![Some(false), Some(true)],
                        outcome: false,
                    },
                    McdcVector {
                        values: vec![Some(true), Some(false)],
                        outcome: false,
                    },
                    McdcVector {
                        values: vec![Some(true), Some(true)],
                        outcome: true,
                    },
                ],
            }],
            hits: vec![
                "statement".into(),
                "branch:true".into(),
                "branch:false".into(),
            ],
            events: vec![],
            logicals: vec![],
            phase_id: None,
        }],
        browser: vec![],
        server: vec![],
    };
    let results = if attribution == Some(crate::coverage_report::ATTRIBUTION_PARTIAL) {
        // It keeps its hits: a lower bound is still a claim.
        let mut partial = result.clone();
        partial.attribution = crate::coverage_report::ATTRIBUTION_PARTIAL.into();
        vec![partial]
    } else if attribution == Some(crate::coverage_report::ATTRIBUTION_RUN_WIDE) {
        let mut unclaimed = result.clone();
        unclaimed.test = "run".into();
        unclaimed.test_id = Some("run".into());
        unclaimed.test_file = None;
        unclaimed.role = "background".into();
        let mut parallel = result.clone();
        parallel.attribution = crate::coverage_report::ATTRIBUTION_RUN_WIDE.into();
        // Named, with its real outcome, and credited with nothing.
        parallel.runtime = vec![RuntimeSnapshot {
            decisions: vec![],
            hits: vec![],
            events: vec![],
            logicals: vec![],
            phase_id: None,
        }];
        vec![unclaimed, parallel]
    } else {
        vec![result]
    };
    let mut entries = vec![
        EvidenceArchiveEntry {
            path: "coverage-model.json".into(),
            contents: serde_json::to_vec(&serde_json::json!({
                "schemaVersion": 1,
                "language": "javascript",
                "variant": "fixture-v1",
                "name": "Fixture model",
                "completenessMeaning": "Every fixture obligation was observed.",
                "measured": ["fixture obligations"],
                "notMeasured": []
            }))
            .unwrap(),
        },
        EvidenceArchiveEntry {
            path: "frontend.json".into(),
            contents: serde_json::to_vec(&serde_json::json!({
                "protocolVersion": 2,
                "frontendId": "javascript",
                "frontendVersion": "fixture-v1",
                "language": "javascript",
                "structuralSource": "owned-probes",
                "runners": [{
                    "runner": "node:test",
                    "executionModel": "serial-in-process",
                    "attribution": {
                        "run": "exact",
                        "worker": "unavailable",
                        "test": "exact",
                        "retry": "exact",
                        "phase": "exact",
                        "action": "exact",
                        "assertion": "exact"
                    },
                    "limitations": [{
                        "id": "fixture-worker-unavailable",
                        "scopes": ["worker"],
                        "reason": "The fixture intentionally has no worker identity"
                    }]
                }],
                "structuralLimitations": []
            }))
            .unwrap(),
        },
        EvidenceArchiveEntry {
            path: "manifest.json".into(),
            contents: serde_json::to_vec(&manifest).unwrap(),
        },
    ];
    for (at, result) in results.iter().enumerate() {
        entries.push(EvidenceArchiveEntry {
            // One record per worker directory: the reader matches
            // `<anything>/mcdc.json`, so a second record needs its own
            // directory rather than a decorated file name.
            path: format!(
                "worker{}/mcdc.json",
                if at == 0 {
                    String::new()
                } else {
                    format!("-{at}")
                }
            ),
            contents: serde_json::to_vec(result).unwrap(),
        });
    }
    let archive = write_archive(entries, &directory.join("evidence.raw.gz")).unwrap();
    let digest = |character: char| std::iter::repeat_n(character, 64).collect::<String>();
    let metadata = RunMetadata {
        id: id.into(),
        started_at: id.into(),
        duration_ms: 1.0,
        command: vec!["node".into(), "--test".into()],
        test_exit_code: Some(0),
        integrity: RunIntegrity {
            schema_version: 2,
            instrumenter_version: "test".into(),
            git: None,
            fingerprint: RunFingerprint {
                algorithm: "sha256".into(),
                source: digest('a'),
                tests: digest('b'),
                dependencies: digest('c'),
                configuration: digest('d'),
                instrumenter: digest('e'),
                execution: digest('f'),
                combined: digest('0'),
                source_files: 1,
                test_files: 1,
            },
            stale: None,
            stale_reasons: None,
        },
        raw_evidence: RawEvidenceMetadata {
            schema_version: archive.schema_version,
            format: archive.format.into(),
            file: archive.file.into(),
            files: archive.files,
            uncompressed_bytes: archive.uncompressed_bytes,
            compressed_bytes: archive.compressed_bytes,
        },
        isolated_build: Some(true),
        instrumented_build_cache: None,
        timings: None,
        merged: None,
        parents: None,
        source_roots: None,
        copy: None,
        merged_across: None,
        recorded_by: None,
        companions: None,
    };
    fs::write(
        directory.join("run.json"),
        serde_json::to_vec_pretty(&metadata).unwrap(),
    )
    .unwrap();
    directory
}

#[derive(Debug, Clone, PartialEq)]
pub struct StoredRun {
    pub id: String,
    pub directory: PathBuf,
    pub evidence_path: PathBuf,
    pub metadata_path: PathBuf,
    pub query_index_path: PathBuf,
    pub metadata: RunMetadata,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RejectedRun {
    pub entry: String,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RunInventory {
    pub runs: Vec<StoredRun>,
    pub rejected: Vec<RejectedRun>,
}

#[derive(Debug)]
pub enum RunStoreError {
    Io(io::Error),
    UnsafeStore(PathBuf),
    NoRuns,
    RunNotFound(String),
    InvalidRun(&'static str),
}

impl From<io::Error> for RunStoreError {
    fn from(value: io::Error) -> Self {
        Self::Io(value)
    }
}

impl std::fmt::Display for RunStoreError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "{error}"),
            Self::UnsafeStore(path) => {
                write!(formatter, "unsafe run-store path: {}", path.display())
            }
            Self::NoRuns => write!(formatter, "no local coverage runs"),
            Self::RunNotFound(selector) => write!(formatter, "coverage run not found: {selector}"),
            Self::InvalidRun(reason) => write!(formatter, "invalid coverage run: {reason}"),
        }
    }
}

impl std::error::Error for RunStoreError {}

fn regular_file(path: &Path) -> Result<fs::Metadata, RunStoreError> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.file_type().is_file() {
        return Err(RunStoreError::UnsafeStore(path.to_owned()));
    }
    Ok(metadata)
}

pub(crate) fn valid_run_id(value: &str) -> bool {
    !value.is_empty()
        && value != "."
        && value != ".."
        && !value
            .chars()
            .any(|character| matches!(character, '/' | '\\' | '\0') || character.is_control())
}

fn valid_hex_digest(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn valid_sha256(value: &str) -> bool {
    valid_hex_digest(value, 64)
}

fn validate_integrity(integrity: &RunIntegrity) -> Result<(), RunStoreError> {
    let fingerprint = &integrity.fingerprint;
    if fingerprint.algorithm != "sha256"
        || ![
            &fingerprint.source,
            &fingerprint.tests,
            &fingerprint.dependencies,
            &fingerprint.configuration,
            &fingerprint.instrumenter,
            &fingerprint.execution,
            &fingerprint.combined,
        ]
        .into_iter()
        .all(|digest| valid_sha256(digest))
    {
        return Err(RunStoreError::InvalidRun("integrity fingerprint"));
    }
    if let Some(git) = &integrity.git
        && git.revision.as_ref().is_some_and(|revision| {
            !valid_hex_digest(revision, 40) && !valid_hex_digest(revision, 64)
        })
    {
        return Err(RunStoreError::InvalidRun("git revision"));
    }
    Ok(())
}

fn read_metadata(path: &Path) -> Result<RunMetadata, RunStoreError> {
    let metadata = regular_file(path)?;
    if metadata.len() > MAX_RUN_METADATA_BYTES {
        return Err(RunStoreError::InvalidRun("run metadata is too large"));
    }
    let capacity =
        usize::try_from(metadata.len()).map_err(|_| RunStoreError::InvalidRun("metadata size"))?;
    let mut bytes = Vec::with_capacity(capacity);
    fs::open(path)?
        .take(MAX_RUN_METADATA_BYTES + 1)
        .read_to_end(&mut bytes)?;
    let metadata: RunMetadata = serde_json::from_slice(&bytes)
        .map_err(|_| RunStoreError::InvalidRun("run metadata JSON"))?;
    validate_integrity(&metadata.integrity)?;
    Ok(metadata)
}

fn load_run(directory: &Path, entry: &str) -> Result<StoredRun, RunStoreError> {
    if !valid_run_id(entry) {
        return Err(RunStoreError::InvalidRun("run directory name"));
    }
    let directory_metadata = fs::symlink_metadata(directory)?;
    if !directory_metadata.file_type().is_dir() {
        return Err(RunStoreError::UnsafeStore(directory.to_owned()));
    }
    let metadata_path = directory.join("run.json");
    let metadata = read_metadata(&metadata_path)?;
    if metadata.id != entry {
        return Err(RunStoreError::InvalidRun(
            "metadata ID differs from directory",
        ));
    }
    if metadata.raw_evidence.schema_version != EVIDENCE_ARCHIVE_SCHEMA_VERSION
        || metadata.raw_evidence.format != "framed+gzip"
        || metadata.raw_evidence.file != "evidence.raw.gz"
        || metadata.raw_evidence.files == 0
    {
        return Err(RunStoreError::InvalidRun("raw evidence metadata"));
    }
    let evidence_path = directory.join("evidence.raw.gz");
    let evidence_metadata = regular_file(&evidence_path)?;
    if evidence_metadata.len() != metadata.raw_evidence.compressed_bytes {
        return Err(RunStoreError::InvalidRun("raw evidence length"));
    }
    // An archive reads only under the current schema's magic, which the
    // metadata was required to name above: reading it is the whole check.
    read_archive_schema_version(&evidence_path)
        .map_err(|_| RunStoreError::InvalidRun("raw evidence archive"))?;
    Ok(StoredRun {
        id: entry.into(),
        directory: directory.to_owned(),
        evidence_path,
        metadata_path,
        query_index_path: directory.join(RUST_QUERY_INDEX_FILE),
        metadata,
    })
}

pub fn discover_runs(project_root: &Path) -> Result<RunInventory, RunStoreError> {
    let store = project_root.join(".supercov").join("runs");
    let store_metadata = match fs::symlink_metadata(&store) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(RunInventory {
                runs: Vec::new(),
                rejected: Vec::new(),
            });
        }
        Err(error) => return Err(error.into()),
    };
    if !store_metadata.file_type().is_dir() {
        return Err(RunStoreError::UnsafeStore(store));
    }
    let mut runs = Vec::new();
    let mut rejected = Vec::new();
    for entry in fs::read_dir(&store)? {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                rejected.push(RejectedRun {
                    entry: "<unreadable>".into(),
                    reason: error.to_string(),
                });
                continue;
            }
        };
        let name = entry.file_name().to_string_lossy().into_owned();
        match load_run(&entry.path(), &name) {
            Ok(run) => runs.push(run),
            Err(error) => rejected.push(RejectedRun {
                entry: name,
                reason: error.to_string(),
            }),
        }
    }
    runs.sort_by(|left, right| {
        right
            .metadata
            .started_at
            .cmp(&left.metadata.started_at)
            .then_with(|| right.id.cmp(&left.id))
    });
    rejected.sort_by(|left, right| left.entry.cmp(&right.entry));
    Ok(RunInventory { runs, rejected })
}

pub fn select_run<'a>(
    inventory: &'a RunInventory,
    selector: Option<&str>,
) -> Result<&'a StoredRun, RunStoreError> {
    if inventory.runs.is_empty() {
        return Err(RunStoreError::NoRuns);
    }
    if selector.is_none() || selector == Some("latest") {
        return Ok(&inventory.runs[0]);
    }
    let selector = selector.expect("checked selector");
    inventory
        .runs
        .iter()
        .find(|run| run.id == selector)
        .or_else(|| {
            inventory
                .runs
                .iter()
                .find(|run| run.id.starts_with(selector))
        })
        .ok_or_else(|| RunStoreError::RunNotFound(selector.into()))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntegrityComparison {
    pub stale: bool,
    pub reasons: Vec<String>,
}

/// Why a run whose source cannot be fingerprinted now -- its measured files
/// deleted or moved -- is no record of the checkout.
pub const SOURCE_UNMEASURABLE: &str = "its source cannot be measured as it is now";

/// A run against the checkout as it is now, `current` being its
/// fingerprint now if it can be taken. A run of source that cannot be
/// measured now is as stale as one of source that changed: a gate never
/// passes on it.
pub fn compare_with_checkout(
    stored: &RunIntegrity,
    current: Option<&RunIntegrity>,
) -> IntegrityComparison {
    match current {
        Some(current) => compare_run_integrity(Some(stored), current),
        None => IntegrityComparison {
            stale: true,
            reasons: vec![SOURCE_UNMEASURABLE.into()],
        },
    }
}

pub fn compare_run_integrity(
    stored: Option<&RunIntegrity>,
    current: &RunIntegrity,
) -> IntegrityComparison {
    let mut stderr = io::stderr();
    let debug = std::env::var_os("SUPERCOV_DEBUG_INTEGRITY").map(|_| &mut stderr as &mut dyn Write);
    compare_run_integrity_to(stored, current, debug)
}

/// [`compare_run_integrity`], writing both fingerprints to `debug` when
/// `SUPERCOV_DEBUG_INTEGRITY` asks for them.
fn compare_run_integrity_to(
    stored: Option<&RunIntegrity>,
    current: &RunIntegrity,
    debug: Option<&mut dyn Write>,
) -> IntegrityComparison {
    let Some(stored) = stored else {
        return IntegrityComparison {
            stale: true,
            reasons: vec!["run predates integrity fingerprints".into()],
        };
    };
    if let Some(debug) = debug {
        let _ = writeln!(debug, "[integrity] stored : {stored:?}");
        let _ = writeln!(debug, "[integrity] current: {current:?}");
    }
    let mut reasons = Vec::new();
    if stored.schema_version != current.schema_version {
        reasons.push("coverage schema changed".into());
    }
    // The declared contract, not the build that produced it. The instrumenter
    // digest covers Supercov's own source, so comparing it here marked every
    // stored run stale on every release -- for a checkout that had not changed
    // and evidence that is still a true record of what ran. A change in what
    // instrumentation *means* is a deliberate act, and it moves this version.
    if stored.instrumenter_version != current.instrumenter_version {
        reasons.push("instrumenter contract changed".into());
    }
    if stored.fingerprint.source != current.fingerprint.source {
        reasons.push("instrumented source changed".into());
    }
    if stored.fingerprint.tests != current.fingerprint.tests {
        reasons.push("test files changed".into());
    }
    if stored.fingerprint.dependencies != current.fingerprint.dependencies {
        reasons.push("dependencies or lockfile changed".into());
    }
    if stored.fingerprint.configuration != current.fingerprint.configuration {
        reasons.push("test/build configuration changed".into());
    }
    if reasons.is_empty() && stored.fingerprint.execution != current.fingerprint.execution {
        reasons.push("execution environment changed".into());
    }
    IntegrityComparison {
        stale: !reasons.is_empty(),
        reasons,
    }
}

pub(crate) fn file_sha256(path: &Path) -> Result<([u8; 32], u64), RunStoreError> {
    let metadata = regular_file(path)?;
    let mut file = fs::open(path)?;
    let mut hash = Sha256::new();
    let mut buffer = [0_u8; 128 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hash.update(&buffer[..read]);
    }
    Ok((hash.finalize().into(), metadata.len()))
}

/// A version of the code that writes a run's query index: the versions it
/// declares and the source it is built from (`build.rs` names the files).
/// Keyed to the analysis alone, not the whole engine, an upgrade that left it
/// as it was keeps every stored run's index; one that changed it re-indexes,
/// whether or not anyone bumped a version.
fn domain_hash(domain: &str, version: u32) -> [u8; 32] {
    let mut hash = Sha256::new();
    hash.update(domain.as_bytes());
    hash.update([0]);
    hash.update(version.to_le_bytes());
    hash.update([0]);
    hash.update(supercov_contracts::COVERAGE_MODEL_SCHEMA_VERSION.to_le_bytes());
    hash.update([0]);
    hash.update(env!("SUPERCOV_ANALYSIS_SOURCE_SHA256").as_bytes());
    hash.finalize().into()
}

pub fn query_index_identity(run: &StoredRun) -> Result<QueryIndexIdentity, RunStoreError> {
    Ok(identity_with(
        run,
        file_sha256(&run.evidence_path)?,
        analysis_digest(),
    ))
}

fn analysis_digest() -> [u8; 32] {
    domain_hash("supercov-analysis", RUST_ANALYSIS_ABI_VERSION)
}

/// The identity of an index of `run` built from evidence with this digest
/// and length, by the analysis `analysis` names.
fn identity_with(
    run: &StoredRun,
    (evidence_sha256, evidence_bytes): ([u8; 32], u64),
    analysis: [u8; 32],
) -> QueryIndexIdentity {
    QueryIndexIdentity {
        evidence_sha256,
        evidence_bytes,
        analysis_sha256: analysis,
        producer_sha256: domain_hash(
            "supercov-query-producer",
            RUST_QUERY_PRODUCER_ABI_VERSION ^ QUERY_INDEX_SCHEMA_VERSION,
        ),
        archive_schema_version: run.metadata.raw_evidence.schema_version,
    }
}

/// A file as the filesystem last changed it: its device and inode, length,
/// and modification and status-change times to the nanosecond. Any write
/// moves the status-change time, which no tool can set back, so a file with
/// the same stamp holds the same bytes. `None` where there is no such time.
#[cfg(unix)]
fn change_stamp(path: &Path) -> Option<String> {
    use std::os::unix::fs::MetadataExt;
    fs::symlink_metadata(path).ok().and_then(|metadata| {
        stamp(
            [metadata.dev(), metadata.ino(), metadata.size()],
            (metadata.mtime(), metadata.mtime_nsec()),
            (metadata.ctime(), metadata.ctime_nsec()),
        )
    })
}

/// The stamp of those fields. A filesystem that keeps whole seconds (FAT,
/// some network mounts) reports no nanoseconds, and a write within the second
/// would leave the stamp as it was: its files get none, and are checked in
/// full each time.
#[cfg(unix)]
fn stamp(file: [u64; 3], modified: (i64, i64), changed: (i64, i64)) -> Option<String> {
    (changed.1 != 0).then(|| {
        format!(
            "{} {} {} {}.{} {}.{}",
            file[0], file[1], file[2], modified.0, modified.1, changed.0, changed.1
        )
    })
}

#[cfg(not(unix))]
fn change_stamp(_: &Path) -> Option<String> {
    None
}

/// Beside an index, the change stamps of the evidence and of the index when
/// the index was last known to be built from that evidence: verified in
/// full, or written from it.
fn verified_record(index: &Path) -> PathBuf {
    let mut name = index.as_os_str().to_owned();
    name.push(".verified");
    PathBuf::from(name)
}

/// Record that `index` holds what the evidence stamped `evidence` gives. The
/// evidence's stamp is taken before it was read, so a change while it was
/// read is one the next open sees. A record that cannot be written costs
/// the next open its full check, nothing more.
fn record_verified(index: &Path, evidence: Option<&str>) {
    let record = evidence
        .zip(change_stamp(index))
        .map(|(evidence, index)| format!("{evidence}\n{index}\n"));
    let path = verified_record(index);
    let temporary = path.with_extension(format!("tmp{}", std::process::id()));
    let written = record.is_some_and(|record| {
        fs::write(&temporary, record).is_ok() && fs::rename(&temporary, &path).is_ok()
    });
    if !written {
        let _ = fs::remove_file(&temporary);
        let _ = fs::remove_file(&path);
    }
}

/// The index at `path`, opened without hashing the evidence or every page of
/// the index, when neither has changed since the index was last verified: its
/// identity is the one its header records, and each page is still checked
/// against its digest the first time a query reads it. On a run with 150 MB
/// of evidence and a 498 MB index the two hashes were 0.25 s of every 0.30 s
/// query.
fn open_unchanged_query_index(
    run: &StoredRun,
    path: &Path,
    analysis: [u8; 32],
    evidence: Option<&str>,
) -> Option<QueryIndex> {
    let recorded = fs::read_to_string(verified_record(path)).ok();
    let current = evidence
        .zip(change_stamp(path))
        .map(|(evidence, index)| format!("{evidence}\n{index}\n"));
    recorded
        .filter(|recorded| current.as_ref() == Some(recorded))
        .and_then(|_| crate::query_index::recorded_evidence(path).ok())
        .and_then(|evidence| {
            open_written_query_index(path, &identity_with(run, evidence, analysis)).ok()
        })
}

/// Open the index at `path`, or build it from the evidence. One unchanged
/// since it was verified opens at once; any other is verified in full, and
/// one that fails is rebuilt. A rebuilt index is named by the digest of the
/// very bytes it was built from, taken as they were read: an archive changed
/// meanwhile can never lend its name to an index of other bytes, and the
/// next open, finding the archive's name changed, builds again.
fn open_or_build_query_index(
    run: &StoredRun,
    path: &Path,
    analysis: [u8; 32],
    filter: Option<&crate::coverage_report::TestFilter>,
) -> Result<QueryIndex, RunIndexError> {
    let evidence = change_stamp(&run.evidence_path);
    // A filtered index is marked as used each time, for pruning to keep.
    let used = filter.is_some();
    if let Some(index) = open_unchanged_query_index(run, path, analysis, evidence.as_deref()) {
        if used {
            touch(path);
            record_verified(path, evidence.as_deref());
        }
        return Ok(index);
    }
    let identity = identity_with(run, file_sha256(&run.evidence_path)?, analysis);
    let index = match open_validated_query_index(path, &identity) {
        Ok(index) => index,
        Err(_) => {
            let (sections, read) = index_sections(run, filter)?;
            let identity = identity_with(run, (read.sha256, read.bytes), analysis);
            write_query_index(&sections, &identity, path)?;
            std::thread::spawn(move || drop(sections));
            if used {
                prune_filtered_indexes(path);
            }
            open_written_query_index(path, &identity)?
        }
    };
    if used {
        touch(path);
    }
    record_verified(path, evidence.as_deref());
    Ok(index)
}

#[derive(Debug)]
pub enum RunIndexError {
    RunStore(RunStoreError),
    QueryIndex(QueryIndexError),
    CoverageIndex(CoverageIndexError),
    Report(ReportError),
    Metadata(serde_json::Error),
}

impl std::fmt::Display for RunIndexError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::RunStore(error) => write!(formatter, "{error}"),
            Self::QueryIndex(error) => write!(formatter, "{error}"),
            Self::CoverageIndex(error) => write!(formatter, "{error}"),
            Self::Report(ReportError::NoEvidence(_)) => {
                write!(formatter, "no coverage evidence was published")
            }
            Self::Report(ReportError::NoTestMatches(filter)) => {
                write!(formatter, "no test of the run matches {filter}")
            }
            Self::Report(error) => write!(formatter, "coverage analysis failed: {error:?}"),
            Self::Metadata(error) => write!(formatter, "run integrity is invalid: {error}"),
        }
    }
}

impl std::error::Error for RunIndexError {}

impl From<RunStoreError> for RunIndexError {
    fn from(value: RunStoreError) -> Self {
        Self::RunStore(value)
    }
}

impl From<QueryIndexError> for RunIndexError {
    fn from(value: QueryIndexError) -> Self {
        Self::QueryIndex(value)
    }
}

impl From<CoverageIndexError> for RunIndexError {
    fn from(value: CoverageIndexError) -> Self {
        Self::CoverageIndex(value)
    }
}

impl From<ReportError> for RunIndexError {
    fn from(value: ReportError) -> Self {
        Self::Report(value)
    }
}

impl From<serde_json::Error> for RunIndexError {
    fn from(value: serde_json::Error) -> Self {
        Self::Metadata(value)
    }
}

fn open_validated_query_index(
    path: &Path,
    identity: &QueryIndexIdentity,
) -> Result<QueryIndex, RunIndexError> {
    let index = QueryIndex::open(path, identity)?;
    index.verify_all()?;
    CoverageIndex::new(&index)?;
    Ok(index)
}

/// Open the index this process has just written. It was synced and renamed
/// into place from the very bytes its page digests were computed over, so
/// hashing the whole file again re-derives what the write already guarantees
/// -- and on a 300-file run that was 726 MB, 29% of the query that follows a
/// test run. Nothing unverified is trusted: every page is still checked against
/// its digest the first time a query reads it. An index found on disk later is
/// verified in full on open, as before, since it may have been torn or changed
/// since it was written.
fn open_written_query_index(
    path: &Path,
    identity: &QueryIndexIdentity,
) -> Result<QueryIndex, RunIndexError> {
    let index = QueryIndex::open(path, identity)?;
    CoverageIndex::new(&index)?;
    Ok(index)
}

/// Open an existing valid index without triggering analysis or publication.
pub fn open_existing_query_index(run: &StoredRun) -> Result<Option<QueryIndex>, RunIndexError> {
    match fs::symlink_metadata(&run.query_index_path) {
        Ok(_) => {
            open_validated_query_index(&run.query_index_path, &query_index_identity(run)?).map(Some)
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(RunStoreError::Io(error).into()),
    }
}

/// A run's coverage, analysed from its evidence archive as its query index is.
pub fn analyze_stored_run(run: &StoredRun) -> Result<CoverageReport, RunIndexError> {
    analyze_stored_run_filtered(run, None)
}

/// [`analyze_stored_run`] for only the tests `filter` picks: their coverage
/// alone, read from what the run already recorded.
pub fn analyze_stored_run_filtered(
    run: &StoredRun,
    filter: Option<&crate::coverage_report::TestFilter>,
) -> Result<CoverageReport, RunIndexError> {
    Ok(analyze_coverage_archive(&ArchiveReportRequest {
        archive_path: run.evidence_path.clone(),
        run_id: run.id.clone(),
        generated_at: run.metadata.started_at.clone(),
        integrity: Some(serde_json::to_value(&run.metadata.integrity)?),
        test_exit_code: ExitCodeInput::Present(run.metadata.test_exit_code),
        test_filter: filter.cloned(),
    })?)
}

/// A stored run's query index, its views built and written one at a time,
/// with the digest of the evidence bytes they were built from.
fn index_sections(
    run: &StoredRun,
    filter: Option<&crate::coverage_report::TestFilter>,
) -> Result<
    (
        Vec<crate::query_index::QueryIndexSection>,
        crate::evidence_archive::ArchiveDigest,
    ),
    RunIndexError,
> {
    use crate::coverage_index::CoverageViewId;
    let (_, normalized, transport, read) =
        crate::coverage_report::archive_request(&ArchiveReportRequest {
            archive_path: run.evidence_path.clone(),
            run_id: run.id.clone(),
            generated_at: run.metadata.started_at.clone(),
            integrity: Some(serde_json::to_value(&run.metadata.integrity)?),
            test_exit_code: ExitCodeInput::Present(run.metadata.test_exit_code),
            test_filter: filter.cloned(),
        })?;
    let sections = crate::coverage_index::coverage_index_sections_from(
        crate::coverage_report::coverage_views(
            &normalized,
            &[
                CoverageViewId::All,
                CoverageViewId::Passed,
                CoverageViewId::Failed,
            ],
            Some(transport),
        )?
        .map(|view| view.map_err(RunIndexError::Report)),
    )?;
    // Every test's records, freed by nobody's wait.
    std::thread::spawn(move || drop(normalized));
    Ok((sections, read))
}

/// The filter as one key: its values normalized and sorted, so the same
/// tests asked for in another order or case find the same index.
fn filter_digest(filter: &crate::coverage_report::TestFilter) -> [u8; 32] {
    let mut parts = [
        filter
            .tags
            .iter()
            .map(|tag| tag.trim_start_matches('@').to_lowercase())
            .collect::<Vec<_>>(),
        filter
            .titles
            .iter()
            .map(|title| title.to_lowercase())
            .collect(),
        filter
            .files
            .iter()
            .map(|file| {
                file.trim_start_matches("./")
                    .trim_end_matches('/')
                    .to_owned()
            })
            .collect(),
    ];
    parts[0].extend(filter.kind.iter().map(|kind| format!("kind\0{kind}")));
    parts[0].extend(
        filter
            .runner
            .iter()
            .map(|runner| format!("runner\0{runner}")),
    );
    let mut hash = Sha256::new();
    hash.update(domain_hash(
        "supercov-test-filter",
        QUERY_INDEX_SCHEMA_VERSION,
    ));
    for part in &mut parts {
        part.sort();
        part.dedup();
        for value in part.iter() {
            hash.update(value.as_bytes());
            hash.update([0]);
        }
        hash.update([1]);
    }
    hash.finalize().into()
}

/// The query index of only the tests `filter` picks, built from the run's
/// evidence the first time it is asked for and kept beside the run's own
/// index. Nothing of it is worked out while the tests run or the run is
/// stored: only a query that names a filter pays for it, once.
pub fn open_or_build_filtered_query_index(
    run: &StoredRun,
    filter: &crate::coverage_report::TestFilter,
) -> Result<QueryIndex, RunIndexError> {
    let digest = filter_digest(filter);
    let name = digest
        .iter()
        .take(8)
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let path = run
        .query_index_path
        .with_file_name(format!("filtered-{name}.index"));
    open_or_build_query_index(run, &path, digest, Some(filter))
}

/// How many filtered indexes a run keeps. Each is about half the run's own
/// index, so a run asked about many filters would otherwise grow without end.
const KEPT_FILTERED_INDEXES: usize = 8;

/// Mark a filtered index as just used, so pruning keeps the ones still asked.
fn touch(path: &Path) {
    if let Ok(file) = fs::File::options().append(true).open(path) {
        let _ = file.set_modified(std::time::SystemTime::now());
    }
}

/// Remove all but the most recently used filtered indexes beside `kept`.
fn prune_filtered_indexes(kept: &Path) {
    let Some(directory) = kept.parent() else {
        return;
    };
    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };
    let mut filtered = entries
        .filter_map(Result::ok)
        .filter(|entry| {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            name.starts_with("filtered-") && name.ends_with(".index")
        })
        .filter_map(|entry| Some((entry.metadata().ok()?.modified().ok()?, entry.path())))
        .collect::<Vec<_>>();
    filtered.sort_by_key(|(modified, _)| std::cmp::Reverse(*modified));
    for (_, path) in filtered.into_iter().skip(KEPT_FILTERED_INDEXES) {
        if path != kept {
            let _ = fs::remove_file(verified_record(&path));
            let _ = fs::remove_file(path);
        }
    }
}

/// Write a run's query index from coverage already analysed from its evidence,
/// so the first query opens it instead of analysing the archive again. The
/// identity is the evidence's digest, not its path, so an index written beside
/// staged evidence stays valid once the directory is renamed into place.
/// The statements that run over more than one line, with whether they ran:
/// what the HTML report marks on a statement's continuation lines. Reading
/// them back from the analysis meant analysing the evidence again, about
/// 12 s per run on supergateway, so publication writes them beside the index.
pub fn statement_spans(report: &CoverageReport) -> Vec<serde_json::Value> {
    report
        .view
        .points
        .iter()
        .filter(|p| p.meta.kind == crate::coverage_analysis::PointKind::Statement)
        .filter_map(|p| {
            let extra = p.meta.source.matches('\n').count();
            (extra > 0).then(|| {
                serde_json::json!({
                    "file": p.meta.file, "line": p.meta.line,
                    "end": p.meta.line + extra, "covered": p.covered,
                })
            })
        })
        .collect()
}

/// The spans publication wrote, when it did.
pub fn read_statement_spans(run: &StoredRun) -> Option<Vec<serde_json::Value>> {
    let bytes = fs::read(run.directory.join(STATEMENT_SPANS_FILE)).ok()?;
    serde_json::from_slice(&bytes).ok()
}

pub(crate) fn write_statement_spans(run: &StoredRun, report: &CoverageReport) -> io::Result<()> {
    let bytes = serde_json::to_vec(&statement_spans(report)).map_err(io::Error::other)?;
    fs::write(run.directory.join(STATEMENT_SPANS_FILE), bytes)
}

/// The assessment input publication wrote, when it did.
pub fn read_assessment_input(
    run: &StoredRun,
) -> Option<crate::assertion_coverage::AssessmentInput> {
    let file = fs::open(run.directory.join(ASSESSMENT_INPUT_FILE)).ok()?;
    serde_json::from_reader(flate2::read::GzDecoder::new(io::BufReader::new(file))).ok()
}

pub(crate) fn write_assessment_input(run: &StoredRun, report: &CoverageReport) -> io::Result<()> {
    let bytes = serde_json::to_vec(&crate::assertion_coverage::assessment_input(report))
        .map_err(io::Error::other)?;
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    encoder.write_all(&bytes)?;
    fs::write(run.directory.join(ASSESSMENT_INPUT_FILE), encoder.finish()?)
}

pub(crate) fn write_query_index_from(
    run: &StoredRun,
    report: &CoverageReport,
) -> Result<(), RunIndexError> {
    let evidence = change_stamp(&run.evidence_path);
    let identity = query_index_identity(run)?;
    let sections = coverage_index_sections(report)?;
    write_query_index(&sections, &identity, &run.query_index_path)?;
    std::thread::spawn(move || drop(sections));
    record_verified(&run.query_index_path, evidence.as_deref());
    Ok(())
}

/// Open a valid disposable index or atomically reconstruct it from evidence.
///
/// `evidence.raw.gz` remains authoritative. Any stale, truncated, linked or
/// otherwise invalid index is ignored and replaced by a fully authenticated
/// new inode (`open_or_build_query_index`).
pub fn open_or_rebuild_query_index(run: &StoredRun) -> Result<QueryIndex, RunIndexError> {
    open_or_build_query_index(run, &run.query_index_path, analysis_digest(), None)
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        time::{SystemTime, UNIX_EPOCH},
    };

    use crate::evidence_archive::{EvidenceArchiveEntry, read_archive, write_archive};

    use super::*;

    /// A stored run is refused for metadata too large to be one, a
    /// directory name no run has, and evidence metadata that does not
    /// describe the archive beside it; a store that is not a directory is
    /// refused whole.
    // Written to Unix paths and filesystem behaviour.
    #[cfg_attr(windows, ignore = "Unix paths and filesystem behaviour")]
    #[test]
    fn a_stored_run_is_refused_for_what_does_not_describe_it() {
        let root = temporary_directory("refusals");
        let edit = |id: &str, change: &dyn Fn(&mut serde_json::Value)| {
            let path = root.join(".supercov/runs").join(id).join("run.json");
            let mut value: serde_json::Value =
                serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
            change(&mut value);
            fs::write(&path, value.to_string()).unwrap();
        };
        for (id, change) in [
            (
                "run-format",
                &(|v: &mut serde_json::Value| v["rawEvidence"]["format"] = "zip".into())
                    as &dyn Fn(&mut serde_json::Value),
            ),
            ("run-file", &|v: &mut serde_json::Value| {
                v["rawEvidence"]["file"] = "other.gz".into()
            }),
            ("run-files", &|v: &mut serde_json::Value| {
                v["rawEvidence"]["files"] = 0.into()
            }),
        ] {
            create_analyzable_test_run(&root, id);
            edit(id, change);
        }
        create_analyzable_test_run(&root, "run-large");
        fs::write(
            root.join(".supercov/runs/run-large/run.json"),
            vec![b' '; (MAX_RUN_METADATA_BYTES + 1) as usize],
        )
        .unwrap();
        fs::create_dir_all(root.join(".supercov/runs/bad\u{1}id")).unwrap();
        let inventory = discover_runs(&root).unwrap();
        assert!(inventory.runs.is_empty(), "{:?}", inventory.runs);
        let reasons = inventory
            .rejected
            .iter()
            .map(|run| (run.entry.clone(), run.reason.clone()))
            .collect::<std::collections::BTreeMap<_, _>>();
        for (id, reason) in [
            ("run-format", "raw evidence metadata"),
            ("run-file", "raw evidence metadata"),
            ("run-files", "raw evidence metadata"),
            ("run-large", "run metadata is too large"),
            ("bad\u{1}id", "run directory name"),
        ] {
            assert!(reasons[id].contains(reason), "{id}: {}", reasons[id]);
        }
        let file_store = temporary_directory("file-store");
        fs::create_dir_all(file_store.join(".supercov")).unwrap();
        fs::write(file_store.join(".supercov/runs"), "").unwrap();
        assert!(matches!(
            discover_runs(&file_store),
            Err(RunStoreError::UnsafeStore(_))
        ));
        fs::remove_dir_all(root).unwrap();
        fs::remove_dir_all(file_store).unwrap();
    }

    /// The execution environment is named only when nothing else changed.
    #[test]
    fn an_execution_change_alone_is_named_and_otherwise_left_unsaid() {
        let root = temporary_directory("execution");
        create_analyzable_test_run(&root, "run-a");
        let stored = discover_runs(&root)
            .unwrap()
            .runs
            .remove(0)
            .metadata
            .integrity;
        let mut current = stored.clone();
        current.fingerprint.execution = "9".repeat(64);
        assert_eq!(
            compare_run_integrity(Some(&stored), &current).reasons,
            ["execution environment changed"]
        );
        current.fingerprint.tests = "8".repeat(64);
        assert_eq!(
            compare_run_integrity(Some(&stored), &current).reasons,
            ["test files changed"]
        );
        assert_eq!(
            compare_run_integrity(None, &current).reasons,
            ["run predates integrity fingerprints"]
        );
        fs::remove_dir_all(root).unwrap();
    }

    /// A store under a file cannot be looked at, which is an error, not an
    /// empty store; nor can an index under a file, while one that is not
    /// there is none.
    #[cfg(unix)]
    #[test]
    fn a_store_or_index_below_a_file_is_an_error_not_an_absence() {
        let root = temporary_directory("below-a-file");
        fs::write(root.join(".supercov"), b"not a directory").unwrap();
        assert!(matches!(discover_runs(&root), Err(RunStoreError::Io(_))));
        fs::remove_file(root.join(".supercov")).unwrap();
        create_analyzable_test_run(&root, "run-a");
        let mut run = discover_runs(&root).unwrap().runs.remove(0);
        assert!(open_existing_query_index(&run).unwrap().is_none());
        run.query_index_path = run.metadata_path.join("index");
        assert!(open_existing_query_index(&run).is_err());
        fs::remove_dir_all(root).unwrap();
    }

    /// An index whose evidence and file are as they were when it was last
    /// verified opens without reading the evidence at all; any change to
    /// either sends the open back to the full check, which rebuilds what no
    /// longer matches.
    #[cfg(unix)]
    #[test]
    fn an_index_unchanged_since_it_was_verified_opens_without_hashing() {
        use std::os::unix::fs::PermissionsExt;
        let root = temporary_directory("unchanged-index");
        let run = create_indexable_python_run(&root);
        drop(open_or_rebuild_query_index(&run).unwrap());
        let record = verified_record(&run.query_index_path);
        assert!(record.is_file(), "a rebuilt index is recorded as verified");

        // The evidence cannot be read, and is recorded as it is now: only an
        // open that never reads it succeeds.
        fs::set_permissions(&run.evidence_path, fs::Permissions::from_mode(0o000)).unwrap();
        let unreadable = fs::File::open(&run.evidence_path).is_err();
        record_verified(
            &run.query_index_path,
            change_stamp(&run.evidence_path).as_deref(),
        );
        assert!(open_or_rebuild_query_index(&run).is_ok());
        // Without the record, the open hashes the evidence, and cannot.
        fs::remove_file(&record).unwrap();
        if unreadable {
            assert!(open_or_rebuild_query_index(&run).is_err());
        }
        fs::set_permissions(&run.evidence_path, fs::Permissions::from_mode(0o644)).unwrap();

        // A filtered index opened again is marked as used, and stays recorded.
        let filter = crate::coverage_report::TestFilter {
            kind: Some("unit".into()),
            ..Default::default()
        };
        let first = open_or_build_filtered_query_index(&run, &filter).unwrap();
        drop(first);
        let filtered = run
            .query_index_path
            .with_file_name(format!("filtered-{}.index", {
                filter_digest(&filter)
                    .iter()
                    .take(8)
                    .map(|byte| format!("{byte:02x}"))
                    .collect::<String>()
            }));
        let before = fs::metadata(&filtered).unwrap().modified().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        drop(open_or_build_filtered_query_index(&run, &filter).unwrap());
        assert!(fs::metadata(&filtered).unwrap().modified().unwrap() > before);
        assert_eq!(
            fs::read_to_string(verified_record(&filtered)).unwrap(),
            format!(
                "{}\n{}\n",
                change_stamp(&run.evidence_path).unwrap(),
                change_stamp(&filtered).unwrap()
            )
        );

        // An index changed since: checked in full, rebuilt, recorded anew.
        let mut bytes = fs::read(&run.query_index_path).unwrap();
        let last = bytes.len() - 1;
        bytes[last] ^= 0xff;
        fs::write(&run.query_index_path, bytes).unwrap();
        let index = open_or_rebuild_query_index(&run).unwrap();
        assert!(index.verify_all().is_ok());
        assert_eq!(
            fs::read_to_string(&record).unwrap(),
            format!(
                "{}\n{}\n",
                change_stamp(&run.evidence_path).unwrap(),
                change_stamp(&run.query_index_path).unwrap()
            )
        );
    }

    /// A record that cannot be written leaves none behind: the next open
    /// checks in full. So does a file with no stamp to record.
    #[test]
    fn an_index_that_cannot_be_recorded_is_checked_in_full_next_time() {
        let root = temporary_directory("unrecorded-index");
        let run = create_indexable_python_run(&root);
        drop(open_or_rebuild_query_index(&run).unwrap());
        let record = verified_record(&run.query_index_path);
        let evidence = change_stamp(&run.evidence_path);
        // Nothing to stamp the evidence with: no record.
        record_verified(&run.query_index_path, None);
        assert!(!record.exists());
        // Its place taken by a directory: written beside, never renamed in.
        fs::create_dir(&record).unwrap();
        fs::write(record.join("keep"), b"").unwrap();
        record_verified(&run.query_index_path, evidence.as_deref());
        assert!(record.is_dir());
        fs::remove_dir_all(&record).unwrap();
        // Nowhere to write it: the temporary's place taken by a directory.
        let temporary = record.with_extension(format!("tmp{}", std::process::id()));
        fs::create_dir(&temporary).unwrap();
        record_verified(&run.query_index_path, evidence.as_deref());
        assert!(!record.exists());
        fs::remove_dir(&temporary).unwrap();
        assert!(open_or_rebuild_query_index(&run).is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn a_file_kept_to_the_second_has_no_stamp() {
        assert_eq!(stamp([1, 2, 3], (4, 5), (6, 0)), None);
        assert_eq!(
            stamp([1, 2, 3], (4, 5), (6, 7)).as_deref(),
            Some("1 2 3 4.5 6.7")
        );
    }

    /// Pruning keeps the most recently used filtered indexes, never the one
    /// just written however old it looks, and nothing that is not one.
    #[test]
    fn pruning_keeps_the_newest_filtered_indexes_and_the_one_just_written() {
        let root = temporary_directory("prune");
        let at = |seconds: u64| UNIX_EPOCH + std::time::Duration::from_secs(1_000_000 + seconds);
        let write = |name: &str, seconds: u64| {
            let path = root.join(name);
            fs::write(&path, b"index").unwrap();
            fs::File::options()
                .append(true)
                .open(&path)
                .unwrap()
                .set_modified(at(seconds))
                .unwrap();
            path
        };
        let kept = write("filtered-kept.index", 0);
        for n in 1..=KEPT_FILTERED_INDEXES as u64 + 2 {
            write(&format!("filtered-{n:02}.index"), n);
        }
        write("filtered-notes.txt", 0);
        prune_filtered_indexes(&kept);
        let mut left = fs::read_dir(&root)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        left.sort();
        let mut expected = (3..=KEPT_FILTERED_INDEXES as u64 + 2)
            .map(|n| format!("filtered-{n:02}.index"))
            .collect::<Vec<_>>();
        expected.extend(["filtered-kept.index".into(), "filtered-notes.txt".into()]);
        expected.sort();
        assert_eq!(left, expected);
        fs::remove_dir_all(root).unwrap();
    }

    fn temporary_directory(label: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "supercov-run-store-{label}-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir_all(&path).unwrap();
        path
    }

    fn digest(character: char) -> String {
        std::iter::repeat_n(character, 64).collect()
    }

    fn integrity() -> RunIntegrity {
        RunIntegrity {
            schema_version: 2,
            instrumenter_version: "2.0.0".into(),
            git: Some(GitIntegrity {
                revision: Some(std::iter::repeat_n('a', 40).collect()),
                branch: None,
                dirty: false,
            }),
            fingerprint: RunFingerprint {
                algorithm: "sha256".into(),
                source: digest('a'),
                tests: digest('b'),
                dependencies: digest('c'),
                configuration: digest('d'),
                instrumenter: digest('e'),
                execution: digest('f'),
                combined: digest('0'),
                source_files: 1,
                test_files: 1,
            },
            stale: None,
            stale_reasons: None,
        }
    }

    fn create_run(root: &Path, id: &str) -> PathBuf {
        let directory = root.join(".supercov/runs").join(id);
        fs::create_dir_all(&directory).unwrap();
        let archive = write_archive(
            vec![
                EvidenceArchiveEntry {
                    path: "coverage-model.json".into(),
                    contents: br#"{"schemaVersion":1,"language":"fixture","variant":"fixture-v1","name":"Fixture model","completenessMeaning":"Fixture archive identity only.","measured":["fixture"],"notMeasured":[]}"#.to_vec(),
                },
                EvidenceArchiveEntry {
                    path: "frontend.json".into(),
                    contents: br#"{"protocolVersion":2,"frontendId":"fixture","frontendVersion":"fixture-v1","language":"fixture","structuralSource":"owned-probes","runners":[{"runner":"fixture","executionModel":"serial-in-process","attribution":{"run":"exact","worker":"unavailable","test":"unavailable","retry":"unavailable","phase":"unavailable","action":"unavailable","assertion":"unavailable"},"limitations":[{"id":"fixture-identities-unavailable","scopes":["worker","test","retry","phase","action","assertion"],"reason":"This store-only fixture has no execution evidence"}]}],"structuralLimitations":[]}"#.to_vec(),
                },
                EvidenceArchiveEntry {
                    path: "manifest.json".into(),
                    contents: b"{}".to_vec(),
                },
            ],
            &directory.join("evidence.raw.gz"),
        )
        .unwrap();
        let metadata = RunMetadata {
            id: id.into(),
            started_at: id.into(),
            duration_ms: 1.0,
            command: vec!["npm".into(), "test".into()],
            test_exit_code: Some(0),
            integrity: integrity(),
            raw_evidence: RawEvidenceMetadata {
                schema_version: archive.schema_version,
                format: archive.format.into(),
                file: archive.file.into(),
                files: archive.files,
                uncompressed_bytes: archive.uncompressed_bytes,
                compressed_bytes: archive.compressed_bytes,
            },
            isolated_build: Some(true),
            instrumented_build_cache: None,
            timings: None,
            merged: None,
            parents: None,
            source_roots: None,
            copy: None,
            merged_across: None,
            recorded_by: None,
            companions: None,
        };
        fs::write(
            directory.join("run.json"),
            serde_json::to_vec_pretty(&metadata).unwrap(),
        )
        .unwrap();
        directory
    }

    fn create_indexable_run(root: &Path) -> StoredRun {
        create_analyzable_test_run(root, "test-run");
        discover_runs(root).unwrap().runs.remove(0)
    }

    fn create_indexable_python_run(root: &Path) -> StoredRun {
        let directory = create_analyzable_test_run(root, "python-run");
        let evidence_path = directory.join("evidence.raw.gz");
        let mut entries = read_archive(&evidence_path).unwrap();
        for entry in &mut entries {
            if entry.path.ends_with("/mcdc.json") {
                let mut result: serde_json::Value =
                    serde_json::from_slice(&entry.contents).unwrap();
                result["provenance"]["runner"] = "pytest".into();
                result["provenance"]["kind"] = "unit".into();
                result["testFile"] = "tests/test_app.py".into();
                entry.contents = serde_json::to_vec(&result).unwrap();
            }
        }
        entries
            .iter_mut()
            .find(|entry| entry.path == "frontend.json")
            .unwrap()
            .contents = serde_json::to_vec(&serde_json::json!({
            "protocolVersion": 2,
            "frontendId": "python",
            "frontendVersion": "python-owned-v1",
            "language": "python",
            "structuralSource": "owned-probes",
            "runners": [{
                "runner": "pytest",
                "executionModel": "serial-in-process",
                "attribution": {
                    "run": "exact",
                    "worker": "unavailable",
                    "test": "exact",
                    "retry": "exact",
                    "phase": "exact",
                    "action": "exact",
                    "assertion": "exact"
                },
                "limitations": [{
                    "id": "test-fixture-worker-unavailable",
                    "scopes": ["worker"],
                    "reason": "The persisted-run fixture intentionally has no worker identity"
                }]
            }],
            "structuralLimitations": []
        }))
        .unwrap();
        entries
            .iter_mut()
            .find(|entry| entry.path == "coverage-model.json")
            .unwrap()
            .contents = serde_json::to_vec(&serde_json::json!({
            "schemaVersion": 1,
            "language": "python",
            "variant": "all",
            "name": "python-owned-control-flow",
            "completenessMeaning": "Every declared owned-probe obligation was observed.",
            "measured": ["owned statements", "owned decisions"],
            "notMeasured": []
        }))
        .unwrap();
        let archive = write_archive(entries, &evidence_path).unwrap();
        let metadata_path = directory.join("run.json");
        let mut metadata: RunMetadata =
            serde_json::from_slice(&fs::read(&metadata_path).unwrap()).unwrap();
        metadata.raw_evidence = RawEvidenceMetadata {
            schema_version: archive.schema_version,
            format: archive.format.into(),
            file: archive.file.into(),
            files: archive.files,
            uncompressed_bytes: archive.uncompressed_bytes,
            compressed_bytes: archive.compressed_bytes,
        };
        fs::write(metadata_path, serde_json::to_vec_pretty(&metadata).unwrap()).unwrap();
        discover_runs(root).unwrap().runs.remove(0)
    }

    #[test]
    fn discovers_valid_runs_in_reverse_order_and_selects_exact_prefix_or_latest() {
        let root = temporary_directory("discovery");
        create_run(&root, "2026-08-24T00-00-00-000Z");
        create_run(&root, "2026-08-25T00-00-00-000Z");
        create_run(&root, "zz-custom-old-run");
        let custom_metadata = root.join(".supercov/runs/zz-custom-old-run/run.json");
        let mut custom: serde_json::Value =
            serde_json::from_slice(&fs::read(&custom_metadata).unwrap()).unwrap();
        custom["startedAt"] = "2026-08-23T00:00:00.000Z".into();
        fs::write(
            &custom_metadata,
            serde_json::to_vec_pretty(&custom).unwrap(),
        )
        .unwrap();
        let inventory = discover_runs(&root).unwrap();
        assert!(inventory.rejected.is_empty());
        assert_eq!(
            inventory
                .runs
                .iter()
                .map(|run| run.id.as_str())
                .collect::<Vec<_>>(),
            [
                "2026-08-25T00-00-00-000Z",
                "2026-08-24T00-00-00-000Z",
                "zz-custom-old-run"
            ]
        );
        assert_eq!(
            select_run(&inventory, None).unwrap().id,
            inventory.runs[0].id
        );
        assert_eq!(
            select_run(&inventory, Some("2026-08-24")).unwrap().id,
            "2026-08-24T00-00-00-000Z"
        );
        assert!(matches!(
            select_run(&inventory, Some("missing")),
            Err(RunStoreError::RunNotFound(_))
        ));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn reports_damaged_entries_without_hiding_valid_runs() {
        let root = temporary_directory("rejected");
        create_run(&root, "valid");
        let mismatched = create_run(&root, "mismatched");
        let mut metadata: serde_json::Value =
            serde_json::from_slice(&fs::read(mismatched.join("run.json")).unwrap()).unwrap();
        metadata["id"] = "different".into();
        fs::write(
            mismatched.join("run.json"),
            serde_json::to_vec(&metadata).unwrap(),
        )
        .unwrap();
        let corrupt = create_run(&root, "wrong-length");
        fs::write(corrupt.join("evidence.raw.gz"), b"truncated").unwrap();
        let schema_mismatch = create_run(&root, "schema-mismatch");
        let metadata_path = schema_mismatch.join("run.json");
        let mut metadata: serde_json::Value =
            serde_json::from_slice(&fs::read(&metadata_path).unwrap()).unwrap();
        metadata["rawEvidence"]["schemaVersion"] = 2.into();
        fs::write(&metadata_path, serde_json::to_vec(&metadata).unwrap()).unwrap();

        let inventory = discover_runs(&root).unwrap();
        assert_eq!(inventory.runs.len(), 1);
        assert_eq!(inventory.runs[0].id, "valid");
        assert_eq!(inventory.rejected.len(), 3);
        assert!(inventory.rejected[0].reason.contains("metadata ID"));
        assert!(
            inventory
                .rejected
                .iter()
                .any(|run| run.reason.contains("raw evidence metadata"))
        );
        assert!(
            inventory
                .rejected
                .iter()
                .any(|run| run.reason.contains("raw evidence length"))
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn refuses_linked_run_directories_and_files() {
        use std::os::unix::fs::symlink;

        let root = temporary_directory("links");
        let target = create_run(&root, "target");
        symlink(&target, root.join(".supercov/runs/linked-run")).unwrap();
        let linked_metadata = create_run(&root, "linked-metadata");
        fs::remove_file(linked_metadata.join("run.json")).unwrap();
        symlink(target.join("run.json"), linked_metadata.join("run.json")).unwrap();
        let linked_evidence = create_run(&root, "linked-evidence");
        fs::remove_file(linked_evidence.join("evidence.raw.gz")).unwrap();
        symlink(
            target.join("evidence.raw.gz"),
            linked_evidence.join("evidence.raw.gz"),
        )
        .unwrap();

        let inventory = discover_runs(&root).unwrap();
        assert_eq!(inventory.runs.len(), 1);
        assert_eq!(inventory.runs[0].id, "target");
        assert_eq!(inventory.rejected.len(), 3);
        assert!(
            inventory
                .rejected
                .iter()
                .all(|rejected| rejected.reason.contains("unsafe run-store path"))
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn compares_integrity_in_stable_contract_order_and_binds_identity_to_evidence() {
        let root = temporary_directory("identity");
        create_run(&root, "run");
        let inventory = discover_runs(&root).unwrap();
        let run = &inventory.runs[0];
        let first = query_index_identity(run).unwrap();
        fs::write(&run.evidence_path, b"different bytes").unwrap();
        let second = query_index_identity(run).unwrap();
        assert_ne!(first.evidence_sha256, second.evidence_sha256);
        assert_ne!(first.evidence_bytes, second.evidence_bytes);
        assert_eq!(first.analysis_sha256, second.analysis_sha256);
        assert_eq!(env!("SUPERCOV_ENGINE_SOURCE_SHA256").len(), 64);
        // The index is keyed to the analysis's source, not the whole engine's.
        assert_eq!(env!("SUPERCOV_ANALYSIS_SOURCE_SHA256").len(), 64);
        assert_ne!(
            env!("SUPERCOV_ANALYSIS_SOURCE_SHA256"),
            env!("SUPERCOV_ENGINE_SOURCE_SHA256")
        );

        let mut current = integrity();
        current.schema_version += 1;
        current.instrumenter_version = "javascript-v2".into();
        current.fingerprint.source = digest('2');
        current.fingerprint.tests = digest('3');
        current.fingerprint.dependencies = digest('4');
        current.fingerprint.configuration = digest('5');
        assert_eq!(
            compare_run_integrity(Some(&integrity()), &current).reasons,
            [
                "coverage schema changed",
                "instrumenter contract changed",
                "instrumented source changed",
                "test files changed",
                "dependencies or lockfile changed",
                "test/build configuration changed",
            ]
        );

        // Supercov's own digest moves on nearly every release. The checkout has
        // not changed and the recorded evidence is still a true record of what
        // ran, so it is not a reason to discard the run. Merging and the build
        // caches still consult this digest directly, where it does matter.
        let mut rebuilt = integrity();
        rebuilt.fingerprint.instrumenter = digest('1');
        assert!(!compare_run_integrity(Some(&integrity()), &rebuilt).stale);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn integrity_debugging_writes_both_fingerprints() {
        let mut debug = Vec::new();
        let comparison =
            compare_run_integrity_to(Some(&integrity()), &integrity(), Some(&mut debug));
        assert!(!comparison.stale);
        let debug = String::from_utf8(debug).unwrap();
        let lines = debug.lines().collect::<Vec<_>>();
        assert_eq!(lines.len(), 2, "{debug}");
        assert!(lines[0].starts_with("[integrity] stored : RunIntegrity"));
        assert!(lines[1].starts_with("[integrity] current: RunIntegrity"));
    }

    #[test]
    fn touching_an_index_that_is_gone_does_not_create_it() {
        let root = temporary_directory("touch-gone");
        let path = root.join("filtered-00.index");
        touch(&path);
        assert!(!path.exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn lazily_builds_reuses_and_repairs_a_fully_authenticated_typed_index() {
        let root = temporary_directory("lazy-index");
        let run = create_indexable_run(&root);
        assert!(!run.query_index_path.exists());

        {
            let index = open_or_rebuild_query_index(&run).unwrap();
            index.verify_all().unwrap();
            CoverageIndex::new(&index).unwrap();
        }
        let canonical = fs::read(&run.query_index_path).unwrap();
        assert!(canonical.len() > crate::query_index::QUERY_INDEX_HEADER_SIZE);
        {
            let index = open_or_rebuild_query_index(&run).unwrap();
            index.verify_all().unwrap();
        }
        assert_eq!(fs::read(&run.query_index_path).unwrap(), canonical);

        let mut corrupt = canonical.clone();
        let offset = crate::query_index::QUERY_INDEX_HEADER_SIZE + 8;
        corrupt[offset] ^= 0xff;
        fs::write(&run.query_index_path, corrupt).unwrap();
        {
            let index = open_or_rebuild_query_index(&run).unwrap();
            index.verify_all().unwrap();
        }
        assert_eq!(fs::read(&run.query_index_path).unwrap(), canonical);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn an_index_opened_without_the_full_check_still_refuses_a_corrupt_page() {
        // The index a process has just written is opened without hashing every
        // page first, since the write already guarantees them. What keeps that
        // safe is that no page is trusted before it is checked: reading one
        // that does not match its digest must fail, not return its bytes.
        let root = temporary_directory("written-index");
        let run = create_indexable_python_run(&root);
        let identity = query_index_identity(&run).unwrap();
        drop(open_or_rebuild_query_index(&run).unwrap());
        let lines = QueryIndex::open(&run.query_index_path, &identity)
            .unwrap()
            .descriptor(crate::coverage_index::SECTION_LINES)
            .unwrap();
        assert!(lines.length > 0, "the fixture has line records to corrupt");

        let mut bytes = fs::read(&run.query_index_path).unwrap();
        bytes[lines.offset as usize] ^= 0xff;
        fs::write(&run.query_index_path, bytes).unwrap();

        let index = open_written_query_index(&run.query_index_path, &identity)
            .expect("opening does not hash every page");
        assert!(
            matches!(
                index.bytes(crate::coverage_index::SECTION_LINES, 0, 1),
                Err(crate::query_index::QueryIndexError::CorruptPage { .. })
            ),
            "the corrupt page is refused when it is read"
        );
        assert!(
            index.verify_all().is_err(),
            "and a full check still finds it"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn indexes_the_declared_language_model() {
        let root = temporary_directory("v3-index");
        let run = create_indexable_python_run(&root);
        assert_eq!(
            run.metadata.raw_evidence.schema_version,
            EVIDENCE_ARCHIVE_SCHEMA_VERSION
        );
        assert_eq!(
            query_index_identity(&run).unwrap().archive_schema_version,
            EVIDENCE_ARCHIVE_SCHEMA_VERSION
        );
        let index = open_or_rebuild_query_index(&run).unwrap();
        index.verify_all().unwrap();
        let coverage_index = CoverageIndex::new(&index).unwrap();
        assert_eq!(
            coverage_index.model().unwrap().name,
            "python-owned-control-flow"
        );
        let summary = coverage_index
            .summary(crate::coverage_index::CoverageViewId::All)
            .unwrap();
        assert_eq!(summary.lines.percentage, 100.0);
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn atomically_replaces_a_linked_disposable_index_without_touching_its_target() {
        use std::os::unix::fs::symlink;

        let root = temporary_directory("linked-index");
        let run = create_indexable_run(&root);
        let outside = root.join("outside");
        fs::write(&outside, b"user data").unwrap();
        symlink(&outside, &run.query_index_path).unwrap();

        let index = open_or_rebuild_query_index(&run).unwrap();
        index.verify_all().unwrap();
        assert_eq!(fs::read(outside).unwrap(), b"user data");
        assert!(
            fs::symlink_metadata(&run.query_index_path)
                .unwrap()
                .file_type()
                .is_file()
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn run_ids_digests_and_integrity_are_checked_part_by_part() {
        assert!(valid_run_id("run_0123"));
        for bad in ["", ".", "..", "a/b", "a\\b", "a\0b", "a\nb"] {
            assert!(!valid_run_id(bad), "{bad:?}");
        }
        assert!(valid_hex_digest("0a1b", 4));
        for bad in ["0a1", "0A1B", "0g1b"] {
            assert!(!valid_hex_digest(bad, 4), "{bad:?}");
        }
        validate_integrity(&integrity()).unwrap();
        type Edit = Box<dyn Fn(&mut RunIntegrity)>;
        let refused: Vec<(&str, Edit)> = vec![
            (
                "integrity fingerprint",
                Box::new(|i| i.fingerprint.algorithm = "md5".into()),
            ),
            (
                "integrity fingerprint",
                Box::new(|i| i.fingerprint.source = "abc".into()),
            ),
            (
                "integrity fingerprint",
                Box::new(|i| i.fingerprint.combined = digest('G')),
            ),
            (
                "git revision",
                Box::new(|i| {
                    i.git = Some(GitIntegrity {
                        revision: Some("abc".into()),
                        branch: None,
                        dirty: false,
                    });
                }),
            ),
        ];
        for (reason, edit) in &refused {
            let mut changed = integrity();
            edit(&mut changed);
            assert!(
                matches!(validate_integrity(&changed), Err(RunStoreError::InvalidRun(r)) if r == *reason),
                "{reason}"
            );
        }
        // A SHA-1 or SHA-256 revision, or none, is a revision.
        for revision in [Some("a".repeat(40)), Some("b".repeat(64)), None] {
            let mut with_git = integrity();
            with_git.git = Some(GitIntegrity {
                revision,
                branch: Some("main".into()),
                dirty: true,
            });
            validate_integrity(&with_git).unwrap();
        }
    }

    #[test]
    fn each_run_store_error_says_what_failed() {
        let messages = [
            RunStoreError::from(io::Error::other("disk full")),
            RunStoreError::UnsafeStore(PathBuf::from(".supercov/runs")),
            RunStoreError::NoRuns,
            RunStoreError::RunNotFound("latest".into()),
            RunStoreError::InvalidRun("run metadata JSON"),
        ]
        .map(|error| error.to_string());
        assert_eq!(
            messages,
            [
                "disk full",
                "unsafe run-store path: .supercov/runs",
                "no local coverage runs",
                "coverage run not found: latest",
                "invalid coverage run: run metadata JSON",
            ]
        );
    }
}
