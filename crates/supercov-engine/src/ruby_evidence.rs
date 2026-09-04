//! Validation and normalization of the Ruby runtime's evidence records.
//!
//! Each Supercov-hooked Ruby interpreter publishes commit-framed JSON records into
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

pub const RUBY_EVIDENCE_VERSION: u32 = 1;
pub const RUBY_FRONTEND_VERSION: &str = "ruby-coverage-v1";
pub const RSPEC_RUNNER: &str = "rspec";
pub const MINITEST_RUNNER: &str = "minitest";
pub const TEST_UNIT_RUNNER: &str = "test-unit";
pub const CUCUMBER_RUNNER: &str = "cucumber";

const TRANSPORT_MAGIC: &[u8; 8] = b"SCVRUBY1";
const TRANSPORT_VERSION: u32 = 1;
const TRANSPORT_HEADER_SIZE: usize = 64;
const TRANSPORT_RECORD_HEADER_SIZE: usize = 16;
const TRANSPORT_MAX_RECORD_SIZE: usize = 4 * 1024 * 1024;

fn default_runner() -> String {
    RSPEC_RUNNER.into()
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
        ruby: String,
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
pub enum RubyEvidenceError {
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
    UnsupportedRuby(String),
}

impl std::fmt::Display for RubyEvidenceError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(reason) => write!(formatter, "could not read Ruby evidence: {reason}"),
            Self::UnsafeEntry(name) => write!(formatter, "unsafe Ruby evidence entry: {name}"),
            Self::InvalidRecord { file, line, reason } => {
                write!(formatter, "invalid Ruby evidence record {file}:{line}: {reason}")
            }
            Self::InvalidTransport { file, reason } => {
                write!(formatter, "invalid Ruby evidence transport {file}: {reason}")
            }
            Self::DroppedRecords { file, count } => write!(
                formatter,
                "Ruby evidence transport {file} exhausted its bounded capacity and dropped {count} record(s)"
            ),
            Self::RunMismatch { expected, actual } => write!(
                formatter,
                "Ruby evidence belongs to run {actual}, expected {expected}"
            ),
            Self::UnsupportedVersion(version) => {
                write!(formatter, "unsupported Ruby evidence version {version}")
            }
            Self::UnknownContext { file, line, context } => write!(
                formatter,
                "Ruby evidence {file}:{line} references undeclared context {context}"
            ),
            Self::UnknownObligation(id) => {
                write!(formatter, "Ruby runtime reported an unknown obligation: {id}")
            }
            Self::InvalidVector {
                id,
                expected,
                actual,
            } => write!(
                formatter,
                "Ruby decision {id} reported {actual} condition values, expected {expected}"
            ),
            Self::NoInterpreter => formatter.write_str(
                "no Supercov-hooked Ruby interpreter ran: the test command did not start Ruby 3.3+ with Supercov's RUBYOPT hook (RUBYOPT may be cleared by the command, or the runner is not Ruby)",
            ),
            Self::NoTests => formatter.write_str(
                "the Ruby run produced no test outcomes; Supercov measures Ruby through RSpec, Minitest, test-unit and Cucumber",
            ),
            Self::UnsupportedRuby(version) => write!(
                formatter,
                "Supercov measures Ruby 3.3 or newer; the test command ran Ruby {version}"
            ),
        }
    }
}

impl std::error::Error for RubyEvidenceError {}

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
    ruby_versions: BTreeSet<String>,
    per_identity: BTreeMap<Identity, Observations>,
    background: BTreeMap<String, Observations>,
    outcomes: OutcomesByAttempt,
    runners: RunnersByAttempt,
    limitations: Vec<RuntimeLimitation>,
}

fn read_evidence_directory(directory: &Path, run_id: &str) -> Result<Evidence, RubyEvidenceError> {
    let mut evidence = Evidence::default();
    let mut files = match fs::read_dir(directory) {
        Ok(entries) => entries
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| RubyEvidenceError::Io(error.to_string()))?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(error) => return Err(RubyEvidenceError::Io(error.to_string())),
    };
    files.sort_by_key(|entry| entry.file_name());
    for entry in files {
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| RubyEvidenceError::UnsafeEntry("<non-utf8>".into()))?;
        if Path::new(&name)
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
            || !name.ends_with(".mmap")
        {
            return Err(RubyEvidenceError::UnsafeEntry(name));
        }
        let metadata = fs::symlink_metadata(entry.path())
            .map_err(|error| RubyEvidenceError::Io(error.to_string()))?;
        if !metadata.file_type().is_file() {
            return Err(RubyEvidenceError::UnsafeEntry(name));
        }
        let file =
            File::open(entry.path()).map_err(|error| RubyEvidenceError::Io(error.to_string()))?;
        // The file is immutable from Supercov's perspective after the wrapped
        // interpreter has exited. No mutable alias is created while this map
        // is alive.
        let contents = unsafe { MmapOptions::new().map(&file) }
            .map_err(|error| RubyEvidenceError::Io(error.to_string()))?;
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
) -> Result<(), RubyEvidenceError> {
    let invalid_transport = |reason: &str| RubyEvidenceError::InvalidTransport {
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
        return Err(RubyEvidenceError::DroppedRecords {
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
        let invalid = |reason: &str| RubyEvidenceError::InvalidRecord {
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
        let record: Record =
            serde_json::from_slice(payload).map_err(|error| RubyEvidenceError::InvalidRecord {
                file: name.into(),
                line: line_number,
                reason: error.to_string(),
            })?;
        match record {
            Record::Process {
                v,
                run,
                pid,
                worker,
                ruby,
                ..
            } => {
                if v != RUBY_EVIDENCE_VERSION {
                    return Err(RubyEvidenceError::UnsupportedVersion(v));
                }
                if run != run_id {
                    return Err(RubyEvidenceError::RunMismatch {
                        expected: run_id.into(),
                        actual: run,
                    });
                }
                if pid != transport_pid {
                    return Err(invalid("process record does not match the transport owner"));
                }
                let supported = ruby
                    .split('.')
                    .take(2)
                    .map(|part| part.parse::<u32>().ok())
                    .collect::<Option<Vec<_>>>()
                    .is_some_and(|parts| parts.len() == 2 && (parts[0], parts[1]) >= (3, 3));
                if !supported {
                    return Err(RubyEvidenceError::UnsupportedRuby(ruby));
                }
                evidence.interpreters += 1;
                evidence.ruby_versions.insert(ruby);
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
                    return Err(invalid("unknown test phase"));
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
                if !matches!(
                    runner.as_str(),
                    RSPEC_RUNNER | MINITEST_RUNNER | TEST_UNIT_RUNNER | CUCUMBER_RUNNER
                ) {
                    return Err(invalid("unknown Ruby test runner"));
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
) -> Result<&'a mut Observations, RubyEvidenceError> {
    if context == 0 {
        return Ok(evidence
            .background
            .entry(process_worker.unwrap_or("main").to_owned())
            .or_default());
    }
    let identity = contexts
        .get(&context)
        .ok_or(RubyEvidenceError::UnknownContext {
            file: file.into(),
            line,
            context,
        })?;
    Ok(evidence.per_identity.entry(identity.clone()).or_default())
}

pub fn ruby_coverage_model() -> CoverageModelDeclaration {
    CoverageModelDeclaration {
        language: "ruby".into(),
        variant: "ruby-owned-coverage".into(),
        name: "ruby-coverage-probes-v1".into(),
        completeness_meaning: "Every statement, method, decision vector, loop, short-circuit, case and rescue obligation Supercov derived from the source was observed through Ruby's Coverage module or a load-time probe with exact test identity; the declared limitations remain separate.".into(),
        measured: vec![
            "executable statements proven by Ruby's line coverage, or a probe when a line holds several".into(),
            "method definitions entered (Ruby's method coverage)".into(),
            "if/unless/elsif/ternary/while/until decisions with masking MC/DC vectors from operand probes".into(),
            "while, until, for and iterator-block (each, map, times, ...) zero-versus-entered iteration".into(),
            "&&, ||, ||= and &&= short-circuit alternatives".into(),
            "case/when and case/in clause selection, safe navigation".into(),
            "begin/rescue completion, handler selection and exception propagation".into(),
            "RSpec, Minitest, test-unit and Cucumber worker, test and setup/call/teardown phase identity".into(),
        ],
        not_measured: vec![
            "blocks and lambdas as function entry points (they are statements inside their methods)".into(),
            "blocks passed by reference (map(&:to_s)) as loops: they have no block body to observe".into(),
            "line, branch and method observations made while test phases overlapped in threads (attributed to the run; probe observations stay per test)".into(),
            "causal linkage to individual actions or passing assertions".into(),
            "code compiled from strings at runtime (eval, instance_eval with strings)".into(),
            "child coverage outside Process.spawn, Kernel#spawn, Kernel#system and fork".into(),
            "all input values, semantic partitions, paths, or concurrency interleavings".into(),
            "mutation score or assertion fault-detection strength".into(),
        ],
    }
}

fn phase_id(run: &str, identity: &Identity) -> String {
    stable_id(
        "ruby-phase",
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
        test_key: stable_id("ruby-test", &[worker, test]),
        retry,
        attempt_id: stable_id("ruby-attempt", &[run, worker, test, &retry.to_string()]),
    }
}

struct ManifestIndex<'a> {
    points: BTreeSet<&'a str>,
    alternatives: BTreeSet<&'a str>,
    decisions: BTreeMap<&'a str, &'a DecisionMeta>,
    lines: BTreeMap<&'a str, (String, usize)>,
    sources: BTreeMap<&'a str, &'a str>,
}

impl<'a> ManifestIndex<'a> {
    fn new(manifest: &'a CoverageManifest) -> Self {
        let mut lines = BTreeMap::new();
        let mut sources = BTreeMap::new();
        for point in &manifest.points {
            lines.insert(point.id.as_str(), (point.file.clone(), point.line));
            sources.insert(point.id.as_str(), point.source.as_str());
        }
        for decision in &manifest.decisions {
            lines.insert(decision.id.as_str(), (decision.file.clone(), decision.line));
            sources.insert(decision.id.as_str(), decision.source.as_str());
        }
        for branch in &manifest.branches {
            lines.insert(branch.id.as_str(), (branch.file.clone(), branch.line));
            sources.insert(branch.id.as_str(), branch.source.as_str());
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
            sources,
        }
    }
}

fn snapshot(
    index: &ManifestIndex<'_>,
    observations: &Observations,
    phase: &str,
) -> Result<RuntimeSnapshot, RubyEvidenceError> {
    let mut hits = BTreeSet::new();
    for id in &observations.hits {
        if !index.points.contains(id.as_str()) && !index.alternatives.contains(id.as_str()) {
            return Err(RubyEvidenceError::UnknownObligation(id.clone()));
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
            environment: "ruby".into(),
        });
        clock += 1;
    }
    for (id, vectors) in &observations.vectors {
        let Some(meta) = index.decisions.get(id.as_str()) else {
            return Err(RubyEvidenceError::UnknownObligation(id.clone()));
        };
        let mut observed = Vec::new();
        for (values, outcome) in vectors {
            if values.len() != meta.conditions.len() {
                return Err(RubyEvidenceError::InvalidVector {
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
                environment: "ruby".into(),
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
pub struct RubyFrontendRun {
    pub declaration: FrontendRunDeclaration,
    pub request: CoverageReportRequest,
    pub tests: usize,
    pub interpreters: usize,
    pub ruby_versions: Vec<String>,
}

impl RubyFrontendRun {
    pub fn archive_entries(&self) -> Result<Vec<EvidenceArchiveEntry>, serde_json::Error> {
        let model = PersistedCoverageModel::from_declaration(
            self.request
                .coverage_model
                .as_ref()
                .expect("Ruby frontend always declares a coverage model"),
        )
        .expect("Ruby coverage model is contract-valid");
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
pub fn build_ruby_frontend_run(
    manifest: &CoverageManifest,
    evidence_directory: &Path,
    run_id: &str,
    generated_at: &str,
    test_exit_code: i32,
) -> Result<RubyFrontendRun, RubyEvidenceError> {
    let evidence = read_evidence_directory(evidence_directory, run_id)?;
    if evidence.interpreters == 0 {
        return Err(RubyEvidenceError::NoInterpreter);
    }
    if evidence.outcomes.is_empty() {
        return Err(RubyEvidenceError::NoTests);
    }
    let Evidence {
        interpreters,
        ruby_versions,
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
        // A phase the runtime entered but the runner never reported (the worker
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
                source: RUBY_FRONTEND_VERSION.into(),
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
                source: RUBY_FRONTEND_VERSION.into(),
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
            test: "Ruby load and background execution".into(),
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
                source: RUBY_FRONTEND_VERSION.into(),
            },
            role: "background".into(),
            phases: vec![CoveragePhase {
                id: phase.clone(),
                kind: "background".into(),
                operation: "Ruby load background".into(),
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
                return Err(RubyEvidenceError::UnknownObligation(obligation.clone()));
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
            let source = limitation
                .obligation
                .as_deref()
                .and_then(|id| index.sources.get(id))
                .map(|source| source.lines().next().unwrap_or_default().to_owned())
                .unwrap_or_default();
            new_limitations.push(json!({
                "id": limitation.id,
                "kind": "semantic-safety",
                "file": file,
                "line": line,
                "column": 0,
                "source": source,
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
    Ok(RubyFrontendRun {
        declaration: FrontendRunDeclaration {
            protocol_version: LANGUAGE_FRONTEND_PROTOCOL_VERSION,
            frontend_id: "ruby".into(),
            frontend_version: RUBY_FRONTEND_VERSION.into(),
            language: "ruby".into(),
            structural_source: StructuralSource::OwnedProbes,
            runners: observed_runners
                .iter()
                .map(|runner| FrontendRunnerDeclaration {
                    runner: runner.clone(),
                    execution_model: ExecutionModel::SerialInProcess,
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
                            id: format!("ruby-{runner}-action-linkage"),
                            scopes: vec![FrontendLimitationScope::Action],
                            reason: format!("{runner} exposes no general action lifecycle"),
                        },
                        FrontendLimitation {
                            id: format!("ruby-{runner}-assertion-linkage"),
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
            coverage_model: Some(ruby_coverage_model()),
            integrity: None,
            test_exit_code: ExitCodeInput::Present(Some(test_exit_code)),
        },
        tests,
        interpreters,
        ruby_versions: ruby_versions.into_iter().collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        coverage_analysis::PointKind, frontend_protocol::validate_frontend_report_request,
        ruby_instrumenter::build_ruby_obligations,
    };

    fn frame(payload: &[u8]) -> Vec<u8> {
        let mut bytes = vec![1u8, 0, 0, 0];
        bytes.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        bytes.extend_from_slice(&transport_checksum(payload).to_le_bytes());
        bytes.extend_from_slice(&[0, 0, 0, 0]);
        bytes.extend_from_slice(payload);
        while bytes.len() % 8 != 0 {
            bytes.push(0);
        }
        bytes
    }

    fn transport(records: &[serde_json::Value]) -> Vec<u8> {
        let mut body = Vec::new();
        for record in records {
            body.extend(frame(record.to_string().as_bytes()));
        }
        let capacity = TRANSPORT_HEADER_SIZE + body.len();
        let mut bytes = vec![0u8; TRANSPORT_HEADER_SIZE];
        bytes[..8].copy_from_slice(TRANSPORT_MAGIC);
        bytes[8..12].copy_from_slice(&TRANSPORT_VERSION.to_le_bytes());
        bytes[12..16].copy_from_slice(&(TRANSPORT_HEADER_SIZE as u32).to_le_bytes());
        bytes[16..24].copy_from_slice(&(capacity as u64).to_le_bytes());
        bytes[32..40].copy_from_slice(&7u64.to_le_bytes());
        bytes.extend(body);
        bytes
    }

    fn temporary(name: &str) -> std::path::PathBuf {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "supercov-ruby-evidence-{}-{nonce}-{name}",
            std::process::id()
        ));
        fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn joins_rspec_and_minitest_outcomes_into_exact_results() {
        let source = "def f(a, b)\n  if a && b\n    1\n  else\n    0\n  end\nend\n";
        let mut probe = 0;
        let obligations =
            build_ruby_obligations("lib/m.rb", source.as_bytes(), &mut probe).unwrap();
        let decision = &obligations.manifest.decisions[0];
        let statement = obligations
            .manifest
            .points
            .iter()
            .find(|point| point.kind == PointKind::Statement)
            .unwrap();
        let directory = temporary("join");
        let records = [
            serde_json::json!({"t":"process","v":1,"run":"run-1","pid":7,"worker":"main","ruby":"4.0.6","executable":"ruby","argv":["rspec"]}),
            serde_json::json!({"t":"hit","ctx":0,"id":statement.id}),
            serde_json::json!({"t":"phase","ctx":1,"at":5,"worker":"main","test":"spec/m_spec.rb[1:1]","retry":0,"phase":"call"}),
            serde_json::json!({"t":"dec","ctx":1,"id":decision.id,"v":"22","o":1}),
            serde_json::json!({"t":"outcome","worker":"main","test":"spec/m_spec.rb[1:1]","retry":0,"phase":"setup","outcome":"passed","xfail":false,"runner":"rspec"}),
            serde_json::json!({"t":"outcome","worker":"main","test":"spec/m_spec.rb[1:1]","retry":0,"phase":"call","outcome":"passed","xfail":false,"runner":"rspec"}),
            serde_json::json!({"t":"outcome","worker":"main","test":"spec/m_spec.rb[1:1]","retry":0,"phase":"teardown","outcome":"passed","xfail":false,"runner":"rspec"}),
            serde_json::json!({"t":"phase","ctx":2,"at":6,"worker":"main","test":"MTest#test_x","retry":0,"phase":"call"}),
            serde_json::json!({"t":"outcome","worker":"main","test":"MTest#test_x","retry":0,"phase":"call","outcome":"skipped","xfail":false,"runner":"minitest"}),
            serde_json::json!({"t":"exit","at":9}),
        ];
        fs::write(directory.join("main.7.a.mmap"), transport(&records)).unwrap();
        let run =
            build_ruby_frontend_run(&obligations.manifest, &directory, "run-1", "now", 0).unwrap();
        validate_frontend_report_request(&run.declaration, &run.request).unwrap();
        assert_eq!(run.tests, 2);
        let runners = run
            .declaration
            .runners
            .iter()
            .map(|runner| runner.runner.clone())
            .collect::<Vec<_>>();
        assert_eq!(runners, ["minitest", "rspec"]);
        assert_eq!(run.ruby_versions, ["4.0.6"]);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn fails_closed_without_an_interpreter() {
        let mut probe = 0;
        let obligations = build_ruby_obligations("m.rb", b"x = 1\n", &mut probe).unwrap();
        let directory = temporary("empty");
        assert!(matches!(
            build_ruby_frontend_run(&obligations.manifest, &directory, "run-1", "now", 0),
            Err(RubyEvidenceError::NoInterpreter)
        ));
        fs::remove_dir_all(directory).unwrap();
    }
}
