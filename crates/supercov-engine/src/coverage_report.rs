//! Language-neutral reconstruction of coverage views from frozen obligations
//! and per-attempt evidence.
//!
//! This module deliberately knows nothing about JavaScript or any test runner.
//! Language frontends provide the manifest and normalized evidence records;
//! Rust owns merging, attempt outcomes, attribution confidence, filtering and
//! every structural coverage verdict.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use supercov_contracts::{COVERAGE_MODEL_SCHEMA_VERSION, FrontendRunDeclaration};

use crate::coverage_analysis::{
    AnalysisError, BranchCoverage, CoverageCoreInput, CoverageSummary, DecisionCoverage,
    McdcVector, PointCoverage, PointKind, analyze_core, find_witnesses_for_conditions,
};
use crate::evidence_archive::{EvidenceArchiveEntry, read_archive};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DecisionMeta {
    pub id: String,
    pub file: String,
    pub line: usize,
    pub column: usize,
    pub source: String,
    pub conditions: Vec<String>,
    pub kind: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PointMeta {
    pub id: String,
    pub kind: PointKind,
    pub file: String,
    pub line: usize,
    pub column: usize,
    pub source: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BranchAlternativeMeta {
    pub id: String,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BranchMeta {
    pub id: String,
    pub kind: String,
    pub file: String,
    pub line: usize,
    pub column: usize,
    pub source: String,
    pub alternatives: Vec<BranchAlternativeMeta>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CoverageManifest {
    pub decisions: Vec<DecisionMeta>,
    pub points: Vec<PointMeta>,
    pub branches: Vec<BranchMeta>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub limitations: Vec<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope: Option<Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DecisionSnapshot {
    pub meta: DecisionMeta,
    pub vectors: Vec<McdcVector>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RuntimeEvent {
    #[serde(rename = "type")]
    pub event_type: String,
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vector: Option<McdcVector>,
    pub timestamp_ms: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub phase_id: Option<String>,
    pub environment: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RuntimeSnapshot {
    #[serde(default)]
    pub decisions: Vec<DecisionSnapshot>,
    #[serde(default)]
    pub hits: Vec<String>,
    #[serde(default)]
    pub events: Vec<RuntimeEvent>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExecutionScope {
    pub version: usize,
    pub run_id: String,
    pub worker_id: String,
    pub test_id: String,
    pub test_key: String,
    pub retry: usize,
    pub attempt_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ServerRecord {
    #[serde(rename = "type")]
    pub record_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub meta: Option<DecisionMeta>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vector: Option<McdcVector>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timestamp_ms: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub phase_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope: Option<ExecutionScope>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CoveragePhase {
    pub id: String,
    pub kind: String,
    pub operation: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub caused_by_phase_id: Option<String>,
    pub started_at_ms: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ended_at_ms: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TestProvenance {
    pub runner: String,
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    pub source: String,
}

impl Default for TestProvenance {
    fn default() -> Self {
        Self {
            runner: "unknown".into(),
            kind: "unknown".into(),
            project: None,
            source: "unknown".into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RawTestResult {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub test_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope: Option<ExecutionScope>,
    pub test: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub test_file: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retry: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected_status: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub flaky: bool,
    #[serde(default)]
    pub provenance: TestProvenance,
    #[serde(default = "default_test_role")]
    pub role: String,
    #[serde(default)]
    pub phases: Vec<CoveragePhase>,
    #[serde(default)]
    pub runtime: Vec<RuntimeSnapshot>,
    #[serde(default)]
    pub browser: Vec<RuntimeSnapshot>,
    #[serde(default)]
    pub server: Vec<ServerRecord>,
}

fn default_test_role() -> String {
    "test".into()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TestAttempt {
    pub retry: usize,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected_status: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CoverageConfidence {
    pub level: String,
    pub setup_only: bool,
    pub background_only: bool,
    pub asserted: bool,
    pub tests: Vec<String>,
    pub asserted_tests: Vec<String>,
    pub runners: Vec<String>,
    pub kinds: Vec<String>,
    pub e2e: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VectorObservation {
    pub vector: McdcVector,
    pub tests: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub phases: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub explicit_phases: Vec<String>,
    pub confidence: CoverageConfidence,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConditionResult {
    pub index: usize,
    pub source: String,
    pub covered: bool,
    pub assertion_covered: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub witness: Option<[McdcVector; 2]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub witness_tests: Option<[Vec<String>; 2]>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DecisionResult {
    pub meta: DecisionMeta,
    pub executed: bool,
    pub covered: bool,
    pub vectors: Vec<McdcVector>,
    pub vector_observations: Vec<VectorObservation>,
    pub conditions: Vec<ConditionResult>,
    pub tests: Vec<String>,
    pub confidence: CoverageConfidence,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PointResult {
    pub meta: PointMeta,
    pub covered: bool,
    pub tests: Vec<String>,
    pub phases: Vec<String>,
    pub confidence: CoverageConfidence,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AlternativeResult {
    pub id: String,
    pub label: String,
    pub covered: bool,
    pub tests: Vec<String>,
    pub phases: Vec<String>,
    pub confidence: CoverageConfidence,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BranchResult {
    pub meta: BranchMeta,
    pub covered: bool,
    pub alternatives: Vec<AlternativeResult>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct SourceLine {
    pub file: String,
    pub line: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LineResult {
    pub file: String,
    pub line: usize,
    pub covered: bool,
    pub tests: Vec<String>,
    pub runners: Vec<String>,
    pub kinds: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exclusive_kind: Option<String>,
    pub phases: Vec<String>,
    pub confidence: CoverageConfidence,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TestDecisionResult {
    pub id: String,
    pub vectors: Vec<McdcVector>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TestCoverageResult {
    pub id: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub retries: Vec<usize>,
    pub attempts: Vec<TestAttempt>,
    pub outcome: String,
    pub provenance: TestProvenance,
    pub role: String,
    pub hits: Vec<String>,
    pub decisions: Vec<TestDecisionResult>,
    pub lines: Vec<SourceLine>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TestFileResult {
    pub file: String,
    pub tests: Vec<String>,
    pub runners: Vec<String>,
    pub kinds: Vec<String>,
    pub lines: Vec<SourceLine>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PhaseResult {
    #[serde(flatten)]
    pub phase: CoveragePhase,
    pub test: String,
    pub hits: Vec<String>,
    pub decisions: Vec<TestDecisionResult>,
    pub lines: Vec<SourceLine>,
    pub browser_events: usize,
    pub server_events: usize,
    pub explicit_events: usize,
    pub inferred_events: usize,
    pub explicit_browser_events: usize,
    pub inferred_browser_events: usize,
    pub explicit_server_events: usize,
    pub inferred_server_events: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DimensionCoverage {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runner: Option<String>,
    pub tests: usize,
    pub setups: usize,
    pub summary: CoverageSummary,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CoverageModel {
    pub language: String,
    pub name: String,
    pub completeness_meaning: String,
    pub measured: Vec<String>,
    pub not_measured: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CoverageModelDeclaration {
    pub language: String,
    pub variant: String,
    pub name: String,
    pub completeness_meaning: String,
    pub measured: Vec<String>,
    pub not_measured: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PersistedCoverageModel {
    pub schema_version: u32,
    pub language: String,
    pub variant: String,
    pub name: String,
    pub completeness_meaning: String,
    pub measured: Vec<String>,
    pub not_measured: Vec<String>,
}

impl PersistedCoverageModel {
    pub fn from_declaration(value: &CoverageModelDeclaration) -> Result<Self, &'static str> {
        let persisted = Self {
            schema_version: COVERAGE_MODEL_SCHEMA_VERSION,
            language: value.language.clone(),
            variant: value.variant.clone(),
            name: value.name.clone(),
            completeness_meaning: value.completeness_meaning.clone(),
            measured: value.measured.clone(),
            not_measured: value.not_measured.clone(),
        };
        persisted.clone().into_declaration()?;
        Ok(persisted)
    }

    fn into_declaration(self) -> Result<CoverageModelDeclaration, &'static str> {
        if self.schema_version != COVERAGE_MODEL_SCHEMA_VERSION {
            return Err("unsupported coverage model schema");
        }
        let valid_identifier = |value: &str| {
            (1..=supercov_contracts::COVERAGE_MODEL_MAX_IDENTIFIER_BYTES).contains(&value.len())
                && value.as_bytes()[0].is_ascii_lowercase()
                && value.bytes().all(|byte| {
                    byte.is_ascii_lowercase()
                        || byte.is_ascii_digit()
                        || matches!(byte, b'.' | b'+' | b'-')
                })
        };
        let valid_description = |value: &str| {
            !value.is_empty()
                && value.trim().len() == value.len()
                && value.len() <= supercov_contracts::COVERAGE_MODEL_MAX_DESCRIPTION_BYTES
                && !value.chars().any(char::is_control)
        };
        if !valid_identifier(&self.language)
            || !valid_identifier(&self.variant)
            || !valid_description(&self.name)
            || !valid_description(&self.completeness_meaning)
            || self.measured.is_empty()
            || self.measured.len() > supercov_contracts::COVERAGE_MODEL_MAX_SURFACES_PER_LIST
            || self.not_measured.len() > supercov_contracts::COVERAGE_MODEL_MAX_SURFACES_PER_LIST
            || self
                .measured
                .iter()
                .chain(&self.not_measured)
                .any(|item| !valid_description(item))
        {
            return Err("invalid coverage model declaration");
        }
        let measured = self.measured.iter().collect::<BTreeSet<_>>();
        let not_measured = self.not_measured.iter().collect::<BTreeSet<_>>();
        if measured.len() != self.measured.len()
            || not_measured.len() != self.not_measured.len()
            || !measured.is_disjoint(&not_measured)
        {
            return Err("invalid coverage model surface partition");
        }
        Ok(CoverageModelDeclaration {
            language: self.language,
            variant: self.variant,
            name: self.name,
            completeness_meaning: self.completeness_meaning,
            measured: self.measured,
            not_measured: self.not_measured,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CoverageView {
    pub generated_at: String,
    pub variant: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope: Option<Value>,
    pub model: CoverageModel,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub integrity: Option<Value>,
    pub limitations: Vec<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transport: Option<TransportStats>,
    pub summary: CoverageSummary,
    pub coverage_by_kind: Vec<DimensionCoverage>,
    pub coverage_by_runner: Vec<DimensionCoverage>,
    pub decisions: Vec<DecisionResult>,
    pub points: Vec<PointResult>,
    pub branches: Vec<BranchResult>,
    pub tests: Vec<TestCoverageResult>,
    pub test_files: Vec<TestFileResult>,
    pub phases: Vec<PhaseResult>,
    pub lines: Vec<LineResult>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CoverageFilters {
    pub passed: CoverageView,
    pub failed: CoverageView,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CoverageReport {
    #[serde(flatten)]
    pub view: CoverageView,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub execution: Option<ExecutionResult>,
    pub filters: CoverageFilters,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecutionResult {
    pub test_exit_code: Option<i32>,
    pub valid: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TransportStats {
    pub processes: usize,
    pub child_launches: usize,
    pub remote_launches: usize,
    pub workspace_capabilities: usize,
    pub scoped_server_records: usize,
    pub background_server_records: usize,
    pub corrupt_records: usize,
    pub corrupt_files: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(untagged)]
enum ExecutionSafeArgument {
    Text(String),
    Digest(ExecutionArgumentDigest),
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ExecutionArgumentDigest {
    bytes: usize,
    sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ExecutionCommandSummary {
    #[serde(default)]
    executable: Option<ExecutionSafeArgument>,
    arguments: Vec<ExecutionSafeArgument>,
    argument_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(
    tag = "event",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
enum ExecutionTraceEvent {
    Process {
        at: String,
        pid: u32,
        ppid: u32,
        cwd: String,
        command: ExecutionCommandSummary,
        #[serde(default)]
        entrypoint: Option<String>,
    },
    ChildLaunch {
        at: String,
        pid: u32,
        ppid: u32,
        method: String,
        command: ExecutionSafeArgument,
    },
    RemoteLaunch {
        at: String,
        pid: u32,
        ppid: u32,
        command: ExecutionCommandSummary,
        guest_root: String,
    },
    WorkspaceCapability {
        at: String,
        pid: u32,
        ppid: u32,
        host_root: String,
        guest_root: String,
        cache_identities: Vec<String>,
    },
}

impl ExecutionTraceEvent {
    fn kind(&self) -> &'static str {
        match self {
            Self::Process { .. } => "process",
            Self::ChildLaunch { .. } => "child-launch",
            Self::RemoteLaunch { .. } => "remote-launch",
            Self::WorkspaceCapability { .. } => "workspace-capability",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CoverageReportRequest {
    pub run_id: String,
    pub manifest: CoverageManifest,
    pub raw_results: Vec<RawTestResult>,
    pub generated_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coverage_model: Option<CoverageModelDeclaration>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub integrity: Option<Value>,
    #[serde(default, deserialize_with = "deserialize_exit_code")]
    pub test_exit_code: ExitCodeInput,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ArchiveReportRequest {
    pub archive_path: PathBuf,
    pub run_id: String,
    pub generated_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub integrity: Option<Value>,
    #[serde(default, deserialize_with = "deserialize_exit_code")]
    pub test_exit_code: ExitCodeInput,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum ExitCodeInput {
    #[default]
    Missing,
    Present(Option<i32>),
}

fn deserialize_exit_code<'de, D>(deserializer: D) -> Result<ExitCodeInput, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Option::<i32>::deserialize(deserializer).map(ExitCodeInput::Present)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReportError {
    Analysis(AnalysisError),
    DecisionAnalysis {
        decision_id: String,
        error: AnalysisError,
    },
    InvalidEvent(String),
    InvalidServerRecord(String),
    InvalidArchive(String),
    MissingManifest,
    InvalidJson {
        path: String,
        reason: String,
    },
    ScopeMismatch {
        expected: String,
        actual: String,
    },
    NoEvidence(String),
}

impl From<AnalysisError> for ReportError {
    fn from(value: AnalysisError) -> Self {
        Self::Analysis(value)
    }
}

#[derive(Clone, Default)]
struct OrderedVectors {
    values: Vec<McdcVector>,
    indexes: HashMap<String, usize>,
}

impl OrderedVectors {
    fn insert(&mut self, vector: &McdcVector) -> usize {
        let key = vector_key(vector);
        if let Some(index) = self.indexes.get(&key) {
            return *index;
        }
        let index = self.values.len();
        self.values.push(vector.clone());
        self.indexes.insert(key, index);
        index
    }
}

#[derive(Clone)]
struct MutableObservation {
    vector: McdcVector,
    tests: BTreeSet<String>,
    phases: BTreeSet<String>,
    explicit_phases: BTreeSet<String>,
}

#[derive(Clone)]
struct MutableTest {
    id: String,
    name: String,
    file: Option<String>,
    title: Option<String>,
    retries: BTreeSet<usize>,
    attempts: BTreeMap<usize, TestAttempt>,
    unstarted: bool,
    runner_reported_flaky: bool,
    provenance: TestProvenance,
    role: String,
    hits: BTreeSet<String>,
    decisions: BTreeMap<String, OrderedVectors>,
}

#[derive(Clone)]
struct MutablePhase {
    phase: CoveragePhase,
    test: String,
    hits: BTreeSet<String>,
    decisions: BTreeMap<String, OrderedVectors>,
    browser_events: usize,
    server_events: usize,
    explicit_events: usize,
    inferred_events: usize,
    explicit_browser_events: usize,
    inferred_browser_events: usize,
    explicit_server_events: usize,
    inferred_server_events: usize,
}

fn vector_key(vector: &McdcVector) -> String {
    let mut key = String::with_capacity(vector.values.len() + 2);
    for value in &vector.values {
        key.push(match value {
            None => '-',
            Some(false) => 'F',
            Some(true) => 'T',
        });
    }
    key.push(':');
    key.push(if vector.outcome { 'T' } else { 'F' });
    key
}

fn sorted<T: Clone + Ord>(values: &BTreeSet<T>) -> Vec<T> {
    values.iter().cloned().collect()
}

fn record_attempt(test: &mut MutableTest, raw: &RawTestResult) {
    let (Some(retry), Some(raw_status)) = (raw.retry, raw.status.as_ref()) else {
        return;
    };
    let previous = test.attempts.get(&retry);
    let status = if raw_status == "unknown" {
        previous.map_or_else(|| raw_status.clone(), |attempt| attempt.status.clone())
    } else {
        raw_status.clone()
    };
    let expected_status = raw
        .expected_status
        .clone()
        .or_else(|| previous.and_then(|attempt| attempt.expected_status.clone()));
    test.attempts.insert(
        retry,
        TestAttempt {
            retry,
            status,
            expected_status,
        },
    );
}

fn effective_attempt_status(attempt: &TestAttempt) -> &str {
    if attempt.expected_status.as_deref() == Some("failed") {
        match attempt.status.as_str() {
            "failed" => "passed",
            "passed" => "failed",
            status => status,
        }
    } else {
        attempt.status.as_str()
    }
}

fn test_outcome(test: &MutableTest) -> String {
    let Some(terminal) = test.attempts.values().next_back() else {
        return if test.unstarted {
            "unstarted".into()
        } else {
            "unknown".into()
        };
    };
    let terminal_status = effective_attempt_status(terminal);
    if terminal_status == "passed"
        && (test.runner_reported_flaky
            || test
                .attempts
                .values()
                .take(test.attempts.len().saturating_sub(1))
                .any(|attempt| effective_attempt_status(attempt) != "passed"))
    {
        "flaky".into()
    } else {
        terminal_status.into()
    }
}

fn raw_test_id(raw: &RawTestResult) -> &str {
    raw.test_id.as_deref().unwrap_or(&raw.test)
}

pub fn passing_coverage_results(raw_results: &[RawTestResult]) -> Vec<RawTestResult> {
    let mut attempts: BTreeMap<(String, usize), (BTreeSet<String>, bool)> = BTreeMap::new();
    for raw in raw_results {
        let Some(retry) = raw.retry else {
            continue;
        };
        let entry = attempts
            .entry((raw_test_id(raw).into(), retry))
            .or_default();
        if let Some(status) = &raw.status {
            entry.0.insert(status.clone());
        }
        entry.1 |= raw.expected_status.as_deref() == Some("failed");
    }
    let mut terminal_retries = BTreeMap::<String, usize>::new();
    for (test, retry) in attempts.keys() {
        terminal_retries
            .entry(test.clone())
            .and_modify(|value| *value = (*value).max(*retry))
            .or_insert(*retry);
    }
    let accepted = terminal_retries
        .into_iter()
        .filter_map(|(test, retry)| {
            let (statuses, expected_failure) = attempts.get(&(test.clone(), retry))?;
            (statuses.contains("passed") && !expected_failure).then_some((test, retry))
        })
        .collect::<BTreeSet<_>>();
    raw_results
        .iter()
        .filter(|raw| {
            raw.retry
                .is_some_and(|retry| accepted.contains(&(raw_test_id(raw).into(), retry)))
        })
        .cloned()
        .collect()
}

pub fn failed_coverage_results(raw_results: &[RawTestResult]) -> Vec<RawTestResult> {
    let mut attempts = BTreeMap::<(String, usize), Vec<&RawTestResult>>::new();
    for raw in raw_results {
        if let Some(retry) = raw.retry {
            attempts
                .entry((raw_test_id(raw).to_owned(), retry))
                .or_default()
                .push(raw);
        }
    }
    let failed = attempts
        .into_iter()
        .filter_map(|(identity, records)| {
            // Runner reporters carry expected-status semantics while hook
            // evidence records often do not. Treat reporter records as the
            // authority for the attempt outcome, then retain every companion
            // evidence record only after the attempt is classified.
            let authoritative = records
                .iter()
                .copied()
                .filter(|raw| raw.expected_status.is_some())
                .collect::<Vec<_>>();
            let authoritative = if authoritative.is_empty() {
                records
            } else {
                authoritative
            };
            authoritative
                .iter()
                .any(
                    |raw| match (raw.status.as_deref(), raw.expected_status.as_deref()) {
                        (Some("failed"), Some("failed")) => false,
                        (Some("passed"), Some("failed")) => true,
                        (Some("failed"), _) => true,
                        _ => false,
                    },
                )
                .then_some(identity)
        })
        .collect::<BTreeSet<_>>();
    raw_results
        .iter()
        .filter(|raw| {
            raw.retry
                .is_some_and(|retry| failed.contains(&(raw_test_id(raw).into(), retry)))
        })
        .cloned()
        .collect()
}

pub(crate) fn javascript_coverage_model() -> CoverageModelDeclaration {
    CoverageModelDeclaration {
        language: "javascript".into(),
        variant: "masking-short-circuit".into(),
        name: "coverage-completeness-v2".into(),
        completeness_meaning: "Every obligation in the measured model was observed by at least one existing test; test assertions and product correctness are separate assumptions.".into(),
        measured: [
            "executable source lines",
            "executable statements",
            "function entries",
            "true and false outcomes of if, ternary, while, do/while, and classic for decisions",
            "true and false outcomes of every atomic condition in those decisions",
            "masking MC/DC independence for every atomic condition in those decisions",
            "short-circuit and right-evaluated selections for &&, ||, and ?? value expressions, including JSX",
            "short-circuit and evaluated alternatives for logical assignments and optional chains",
            "provided and default-evaluated parameter and destructuring values",
            "try success and catch entry",
            "zero and entered for-in/for-of loops",
            "entered switch cases, defaults, and implicit no-match alternatives",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect(),
        not_measured: [
            "all input values or semantic input partitions",
            "all execution paths or ordering/concurrency interleavings",
            "destructuring defaults in classic for initializers (reported as blockers when discovered)",
            "the internal statements and decisions of runtime-generated eval/Function source",
            "mutation score or assertion fault-detection strength",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect(),
    }
}

fn summary_for_results(
    decisions: &[DecisionResult],
    points: &[PointResult],
    branches: &[BranchResult],
    lines: &[LineResult],
    test_ids: Option<&BTreeSet<String>>,
) -> Result<CoverageSummary, ReportError> {
    let includes = |tests: &[String], covered: bool| {
        test_ids.map_or(covered, |selected| {
            tests.iter().any(|test| selected.contains(test))
        })
    };
    let input = CoverageCoreInput {
        decisions: decisions
            .iter()
            .map(|decision| DecisionCoverage {
                condition_count: decision.meta.conditions.len(),
                vectors: decision
                    .vector_observations
                    .iter()
                    .filter(|observation| includes(&observation.tests, true))
                    .map(|observation| observation.vector.clone())
                    .collect(),
            })
            .collect(),
        points: points
            .iter()
            .map(|point| PointCoverage {
                kind: point.meta.kind.clone(),
                covered: includes(&point.tests, point.covered),
            })
            .collect(),
        branches: branches
            .iter()
            .map(|branch| BranchCoverage {
                kind: branch.meta.kind.clone(),
                alternatives: branch
                    .alternatives
                    .iter()
                    .map(|alternative| includes(&alternative.tests, alternative.covered))
                    .collect(),
            })
            .collect(),
        lines: lines
            .iter()
            .map(|line| includes(&line.tests, line.covered))
            .collect(),
    };
    Ok(analyze_core(&input)?.summary)
}

/// Recompute every structural coverage metric for an arbitrary set of test,
/// setup, and background evidence identities. MC/DC witnesses are rebuilt
/// from the selected observations; existing aggregate verdicts are never
/// reused.
pub fn coverage_summary_for_tests(
    view: &CoverageView,
    test_ids: &BTreeSet<String>,
) -> Result<CoverageSummary, ReportError> {
    summary_for_results(
        &view.decisions,
        &view.points,
        &view.branches,
        &view.lines,
        Some(test_ids),
    )
}

fn confidence_for(
    test_ids: impl IntoIterator<Item = String>,
    phase_ids: impl IntoIterator<Item = String>,
    explicit_phase_ids: impl IntoIterator<Item = String>,
    tests: &HashMap<String, MutableTest>,
    phases: &HashMap<String, MutablePhase>,
    asserted_phase_ids: &BTreeSet<String>,
) -> CoverageConfidence {
    let test_ids = test_ids.into_iter().collect::<BTreeSet<_>>();
    let phase_ids = phase_ids.into_iter().collect::<BTreeSet<_>>();
    let explicit_phase_ids = explicit_phase_ids.into_iter().collect::<BTreeSet<_>>();
    let asserted_phases = explicit_phase_ids
        .iter()
        .filter(|id| asserted_phase_ids.contains(*id))
        .collect::<Vec<_>>();
    let asserted_tests = asserted_phases
        .iter()
        .filter_map(|id| phases.get(*id).map(|phase| phase.test.clone()))
        .collect::<BTreeSet<_>>();
    let provenances = test_ids
        .iter()
        .filter_map(|id| tests.get(id).map(|test| &test.provenance))
        .collect::<Vec<_>>();
    let roles = test_ids
        .iter()
        .filter_map(|id| tests.get(id).map(|test| test.role.as_str()))
        .collect::<Vec<_>>();
    let phase_kinds = phase_ids
        .iter()
        .filter_map(|id| phases.get(id).map(|phase| phase.phase.kind.as_str()))
        .collect::<Vec<_>>();
    let only = |phase_kind: &str, role: &str| {
        if phase_kinds.is_empty() {
            !roles.is_empty() && roles.iter().all(|value| *value == role)
        } else {
            phase_kinds.iter().all(|value| *value == phase_kind)
        }
    };
    let has_action = explicit_phase_ids.iter().any(|id| {
        phases
            .get(id)
            .is_some_and(|phase| phase.phase.kind == "action")
    });
    let level = if test_ids.is_empty() {
        "unexecuted"
    } else if !asserted_tests.is_empty() {
        "asserted"
    } else if has_action {
        "action"
    } else {
        "executed"
    };
    let runners = provenances
        .iter()
        .map(|provenance| provenance.runner.clone())
        .collect::<BTreeSet<_>>();
    let kinds = provenances
        .iter()
        .map(|provenance| provenance.kind.clone())
        .collect::<BTreeSet<_>>();
    CoverageConfidence {
        level: level.into(),
        setup_only: only("setup", "setup"),
        background_only: only("background", "background"),
        asserted: !asserted_tests.is_empty(),
        tests: sorted(&test_ids),
        asserted_tests: sorted(&asserted_tests),
        runners: sorted(&runners),
        e2e: kinds.contains("e2e"),
        kinds: sorted(&kinds),
    }
}

fn add_reference(map: &mut HashMap<String, BTreeSet<String>>, id: &str, value: &str) {
    map.entry(id.into()).or_default().insert(value.into());
}

pub fn create_coverage_view(
    manifest: &CoverageManifest,
    raw_results: &[RawTestResult],
    generated_at: &str,
) -> Result<CoverageView, ReportError> {
    create_coverage_view_with_model(
        manifest,
        raw_results,
        generated_at,
        &javascript_coverage_model(),
    )
}

fn create_coverage_view_with_model(
    manifest: &CoverageManifest,
    raw_results: &[RawTestResult],
    generated_at: &str,
    coverage_model: &CoverageModelDeclaration,
) -> Result<CoverageView, ReportError> {
    let mut decision_metadata = manifest.decisions.clone();
    let decision_indexes = decision_metadata
        .iter()
        .enumerate()
        .map(|(index, meta)| (meta.id.clone(), index))
        .collect::<HashMap<_, _>>();
    let manifest_files = manifest
        .decisions
        .iter()
        .map(|meta| meta.file.clone())
        .chain(manifest.points.iter().map(|meta| meta.file.clone()))
        .chain(manifest.branches.iter().map(|meta| meta.file.clone()))
        .chain(
            manifest
                .scope
                .as_ref()
                .and_then(|scope| scope.get("entries"))
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter(|entry| entry.get("status").and_then(Value::as_str) == Some("included"))
                .filter_map(|entry| entry.get("file").and_then(Value::as_str).map(str::to_owned)),
        )
        .collect::<BTreeSet<_>>();
    let mut vectors_by_decision = HashMap::<String, Vec<MutableObservation>>::new();
    let mut vector_indexes = HashMap::<String, HashMap<String, usize>>::new();
    let mut tests_by_decision = HashMap::<String, BTreeSet<String>>::new();
    let mut tests_by_hit = HashMap::<String, BTreeSet<String>>::new();
    let mut tests_by_id = HashMap::<String, MutableTest>::new();
    let mut test_order = Vec::<String>::new();
    let mut phases_by_id = HashMap::<String, MutablePhase>::new();
    let mut phases_by_hit = HashMap::<String, BTreeSet<String>>::new();
    let mut explicit_phases_by_hit = HashMap::<String, BTreeSet<String>>::new();

    for raw in raw_results {
        let id = raw_test_id(raw).to_owned();
        if !tests_by_id.contains_key(&id) {
            test_order.push(id.clone());
            tests_by_id.insert(
                id.clone(),
                MutableTest {
                    id: id.clone(),
                    name: raw.test.clone(),
                    file: raw.test_file.clone(),
                    title: raw.title.clone(),
                    retries: raw.retry.into_iter().collect(),
                    attempts: BTreeMap::new(),
                    unstarted: raw.status.as_deref() == Some("unstarted"),
                    runner_reported_flaky: raw.flaky,
                    provenance: raw.provenance.clone(),
                    role: raw.role.clone(),
                    hits: BTreeSet::new(),
                    decisions: BTreeMap::new(),
                },
            );
        }
        let test = tests_by_id.get_mut(&id).expect("test was inserted");
        test.unstarted |= raw.status.as_deref() == Some("unstarted");
        if let Some(retry) = raw.retry {
            test.retries.insert(retry);
        }
        test.runner_reported_flaky |= raw.flaky;
        record_attempt(test, raw);

        let mut ordered_phases = raw.phases.clone();
        ordered_phases.sort_by_key(|phase| phase.started_at_ms);
        for phase in &ordered_phases {
            phases_by_id.insert(
                phase.id.clone(),
                MutablePhase {
                    phase: phase.clone(),
                    test: id.clone(),
                    hits: BTreeSet::new(),
                    decisions: BTreeMap::new(),
                    browser_events: 0,
                    server_events: 0,
                    explicit_events: 0,
                    inferred_events: 0,
                    explicit_browser_events: 0,
                    inferred_browser_events: 0,
                    explicit_server_events: 0,
                    inferred_server_events: 0,
                },
            );
        }

        let correlate = |event: &RuntimeEvent| {
            event.phase_id.clone().or_else(|| {
                ordered_phases
                    .iter()
                    .take_while(|phase| phase.started_at_ms <= event.timestamp_ms)
                    .last()
                    .map(|phase| phase.id.clone())
            })
        };

        let snapshots = raw.runtime.iter().chain(&raw.browser);
        for snapshot in snapshots {
            for decision in &snapshot.decisions {
                let Some(index) = decision_indexes.get(&decision.meta.id).copied() else {
                    if manifest_files.contains(&decision.meta.file) {
                        return Err(ReportError::InvalidServerRecord(format!(
                            "decision {} is absent from the frozen manifest",
                            decision.meta.id
                        )));
                    }
                    continue;
                };
                if decision_metadata[index] != decision.meta {
                    return Err(ReportError::InvalidServerRecord(format!(
                        "decision {} metadata differs from the frozen manifest",
                        decision.meta.id
                    )));
                }
                for vector in &decision.vectors {
                    let key = vector_key(vector);
                    let indexes = vector_indexes.entry(decision.meta.id.clone()).or_default();
                    let observations = vectors_by_decision
                        .entry(decision.meta.id.clone())
                        .or_default();
                    let observation_index = *indexes.entry(key.clone()).or_insert_with(|| {
                        observations.push(MutableObservation {
                            vector: vector.clone(),
                            tests: BTreeSet::new(),
                            phases: BTreeSet::new(),
                            explicit_phases: BTreeSet::new(),
                        });
                        observations.len() - 1
                    });
                    observations[observation_index].tests.insert(id.clone());
                    tests_by_id
                        .get_mut(&id)
                        .expect("registered test")
                        .decisions
                        .entry(decision.meta.id.clone())
                        .or_default()
                        .insert(vector);
                }
                if !decision.vectors.is_empty() {
                    add_reference(&mut tests_by_decision, &decision.meta.id, &id);
                }
            }
            for hit in &snapshot.hits {
                add_reference(&mut tests_by_hit, hit, &id);
                tests_by_id
                    .get_mut(&id)
                    .expect("registered test")
                    .hits
                    .insert(hit.clone());
            }
            for event in &snapshot.events {
                let explicit = event.phase_id.is_some();
                let Some(phase_id) = correlate(event) else {
                    continue;
                };
                let Some(phase) = phases_by_id.get_mut(&phase_id) else {
                    continue;
                };
                if event.environment == "browser" {
                    phase.browser_events += 1;
                    if explicit {
                        phase.explicit_browser_events += 1;
                    } else {
                        phase.inferred_browser_events += 1;
                    }
                } else {
                    phase.server_events += 1;
                    if explicit {
                        phase.explicit_server_events += 1;
                    } else {
                        phase.inferred_server_events += 1;
                    }
                }
                if explicit {
                    phase.explicit_events += 1;
                } else {
                    phase.inferred_events += 1;
                }
                if event.event_type == "hit" {
                    phase.hits.insert(event.id.clone());
                    add_reference(&mut phases_by_hit, &event.id, &phase_id);
                    if explicit {
                        add_reference(&mut explicit_phases_by_hit, &event.id, &phase_id);
                    }
                } else if event.event_type == "decision" {
                    let vector = event
                        .vector
                        .as_ref()
                        .ok_or_else(|| ReportError::InvalidEvent(event.id.clone()))?;
                    phase
                        .decisions
                        .entry(event.id.clone())
                        .or_default()
                        .insert(vector);
                    if let Some(index) = vector_indexes
                        .get(&event.id)
                        .and_then(|indexes| indexes.get(&vector_key(vector)))
                        .copied()
                        && let Some(observation) = vectors_by_decision
                            .get_mut(&event.id)
                            .and_then(|observations| observations.get_mut(index))
                    {
                        observation.phases.insert(phase_id.clone());
                        if explicit {
                            observation.explicit_phases.insert(phase_id.clone());
                        }
                    }
                } else {
                    return Err(ReportError::InvalidEvent(event.event_type.clone()));
                }
            }
        }

        for record in &raw.server {
            let (record_id, decision) = if record.record_type == "decision" {
                let meta = record
                    .meta
                    .as_ref()
                    .ok_or_else(|| ReportError::InvalidServerRecord("missing meta".into()))?;
                let vector = record
                    .vector
                    .as_ref()
                    .ok_or_else(|| ReportError::InvalidServerRecord("missing vector".into()))?;
                let Some(index) = decision_indexes.get(&meta.id).copied() else {
                    if manifest_files.contains(&meta.file) {
                        return Err(ReportError::InvalidServerRecord(format!(
                            "decision {} is absent from the frozen manifest",
                            meta.id
                        )));
                    }
                    continue;
                };
                if decision_metadata[index] != *meta {
                    return Err(ReportError::InvalidServerRecord(format!(
                        "decision {} metadata differs from the frozen manifest",
                        meta.id
                    )));
                }
                let key = vector_key(vector);
                let indexes = vector_indexes.entry(meta.id.clone()).or_default();
                let observations = vectors_by_decision.entry(meta.id.clone()).or_default();
                let index = *indexes.entry(key).or_insert_with(|| {
                    observations.push(MutableObservation {
                        vector: vector.clone(),
                        tests: BTreeSet::new(),
                        phases: BTreeSet::new(),
                        explicit_phases: BTreeSet::new(),
                    });
                    observations.len() - 1
                });
                observations[index].tests.insert(id.clone());
                tests_by_id
                    .get_mut(&id)
                    .expect("registered test")
                    .decisions
                    .entry(meta.id.clone())
                    .or_default()
                    .insert(vector);
                add_reference(&mut tests_by_decision, &meta.id, &id);
                (meta.id.clone(), Some(vector.clone()))
            } else if record.record_type == "hit" {
                let hit = record
                    .id
                    .as_ref()
                    .ok_or_else(|| ReportError::InvalidServerRecord("missing hit id".into()))?;
                add_reference(&mut tests_by_hit, hit, &id);
                tests_by_id
                    .get_mut(&id)
                    .expect("registered test")
                    .hits
                    .insert(hit.clone());
                (hit.clone(), None)
            } else {
                return Err(ReportError::InvalidServerRecord(record.record_type.clone()));
            };
            let Some(timestamp_ms) = record.timestamp_ms else {
                continue;
            };
            let event = RuntimeEvent {
                event_type: record.record_type.clone(),
                id: record_id,
                vector: decision,
                timestamp_ms,
                phase_id: record.phase_id.clone(),
                environment: "server".into(),
            };
            let explicit = event.phase_id.is_some();
            let phase_id = correlate(&event);
            let Some(phase_id) = phase_id else { continue };
            let Some(phase) = phases_by_id.get_mut(&phase_id) else {
                continue;
            };
            phase.server_events += 1;
            if explicit {
                phase.explicit_events += 1;
                phase.explicit_server_events += 1;
            } else {
                phase.inferred_events += 1;
                phase.inferred_server_events += 1;
            }
            if event.event_type == "hit" {
                phase.hits.insert(event.id.clone());
                add_reference(&mut phases_by_hit, &event.id, &phase_id);
                if explicit {
                    add_reference(&mut explicit_phases_by_hit, &event.id, &phase_id);
                }
            } else if let Some(vector) = &event.vector {
                phase
                    .decisions
                    .entry(event.id.clone())
                    .or_default()
                    .insert(vector);
                if let Some(index) = vector_indexes
                    .get(&event.id)
                    .and_then(|indexes| indexes.get(&vector_key(vector)))
                    .copied()
                    && let Some(observation) = vectors_by_decision
                        .get_mut(&event.id)
                        .and_then(|observations| observations.get_mut(index))
                {
                    observation.phases.insert(phase_id.clone());
                    if explicit {
                        observation.explicit_phases.insert(phase_id.clone());
                    }
                }
            }
        }
    }

    let mut asserted_phase_ids = BTreeSet::new();
    for phase in phases_by_id.values() {
        if phase.phase.kind == "assertion" && phase.phase.status.as_deref() == Some("passed") {
            asserted_phase_ids.insert(phase.phase.id.clone());
            if let Some(cause) = &phase.phase.caused_by_phase_id {
                asserted_phase_ids.insert(cause.clone());
            }
        }
    }

    decision_metadata.sort_by(|left, right| {
        left.file
            .cmp(&right.file)
            .then(left.line.cmp(&right.line))
            .then(left.column.cmp(&right.column))
    });
    let mut decisions = Vec::with_capacity(decision_metadata.len());
    for meta in decision_metadata {
        let mutable = vectors_by_decision.remove(&meta.id).unwrap_or_default();
        let mut observations = Vec::with_capacity(mutable.len());
        for observation in mutable {
            let confidence = confidence_for(
                sorted(&observation.tests),
                sorted(&observation.phases),
                sorted(&observation.explicit_phases),
                &tests_by_id,
                &phases_by_id,
                &asserted_phase_ids,
            );
            observations.push(VectorObservation {
                vector: observation.vector,
                tests: sorted(&observation.tests),
                phases: sorted(&observation.phases),
                explicit_phases: sorted(&observation.explicit_phases),
                confidence,
            });
        }
        let vectors = observations
            .iter()
            .map(|observation| observation.vector.clone())
            .collect::<Vec<_>>();
        let witnesses =
            find_witnesses_for_conditions(&vectors, meta.conditions.len()).map_err(|error| {
                ReportError::DecisionAnalysis {
                    decision_id: meta.id.clone(),
                    error,
                }
            })?;
        let mut conditions = Vec::with_capacity(meta.conditions.len());
        for (index, source) in meta.conditions.iter().enumerate() {
            let witness = witnesses[index].map(|witness| {
                [
                    vectors[witness.first].clone(),
                    vectors[witness.second].clone(),
                ]
            });
            let witness_tests = witnesses[index].map(|witness| {
                [
                    observations[witness.first].tests.clone(),
                    observations[witness.second].tests.clone(),
                ]
            });
            let assertion_covered = (0..observations.len()).any(|left| {
                ((left + 1)..observations.len()).any(|right| {
                    observations[left].confidence.asserted
                        && observations[right].confidence.asserted
                        && crate::coverage_analysis::is_independence_pair(
                            &observations[left].vector,
                            &observations[right].vector,
                            index,
                        )
                })
            });
            conditions.push(ConditionResult {
                index,
                source: source.clone(),
                covered: witness.is_some(),
                assertion_covered,
                witness,
                witness_tests,
            });
        }
        let decision_tests = tests_by_decision.remove(&meta.id).unwrap_or_default();
        let confidence = confidence_for(
            sorted(&decision_tests),
            observations
                .iter()
                .flat_map(|observation| observation.phases.clone()),
            observations
                .iter()
                .flat_map(|observation| observation.explicit_phases.clone()),
            &tests_by_id,
            &phases_by_id,
            &asserted_phase_ids,
        );
        decisions.push(DecisionResult {
            executed: !vectors.is_empty(),
            covered: conditions.iter().all(|condition| condition.covered),
            meta,
            vectors,
            vector_observations: observations,
            conditions,
            tests: sorted(&decision_tests),
            confidence,
        });
    }

    let points = manifest
        .points
        .iter()
        .cloned()
        .map(|meta| {
            let tests = tests_by_hit.get(&meta.id).cloned().unwrap_or_default();
            let phases = phases_by_hit.get(&meta.id).cloned().unwrap_or_default();
            let explicit = explicit_phases_by_hit
                .get(&meta.id)
                .cloned()
                .unwrap_or_default();
            PointResult {
                covered: tests_by_hit.contains_key(&meta.id),
                confidence: confidence_for(
                    sorted(&tests),
                    sorted(&phases),
                    sorted(&explicit),
                    &tests_by_id,
                    &phases_by_id,
                    &asserted_phase_ids,
                ),
                meta,
                tests: sorted(&tests),
                phases: sorted(&phases),
            }
        })
        .collect::<Vec<_>>();

    let branches = manifest
        .branches
        .iter()
        .cloned()
        .map(|meta| {
            let alternatives = meta
                .alternatives
                .iter()
                .map(|alternative| {
                    let tests = tests_by_hit
                        .get(&alternative.id)
                        .cloned()
                        .unwrap_or_default();
                    let phases = phases_by_hit
                        .get(&alternative.id)
                        .cloned()
                        .unwrap_or_default();
                    let explicit = explicit_phases_by_hit
                        .get(&alternative.id)
                        .cloned()
                        .unwrap_or_default();
                    AlternativeResult {
                        id: alternative.id.clone(),
                        label: alternative.label.clone(),
                        covered: tests_by_hit.contains_key(&alternative.id),
                        tests: sorted(&tests),
                        phases: sorted(&phases),
                        confidence: confidence_for(
                            sorted(&tests),
                            sorted(&phases),
                            sorted(&explicit),
                            &tests_by_id,
                            &phases_by_id,
                            &asserted_phase_ids,
                        ),
                    }
                })
                .collect::<Vec<_>>();
            BranchResult {
                covered: alternatives.iter().all(|alternative| alternative.covered),
                meta,
                alternatives,
            }
        })
        .collect::<Vec<_>>();

    let mut line_aggregates =
        BTreeMap::<SourceLine, (bool, BTreeSet<String>, BTreeSet<String>, BTreeSet<String>)>::new();
    for point in &points {
        let aggregate = line_aggregates
            .entry(SourceLine {
                file: point.meta.file.clone(),
                line: point.meta.line,
            })
            .or_default();
        aggregate.0 |= point.covered;
        aggregate.1.extend(point.tests.clone());
        aggregate.2.extend(point.phases.clone());
        aggregate.3.extend(
            explicit_phases_by_hit
                .get(&point.meta.id)
                .into_iter()
                .flatten()
                .cloned(),
        );
    }
    let lines = line_aggregates
        .into_iter()
        .map(|(location, (covered, test_ids, phase_ids, explicit_ids))| {
            let provenances = test_ids
                .iter()
                .filter_map(|id| tests_by_id.get(id).map(|test| &test.provenance))
                .collect::<Vec<_>>();
            let runners = provenances
                .iter()
                .map(|provenance| provenance.runner.clone())
                .collect::<BTreeSet<_>>();
            let kinds = provenances
                .iter()
                .map(|provenance| provenance.kind.clone())
                .collect::<BTreeSet<_>>();
            LineResult {
                file: location.file,
                line: location.line,
                covered,
                tests: sorted(&test_ids),
                runners: sorted(&runners),
                exclusive_kind: (kinds.len() == 1).then(|| kinds.first().unwrap().clone()),
                phases: sorted(&phase_ids),
                confidence: confidence_for(
                    sorted(&test_ids),
                    sorted(&phase_ids),
                    sorted(&explicit_ids),
                    &tests_by_id,
                    &phases_by_id,
                    &asserted_phase_ids,
                ),
                kinds: sorted(&kinds),
            }
        })
        .collect::<Vec<_>>();

    let point_locations = manifest
        .points
        .iter()
        .map(|point| {
            (
                point.id.clone(),
                SourceLine {
                    file: point.file.clone(),
                    line: point.line,
                },
            )
        })
        .collect::<HashMap<_, _>>();
    test_order.sort_by(|left, right| tests_by_id[left].name.cmp(&tests_by_id[right].name));
    let tests = test_order
        .into_iter()
        .map(|id| {
            let test = tests_by_id.get(&id).expect("test order references test");
            let lines = test
                .hits
                .iter()
                .filter_map(|hit| point_locations.get(hit).cloned())
                .collect::<BTreeSet<_>>();
            TestCoverageResult {
                id: test.id.clone(),
                name: test.name.clone(),
                file: test.file.clone(),
                title: test.title.clone(),
                retries: sorted(&test.retries),
                attempts: test.attempts.values().cloned().collect(),
                outcome: test_outcome(test),
                provenance: test.provenance.clone(),
                role: test.role.clone(),
                hits: sorted(&test.hits),
                decisions: test
                    .decisions
                    .iter()
                    .map(|(id, vectors)| TestDecisionResult {
                        id: id.clone(),
                        vectors: vectors.values.clone(),
                    })
                    .collect(),
                lines: lines.into_iter().collect(),
            }
        })
        .collect::<Vec<_>>();

    let mut test_files = BTreeMap::<
        String,
        (
            BTreeSet<String>,
            BTreeSet<String>,
            BTreeSet<String>,
            BTreeSet<SourceLine>,
        ),
    >::new();
    for test in &tests {
        let aggregate = test_files
            .entry(
                test.file
                    .clone()
                    .unwrap_or_else(|| "(unknown test file)".into()),
            )
            .or_default();
        aggregate.0.insert(test.id.clone());
        aggregate.1.insert(test.provenance.runner.clone());
        aggregate.2.insert(test.provenance.kind.clone());
        aggregate.3.extend(test.lines.clone());
    }
    let test_files = test_files
        .into_iter()
        .map(|(file, (tests, runners, kinds, lines))| TestFileResult {
            file,
            tests: sorted(&tests),
            runners: sorted(&runners),
            kinds: sorted(&kinds),
            lines: lines.into_iter().collect(),
        })
        .collect::<Vec<_>>();

    let mut phases = phases_by_id.values().cloned().collect::<Vec<_>>();
    phases.sort_by(|left, right| {
        left.phase
            .started_at_ms
            .cmp(&right.phase.started_at_ms)
            .then(left.phase.id.cmp(&right.phase.id))
    });
    let phases = phases
        .into_iter()
        .map(|phase| {
            let lines = phase
                .hits
                .iter()
                .filter_map(|hit| point_locations.get(hit).cloned())
                .collect::<BTreeSet<_>>();
            PhaseResult {
                phase: phase.phase,
                test: phase.test,
                hits: sorted(&phase.hits),
                decisions: phase
                    .decisions
                    .into_iter()
                    .map(|(id, vectors)| TestDecisionResult {
                        id,
                        vectors: vectors.values,
                    })
                    .collect(),
                lines: lines.into_iter().collect(),
                browser_events: phase.browser_events,
                server_events: phase.server_events,
                explicit_events: phase.explicit_events,
                inferred_events: phase.inferred_events,
                explicit_browser_events: phase.explicit_browser_events,
                inferred_browser_events: phase.inferred_browser_events,
                explicit_server_events: phase.explicit_server_events,
                inferred_server_events: phase.inferred_server_events,
            }
        })
        .collect::<Vec<_>>();

    let mut summary = summary_for_results(&decisions, &points, &branches, &lines, None)?;
    if !manifest.limitations.is_empty() {
        summary.coverage_complete = false;
        summary.completeness_blocked = Some(true);
    }

    let dimension_coverage = |field: &str| -> Result<Vec<DimensionCoverage>, ReportError> {
        let values = tests
            .iter()
            .map(|test| {
                if field == "kind" {
                    test.provenance.kind.clone()
                } else {
                    test.provenance.runner.clone()
                }
            })
            .collect::<BTreeSet<_>>();
        values
            .into_iter()
            .map(|value| {
                let selected = tests
                    .iter()
                    .filter(|test| {
                        if field == "kind" {
                            test.provenance.kind == value
                        } else {
                            test.provenance.runner == value
                        }
                    })
                    .map(|test| test.id.clone())
                    .collect::<BTreeSet<_>>();
                Ok(DimensionCoverage {
                    kind: (field == "kind").then(|| value.clone()),
                    runner: (field == "runner").then(|| value.clone()),
                    tests: tests
                        .iter()
                        .filter(|test| selected.contains(&test.id) && test.role == "test")
                        .count(),
                    setups: tests
                        .iter()
                        .filter(|test| selected.contains(&test.id) && test.role == "setup")
                        .count(),
                    summary: summary_for_results(
                        &decisions,
                        &points,
                        &branches,
                        &lines,
                        Some(&selected),
                    )?,
                })
            })
            .collect()
    };

    Ok(CoverageView {
        generated_at: generated_at.into(),
        variant: coverage_model.variant.clone(),
        scope: manifest.scope.clone(),
        model: CoverageModel {
            language: coverage_model.language.clone(),
            name: coverage_model.name.clone(),
            completeness_meaning: coverage_model.completeness_meaning.clone(),
            measured: coverage_model.measured.clone(),
            not_measured: coverage_model.not_measured.clone(),
        },
        integrity: None,
        limitations: manifest.limitations.clone(),
        transport: None,
        summary,
        coverage_by_kind: dimension_coverage("kind")?,
        coverage_by_runner: dimension_coverage("runner")?,
        decisions,
        points,
        branches,
        tests,
        test_files,
        phases,
        lines,
    })
}

pub fn analyze_coverage_results(
    request: &CoverageReportRequest,
) -> Result<CoverageReport, ReportError> {
    if let Some(scope) = request
        .raw_results
        .iter()
        .filter_map(|raw| raw.scope.as_ref())
        .find(|scope| scope.run_id != request.run_id)
    {
        return Err(ReportError::ScopeMismatch {
            expected: request.run_id.clone(),
            actual: scope.run_id.clone(),
        });
    }
    if request.raw_results.is_empty() {
        return Err(ReportError::NoEvidence(request.run_id.clone()));
    }
    let default_model = javascript_coverage_model();
    let coverage_model = request.coverage_model.as_ref().unwrap_or(&default_model);
    PersistedCoverageModel::from_declaration(coverage_model).map_err(|reason| {
        ReportError::InvalidJson {
            path: "coverage-model.json".into(),
            reason: reason.into(),
        }
    })?;
    let view = create_coverage_view_with_model(
        &request.manifest,
        &request.raw_results,
        &request.generated_at,
        coverage_model,
    )?;
    let passed = create_coverage_view_with_model(
        &request.manifest,
        &passing_coverage_results(&request.raw_results),
        &request.generated_at,
        coverage_model,
    )?;
    let failed = create_coverage_view_with_model(
        &request.manifest,
        &failed_coverage_results(&request.raw_results),
        &request.generated_at,
        coverage_model,
    )?;
    let execution = match request.test_exit_code {
        ExitCodeInput::Missing => None,
        ExitCodeInput::Present(test_exit_code) => Some(ExecutionResult {
            valid: test_exit_code == Some(0),
            test_exit_code,
        }),
    };
    let mut view = view;
    let mut passed = passed;
    let mut failed = failed;
    if let Some(integrity) = &request.integrity {
        view.integrity = Some(integrity.clone());
        passed.integrity = Some(integrity.clone());
        failed.integrity = Some(integrity.clone());
    }
    Ok(CoverageReport {
        view,
        execution,
        filters: CoverageFilters { passed, failed },
    })
}

fn parse_entry<T: for<'de> Deserialize<'de>>(
    entry: &EvidenceArchiveEntry,
) -> Result<T, ReportError> {
    serde_json::from_slice(&entry.contents).map_err(|error| ReportError::InvalidJson {
        path: entry.path.clone(),
        reason: error.to_string(),
    })
}

fn parse_json_lines<'a, T: for<'de> Deserialize<'de>>(
    entries: impl Iterator<Item = &'a EvidenceArchiveEntry>,
) -> Result<Vec<T>, ReportError> {
    let mut records = Vec::new();
    for entry in entries {
        let Some(contents) = entry.contents.strip_suffix(b"\n") else {
            return Err(ReportError::InvalidJson {
                path: entry.path.clone(),
                reason: "recognized JSONL evidence must end with a newline".into(),
            });
        };
        if contents.is_empty() {
            return Err(ReportError::InvalidJson {
                path: entry.path.clone(),
                reason: "recognized JSONL evidence must contain at least one record".into(),
            });
        }
        for (index, line) in contents.split(|byte| *byte == b'\n').enumerate() {
            if line.is_empty() {
                return Err(ReportError::InvalidJson {
                    path: entry.path.clone(),
                    reason: format!("blank JSONL record at line {}", index + 1),
                });
            }
            records.push(serde_json::from_slice(line).map_err(|error| {
                ReportError::InvalidJson {
                    path: entry.path.clone(),
                    reason: format!("invalid JSONL record at line {}: {error}", index + 1),
                }
            })?);
        }
    }
    Ok(records)
}

fn is_mcdc_result(path: &str) -> bool {
    path == "mcdc.json" || path.ends_with("/mcdc.json")
}

fn is_mcdc_journal(path: &str) -> bool {
    path == "mcdc.jsonl" || path.ends_with(".mcdc.jsonl")
}

fn validate_rust_compiler_scope(manifest: &CoverageManifest) -> Result<(), ReportError> {
    let scope = manifest
        .scope
        .as_ref()
        .and_then(Value::as_object)
        .ok_or_else(|| ReportError::InvalidArchive("missing Rust compiler source scope".into()))?;
    let expected = BTreeSet::from([
        "crate",
        "language",
        "measurementComplete",
        "model",
        "sourceFingerprint",
    ]);
    if scope.keys().map(String::as_str).collect::<BTreeSet<_>>() != expected
        || scope.get("language").and_then(Value::as_str) != Some("rust")
        || scope.get("model").and_then(Value::as_str) != Some("rust-source-v1")
        || scope
            .get("crate")
            .and_then(Value::as_str)
            .is_none_or(str::is_empty)
        || !scope
            .get("measurementComplete")
            .is_some_and(Value::is_boolean)
    {
        return Err(ReportError::InvalidArchive(
            "malformed Rust compiler source scope".into(),
        ));
    }
    let fingerprint = scope
        .get("sourceFingerprint")
        .and_then(Value::as_object)
        .ok_or_else(|| {
            ReportError::InvalidArchive("missing Rust compiler source fingerprint".into())
        })?;
    let expected_fingerprint = BTreeSet::from(["algorithm", "digest", "files", "generatedFiles"]);
    let digest = fingerprint.get("digest").and_then(Value::as_str);
    let files = fingerprint.get("files").and_then(Value::as_u64);
    let generated = fingerprint.get("generatedFiles").and_then(Value::as_u64);
    if fingerprint
        .keys()
        .map(String::as_str)
        .collect::<BTreeSet<_>>()
        != expected_fingerprint
        || fingerprint.get("algorithm").and_then(Value::as_str) != Some("sha256")
        || !digest.is_some_and(|digest| {
            digest.len() == 64
                && digest
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        })
        || files.is_none_or(|files| files == 0)
        || generated
            .zip(files)
            .is_none_or(|(generated, files)| generated > files)
    {
        return Err(ReportError::InvalidArchive(
            "malformed Rust compiler source fingerprint".into(),
        ));
    }
    Ok(())
}

pub fn analyze_coverage_archive(
    request: &ArchiveReportRequest,
) -> Result<CoverageReport, ReportError> {
    let entries = read_archive(Path::new(&request.archive_path))
        .map_err(|error| ReportError::InvalidArchive(error.to_string()))?;
    let manifest = entries
        .iter()
        .find(|entry| entry.path == "manifest.json")
        .ok_or(ReportError::MissingManifest)
        .and_then(parse_entry::<CoverageManifest>)?;
    let mut raw_results = entries
        .iter()
        .filter(|entry| is_mcdc_result(&entry.path))
        .map(parse_entry::<RawTestResult>)
        .collect::<Result<Vec<_>, _>>()?;
    let journal_results = parse_json_lines::<RawTestResult>(
        entries.iter().filter(|entry| is_mcdc_journal(&entry.path)),
    )?;
    raw_results.extend(journal_results);

    let scoped_records = parse_json_lines::<ServerRecord>(entries.iter().filter(|entry| {
        entry.path.starts_with("server/")
            && !entry.path.starts_with("server/background/")
            && entry.path.ends_with(".jsonl")
    }))?;
    for record in &scoped_records {
        let Some(scope) = &record.scope else { continue };
        let Some(raw) = raw_results
            .iter_mut()
            .find(|raw| raw.scope.as_ref() == Some(scope))
        else {
            continue;
        };
        if !raw.server.contains(record) {
            raw.server.push(record.clone());
        }
    }

    let background_records = parse_json_lines::<ServerRecord>(entries.iter().filter(|entry| {
        entry.path.starts_with("server/background/") && entry.path.ends_with(".jsonl")
    }))?;
    if !background_records.is_empty() {
        raw_results.push(RawTestResult {
            test_id: Some(format!("background:{}", request.run_id)),
            scope: None,
            test: "Background / unattributed".into(),
            test_file: None,
            title: Some("Background / unattributed".into()),
            retry: None,
            status: Some("unknown".into()),
            expected_status: None,
            flaky: false,
            provenance: TestProvenance {
                runner: "background".into(),
                kind: "background".into(),
                project: None,
                source: "explicit".into(),
            },
            role: "background".into(),
            phases: vec![],
            runtime: vec![],
            browser: vec![],
            server: background_records.clone(),
        });
    }

    let execution_events =
        parse_json_lines::<ExecutionTraceEvent>(entries.iter().filter(|entry| {
            entry.path.starts_with("execution.") && entry.path.ends_with(".jsonl")
        }))?;
    let count_event = |name: &str| {
        execution_events
            .iter()
            .filter(|event| event.kind() == name)
            .count()
    };
    let transport = TransportStats {
        processes: count_event("process"),
        child_launches: count_event("child-launch"),
        remote_launches: count_event("remote-launch"),
        workspace_capabilities: count_event("workspace-capability"),
        scoped_server_records: scoped_records.len(),
        background_server_records: background_records.len(),
        corrupt_records: 0,
        corrupt_files: 0,
    };
    let frontend = entries
        .iter()
        .find(|entry| entry.path == "frontend.json")
        .ok_or_else(|| ReportError::InvalidArchive("missing frontend.json".into()))
        .and_then(parse_entry::<FrontendRunDeclaration>)?;
    let persisted = entries
        .iter()
        .find(|entry| entry.path == "coverage-model.json")
        .ok_or_else(|| ReportError::InvalidArchive("missing coverage-model.json".into()))
        .and_then(parse_entry::<PersistedCoverageModel>)?;
    let coverage_model =
        persisted
            .into_declaration()
            .map_err(|reason| ReportError::InvalidJson {
                path: "coverage-model.json".into(),
                reason: reason.into(),
            })?;
    if frontend.language != coverage_model.language {
        return Err(ReportError::InvalidArchive(format!(
            "frontend language {} differs from coverage model language {}",
            frontend.language, coverage_model.language
        )));
    }
    if frontend.frontend_version == "rust-compiler-v1" {
        validate_rust_compiler_scope(&manifest)?;
    }
    let normalized = CoverageReportRequest {
        run_id: request.run_id.clone(),
        manifest,
        raw_results,
        generated_at: request.generated_at.clone(),
        coverage_model: Some(coverage_model),
        integrity: request.integrity.clone(),
        test_exit_code: request.test_exit_code.clone(),
    };
    let mut report = crate::frontend_protocol::analyze_frontend_results(&frontend, &normalized)
        .map_err(|error| ReportError::InvalidArchive(error.to_string()))?;
    report.view.transport = Some(transport.clone());
    report.filters.passed.transport = Some(transport.clone());
    report.filters.failed.transport = Some(transport);
    Ok(report)
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        sync::atomic::{AtomicU64, Ordering},
        time::SystemTime,
    };

    use crate::evidence_archive::{EvidenceArchiveEntry, write_archive};

    use super::*;

    static ARCHIVE_ID: AtomicU64 = AtomicU64::new(0);

    fn point(id: &str, line: usize) -> PointMeta {
        PointMeta {
            id: id.into(),
            kind: PointKind::Statement,
            file: "src/app.js".into(),
            line,
            column: 0,
            source: "work();".into(),
            label: None,
        }
    }

    fn raw(id: &str, retry: usize, status: &str, hits: &[&str]) -> RawTestResult {
        RawTestResult {
            test_id: Some(id.into()),
            scope: None,
            test: id.into(),
            test_file: Some("tests/app.test.js".into()),
            title: None,
            retry: Some(retry),
            status: Some(status.into()),
            expected_status: None,
            flaky: false,
            provenance: TestProvenance {
                runner: "node:test".into(),
                kind: "unit".into(),
                project: None,
                source: "runner-default".into(),
            },
            role: "test".into(),
            phases: vec![],
            runtime: vec![RuntimeSnapshot {
                decisions: vec![],
                hits: hits.iter().map(|hit| (*hit).into()).collect(),
                events: vec![],
            }],
            browser: vec![],
            server: vec![],
        }
    }

    #[test]
    fn report_retains_unexecuted_manifest_conditions() {
        let manifest = CoverageManifest {
            decisions: vec![DecisionMeta {
                id: "decision".into(),
                file: "src/app.js".into(),
                line: 1,
                column: 0,
                source: "left && right".into(),
                conditions: vec!["left".into(), "right".into()],
                kind: "if".into(),
            }],
            points: vec![],
            branches: vec![],
            limitations: vec![],
            scope: None,
        };
        let view =
            create_coverage_view(&manifest, &[raw("test", 0, "passed", &[])], "time").unwrap();
        assert_eq!(view.summary.conditions, 2);
        assert_eq!(view.summary.covered_conditions, 0);
        assert_eq!(view.decisions[0].conditions.len(), 2);
    }

    #[test]
    fn frozen_manifest_ignores_out_of_scope_synthetic_decisions() {
        let manifest = CoverageManifest {
            decisions: vec![DecisionMeta {
                id: "application-decision".into(),
                file: "src/app.js".into(),
                line: 1,
                column: 0,
                source: "left && right".into(),
                conditions: vec!["left".into(), "right".into()],
                kind: "if".into(),
            }],
            points: vec![],
            branches: vec![],
            limitations: vec![],
            scope: None,
        };
        let mut attempt = raw("test", 0, "passed", &[]);
        attempt.runtime[0].decisions.push(DecisionSnapshot {
            meta: DecisionMeta {
                id: "synthetic-fixture".into(),
                file: "fixtures/generated.js".into(),
                line: 1,
                column: 0,
                source: "a && b && c".into(),
                conditions: vec!["a".into(), "b".into(), "c".into()],
                kind: "if".into(),
            },
            vectors: vec![McdcVector {
                values: vec![Some(true), Some(true), Some(true)],
                outcome: true,
            }],
        });

        let view = create_coverage_view(&manifest, &[attempt], "time").unwrap();
        assert_eq!(view.decisions.len(), 1);
        assert_eq!(view.decisions[0].meta.id, "application-decision");
        assert!(view.decisions[0].vectors.is_empty());
    }

    #[test]
    fn frozen_manifest_rejects_in_scope_unknown_or_changed_decisions() {
        let expected = DecisionMeta {
            id: "application-decision".into(),
            file: "src/app.js".into(),
            line: 1,
            column: 0,
            source: "left && right".into(),
            conditions: vec!["left".into(), "right".into()],
            kind: "if".into(),
        };
        let manifest = CoverageManifest {
            decisions: vec![expected.clone()],
            points: vec![],
            branches: vec![],
            limitations: vec![],
            scope: Some(serde_json::json!({
                "entries": [{ "file": "src/empty.js", "status": "included" }]
            })),
        };
        let vector = McdcVector {
            values: vec![Some(true), Some(true)],
            outcome: true,
        };

        let mut unknown = raw("test", 0, "passed", &[]);
        unknown.runtime[0].decisions.push(DecisionSnapshot {
            meta: DecisionMeta {
                id: "unknown-decision".into(),
                file: "src/empty.js".into(),
                ..expected.clone()
            },
            vectors: vec![vector.clone()],
        });
        assert!(matches!(
            create_coverage_view(&manifest, &[unknown], "time"),
            Err(ReportError::InvalidServerRecord(reason))
                if reason.contains("absent from the frozen manifest")
        ));

        let mut changed = raw("test", 0, "passed", &[]);
        changed.runtime[0].decisions.push(DecisionSnapshot {
            meta: DecisionMeta {
                source: "left || right".into(),
                ..expected
            },
            vectors: vec![vector],
        });
        assert!(matches!(
            create_coverage_view(&manifest, &[changed], "time"),
            Err(ReportError::InvalidServerRecord(reason))
                if reason.contains("differs from the frozen manifest")
        ));
    }

    #[test]
    fn timestamp_overlap_cannot_upgrade_assertion_confidence() {
        let manifest = CoverageManifest {
            decisions: vec![],
            points: vec![point("hit", 1)],
            branches: vec![],
            limitations: vec![],
            scope: None,
        };
        let phase = CoveragePhase {
            id: "assertion".into(),
            kind: "assertion".into(),
            operation: "equal".into(),
            source: None,
            caused_by_phase_id: None,
            started_at_ms: 100,
            ended_at_ms: Some(120),
            status: Some("passed".into()),
            error: None,
        };
        let mut attempt = raw("test", 0, "passed", &["hit"]);
        attempt.phases.push(phase.clone());
        attempt.runtime[0].events.push(RuntimeEvent {
            event_type: "hit".into(),
            id: "hit".into(),
            vector: None,
            timestamp_ms: 110,
            phase_id: None,
            environment: "server".into(),
        });
        let inferred = create_coverage_view(&manifest, &[attempt.clone()], "time").unwrap();
        assert_eq!(inferred.points[0].confidence.level, "executed");
        assert!(!inferred.points[0].confidence.asserted);

        attempt.runtime[0].events[0].phase_id = Some(phase.id);
        let explicit = create_coverage_view(&manifest, &[attempt], "time").unwrap();
        assert_eq!(explicit.points[0].confidence.level, "asserted");
        assert!(explicit.points[0].confidence.asserted);
    }

    #[test]
    fn verified_view_uses_only_the_terminal_successful_attempt() {
        let manifest = CoverageManifest {
            decisions: vec![],
            points: vec![point("failed", 1), point("passed", 2), point("expected", 3)],
            branches: vec![],
            limitations: vec![],
            scope: None,
        };
        let failed = raw("flaky", 0, "failed", &["failed"]);
        let mut passed = raw("flaky", 1, "passed", &["passed"]);
        passed.flaky = true;
        let mut expected = raw("expected-failure", 0, "passed", &["expected"]);
        expected.expected_status = Some("failed".into());
        let request = CoverageReportRequest {
            run_id: "run".into(),
            manifest,
            raw_results: vec![failed, passed, expected],
            generated_at: "time".into(),
            coverage_model: None,
            integrity: None,
            test_exit_code: ExitCodeInput::Missing,
        };
        let report = analyze_coverage_results(&request).unwrap();
        assert!(!report.filters.passed.points[0].covered);
        assert!(report.filters.passed.points[1].covered);
        assert!(!report.filters.passed.points[2].covered);
        assert!(report.filters.failed.points[0].covered);
        assert_eq!(
            report
                .view
                .tests
                .iter()
                .find(|test| test.name == "expected-failure")
                .unwrap()
                .outcome,
            "failed"
        );
        assert_eq!(report.view.tests[1].outcome, "flaky");
    }

    #[test]
    fn expected_failure_is_a_green_outcome_but_not_verified_or_failed_coverage() {
        let mut expected = raw("expected-failure", 0, "failed", &["expected"]);
        expected.expected_status = Some("failed".into());
        let companion = raw("expected-failure", 0, "failed", &["expected"]);
        let request = CoverageReportRequest {
            run_id: "run".into(),
            manifest: CoverageManifest {
                decisions: vec![],
                points: vec![point("expected", 1)],
                branches: vec![],
                limitations: vec![],
                scope: None,
            },
            raw_results: vec![companion, expected],
            generated_at: "time".into(),
            coverage_model: None,
            integrity: None,
            test_exit_code: ExitCodeInput::Missing,
        };
        let report = analyze_coverage_results(&request).unwrap();
        assert_eq!(report.view.tests[0].outcome, "passed");
        assert!(!report.filters.passed.points[0].covered);
        assert!(!report.filters.failed.points[0].covered);
    }

    #[test]
    fn selected_but_unstarted_test_is_not_an_invented_attempt() {
        let mut unstarted = raw("unstarted", 0, "unstarted", &[]);
        unstarted.scope = None;
        unstarted.retry = None;
        unstarted.runtime.clear();
        let request = CoverageReportRequest {
            run_id: "run".into(),
            manifest: CoverageManifest {
                decisions: vec![],
                points: vec![],
                branches: vec![],
                limitations: vec![],
                scope: None,
            },
            raw_results: vec![unstarted],
            generated_at: "time".into(),
            coverage_model: None,
            integrity: None,
            test_exit_code: ExitCodeInput::Present(Some(100)),
        };
        let report = analyze_coverage_results(&request).unwrap();
        assert_eq!(report.view.tests[0].outcome, "unstarted");
        assert!(report.view.tests[0].attempts.is_empty());
        assert!(report.filters.passed.tests.is_empty());
        assert!(report.filters.failed.tests.is_empty());
    }

    fn archive(mut entries: Vec<EvidenceArchiveEntry>) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "supercov-rust-report-{}-{nonce}-{}",
            std::process::id(),
            ARCHIVE_ID.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&root).unwrap();
        let path = root.join("evidence.raw.gz");
        if !entries
            .iter()
            .any(|entry| entry.path == "coverage-model.json")
        {
            entries.push(EvidenceArchiveEntry {
                path: "coverage-model.json".into(),
                contents: serde_json::to_vec(
                    &PersistedCoverageModel::from_declaration(&javascript_coverage_model())
                        .unwrap(),
                )
                .unwrap(),
            });
        }
        if !entries.iter().any(|entry| entry.path == "frontend.json") {
            entries.push(EvidenceArchiveEntry {
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
                            "reason": "The fixture does not require worker identity"
                        }]
                    }],
                    "structuralLimitations": []
                }))
                .unwrap(),
            });
        }
        write_archive(entries, &path).unwrap();
        path
    }

    #[test]
    fn archive_analysis_rejects_any_malformed_recognized_jsonl() {
        let manifest = CoverageManifest {
            decisions: vec![],
            points: vec![point("background-hit", 1), point("test-hit", 2)],
            branches: vec![],
            limitations: vec![],
            scope: None,
        };
        let path = archive(vec![
            EvidenceArchiveEntry {
                path: "manifest.json".into(),
                contents: serde_json::to_vec(&manifest).unwrap(),
            },
            EvidenceArchiveEntry {
                path: "playwright-worker-1.mcdc.jsonl".into(),
                contents: {
                    let mut contents =
                        serde_json::to_vec(&raw("journal-test", 0, "passed", &["test-hit"]))
                            .unwrap();
                    contents.extend_from_slice(b"\npartial-final-line");
                    contents
                },
            },
            EvidenceArchiveEntry {
                path: "server/background/worker.jsonl".into(),
                contents: b"{\"type\":\"hit\",\"id\":\"background-hit\"}\nnot-json\n".to_vec(),
            },
        ]);
        let result = analyze_coverage_archive(&ArchiveReportRequest {
            archive_path: path.clone(),
            run_id: "run".into(),
            generated_at: "time".into(),
            integrity: None,
            test_exit_code: ExitCodeInput::Missing,
        });
        assert!(matches!(
            result,
            Err(ReportError::InvalidJson { path, .. })
                if path == "playwright-worker-1.mcdc.jsonl"
        ));
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn archive_analysis_rejects_cross_run_evidence() {
        let manifest = CoverageManifest {
            decisions: vec![],
            points: vec![],
            branches: vec![],
            limitations: vec![],
            scope: None,
        };
        let mut result = raw("test", 0, "passed", &[]);
        result.scope = Some(ExecutionScope {
            version: 1,
            run_id: "other-run".into(),
            worker_id: "worker".into(),
            test_id: "test".into(),
            test_key: "key".into(),
            retry: 0,
            attempt_id: "attempt".into(),
        });
        let path = archive(vec![
            EvidenceArchiveEntry {
                path: "manifest.json".into(),
                contents: serde_json::to_vec(&manifest).unwrap(),
            },
            EvidenceArchiveEntry {
                path: "worker/mcdc.json".into(),
                contents: serde_json::to_vec(&result).unwrap(),
            },
        ]);
        assert!(matches!(
            analyze_coverage_archive(&ArchiveReportRequest {
                archive_path: path.clone(),
                run_id: "run".into(),
                generated_at: "time".into(),
                integrity: None,
                test_exit_code: ExitCodeInput::Missing,
            }),
            Err(ReportError::InvalidArchive(reason))
                if reason.contains("expected=run actual=other-run")
        ));
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn coverage_model_vectors_are_strict_and_language_binding_is_fatal() {
        let vectors: Value = serde_json::from_str(include_str!(
            "../test-assets/coverage-model-v1/vectors.json"
        ))
        .unwrap();
        for value in vectors["valid"].as_array().unwrap() {
            let model: PersistedCoverageModel = serde_json::from_value(value.clone()).unwrap();
            model.into_declaration().unwrap();
        }
        for vector in vectors["invalid"].as_array().unwrap() {
            let value = vector["value"].clone();
            if let Ok(model) = serde_json::from_value::<PersistedCoverageModel>(value) {
                assert!(
                    model.into_declaration().is_err(),
                    "accepted invalid model vector: {}",
                    vector["reason"]
                );
            }
        }

        let manifest = CoverageManifest {
            decisions: vec![],
            points: vec![],
            branches: vec![],
            limitations: vec![],
            scope: None,
        };
        let path = archive(vec![
            EvidenceArchiveEntry {
                path: "coverage-model.json".into(),
                contents: serde_json::to_vec(&PersistedCoverageModel {
                    schema_version: COVERAGE_MODEL_SCHEMA_VERSION,
                    language: "rust".into(),
                    variant: "rust-source-v1".into(),
                    name: "Rust source coverage".into(),
                    completeness_meaning: "Every Rust obligation was satisfied.".into(),
                    measured: vec!["Rust statements".into()],
                    not_measured: vec![],
                })
                .unwrap(),
            },
            EvidenceArchiveEntry {
                path: "manifest.json".into(),
                contents: serde_json::to_vec(&manifest).unwrap(),
            },
        ]);
        assert!(matches!(
            analyze_coverage_archive(&ArchiveReportRequest {
                archive_path: path.clone(),
                run_id: "run".into(),
                generated_at: "time".into(),
                integrity: None,
                test_exit_code: ExitCodeInput::Missing,
            }),
            Err(ReportError::InvalidArchive(reason))
                if reason.contains("javascript differs from coverage model language rust")
        ));
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn rust_compiler_scope_requires_an_exact_full_source_fingerprint() {
        let mut manifest = CoverageManifest {
            decisions: vec![],
            points: vec![],
            branches: vec![],
            limitations: vec![],
            scope: Some(serde_json::json!({
                "language": "rust",
                "model": "rust-source-v1",
                "crate": "fixture",
                "measurementComplete": false,
                "sourceFingerprint": {
                    "algorithm": "sha256",
                    "digest": "1".repeat(64),
                    "files": 2,
                    "generatedFiles": 1,
                },
            })),
        };
        validate_rust_compiler_scope(&manifest).unwrap();

        manifest.scope.as_mut().unwrap()["sourceFingerprint"]["digest"] =
            Value::String("not-a-digest".into());
        assert!(matches!(
            validate_rust_compiler_scope(&manifest),
            Err(ReportError::InvalidArchive(reason))
                if reason == "malformed Rust compiler source fingerprint"
        ));

        manifest.scope.as_mut().unwrap()["sourceFingerprint"]["digest"] =
            Value::String("1".repeat(64));
        manifest.scope.as_mut().unwrap()["sourceFingerprint"]["unexpected"] = Value::Bool(true);
        assert!(validate_rust_compiler_scope(&manifest).is_err());
    }

    #[test]
    fn explicit_null_exit_code_remains_distinct_from_an_absent_exit_code() {
        let request: CoverageReportRequest = serde_json::from_value(serde_json::json!({
            "runId": "run",
            "manifest": { "decisions": [], "points": [], "branches": [] },
            "rawResults": [{
                "test": "test",
                "status": "passed",
                "browser": [],
                "server": []
            }],
            "generatedAt": "time",
            "testExitCode": null
        }))
        .unwrap();
        let report = analyze_coverage_results(&request).unwrap();
        assert_eq!(
            report.execution,
            Some(ExecutionResult {
                test_exit_code: None,
                valid: false,
            })
        );
    }
}
