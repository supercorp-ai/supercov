//! Validation and normalization of the Python runtime's evidence records.
//!
//! Each Supercov-hooked interpreter publishes commit-framed JSON records into
//! its own mmap: the process identity, every phase it entered (with the exact
//! test identity that phase stands for), runner outcomes, first-sighting hits,
//! decision vectors and any measurement limitation the runtime detected. The
//! one-byte commit marker is written last, so records completed before a hard
//! kill remain readable while a torn tail stays inert. Rust joins those records
//! into the shared frontend protocol; the runtime never computes a verdict.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File},
    path::{Component, Path},
};

use memmap2::{Mmap, MmapOptions};
use serde::Deserialize;
use serde_json::json;
use sha2::{Digest, Sha256};
use supercov_contracts::{
    AttributionPrecision, ExecutionModel, FrontendAttribution, FrontendLimitation,
    FrontendLimitationScope, FrontendRunDeclaration, FrontendRunnerDeclaration,
    LANGUAGE_FRONTEND_PROTOCOL_VERSION, StructuralSource,
};

use crate::{
    coverage_analysis::McdcVector,
    coverage_report::{
        CoverageManifest, CoverageModelDeclaration, CoveragePhase, CoverageReportRequest,
        DecisionMeta, DecisionSnapshot, ExecutionScope, ExitCodeInput, PersistedCoverageModel,
        RawTestResult, RuntimeEvent, RuntimeSnapshot, TestProvenance,
    },
    evidence_archive::EvidenceArchiveEntry,
};

pub const PYTHON_EVIDENCE_VERSION: u32 = 1;
pub const PYTHON_FRONTEND_VERSION: &str = "python-monitoring-v1";
pub const PYTEST_RUNNER: &str = "pytest";
pub const UNITTEST_RUNNER: &str = "unittest";

const TRANSPORT_MAGIC: &[u8; 8] = b"SCVPYTH1";
const TRANSPORT_VERSION: u32 = 1;
const TRANSPORT_HEADER_SIZE: usize = 64;
const TRANSPORT_RECORD_HEADER_SIZE: usize = 16;
const TRANSPORT_MAX_RECORD_SIZE: usize = 4 * 1024 * 1024;

fn default_runner() -> String {
    PYTEST_RUNNER.into()
}

/// Every field the runtime writes is named so `deny_unknown_fields` keeps
/// the record shape frozen, even where Rust does not read the value yet.
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
#[serde(tag = "t", rename_all = "lowercase", deny_unknown_fields)]
enum Record {
    Process {
        v: u32,
        run: String,
        pid: u64,
        worker: String,
        python: String,
        executable: String,
        argv: Vec<String>,
    },
    Worker {
        worker: String,
    },
    Phase {
        ctx: u64,
        at: i64,
        worker: String,
        test: String,
        retry: usize,
        phase: String,
    },
    Outcome {
        worker: String,
        test: String,
        retry: usize,
        phase: String,
        outcome: String,
        xfail: bool,
        #[serde(default = "default_runner")]
        runner: String,
    },
    Hit {
        ctx: u64,
        id: String,
    },
    Dec {
        ctx: u64,
        id: String,
        v: String,
        o: u8,
    },
    Limitation {
        id: String,
        reason: String,
        #[serde(default)]
        file: Option<String>,
        #[serde(default)]
        obligation: Option<String>,
    },
    Exit {
        at: i64,
    },
}

#[derive(Debug)]
pub enum PythonEvidenceError {
    Io(String),
    UnsafeEntry(String),
    InvalidRecord {
        file: String,
        line: usize,
        reason: String,
    },
    InvalidTransport {
        file: String,
        reason: String,
    },
    DroppedRecords {
        file: String,
        count: u64,
    },
    RunMismatch {
        expected: String,
        actual: String,
    },
    UnsupportedVersion(u32),
    UnknownContext {
        file: String,
        line: usize,
        context: u64,
    },
    UnknownObligation(String),
    InvalidVector {
        id: String,
        expected: usize,
        actual: usize,
    },
    NoInterpreter,
    NoTests,
    UnsupportedPython(String),
}

impl std::fmt::Display for PythonEvidenceError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(reason) => write!(formatter, "could not read Python evidence: {reason}"),
            Self::UnsafeEntry(name) => write!(formatter, "unsafe Python evidence entry: {name}"),
            Self::InvalidRecord { file, line, reason } => {
                write!(formatter, "invalid Python evidence record {file}:{line}: {reason}")
            }
            Self::InvalidTransport { file, reason } => {
                write!(formatter, "invalid Python evidence transport {file}: {reason}")
            }
            Self::DroppedRecords { file, count } => write!(
                formatter,
                "Python evidence transport {file} exhausted its bounded capacity and dropped {count} record(s)"
            ),
            Self::RunMismatch { expected, actual } => write!(
                formatter,
                "Python evidence belongs to run {actual}, expected {expected}"
            ),
            Self::UnsupportedVersion(version) => {
                write!(formatter, "unsupported Python evidence version {version}")
            }
            Self::UnknownContext { file, line, context } => write!(
                formatter,
                "Python evidence {file}:{line} references undeclared context {context}"
            ),
            Self::UnknownObligation(id) => {
                write!(formatter, "Python runtime reported an unknown obligation: {id}")
            }
            Self::InvalidVector {
                id,
                expected,
                actual,
            } => write!(
                formatter,
                "Python decision {id} reported {actual} condition values, expected {expected}"
            ),
            Self::NoInterpreter => formatter.write_str(
                "no Supercov-hooked Python interpreter ran: the test command did not start CPython 3.12+ with Supercov's start-up hook (PYTHONPATH may be ignored by -I/-E/-S, or the runner is not Python)",
            ),
            Self::NoTests => formatter.write_str(
                "the Python run produced no test outcomes; Supercov measures Python through pytest and unittest",
            ),
            Self::UnsupportedPython(version) => write!(
                formatter,
                "Supercov measures CPython 3.12 or newer; the test command ran Python {version}"
            ),
        }
    }
}

impl std::error::Error for PythonEvidenceError {}

fn stable_id(prefix: &str, values: &[&str]) -> String {
    let mut hash = Sha256::new();
    for value in values {
        hash.update(value.as_bytes());
        hash.update([0]);
    }
    let digest = hash.finalize();
    let mut encoded = String::with_capacity(prefix.len() + 25);
    encoded.push_str(prefix);
    encoded.push(':');
    for byte in &digest[..12] {
        use std::fmt::Write as _;
        write!(&mut encoded, "{byte:02x}").expect("string formatting");
    }
    encoded
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct Identity {
    worker: String,
    test: String,
    retry: usize,
    phase: String,
}

type ObservedVectors = BTreeSet<(Vec<Option<bool>>, bool)>;
/// (worker, test, retry) -> [(phase, outcome, xfail)]
type OutcomesByAttempt = BTreeMap<(String, String, usize), Vec<(String, String, bool)>>;
/// (worker, test, retry) -> runner that reported the attempt
type RunnersByAttempt = BTreeMap<(String, String, usize), String>;

#[derive(Debug, Default)]
struct Observations {
    hits: BTreeSet<String>,
    vectors: BTreeMap<String, ObservedVectors>,
}

#[derive(Debug, Clone)]
struct RuntimeLimitation {
    id: String,
    reason: String,
    file: Option<String>,
    obligation: Option<String>,
}

#[derive(Debug, Default)]
struct Evidence {
    interpreters: usize,
    python_versions: BTreeSet<String>,
    per_identity: BTreeMap<Identity, Observations>,
    background: BTreeMap<String, Observations>,
    outcomes: OutcomesByAttempt,
    runners: RunnersByAttempt,
    limitations: Vec<RuntimeLimitation>,
}

fn read_evidence_directory(
    directory: &Path,
    run_id: &str,
) -> Result<Evidence, PythonEvidenceError> {
    let mut evidence = Evidence::default();
    let mut files = match fs::read_dir(directory) {
        Ok(entries) => entries
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| PythonEvidenceError::Io(error.to_string()))?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(error) => return Err(PythonEvidenceError::Io(error.to_string())),
    };
    files.sort_by_key(|entry| entry.file_name());
    for entry in files {
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| PythonEvidenceError::UnsafeEntry("<non-utf8>".into()))?;
        if Path::new(&name)
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
            || !name.ends_with(".mmap")
        {
            return Err(PythonEvidenceError::UnsafeEntry(name));
        }
        let metadata = fs::symlink_metadata(entry.path())
            .map_err(|error| PythonEvidenceError::Io(error.to_string()))?;
        if !metadata.file_type().is_file() {
            return Err(PythonEvidenceError::UnsafeEntry(name));
        }
        let file =
            File::open(entry.path()).map_err(|error| PythonEvidenceError::Io(error.to_string()))?;
        // The file is immutable from Supercov's perspective after the wrapped
        // interpreter has exited. No mutable alias is created while this map
        // is alive.
        let contents = unsafe { MmapOptions::new().map(&file) }
            .map_err(|error| PythonEvidenceError::Io(error.to_string()))?;
        read_evidence_file(&name, &contents, run_id, &mut evidence)?;
    }
    Ok(evidence)
}

fn transport_u32(bytes: &[u8], offset: usize) -> Option<u32> {
    bytes
        .get(offset..offset + 4)
        .and_then(|value| value.try_into().ok())
        .map(u32::from_le_bytes)
}

fn transport_u64(bytes: &[u8], offset: usize) -> Option<u64> {
    bytes
        .get(offset..offset + 8)
        .and_then(|value| value.try_into().ok())
        .map(u64::from_le_bytes)
}

fn transport_checksum(payload: &[u8]) -> u32 {
    payload.iter().fold(0x811c_9dc5_u32, |value, byte| {
        (value ^ u32::from(*byte)).wrapping_mul(0x0100_0193)
    })
}

fn align_transport(value: usize) -> Option<usize> {
    value.checked_add(7).map(|value| value & !7)
}

fn read_evidence_file(
    name: &str,
    contents: &Mmap,
    run_id: &str,
    evidence: &mut Evidence,
) -> Result<(), PythonEvidenceError> {
    let invalid_transport = |reason: &str| PythonEvidenceError::InvalidTransport {
        file: name.into(),
        reason: reason.into(),
    };
    if contents.len() < TRANSPORT_HEADER_SIZE
        || contents.get(..8) != Some(TRANSPORT_MAGIC.as_slice())
        || transport_u32(contents, 8) != Some(TRANSPORT_VERSION)
        || transport_u32(contents, 12) != Some(TRANSPORT_HEADER_SIZE as u32)
    {
        return Err(invalid_transport("header or version does not match"));
    }
    let declared_capacity =
        transport_u64(contents, 16).ok_or_else(|| invalid_transport("capacity is missing"))?;
    if declared_capacity < TRANSPORT_HEADER_SIZE as u64 || declared_capacity > contents.len() as u64
    {
        return Err(invalid_transport(
            "declared capacity is outside the mapped file",
        ));
    }
    let dropped =
        transport_u64(contents, 24).ok_or_else(|| invalid_transport("drop counter is missing"))?;
    if dropped != 0 {
        return Err(PythonEvidenceError::DroppedRecords {
            file: name.into(),
            count: dropped,
        });
    }
    let transport_pid = transport_u64(contents, 32)
        .filter(|pid| *pid != 0)
        .ok_or_else(|| invalid_transport("process id is missing"))?;
    let mut contexts = BTreeMap::<u64, Identity>::new();
    let mut process_worker: Option<String> = None;
    let mut cursor = TRANSPORT_HEADER_SIZE;
    let mut record_index = 0;
    while cursor + TRANSPORT_RECORD_HEADER_SIZE <= contents.len() {
        let commit = contents[cursor];
        if commit == 0 {
            // Payload bytes can exist after a killed writer, but an absent
            // commit byte makes that frame and every later zeroed frame inert.
            break;
        }
        record_index += 1;
        let line_number = record_index;
        let invalid = |reason: &str| PythonEvidenceError::InvalidRecord {
            file: name.into(),
            line: line_number,
            reason: reason.into(),
        };
        if commit != 1
            || contents[cursor + 1..cursor + 4] != [0, 0, 0]
            || contents[cursor + 12..cursor + 16] != [0, 0, 0, 0]
        {
            return Err(invalid("commit marker or reserved bytes are invalid"));
        }
        let length = transport_u32(contents, cursor + 4)
            .map(|value| value as usize)
            .ok_or_else(|| invalid("payload length is missing"))?;
        if length == 0 || length > TRANSPORT_MAX_RECORD_SIZE {
            return Err(invalid("payload length is outside the transport bound"));
        }
        let payload_start = cursor + TRANSPORT_RECORD_HEADER_SIZE;
        let payload_end = payload_start
            .checked_add(length)
            .filter(|end| *end <= contents.len())
            .ok_or_else(|| invalid("payload extends past the mapped file"))?;
        let next_cursor = align_transport(payload_end)
            .filter(|end| *end <= contents.len())
            .ok_or_else(|| invalid("aligned frame extends past the mapped file"))?;
        if contents[payload_end..next_cursor]
            .iter()
            .any(|byte| *byte != 0)
        {
            return Err(invalid("frame padding is not zero"));
        }
        let payload = &contents[payload_start..payload_end];
        let expected_checksum = transport_u32(contents, cursor + 8)
            .ok_or_else(|| invalid("payload checksum is missing"))?;
        if transport_checksum(payload) != expected_checksum {
            return Err(invalid("payload checksum does not match"));
        }
        let record: Record = serde_json::from_slice(payload).map_err(|error| {
            PythonEvidenceError::InvalidRecord {
                file: name.into(),
                line: line_number,
                reason: error.to_string(),
            }
        })?;
        match record {
            Record::Process {
                v,
                run,
                pid,
                worker,
                python,
                ..
            } => {
                if v != PYTHON_EVIDENCE_VERSION {
                    return Err(PythonEvidenceError::UnsupportedVersion(v));
                }
                if run != run_id {
                    return Err(PythonEvidenceError::RunMismatch {
                        expected: run_id.into(),
                        actual: run,
                    });
                }
                if pid != transport_pid {
                    return Err(invalid("process record does not match the transport owner"));
                }
                let supported = python
                    .split('.')
                    .take(2)
                    .map(|part| part.parse::<u32>().ok())
                    .collect::<Option<Vec<_>>>()
                    .is_some_and(|parts| parts.len() == 2 && (parts[0], parts[1]) >= (3, 12));
                if !supported {
                    return Err(PythonEvidenceError::UnsupportedPython(python));
                }
                evidence.interpreters += 1;
                evidence.python_versions.insert(python);
                process_worker = Some(worker);
            }
            Record::Worker { worker } => process_worker = Some(worker),
            Record::Phase {
                ctx,
                worker,
                test,
                retry,
                phase,
                ..
            } => {
                if ctx == 0 {
                    return Err(invalid("phase context 0 is reserved for background"));
                }
                if !matches!(phase.as_str(), "setup" | "call" | "teardown") {
                    return Err(invalid("unknown pytest phase"));
                }
                if test.trim().is_empty() || worker.trim().is_empty() {
                    return Err(invalid("phase identity must name a worker and test"));
                }
                contexts.insert(
                    ctx,
                    Identity {
                        worker,
                        test,
                        retry,
                        phase,
                    },
                );
            }
            Record::Outcome {
                worker,
                test,
                retry,
                phase,
                outcome,
                xfail,
                runner,
            } => {
                if !matches!(phase.as_str(), "setup" | "call" | "teardown") {
                    return Err(invalid("unknown test outcome phase"));
                }
                if !matches!(
                    outcome.as_str(),
                    "passed" | "failed" | "skipped" | "rerun" | "error"
                ) {
                    return Err(invalid("unknown test outcome"));
                }
                if !matches!(runner.as_str(), PYTEST_RUNNER | UNITTEST_RUNNER) {
                    return Err(invalid("unknown Python test runner"));
                }
                let key = (worker, test, retry);
                if let Some(previous) = evidence.runners.get(&key)
                    && previous != &runner
                {
                    return Err(invalid("one attempt was reported by two runners"));
                }
                evidence.runners.insert(key.clone(), runner);
                evidence
                    .outcomes
                    .entry(key)
                    .or_default()
                    .push((phase, outcome, xfail));
            }
            Record::Hit { ctx, id } => {
                observations(
                    evidence,
                    &contexts,
                    process_worker.as_deref(),
                    ctx,
                    name,
                    line_number,
                )?
                .hits
                .insert(id);
            }
            Record::Dec { ctx, id, v, o } => {
                if v.is_empty() || !v.bytes().all(|digit| matches!(digit, b'0' | b'1' | b'2')) {
                    return Err(invalid("decision vector digits must be 0, 1 or 2"));
                }
                if o > 1 {
                    return Err(invalid("decision outcome must be 0 or 1"));
                }
                let values = v
                    .bytes()
                    .map(|digit| match digit {
                        b'0' => None,
                        b'1' => Some(false),
                        _ => Some(true),
                    })
                    .collect::<Vec<_>>();
                observations(
                    evidence,
                    &contexts,
                    process_worker.as_deref(),
                    ctx,
                    name,
                    line_number,
                )?
                .vectors
                .entry(id)
                .or_default()
                .insert((values, o == 1));
            }
            Record::Limitation {
                id,
                reason,
                file,
                obligation,
            } => evidence.limitations.push(RuntimeLimitation {
                id,
                reason,
                file,
                obligation,
            }),
            Record::Exit { .. } => {}
        }
        cursor = next_cursor;
    }
    Ok(())
}

fn observations<'a>(
    evidence: &'a mut Evidence,
    contexts: &BTreeMap<u64, Identity>,
    process_worker: Option<&str>,
    context: u64,
    file: &str,
    line: usize,
) -> Result<&'a mut Observations, PythonEvidenceError> {
    if context == 0 {
        return Ok(evidence
            .background
            .entry(process_worker.unwrap_or("main").to_owned())
            .or_default());
    }
    let identity = contexts
        .get(&context)
        .ok_or(PythonEvidenceError::UnknownContext {
            file: file.into(),
            line,
            context,
        })?;
    Ok(evidence.per_identity.entry(identity.clone()).or_default())
}

pub fn python_coverage_model() -> CoverageModelDeclaration {
    CoverageModelDeclaration {
        language: "python".into(),
        variant: "python-owned-monitoring".into(),
        name: "python-sys-monitoring-v1".into(),
        completeness_meaning: "Every statement, function, decision vector, loop, short-circuit, match and exception-flow obligation Supercov derived from the source was observed through CPython's monitoring events with exact test identity; the declared runtime limitations remain separate.".into(),
        measured: vec![
            "executable statements proven by CPython LINE events on their header lines, or INSTRUCTION events when they share a line".into(),
            "function and lambda entry".into(),
            "boolean decision vectors with masking MC/DC from conditional-jump events".into(),
            "for-loop and comprehension zero-versus-entered iteration".into(),
            "logical and/or short-circuit alternatives".into(),
            "match case selection and guards".into(),
            "try completion, handler selection and exception propagation".into(),
            "pytest and unittest worker, test, retry and setup/call/teardown phase identity".into(),
        ],
        not_measured: vec![
            "zero-iteration executions of a loop after it has run and exited 16 times within one test phase on CPython 3.14".into(),
            "causal linkage to individual actions or passing assertions".into(),
            "code compiled from strings at runtime".into(),
            "causal test context for raw _thread or native-extension-created threads".into(),
            "child coverage outside subprocess.Popen and multiprocessing adapters".into(),
            "all input values, semantic partitions, paths, or concurrency interleavings".into(),
            "mutation score or assertion fault-detection strength".into(),
        ],
    }
}

fn phase_id(run: &str, identity: &Identity) -> String {
    stable_id(
        "python-phase",
        &[
            run,
            &identity.worker,
            &identity.test,
            &identity.retry.to_string(),
            &identity.phase,
        ],
    )
}

fn scope(run: &str, worker: &str, test: &str, retry: usize) -> ExecutionScope {
    ExecutionScope {
        version: 1,
        run_id: run.into(),
        worker_id: worker.into(),
        test_id: test.into(),
        test_key: stable_id("python-test", &[worker, test]),
        retry,
        attempt_id: stable_id("python-attempt", &[run, worker, test, &retry.to_string()]),
    }
}

struct ManifestIndex<'a> {
    points: BTreeSet<&'a str>,
    alternatives: BTreeSet<&'a str>,
    decisions: BTreeMap<&'a str, &'a DecisionMeta>,
    lines: BTreeMap<&'a str, (String, usize)>,
}

impl<'a> ManifestIndex<'a> {
    fn new(manifest: &'a CoverageManifest) -> Self {
        let mut lines = BTreeMap::new();
        for point in &manifest.points {
            lines.insert(point.id.as_str(), (point.file.clone(), point.line));
        }
        for decision in &manifest.decisions {
            lines.insert(decision.id.as_str(), (decision.file.clone(), decision.line));
        }
        for branch in &manifest.branches {
            lines.insert(branch.id.as_str(), (branch.file.clone(), branch.line));
        }
        Self {
            points: manifest
                .points
                .iter()
                .map(|point| point.id.as_str())
                .collect(),
            alternatives: manifest
                .branches
                .iter()
                .flat_map(|branch| branch.alternatives.iter().map(|alt| alt.id.as_str()))
                .collect(),
            decisions: manifest
                .decisions
                .iter()
                .map(|decision| (decision.id.as_str(), decision))
                .collect(),
            lines,
        }
    }
}

fn snapshot(
    index: &ManifestIndex<'_>,
    observations: &Observations,
    phase: &str,
) -> Result<RuntimeSnapshot, PythonEvidenceError> {
    let mut hits = BTreeSet::new();
    for id in &observations.hits {
        if !index.points.contains(id.as_str()) && !index.alternatives.contains(id.as_str()) {
            return Err(PythonEvidenceError::UnknownObligation(id.clone()));
        }
        hits.insert(id.clone());
    }
    let mut decisions = Vec::new();
    let mut events = Vec::new();
    let mut clock = 1;
    for id in &hits {
        events.push(RuntimeEvent {
            event_type: "hit".into(),
            id: id.clone(),
            vector: None,
            timestamp_ms: clock,
            phase_id: Some(phase.into()),
            environment: "python".into(),
        });
        clock += 1;
    }
    for (id, vectors) in &observations.vectors {
        let Some(meta) = index.decisions.get(id.as_str()) else {
            return Err(PythonEvidenceError::UnknownObligation(id.clone()));
        };
        let mut observed = Vec::new();
        for (values, outcome) in vectors {
            if values.len() != meta.conditions.len() {
                return Err(PythonEvidenceError::InvalidVector {
                    id: id.clone(),
                    expected: meta.conditions.len(),
                    actual: values.len(),
                });
            }
            let vector = McdcVector {
                values: values.clone(),
                outcome: *outcome,
            };
            events.push(RuntimeEvent {
                event_type: "decision".into(),
                id: id.clone(),
                vector: Some(vector.clone()),
                timestamp_ms: clock,
                phase_id: Some(phase.into()),
                environment: "python".into(),
            });
            clock += 1;
            observed.push(vector);
        }
        decisions.push(DecisionSnapshot {
            meta: (*meta).clone(),
            vectors: observed,
        });
    }
    Ok(RuntimeSnapshot {
        decisions,
        hits: hits.into_iter().collect(),
        events,
    })
}

fn attempt_status(outcomes: &[(String, String, bool)]) -> String {
    if outcomes
        .iter()
        .any(|(_, outcome, _)| matches!(outcome.as_str(), "failed" | "rerun" | "error"))
    {
        "failed"
    } else if outcomes.iter().any(|(_, outcome, _)| outcome == "skipped") {
        "skipped"
    } else {
        "passed"
    }
    .into()
}

#[derive(Debug, Clone, PartialEq)]
pub struct PythonFrontendRun {
    pub declaration: FrontendRunDeclaration,
    pub request: CoverageReportRequest,
    pub tests: usize,
    pub interpreters: usize,
    pub python_versions: Vec<String>,
}

impl PythonFrontendRun {
    pub fn archive_entries(&self) -> Result<Vec<EvidenceArchiveEntry>, serde_json::Error> {
        let model = PersistedCoverageModel::from_declaration(
            self.request
                .coverage_model
                .as_ref()
                .expect("Python frontend always declares a coverage model"),
        )
        .expect("Python coverage model is contract-valid");
        let mut entries = vec![
            EvidenceArchiveEntry {
                path: "coverage-model.json".into(),
                contents: serde_json::to_vec(&model)?,
            },
            EvidenceArchiveEntry {
                path: "frontend.json".into(),
                contents: serde_json::to_vec(&self.declaration)?,
            },
            EvidenceArchiveEntry {
                path: "manifest.json".into(),
                contents: serde_json::to_vec(&self.request.manifest)?,
            },
        ];
        for (index, result) in self.request.raw_results.iter().enumerate() {
            entries.push(EvidenceArchiveEntry {
                path: format!("results/{index:08}/mcdc.json"),
                contents: serde_json::to_vec(result)?,
            });
        }
        Ok(entries)
    }
}

/// Join the runtime's evidence directory with the ahead-of-run manifest into
/// a protocol-conformant frontend run.
pub fn build_python_frontend_run(
    manifest: &CoverageManifest,
    evidence_directory: &Path,
    run_id: &str,
    generated_at: &str,
    test_exit_code: i32,
) -> Result<PythonFrontendRun, PythonEvidenceError> {
    let evidence = read_evidence_directory(evidence_directory, run_id)?;
    if evidence.interpreters == 0 {
        return Err(PythonEvidenceError::NoInterpreter);
    }
    if evidence.outcomes.is_empty() {
        return Err(PythonEvidenceError::NoTests);
    }
    let Evidence {
        interpreters,
        python_versions,
        per_identity,
        background,
        outcomes,
        runners,
        limitations,
    } = evidence;
    let mut manifest = manifest.clone();
    let index = ManifestIndex::new(&manifest);

    let mut raw_results = Vec::new();
    let mut observed_runners = BTreeSet::new();
    let mut identities_by_attempt =
        BTreeMap::<(String, String, usize), Vec<(&Identity, &Observations)>>::new();
    for (identity, observations) in &per_identity {
        identities_by_attempt
            .entry((
                identity.worker.clone(),
                identity.test.clone(),
                identity.retry,
            ))
            .or_default()
            .push((identity, observations));
    }
    for ((worker, test, retry), mut outcomes) in outcomes {
        let runner = runners
            .get(&(worker.clone(), test.clone(), retry))
            .cloned()
            .unwrap_or_else(default_runner);
        let attempt_identities = identities_by_attempt
            .remove(&(worker.clone(), test.clone(), retry))
            .unwrap_or_default();
        observed_runners.insert(runner.clone());
        outcomes.sort_by_key(|(phase, _, _)| match phase.as_str() {
            "setup" => 0,
            "call" => 1,
            _ => 2,
        });
        let mut phases = Vec::new();
        let mut runtime = Vec::new();
        let mut observed_phases = BTreeSet::new();
        for (position, (phase_name, outcome, _)) in outcomes.iter().enumerate() {
            observed_phases.insert(phase_name.clone());
            let identity = Identity {
                worker: worker.clone(),
                test: test.clone(),
                retry,
                phase: phase_name.clone(),
            };
            let id = phase_id(run_id, &identity);
            phases.push(CoveragePhase {
                id: id.clone(),
                kind: match phase_name.as_str() {
                    "call" => "test",
                    value => value,
                }
                .into(),
                operation: format!("{runner} {phase_name}"),
                source: Some(test.clone()),
                caused_by_phase_id: None,
                started_at_ms: position as i64 * 2 + 1,
                ended_at_ms: Some(position as i64 * 2 + 2),
                status: Some(match outcome.as_str() {
                    "rerun" | "error" => "failed".into(),
                    value => value.into(),
                }),
                error: None,
            });
            if let Some((_, observations)) = attempt_identities
                .iter()
                .find(|(candidate, _)| candidate.phase == phase_name.as_str())
            {
                runtime.push(snapshot(&index, observations, &id)?);
            }
        }
        // A phase the runtime entered but pytest never reported (the worker
        // died inside it) is a failed phase with its evidence kept.
        for (identity, observations) in attempt_identities {
            if !observed_phases.contains(&identity.phase) {
                let id = phase_id(run_id, identity);
                phases.push(CoveragePhase {
                    id: id.clone(),
                    kind: match identity.phase.as_str() {
                        "call" => "test",
                        value => value,
                    }
                    .into(),
                    operation: format!("{runner} {}", identity.phase),
                    source: Some(test.clone()),
                    caused_by_phase_id: None,
                    started_at_ms: phases.len() as i64 * 2 + 1,
                    ended_at_ms: None,
                    status: Some("failed".into()),
                    error: Some("the phase started but the runner reported no outcome".into()),
                });
                runtime.push(snapshot(&index, observations, &id)?);
            }
        }
        let status = if phases.iter().any(|phase| phase.error.is_some()) {
            "failed".into()
        } else {
            attempt_status(&outcomes)
        };
        raw_results.push(RawTestResult {
            test_id: Some(test.clone()),
            scope: Some(scope(run_id, &worker, &test, retry)),
            test: test.clone(),
            test_file: test.split("::").next().map(str::to_owned),
            title: test.rsplit("::").next().map(str::to_owned),
            retry: Some(retry),
            status: Some(status),
            expected_status: Some(
                if outcomes.iter().any(|(_, _, xfail)| *xfail) {
                    "failed"
                } else {
                    "passed"
                }
                .into(),
            ),
            flaky: false,
            provenance: TestProvenance {
                runner: runner.clone(),
                kind: "unit".into(),
                project: None,
                source: PYTHON_FRONTEND_VERSION.into(),
            },
            role: "test".into(),
            phases,
            runtime,
            browser: Vec::new(),
            server: Vec::new(),
        });
    }
    // Phases with observations whose test never produced any outcome at all
    // (for example a worker killed during its first phase).
    let default_observed = observed_runners
        .iter()
        .next()
        .cloned()
        .unwrap_or_else(default_runner);
    for ((worker, test, retry), identities) in identities_by_attempt {
        let runner = default_observed.clone();
        let mut phases = Vec::new();
        let mut runtime = Vec::new();
        for (position, (identity, observations)) in identities.iter().enumerate() {
            let id = phase_id(run_id, identity);
            phases.push(CoveragePhase {
                id: id.clone(),
                kind: match identity.phase.as_str() {
                    "call" => "test",
                    value => value,
                }
                .into(),
                operation: format!("{runner} {}", identity.phase),
                source: Some(test.clone()),
                caused_by_phase_id: None,
                started_at_ms: position as i64 * 2 + 1,
                ended_at_ms: None,
                status: Some("failed".into()),
                error: Some("the phase started but the runner reported no outcome".into()),
            });
            runtime.push(snapshot(&index, observations, &id)?);
        }
        raw_results.push(RawTestResult {
            test_id: Some(test.clone()),
            scope: Some(scope(run_id, &worker, &test, retry)),
            test: test.clone(),
            test_file: test.split("::").next().map(str::to_owned),
            title: test.rsplit("::").next().map(str::to_owned),
            retry: Some(retry),
            status: Some("failed".into()),
            expected_status: Some("passed".into()),
            flaky: false,
            provenance: TestProvenance {
                runner: runner.clone(),
                kind: "unit".into(),
                project: None,
                source: PYTHON_FRONTEND_VERSION.into(),
            },
            role: "test".into(),
            phases,
            runtime,
            browser: Vec::new(),
            server: Vec::new(),
        });
    }
    for (worker, observations) in &background {
        if observations.hits.is_empty() && observations.vectors.is_empty() {
            continue;
        }
        let test = format!("__supercov_background__:{worker}");
        let identity = Identity {
            worker: worker.clone(),
            test: test.clone(),
            retry: 0,
            phase: "background".into(),
        };
        let phase = phase_id(run_id, &identity);
        raw_results.push(RawTestResult {
            test_id: Some(test.clone()),
            scope: Some(scope(run_id, worker, &test, 0)),
            test: "Python import, collection and background execution".into(),
            test_file: None,
            title: None,
            retry: Some(0),
            status: Some("unknown".into()),
            expected_status: None,
            flaky: false,
            provenance: TestProvenance {
                runner: default_observed.clone(),
                kind: "unit".into(),
                project: None,
                source: PYTHON_FRONTEND_VERSION.into(),
            },
            role: "background".into(),
            phases: vec![CoveragePhase {
                id: phase.clone(),
                kind: "background".into(),
                operation: "Python import and collection background".into(),
                source: None,
                caused_by_phase_id: None,
                started_at_ms: 0,
                ended_at_ms: Some(0),
                status: Some("passed".into()),
                error: None,
            }],
            runtime: vec![snapshot(&index, observations, &phase)?],
            browser: Vec::new(),
            server: Vec::new(),
        });
    }

    // Runtime-detected limitations: obligations the runtime could not map
    // become unmeasured, and every limitation ID joins the manifest so the
    // declaration and manifest agree.
    let mut limitation_ids = manifest
        .limitations
        .iter()
        .filter_map(|item| item.get("id").and_then(serde_json::Value::as_str))
        .map(str::to_owned)
        .collect::<BTreeSet<_>>();
    let mut unmeasured = manifest.unmeasured.iter().cloned().collect::<BTreeSet<_>>();
    let mut new_limitations = Vec::new();
    for limitation in &limitations {
        if let Some(obligation) = &limitation.obligation {
            if !index.lines.contains_key(obligation.as_str()) {
                return Err(PythonEvidenceError::UnknownObligation(obligation.clone()));
            }
            unmeasured.insert(obligation.clone());
        } else if let Some(file) = &limitation.file {
            // A code-object mapping failure or missing debug ranges prevents
            // every obligation in that source file from being observed. Mark
            // the whole file unmeasured instead of presenting its denominator
            // as ordinary uncovered code.
            unmeasured.extend(
                index
                    .lines
                    .iter()
                    .filter(|(_, (obligation_file, _))| obligation_file == file)
                    .map(|(id, _)| (*id).to_owned()),
            );
        }
        if limitation_ids.insert(limitation.id.clone()) {
            let (file, line) = limitation
                .obligation
                .as_deref()
                .and_then(|id| index.lines.get(id).cloned())
                .unwrap_or_else(|| {
                    (
                        limitation.file.clone().unwrap_or_else(|| {
                            manifest
                                .points
                                .first()
                                .map_or(".".into(), |point| point.file.clone())
                        }),
                        1,
                    )
                });
            new_limitations.push(json!({
                "id": limitation.id,
                "kind": "semantic-safety",
                "file": file,
                "line": line,
                "column": 0,
                "source": "",
                "reason": limitation.reason
            }));
        }
    }
    manifest.limitations.extend(new_limitations);
    manifest.unmeasured = unmeasured.into_iter().collect();
    let structural_limitations = limitation_ids.into_iter().collect::<Vec<_>>();

    // Retries are separate raw results so their coverage remains attempt
    // exact, but the public lifecycle diagnostic reports logical tests rather
    // than inflating the count when a flaky test is rerun.
    let tests = raw_results
        .iter()
        .filter(|raw| raw.role == "test")
        .map(|raw| raw.test.as_str())
        .collect::<BTreeSet<_>>()
        .len();
    Ok(PythonFrontendRun {
        declaration: FrontendRunDeclaration {
            protocol_version: LANGUAGE_FRONTEND_PROTOCOL_VERSION,
            frontend_id: "python".into(),
            frontend_version: PYTHON_FRONTEND_VERSION.into(),
            language: "python".into(),
            structural_source: StructuralSource::OwnedProbes,
            runners: observed_runners
                .iter()
                .map(|runner| FrontendRunnerDeclaration {
                    runner: runner.clone(),
                    execution_model: if runner == UNITTEST_RUNNER {
                        ExecutionModel::SerialInProcess
                    } else {
                        ExecutionModel::ParallelContextPropagated
                    },
                    attribution: FrontendAttribution {
                        run: AttributionPrecision::Exact,
                        worker: AttributionPrecision::Exact,
                        test: AttributionPrecision::Exact,
                        retry: AttributionPrecision::Exact,
                        phase: AttributionPrecision::Exact,
                        action: AttributionPrecision::Unavailable,
                        assertion: AttributionPrecision::Unavailable,
                    },
                    limitations: vec![
                        FrontendLimitation {
                            id: "python-action-linkage".into(),
                            scopes: vec![FrontendLimitationScope::Action],
                            reason: format!("{runner} exposes no general action lifecycle"),
                        },
                        FrontendLimitation {
                            id: "python-assertion-linkage".into(),
                            scopes: vec![FrontendLimitationScope::Assertion],
                            reason: format!("{runner} phase outcomes do not identify each passing assertion or the obligations caused by it"),
                        },
                    ],
                })
                .collect(),
            structural_limitations,
        },
        request: CoverageReportRequest {
            run_id: run_id.into(),
            manifest,
            raw_results,
            generated_at: generated_at.into(),
            coverage_model: Some(python_coverage_model()),
            integrity: None,
            test_exit_code: ExitCodeInput::Present(Some(test_exit_code)),
        },
        tests,
        interpreters,
        python_versions: python_versions.into_iter().collect(),
    })
}

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;
    use crate::{
        frontend_protocol::validate_frontend_report_request,
        python_instrumenter::build_python_obligations,
    };

    fn temporary(name: &str) -> std::path::PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "supercov-python-evidence-{}-{nonce}-{name}",
            std::process::id()
        ));
        fs::create_dir_all(&path).unwrap();
        path
    }

    fn write_transport(path: &Path, records: &[serde_json::Value], dropped: u64) {
        let payloads = records
            .iter()
            .map(|record| serde_json::to_vec(record).unwrap())
            .collect::<Vec<_>>();
        let capacity = payloads
            .iter()
            .fold(TRANSPORT_HEADER_SIZE, |cursor, payload| {
                align_transport(cursor + TRANSPORT_RECORD_HEADER_SIZE + payload.len()).unwrap()
            })
            + 64;
        let mut bytes = vec![0_u8; capacity];
        bytes[..8].copy_from_slice(TRANSPORT_MAGIC);
        bytes[8..12].copy_from_slice(&TRANSPORT_VERSION.to_le_bytes());
        bytes[12..16].copy_from_slice(&(TRANSPORT_HEADER_SIZE as u32).to_le_bytes());
        bytes[16..24].copy_from_slice(&(capacity as u64).to_le_bytes());
        bytes[24..32].copy_from_slice(&dropped.to_le_bytes());
        bytes[32..40].copy_from_slice(&1_u64.to_le_bytes());
        let mut cursor = TRANSPORT_HEADER_SIZE;
        for payload in payloads {
            let payload_start = cursor + TRANSPORT_RECORD_HEADER_SIZE;
            let payload_end = payload_start + payload.len();
            bytes[payload_start..payload_end].copy_from_slice(&payload);
            bytes[cursor + 4..cursor + 8].copy_from_slice(&(payload.len() as u32).to_le_bytes());
            bytes[cursor + 8..cursor + 12]
                .copy_from_slice(&transport_checksum(&payload).to_le_bytes());
            bytes[cursor] = 1;
            cursor = align_transport(payload_end).unwrap();
        }
        fs::write(path, bytes).unwrap();
    }

    #[test]
    fn joins_phases_outcomes_hits_and_vectors_into_exact_results() {
        let source = "def f(a, b):\n    if a and b:\n        return 1\n    return 0\n";
        let obligations = build_python_obligations("m.py", source).unwrap();
        let decision = &obligations.plan.decisions[0];
        let statement = &obligations.plan.statements[0];
        let directory = temporary("join");
        let lines = [
            json!({"t":"process","v":1,"run":"run-1","pid":1,"worker":"main","python":"3.14.4","executable":"python","argv":["pytest"]}),
            json!({"t":"hit","ctx":0,"id":statement.id}),
            json!({"t":"phase","ctx":1,"at":5,"worker":"main","test":"tests/test_m.py::test_a","retry":0,"phase":"call"}),
            json!({"t":"dec","ctx":1,"id":decision.id,"v":"22","o":1}),
            json!({"t":"hit","ctx":1,"id":decision.outcome_true}),
            json!({"t":"outcome","worker":"main","test":"tests/test_m.py::test_a","retry":0,"phase":"setup","outcome":"passed","xfail":false}),
            json!({"t":"outcome","worker":"main","test":"tests/test_m.py::test_a","retry":0,"phase":"call","outcome":"passed","xfail":false}),
            json!({"t":"outcome","worker":"main","test":"tests/test_m.py::test_a","retry":0,"phase":"teardown","outcome":"passed","xfail":false}),
            json!({"t":"limitation","id":"python-decision-partially-mapped","reason":"folded","obligation":decision.id}),
            json!({"t":"exit","at":9}),
        ];
        write_transport(&directory.join("main.1.mmap"), &lines, 0);
        let run = build_python_frontend_run(&obligations.manifest, &directory, "run-1", "now", 0)
            .unwrap();
        validate_frontend_report_request(&run.declaration, &run.request).unwrap();
        assert_eq!(run.tests, 1);
        assert_eq!(run.request.raw_results.len(), 2);
        let test = &run.request.raw_results[0];
        assert_eq!(test.status.as_deref(), Some("passed"));
        assert_eq!(test.phases.len(), 3);
        assert_eq!(test.runtime.len(), 1);
        assert_eq!(test.runtime[0].decisions.len(), 1);
        assert!(
            test.runtime[0]
                .events
                .iter()
                .all(|event| event.phase_id.is_some())
        );
        let background = &run.request.raw_results[1];
        assert_eq!(background.role, "background");
        assert!(run.request.manifest.unmeasured.contains(&decision.id));
        assert!(
            run.declaration
                .structural_limitations
                .contains(&"python-decision-partially-mapped".to_owned())
        );
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn fails_closed_without_an_interpreter_or_tests() {
        let obligations = build_python_obligations("m.py", "x = 1\n").unwrap();
        let directory = temporary("empty");
        assert!(matches!(
            build_python_frontend_run(&obligations.manifest, &directory, "run-1", "now", 0),
            Err(PythonEvidenceError::NoInterpreter)
        ));
        let path = directory.join("main.1.mmap");
        write_transport(
            &path,
            &[
                json!({"t":"process","v":1,"run":"run-1","pid":1,"worker":"main","python":"3.14.4","executable":"p","argv":[]}),
            ],
            0,
        );
        assert!(matches!(
            build_python_frontend_run(&obligations.manifest, &directory, "run-1", "now", 0),
            Err(PythonEvidenceError::NoTests)
        ));
        // A killed writer may have copied part of its next payload without
        // publishing the commit byte. The reader must stop at that frame,
        // even when the tail would be invalid JSON if treated as committed.
        let mut torn = fs::read(&path).unwrap();
        let process_length = transport_u32(&torn, TRANSPORT_HEADER_SIZE + 4).unwrap() as usize;
        let torn_cursor =
            align_transport(TRANSPORT_HEADER_SIZE + TRANSPORT_RECORD_HEADER_SIZE + process_length)
                .unwrap();
        torn[torn_cursor + 4..torn_cursor + 8].copy_from_slice(&5_u32.to_le_bytes());
        torn[torn_cursor + TRANSPORT_RECORD_HEADER_SIZE
            ..torn_cursor + TRANSPORT_RECORD_HEADER_SIZE + 5]
            .copy_from_slice(b"{nope");
        fs::write(&path, torn).unwrap();
        assert!(matches!(
            build_python_frontend_run(&obligations.manifest, &directory, "run-1", "now", 0),
            Err(PythonEvidenceError::NoTests)
        ));
        write_transport(
            &path,
            &[
                json!({"t":"process","v":1,"run":"run-1","pid":1,"worker":"main","python":"3.11.9","executable":"p","argv":[]}),
            ],
            0,
        );
        assert!(matches!(
            build_python_frontend_run(&obligations.manifest, &directory, "run-1", "now", 0),
            Err(PythonEvidenceError::UnsupportedPython(_))
        ));
        write_transport(
            &path,
            &[
                json!({"t":"process","v":1,"run":"run-1","pid":1,"worker":"main","python":"3.14.4","executable":"p","argv":[]}),
            ],
            2,
        );
        assert!(matches!(
            build_python_frontend_run(&obligations.manifest, &directory, "run-1", "now", 0),
            Err(PythonEvidenceError::DroppedRecords { count: 2, .. })
        ));
        fs::remove_dir_all(directory).unwrap();
    }
}
