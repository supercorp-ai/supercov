//! Language-neutral reconstruction of coverage views from frozen obligations
//! and per-attempt evidence.
//!
//! This module deliberately knows nothing about JavaScript or any test runner.
//! Language frontends provide the manifest and normalized evidence records;
//! Rust owns merging, attempt outcomes, attribution confidence, filtering and
//! every structural coverage verdict.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use supercov_contracts::{COVERAGE_MODEL_SCHEMA_VERSION, FrontendRunDeclaration};

use crate::coverage_analysis::{
    AnalysisError, BranchCoverage, CoverageCoreInput, CoverageSummary, DecisionCoverage,
    McdcVector, PointCoverage, PointKind, analyze_core, find_witnesses_for_conditions,
};
use crate::evidence_archive::EvidenceArchiveEntry;
use crate::interned::{FastMap, Id, Interner};

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
    /// Obligation IDs the frontend declined to measure exactly. They stay in
    /// the manifest so the report can say what was not measured, but they must
    /// never be counted as uncovered.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unmeasured: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope: Option<Value>,
    /// Statements in test code that carry an assertion marker. Test code is
    /// not measured, so these are no obligation; they say where a passing
    /// assertion stood.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub assertion_sites: Vec<PointMeta>,
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

/// One observed (side selected, result truthy) pair of a value-position logical expression.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LogicalVector {
    /// The right operand was evaluated (the left did not short-circuit).
    pub right: bool,
    /// The selected result was truthy.
    pub truthy: bool,
}

/// Distinct outcomes of one `logical-value` branch within a test; gives per-operand outcomes
/// once combined with the operator recorded in the manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LogicalSnapshot {
    pub id: String,
    pub vectors: Vec<LogicalVector>,
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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub logicals: Vec<LogicalSnapshot>,
    /// Every hit and decision vector of this snapshot is an explicit event of
    /// this phase. A frontend that observes a whole phase at once -- Python
    /// reads a phase's slot -- names the phase here instead of writing an
    /// event per observation, which on a 3,900-test run was three quarters
    /// of the evidence. The analysis reads it exactly as those events.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
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
    /// Made while the server handled a request that named no test -- a
    /// webhook a mock server sends -- numbered per request in its process.
    /// Only such work is credited by time: start-up and a test process's own
    /// module loading are not a request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request: Option<u64>,
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
    /// The labels the test carries, without a leading `@`: a Playwright tag
    /// or an `@word` in a title, a pytest marker, an RSpec or Cucumber tag, a
    /// JUnit `@Tag`. Read from what the runner already hands over, in its
    /// order, each once.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
}

impl Default for TestProvenance {
    fn default() -> Self {
        Self {
            runner: "unknown".into(),
            kind: "unknown".into(),
            project: None,
            source: "unknown".into(),
            tags: Vec::new(),
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
    /// How exactly this test's coverage could be credited to it.
    ///
    /// `exact` is the ordinary case and the default, so a frontend that has
    /// not been taught the difference keeps its behaviour. `run-wide` says the
    /// test ran and reached code, and that nothing can say which code: a Go
    /// test that called `t.Parallel()` stores into the same probe array as the
    /// tests running beside it.
    ///
    /// The distinction is the whole point of the field. A test that reached
    /// nothing and a test whose reach cannot be narrowed both have an empty
    /// hit set, and reporting the second as the first is a wrong number
    /// wearing the shape of a right one.
    #[serde(default = "default_attribution")]
    pub attribution: String,
    #[serde(default)]
    pub phases: Vec<CoveragePhase>,
    #[serde(default)]
    pub runtime: Vec<RuntimeSnapshot>,
    #[serde(default)]
    pub browser: Vec<RuntimeSnapshot>,
    #[serde(default)]
    pub server: Vec<ServerRecord>,
    /// When the attempt ran, on the runner's clock: its first and last
    /// moment, in milliseconds since the epoch. Server work that names no
    /// test is judged by it; a frontend that runs each test in a process of
    /// its own has no need of it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub span: Option<[i64; 2]>,
}

fn default_test_role() -> String {
    "test".into()
}

fn default_attribution() -> String {
    ATTRIBUTION_EXACT.into()
}

/// This test's coverage is its own: what it reached was credited to it.
pub const ATTRIBUTION_EXACT: &str = "exact";

/// This test ran, and what it reached was recorded against the run rather than
/// against it. Its own reach is unknown, and bounded above by the run's.
pub const ATTRIBUTION_RUN_WIDE: &str = "run-wide";

/// What this test is recorded as reaching is really its own, and is not all of
/// it: a lower bound, with the run's coverage as the upper one.
///
/// Ruby records a line against the first test that reaches it and never again,
/// which is what makes its collection cheap. Every later test that runs the
/// same line is recorded as having reached nothing there -- so a test's hits
/// prove what it ran and its silence proves nothing.
pub const ATTRIBUTION_PARTIAL: &str = "partial";

/// Exact, and credited besides with server work that no request named: work
/// an app server did while this test, and no other, was running -- a webhook
/// a mock server sends, a job the test set off. Credited by the time it ran.
///
/// What the test is recorded as reaching is still all its own work, so its
/// silence still means it did not run the code; the credited work may in
/// rare cases be another test's, finished late, which adds to this test's
/// reach and never hides any of it.
pub const ATTRIBUTION_BY_TIME: &str = "by-time";

/// Server work for a request that named no test -- a webhook a mock server
/// sends -- done while one test, and only one, was running is that test's,
/// credited by the time it ran. Where tests ran at once, or none was
/// running (start-up, between tests), it stays no test's, and is returned.
///
/// A test's time is from its first phase to the end of its last: what it did
/// is known to fall inside, and work after it is left alone rather than
/// guessed at.
fn credit_by_time(
    raw_results: &mut [RawTestResult],
    processes: Vec<Vec<ServerRecord>>,
) -> Vec<ServerRecord> {
    let windows = raw_results
        .iter()
        .enumerate()
        .filter(|(_, raw)| raw.role != "background")
        .filter_map(|(index, raw)| {
            let start = raw.phases.iter().map(|phase| phase.started_at_ms).min()?;
            let end = raw
                .phases
                .iter()
                .map(|phase| phase.ended_at_ms.unwrap_or(phase.started_at_ms))
                .max()?;
            let attempt = raw.scope.as_ref().map(|scope| scope.attempt_id.clone());
            Some((index, start, end, attempt))
        })
        .collect::<Vec<_>>();
    let mut work = Vec::new();
    for records in processes {
        // The attempts this process served a request naming, and among those
        // alone, where it served any: with a server per worker, the worker's
        // test, whatever the other workers ran meanwhile. A process that
        // served none -- one only an unmeasured mock calls -- is judged
        // against every test running.
        let served = records
            .iter()
            .filter(|record| record.record_type == "served")
            .filter_map(|record| record.id.clone())
            .collect::<std::collections::HashSet<_>>();
        let candidates = windows
            .iter()
            .filter(|(_, _, _, attempt)| {
                served.is_empty() || attempt.as_ref().is_some_and(|id| served.contains(id))
            })
            .collect::<Vec<_>>();
        for record in records
            .into_iter()
            .filter(|record| record.record_type != "served")
        {
            let owner = record.request.and(record.timestamp_ms).and_then(|at| {
                let mut running = candidates
                    .iter()
                    .filter(|(_, start, end, _)| *start <= at && at <= *end);
                let only = running.next()?;
                running.next().is_none().then_some(only.0)
            });
            work.push((owner, record));
        }
    }
    let mut unattributed = Vec::new();
    // A request's work appears once per request; what stays no test's is
    // counted once.
    let mut kept = std::collections::HashSet::new();
    for (owner, record) in work {
        match owner {
            Some(index) => {
                let raw = &mut raw_results[index];
                raw.attribution = least_exact(&raw.attribution, ATTRIBUTION_BY_TIME);
                raw.server.push(record);
            }
            None => {
                let mut record = record;
                record.request = None;
                if kept.insert(serde_json::to_vec(&record).unwrap_or_default()) {
                    unattributed.push(record);
                }
            }
        }
    }
    unattributed
}

/// Of two attributions for one test, the one that claims less.
fn least_exact(left: &str, right: &str) -> String {
    let rank = |attribution: &str| match attribution {
        ATTRIBUTION_RUN_WIDE => 3,
        ATTRIBUTION_PARTIAL => 2,
        ATTRIBUTION_BY_TIME => 1,
        _ => 0,
    };
    if rank(right) > rank(left) {
        right.to_owned()
    } else {
        left.to_owned()
    }
}

/// Whether what this test is recorded as reaching is its own.
///
/// True for `exact` and for `partial`: both record real coverage of this
/// test's, and a union over them is a union of things that happened. False
/// only where the coverage went to the run instead, and counting the test in a
/// percentage would divide by a test that contributes nothing.
pub fn coverage_is_its_own(attribution: &str) -> bool {
    attribution != ATTRIBUTION_RUN_WIDE
}

/// Whether this test's silence means it did not run the code.
///
/// Only `exact` earns that. Under `partial` a test that is recorded as
/// reaching nothing may have run the same lines as the test that was credited
/// with them, and under `run-wide` nothing was recorded at all -- so reading
/// an absence as proof is how a change to code a test exercised comes back as
/// "this test is unaffected".
pub fn coverage_is_complete(attribution: &str) -> bool {
    attribution == ATTRIBUTION_EXACT || attribution == ATTRIBUTION_BY_TIME || attribution.is_empty()
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
    pub tests: Vec<Id>,
    pub runners: Vec<String>,
    pub kinds: Vec<String>,
    pub e2e: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VectorObservation {
    pub vector: McdcVector,
    pub tests: Vec<Id>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub phases: Vec<Id>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub explicit_phases: Vec<Id>,
    pub confidence: CoverageConfidence,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConditionResult {
    pub index: usize,
    pub source: String,
    pub covered: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub witness: Option<[McdcVector; 2]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub witness_tests: Option<[Vec<Id>; 2]>,
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
    pub tests: Vec<Id>,
    pub confidence: CoverageConfidence,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PointResult {
    pub meta: PointMeta,
    pub covered: bool,
    #[serde(skip)]
    pub measured: bool,
    pub tests: Vec<Id>,
    pub phases: Vec<Id>,
    pub confidence: CoverageConfidence,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AlternativeResult {
    pub id: String,
    pub label: String,
    pub covered: bool,
    pub tests: Vec<Id>,
    pub phases: Vec<Id>,
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
    pub file: Id,
    pub line: usize,
}

/// One line's share of the points that sit on it, while the view is built.
#[derive(Debug, Default)]
struct LineAggregate {
    covered: bool,
    measured: bool,
    /// Numbers of the relation each is drawn from, in no order.
    tests: Vec<u32>,
    phases: Vec<u32>,
    explicit_phases: Vec<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LineResult {
    pub file: String,
    pub line: usize,
    pub covered: bool,
    /// False when every obligation on this line was declined, which keeps the
    /// line addressable while leaving it out of the covered/total counts. Not
    /// serialized: it decides a total, it is not a fact about the line worth
    /// publishing, and the limitation records already say why.
    #[serde(skip)]
    pub measured: bool,
    pub tests: Vec<Id>,
    pub runners: Vec<String>,
    pub kinds: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exclusive_kind: Option<String>,
    pub phases: Vec<Id>,
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
    /// `exact`, or `run-wide` when the test ran but nothing can say what it
    /// reached. An empty `hits` means "reached nothing" only under `exact`.
    pub attribution: String,
    pub hits: Vec<Id>,
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
    pub test: Id,
    pub hits: Vec<Id>,
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
    /// How many of `tests` have coverage of their own. The summary below is
    /// computed over exactly these: a test whose reach nothing can narrow
    /// contributes no hits, and counting it would report a suite that covers
    /// everything as covering nothing.
    pub attributed: usize,
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
    /// Processes that ran the workspace without Supercov's settings -- in a
    /// container or VM it was mounted into -- and set themselves up from it.
    /// Their coverage counts for the run and is credited to no test.
    pub guest_processes: usize,
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
    GuestProcess {
        at: String,
        pid: u32,
        ppid: u32,
        guest_root: String,
    },
}

impl ExecutionTraceEvent {
    fn kind(&self) -> &'static str {
        match self {
            Self::Process { .. } => "process",
            Self::ChildLaunch { .. } => "child-launch",
            Self::RemoteLaunch { .. } => "remote-launch",
            Self::WorkspaceCapability { .. } => "workspace-capability",
            Self::GuestProcess { .. } => "guest-process",
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
    /// Only the coverage of the tests this names, when it names any.
    #[serde(skip)]
    pub test_filter: Option<TestFilter>,
}

/// Tests picked by a tag, a title or a file, for the coverage of those tests
/// alone, read from a run already measured. Any value of one kind of
/// criterion picks a test; every kind given must pick it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TestFilter {
    /// Tags, with or without the `@`, in any case.
    pub tags: Vec<String>,
    /// Text the test's full name contains, in any case.
    pub titles: Vec<String>,
    /// Test files, or directories that hold them, from the project root.
    pub files: Vec<String>,
    /// A test kind and a runner, for a reader with no index to project them:
    /// a query's index already holds coverage by kind and by runner.
    pub kind: Option<String>,
    pub runner: Option<String>,
}

/// Whether a test passes one criterion's values: one of the values it names
/// matches, and none it excludes with a leading `!` does (`--tag '!api'`, as
/// ripgrep's `-g '!*.js'`). With only exclusions, every other test passes.
fn picks(values: &[String], matches: impl Fn(&str) -> bool) -> bool {
    let (excluded, wanted): (Vec<&str>, Vec<&str>) = values
        .iter()
        .map(String::as_str)
        .partition(|value| value.starts_with('!'));
    (wanted.is_empty() || wanted.iter().any(|value| matches(value)))
        && !excluded.iter().any(|value| matches(&value[1..]))
}

impl TestFilter {
    pub fn is_empty(&self) -> bool {
        self.tags.is_empty()
            && self.titles.is_empty()
            && self.files.is_empty()
            && self.kind.is_none()
            && self.runner.is_none()
    }

    pub fn matches(
        &self,
        name: &str,
        title: Option<&str>,
        file: Option<&str>,
        provenance: &TestProvenance,
    ) -> bool {
        let kinded = self
            .kind
            .as_ref()
            .is_none_or(|kind| provenance.kind == *kind)
            && self
                .runner
                .as_ref()
                .is_none_or(|runner| provenance.runner == *runner);
        let tags = &provenance.tags;
        let tag = |value: &str| value.trim_start_matches('@').to_lowercase();
        let tagged = picks(&self.tags, |wanted| {
            tags.iter().any(|carried| tag(carried) == tag(wanted))
        });
        // The full name, and the title where the name is only a position
        // (`spec/cart_spec.rb[1:2]`, with "Cart adds an item" its title).
        let name = name.to_lowercase();
        let title = title.map(str::to_lowercase).unwrap_or_default();
        let titled = picks(&self.titles, |wanted| {
            let wanted = wanted.to_lowercase();
            name.contains(&wanted) || title.contains(&wanted)
        });
        let file = file.map(|file| file.trim_start_matches("./"));
        let filed = picks(&self.files, |wanted| {
            let wanted = wanted.trim_start_matches("./").trim_end_matches('/');
            file.is_some_and(|file| {
                file == wanted
                    || file
                        .strip_prefix(wanted)
                        .is_some_and(|rest| rest.starts_with('/'))
            })
        });
        kinded && tagged && titled && filed
    }

    /// The filter in words, as `--tag api --title checkout`.
    pub fn describe(&self) -> String {
        let mut words = Vec::new();
        for (flag, value) in [("--kind", &self.kind), ("--runner", &self.runner)] {
            if let Some(value) = value {
                words.push(format!("{flag} {value}"));
            }
        }
        for (flag, values) in [
            ("--tag", &self.tags),
            ("--title", &self.titles),
            ("--test-file", &self.files),
        ] {
            for value in values {
                words.push(format!("{flag} {value}"));
            }
        }
        words.join(" ")
    }

    /// The tests the filter picks, with the setup that ran in their files;
    /// what ran outside any test is no picked test's.
    fn select(&self, raw_results: Vec<RawTestResult>) -> Result<Vec<RawTestResult>, ReportError> {
        let picked = |raw: &RawTestResult| {
            raw.role == "test"
                && self.matches(
                    &raw.test,
                    raw.title.as_deref(),
                    raw.test_file.as_deref(),
                    &raw.provenance,
                )
        };
        let files = raw_results
            .iter()
            .filter(|raw| picked(raw))
            .filter_map(|raw| raw.test_file.clone())
            .collect::<BTreeSet<_>>();
        if !raw_results.iter().any(picked) {
            return Err(ReportError::NoTestMatches(self.describe()));
        }
        Ok(raw_results
            .into_iter()
            .filter(|raw| {
                picked(raw)
                    || (raw.role == "setup"
                        && raw
                            .test_file
                            .as_ref()
                            .is_some_and(|file| files.contains(file)))
            })
            .collect())
    }
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
    /// No test of the run is one the filter names.
    NoTestMatches(String),
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
    /// Numbers of the view's test, phase and explicit phase relations, as
    /// they arrive: sorted and made unique when the view is written.
    tests: Vec<u32>,
    phases: Vec<u32>,
    explicit_phases: Vec<u32>,
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
    attribution: String,
    /// Hit numbers as they arrive, repeats included: sorted and made unique
    /// once, when the view is written.
    hits: Vec<usize>,
    decisions: BTreeMap<String, OrderedVectors>,
}

#[derive(Clone)]
struct MutablePhase {
    phase: CoveragePhase,
    test: Id,
    hits: Vec<usize>,
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
    // A later record does not pass an attempt an earlier one failed: the same
    // tests run twice under one identity fail if either execution did.
    let status =
        if raw_status == "unknown" || previous.is_some_and(|attempt| attempt.status == "failed") {
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

/// Borrowed rather than copied: a view only reads its results, and copying
/// every result of a 3,900-test run to build the passing view cost a sixth of
/// the analysis.
pub fn passing_coverage_results(raw_results: &[RawTestResult]) -> Vec<&RawTestResult> {
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
        .collect()
}

pub fn failed_coverage_results(raw_results: &[RawTestResult]) -> Vec<&RawTestResult> {
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

/// One file's counts, derived by the same function that produced the run's
/// totals.
///
/// Every feature that judges a run -- a threshold gate, a changed-line check,
/// an export, an HTML report -- has to agree with the summary it sits beside.
/// Re-deriving a file's denominator independently is how a gate and a report
/// come to disagree about the same run, so this filters the same results and
/// calls the same core.
pub fn coverage_summary_for_file(
    view: &CoverageView,
    file: &str,
) -> Result<CoverageSummary, ReportError> {
    let decisions = view
        .decisions
        .iter()
        .filter(|decision| decision.meta.file == file)
        .cloned()
        .collect::<Vec<_>>();
    let points = view
        .points
        .iter()
        .filter(|point| point.meta.file == file)
        .cloned()
        .collect::<Vec<_>>();
    let branches = view
        .branches
        .iter()
        .filter(|branch| branch.meta.file == file)
        .cloned()
        .collect::<Vec<_>>();
    let lines = view
        .lines
        .iter()
        .filter(|line| line.file == file)
        .cloned()
        .collect::<Vec<_>>();
    summary_for_results(&decisions, &points, &branches, &lines, None)
}

/// The entries whose coverage a kind's row counts: every test of the view,
/// and the setup scopes of a kind that has tests. A kind's row counts both.
/// Leaving a test file's own setup out of this put one line in the `unit` row
/// and in `no test`, and the rows added up to more than the run's total. A
/// setup scope of a kind with no test has no row: a server started there is
/// in the total only, and that is what `no test` is for.
pub(crate) fn entries_in_kind_rows(view: &CoverageView) -> BTreeSet<String> {
    let with_tests = view
        .tests
        .iter()
        .filter(|test| test.role == "test")
        .map(|test| test.provenance.kind.as_str())
        .collect::<BTreeSet<_>>();
    view.tests
        .iter()
        .filter(|test| {
            coverage_is_its_own(&test.attribution)
                && (test.role == "test"
                    || (test.role == "setup" && with_tests.contains(test.provenance.kind.as_str())))
        })
        .map(|test| test.id.clone())
        .collect()
}

/// What the tests of a view cover between them, leaving out what ran while
/// no test was running: start-up, module loading, work between tests.
pub(crate) fn summary_for_tests(view: &CoverageView) -> Result<CoverageSummary, ReportError> {
    let selected = entries_in_kind_rows(view);
    summary_for_results(
        &view.decisions,
        &view.points,
        &view.branches,
        &view.lines,
        Some(&selected),
    )
}

fn summary_for_results(
    decisions: &[DecisionResult],
    points: &[PointResult],
    branches: &[BranchResult],
    lines: &[LineResult],
    test_ids: Option<&BTreeSet<String>>,
) -> Result<CoverageSummary, ReportError> {
    // Hashed once: every obligation asks it about each of its tests.
    let test_ids = test_ids.map(|ids| {
        ids.iter()
            .map(String::as_str)
            .collect::<crate::interned::FastSet<&str>>()
    });
    let includes = |tests: &[Id], covered: bool| {
        test_ids.as_ref().map_or(covered, |selected| {
            tests.iter().any(|test| selected.contains(test.as_str()))
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
        // A line every frontend declined carries no coverage question, so it
        // is absent from the total rather than counted as uncovered.
        lines: lines
            .iter()
            .filter(|line| line.measured)
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

/// Whether a manifest limitation blocks measurement of the denominator, as
/// opposed to declaring a boundary of it. Absent means blocking.
pub fn blocking_limitation(limitation: &Value) -> bool {
    limitation
        .get("blocking")
        .and_then(Value::as_bool)
        .unwrap_or(true)
}

fn hash_slot<'m, V: Default, S: std::hash::BuildHasher>(
    map: &'m mut HashMap<String, V, S>,
    key: &str,
) -> &'m mut V {
    if !map.contains_key(key) {
        map.insert(key.to_owned(), V::default());
    }
    map.get_mut(key).expect("slot was inserted")
}

fn tree_slot<'m, V: Default>(map: &'m mut BTreeMap<String, V>, key: &str) -> &'m mut V {
    if !map.contains_key(key) {
        map.insert(key.to_owned(), V::default());
    }
    map.get_mut(key).expect("slot was inserted")
}

/// The phase an event belongs to: its own, or the one it falls in by time.
fn correlate_event<'a>(
    event: &'a RuntimeEvent,
    ordered_phases: &'a [CoveragePhase],
) -> Option<&'a str> {
    event
        .phase_id
        .as_deref()
        .or_else(|| phase_at(ordered_phases, event.timestamp_ms))
}

/// The phase an event with none of its own falls in: the last to start at or
/// before it. Times are whole milliseconds, and one phase often ends in the
/// millisecond the next begins: `page.goto` resolves and the test's `expect`
/// starts. What a page ran while it loaded, stamped with that millisecond,
/// went to the `expect`, and now and then all of it did. Work a phase waited
/// for comes before the phase ends, so in that millisecond it is the ending
/// phase's.
fn phase_at(ordered_phases: &[CoveragePhase], timestamp_ms: i64) -> Option<&str> {
    let started = ordered_phases
        .iter()
        .take_while(|phase| phase.started_at_ms <= timestamp_ms)
        .count();
    let last = ordered_phases[..started].last()?;
    let handed_over = last.started_at_ms == timestamp_ms
        && started >= 2
        && ordered_phases[started - 2].ended_at_ms == Some(timestamp_ms)
        && ordered_phases[started - 2].started_at_ms < timestamp_ms;
    Some(
        if handed_over {
            &ordered_phases[started - 2]
        } else {
            last
        }
        .id
        .as_str(),
    )
}

/// One lookup per hit while ingesting evidence. All hit relations share these
/// dense numbers, avoiding repeated string hashing and tree comparisons. Even
/// IDs absent from the manifest are retained; output restores lexical ordering.
#[derive(Default)]
struct HitIds {
    names: Vec<Id>,
    numbers: FastMap<String, usize>,
}

impl HitIds {
    fn number(&mut self, name: &str, ids: &mut Interner) -> usize {
        if let Some(number) = self.numbers.get(name) {
            return *number;
        }
        let number = self.names.len();
        self.names.push(ids.id(name));
        self.numbers.insert(name.to_owned(), number);
        number
    }

    /// Each hit number's position in id order: hits sort by these numbers
    /// instead of by comparing their text, per test and per phase.
    fn ranks(&self) -> Vec<u32> {
        let mut order = (0..self.names.len()).collect::<Vec<_>>();
        order.sort_unstable_by(|left, right| self.names[*left].cmp(&self.names[*right]));
        let mut rank = vec![0_u32; self.names.len()];
        for (position, number) in order.into_iter().enumerate() {
            rank[number] = position as u32;
        }
        rank
    }

    /// `sorted`, by precomputed rank.
    fn sorted_by(&self, mut numbers: Vec<usize>, rank: &[u32]) -> Vec<Id> {
        numbers.sort_unstable_by_key(|number| rank[*number]);
        numbers.dedup();
        numbers
            .into_iter()
            .map(|number| self.names[number].clone())
            .collect()
    }

    #[cfg(test)]
    fn sorted(&self, numbers: BTreeSet<usize>) -> Vec<Id> {
        let mut names = numbers
            .into_iter()
            .map(|n| self.names[n].clone())
            .collect::<Vec<_>>();
        names.sort_unstable();
        names
    }
}

/// Which tests or phases reached each obligation. Both sides are numbered
/// while ingesting evidence, then restored to sorted ID sets for the report.
#[derive(Default)]
struct References {
    names: Vec<Id>,
    numbers: FastMap<Id, u32>,
    last: Option<u32>,
    by_obligation: Vec<Vec<u32>>,
}

impl References {
    /// The number standing for a test or phase id. The same id usually
    /// arrives many times in a row, so the last one is checked first.
    fn number(&mut self, name: &str, ids: &mut Interner) -> u32 {
        if let Some(number) = self.last
            && self.names[number as usize] == name
        {
            return number;
        }
        let number = match self.numbers.get(name) {
            Some(number) => *number,
            None => {
                let number = self.names.len() as u32;
                let id = ids.id(name);
                self.names.push(id.clone());
                self.numbers.insert(id, number);
                number
            }
        };
        self.last = Some(number);
        number
    }

    fn add(&mut self, obligation: usize, name: &str, ids: &mut Interner) {
        let number = self.number(name, ids);
        if self.by_obligation.len() <= obligation {
            self.by_obligation.resize_with(obligation + 1, Vec::new);
        }
        let reached = &mut self.by_obligation[obligation];
        if reached.last() != Some(&number) {
            reached.push(number);
        }
    }

    fn into_relation(self, hits: &HitIds) -> Relation {
        let mut order = (0..self.names.len()).collect::<Vec<_>>();
        order.sort_unstable_by(|left, right| self.names[*left].cmp(&self.names[*right]));
        let mut rank = vec![0_u32; self.names.len()];
        for (position, number) in order.into_iter().enumerate() {
            rank[number] = position as u32;
        }
        let mut relation = Relation {
            names: self.names,
            rank,
            by_hit: HashMap::new(),
            none: Reached::default(),
        };
        for (obligation, reached) in self.by_obligation.into_iter().enumerate() {
            if !reached.is_empty() {
                let reached = relation.reached(reached);
                relation
                    .by_hit
                    .insert(hits.names[obligation].to_string(), reached);
            }
        }
        relation
    }
}

/// A `References` restored for the report: what reached each obligation,
/// and the table its numbers index.
struct Relation {
    names: Vec<Id>,
    /// Each number's position in id order.
    rank: Vec<u32>,
    by_hit: HashMap<String, Reached>,
    none: Reached,
}

impl Relation {
    /// What reached obligation `id`: nothing, for one no test reached.
    fn of(&self, id: &str) -> &Reached {
        self.by_hit.get(id).unwrap_or(&self.none)
    }

    /// Numbers of this relation as the ids they stand for, in id order and
    /// once each: the set the report writes.
    fn reached(&self, mut numbers: Vec<u32>) -> Reached {
        numbers.sort_unstable_by_key(|number| self.rank[*number as usize]);
        numbers.dedup();
        Reached {
            names: numbers
                .iter()
                .map(|number| self.names[*number as usize].clone())
                .collect(),
            numbers,
        }
    }
}

/// Which tests or phases reached one obligation: their ids in id order, and
/// beside each the number its relation gave it.
#[derive(Default)]
struct Reached {
    names: Vec<Id>,
    numbers: Vec<u32>,
}

/// What confidence reads of one test.
struct TestTraits<'t> {
    setup: bool,
    background: bool,
    runner: &'t str,
    kind: &'t str,
}

impl<'t> TestTraits<'t> {
    fn of(test: &'t MutableTest) -> Self {
        Self {
            setup: test.role == "setup",
            background: test.role == "background",
            runner: &test.provenance.runner,
            kind: &test.provenance.kind,
        }
    }
}

/// How a point, alternative, line or vector was reached, from the tests
/// and phases that reached it: read from per-number tables, since a run
/// mentions its tests millions of times and a lookup per mention hashed an
/// id each time.
fn confidence_from(
    tests: &Reached,
    phases: &Reached,
    explicit: &Reached,
    test_traits: &[Option<TestTraits<'_>>],
    phase_kinds: &[Option<&str>],
    explicit_kinds: &[Option<&str>],
) -> CoverageConfidence {
    let mut has_test = false;
    let mut setup_roles = true;
    let mut background_roles = true;
    let mut runners = BTreeSet::new();
    let mut kinds = BTreeSet::new();
    for number in &tests.numbers {
        if let Some(test) = &test_traits[*number as usize] {
            has_test = true;
            setup_roles &= test.setup;
            background_roles &= test.background;
            runners.insert(test.runner);
            kinds.insert(test.kind);
        }
    }
    let mut has_phase = false;
    let mut setup_phases = true;
    let mut background_phases = true;
    for number in &phases.numbers {
        if let Some(kind) = phase_kinds[*number as usize] {
            has_phase = true;
            setup_phases &= kind == "setup";
            background_phases &= kind == "background";
        }
    }
    let has_action = explicit
        .numbers
        .iter()
        .any(|number| explicit_kinds[*number as usize] == Some("action"));
    let level = if tests.names.is_empty() {
        "unexecuted"
    } else if has_action {
        "action"
    } else {
        "executed"
    };
    CoverageConfidence {
        level: level.into(),
        setup_only: if has_phase {
            setup_phases
        } else {
            has_test && setup_roles
        },
        background_only: if has_phase {
            background_phases
        } else {
            has_test && background_roles
        },
        tests: tests.names.clone(),
        runners: runners.into_iter().map(str::to_owned).collect(),
        e2e: kinds.contains("e2e"),
        kinds: kinds.into_iter().map(str::to_owned).collect(),
    }
}

pub fn create_coverage_view(
    manifest: &CoverageManifest,
    raw_results: &[RawTestResult],
    generated_at: &str,
) -> Result<CoverageView, ReportError> {
    create_coverage_view_with_model(
        manifest,
        &raw_results.iter().collect::<Vec<_>>(),
        generated_at,
        &javascript_coverage_model(),
        &mut Interner::default(),
    )
}

fn create_coverage_view_with_model(
    manifest: &CoverageManifest,
    raw_results: &[&RawTestResult],
    generated_at: &str,
    coverage_model: &CoverageModelDeclaration,
    ids: &mut Interner,
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
    let mut vectors_by_decision = FastMap::<String, Vec<MutableObservation>>::default();
    let mut vector_indexes = FastMap::<String, FastMap<String, usize>>::default();
    // Numbers of `tests_by_hit`'s tests, as they arrive.
    let mut tests_by_decision = FastMap::<String, Vec<u32>>::default();
    let mut tests_by_hit = References::default();
    let mut hit_ids = HitIds::default();
    let mut tests_by_id = FastMap::<Id, MutableTest>::default();
    let mut test_order = Vec::<Id>::new();
    let mut phases_by_id = FastMap::<Id, MutablePhase>::default();
    let mut phases_by_hit = References::default();
    let mut explicit_phases_by_hit = References::default();

    for raw in raw_results {
        let id = ids.id(raw_test_id(raw));
        if !tests_by_id.contains_key(&id) {
            test_order.push(id.clone());
            tests_by_id.insert(
                id.clone(),
                MutableTest {
                    id: id.to_string(),
                    name: raw.test.clone(),
                    file: raw.test_file.clone(),
                    title: raw.title.clone(),
                    retries: raw.retry.into_iter().collect(),
                    attempts: BTreeMap::new(),
                    unstarted: raw.status.as_deref() == Some("unstarted"),
                    runner_reported_flaky: raw.flaky,
                    provenance: raw.provenance.clone(),
                    role: raw.role.clone(),
                    attribution: raw.attribution.clone(),
                    hits: Vec::new(),
                    decisions: BTreeMap::new(),
                },
            );
        }
        let test = tests_by_id.get_mut(&id).expect("test was inserted");
        // A test's records can disagree about how exactly its coverage is its
        // own, and the least exact one is true of the whole: Playwright's
        // outcome record says nothing about coverage and often arrives first,
        // before the record that says a worker could not be read.
        test.attribution = least_exact(&test.attribution, &raw.attribution);
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
                ids.id(&phase.id),
                MutablePhase {
                    phase: phase.clone(),
                    test: id.clone(),
                    hits: Vec::new(),
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
            event
                .phase_id
                .clone()
                .or_else(|| phase_at(&ordered_phases, event.timestamp_ms).map(str::to_owned))
        };

        let snapshots = raw
            .runtime
            .iter()
            .map(|snapshot| (snapshot, false))
            .chain(raw.browser.iter().map(|snapshot| (snapshot, true)));
        for (snapshot, browser) in snapshots {
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
                    let indexes = hash_slot(&mut vector_indexes, &decision.meta.id);
                    let observations = hash_slot(&mut vectors_by_decision, &decision.meta.id);
                    let observation_index = *indexes.entry(key).or_insert_with(|| {
                        observations.push(MutableObservation {
                            vector: vector.clone(),
                            tests: Vec::new(),
                            phases: Vec::new(),
                            explicit_phases: Vec::new(),
                        });
                        observations.len() - 1
                    });
                    observations[observation_index]
                        .tests
                        .push(tests_by_hit.number(&id, ids));
                    tree_slot(
                        &mut tests_by_id.get_mut(&id).expect("registered test").decisions,
                        &decision.meta.id,
                    )
                    .insert(vector);
                }
                if !decision.vectors.is_empty() {
                    hash_slot(&mut tests_by_decision, &decision.meta.id)
                        .push(tests_by_hit.number(&id, ids));
                }
            }
            let test_hits = &mut tests_by_id.get_mut(&id).expect("registered test").hits;
            for hit in &snapshot.hits {
                let hit = hit_ids.number(hit, ids);
                tests_by_hit.add(hit, &id, ids);
                test_hits.push(hit);
            }
            for event in &snapshot.events {
                let explicit = event.phase_id.is_some();
                let Some(phase_id) = correlate_event(event, &ordered_phases) else {
                    continue;
                };
                let Some(phase) = phases_by_id.get_mut(phase_id) else {
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
                    let hit = hit_ids.number(&event.id, ids);
                    phase.hits.push(hit);
                    phases_by_hit.add(hit, phase_id, ids);
                    if explicit {
                        explicit_phases_by_hit.add(hit, phase_id, ids);
                    }
                } else if event.event_type == "decision" {
                    let vector = event
                        .vector
                        .as_ref()
                        .ok_or_else(|| ReportError::InvalidEvent(event.id.clone()))?;
                    tree_slot(&mut phase.decisions, &event.id).insert(vector);
                    if let Some(index) = vector_indexes
                        .get(&event.id)
                        .and_then(|indexes| indexes.get(&vector_key(vector)))
                        .copied()
                        && let Some(observation) = vectors_by_decision
                            .get_mut(&event.id)
                            .and_then(|observations| observations.get_mut(index))
                    {
                        observation.phases.push(phases_by_hit.number(phase_id, ids));
                        if explicit {
                            observation
                                .explicit_phases
                                .push(explicit_phases_by_hit.number(phase_id, ids));
                        }
                    }
                } else {
                    return Err(ReportError::InvalidEvent(event.event_type.clone()));
                }
            }
            // A snapshot that names its phase: each hit and each decision
            // vector is an explicit event of it, counted and related exactly
            // as the event the frontend no longer writes would have been.
            if let Some(phase_id) = snapshot.phase_id.as_deref()
                && let Some(phase) = phases_by_id.get_mut(phase_id)
            {
                let observed = snapshot.hits.len()
                    + snapshot
                        .decisions
                        .iter()
                        .map(|decision| decision.vectors.len())
                        .sum::<usize>();
                if browser {
                    phase.browser_events += observed;
                    phase.explicit_browser_events += observed;
                } else {
                    phase.server_events += observed;
                    phase.explicit_server_events += observed;
                }
                phase.explicit_events += observed;
                for hit in &snapshot.hits {
                    let hit = hit_ids.number(hit, ids);
                    phase.hits.push(hit);
                    phases_by_hit.add(hit, phase_id, ids);
                    explicit_phases_by_hit.add(hit, phase_id, ids);
                }
                for decision in &snapshot.decisions {
                    let id = &decision.meta.id;
                    for vector in &decision.vectors {
                        tree_slot(&mut phase.decisions, id).insert(vector);
                        if let Some(index) = vector_indexes
                            .get(id)
                            .and_then(|indexes| indexes.get(&vector_key(vector)))
                            .copied()
                            && let Some(observation) = vectors_by_decision
                                .get_mut(id)
                                .and_then(|observations| observations.get_mut(index))
                        {
                            observation.phases.push(phases_by_hit.number(phase_id, ids));
                            observation
                                .explicit_phases
                                .push(explicit_phases_by_hit.number(phase_id, ids));
                        }
                    }
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
                        tests: Vec::new(),
                        phases: Vec::new(),
                        explicit_phases: Vec::new(),
                    });
                    observations.len() - 1
                });
                observations[index]
                    .tests
                    .push(tests_by_hit.number(&id, ids));
                tests_by_id
                    .get_mut(&id)
                    .expect("registered test")
                    .decisions
                    .entry(meta.id.clone())
                    .or_default()
                    .insert(vector);
                hash_slot(&mut tests_by_decision, &meta.id).push(tests_by_hit.number(&id, ids));
                (meta.id.clone(), Some(vector.clone()))
            } else if record.record_type == "hit" {
                let hit = record
                    .id
                    .as_ref()
                    .ok_or_else(|| ReportError::InvalidServerRecord("missing hit id".into()))?;
                let number = hit_ids.number(hit, ids);
                tests_by_hit.add(number, &id, ids);
                tests_by_id
                    .get_mut(&id)
                    .expect("registered test")
                    .hits
                    .push(number);
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
            let Some(phase) = phases_by_id.get_mut(phase_id.as_str()) else {
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
                let hit = hit_ids.number(&event.id, ids);
                phase.hits.push(hit);
                phases_by_hit.add(hit, &phase_id, ids);
                if explicit {
                    explicit_phases_by_hit.add(hit, &phase_id, ids);
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
                    observation
                        .phases
                        .push(phases_by_hit.number(&phase_id, ids));
                    if explicit {
                        observation
                            .explicit_phases
                            .push(explicit_phases_by_hit.number(&phase_id, ids));
                    }
                }
            }
        }
    }

    decision_metadata.sort_by(|left, right| {
        left.file
            .cmp(&right.file)
            .then(left.line.cmp(&right.line))
            .then(left.column.cmp(&right.column))
    });
    let tests_by_hit = tests_by_hit.into_relation(&hit_ids);
    let phases_by_hit = phases_by_hit.into_relation(&hit_ids);
    let explicit_phases_by_hit = explicit_phases_by_hit.into_relation(&hit_ids);
    // A frontend may decline one outcome of a branch (a block that can never
    // be entered, a rescue that can never be reached) or the whole branch;
    // a branch whose every outcome is declined is declined itself.
    let mut declined = manifest.unmeasured.iter().collect::<BTreeSet<_>>();
    for branch in &manifest.branches {
        if !branch.alternatives.is_empty()
            && branch
                .alternatives
                .iter()
                .all(|alternative| declined.contains(&alternative.id))
        {
            declined.insert(&branch.id);
        }
    }
    // What confidence asks of each test and phase a relation numbers, looked
    // up once per number rather than once per mention: a run's points and
    // lines mention tests millions of times, and each lookup hashed an id.
    let test_traits = tests_by_hit
        .names
        .iter()
        .map(|name| tests_by_id.get(name).map(TestTraits::of))
        .collect::<Vec<_>>();
    let phase_kind = |relation: &Relation| {
        relation
            .names
            .iter()
            .map(|name| {
                phases_by_id
                    .get(name)
                    .map(|phase| phase.phase.kind.as_str())
            })
            .collect::<Vec<_>>()
    };
    let phase_kinds = phase_kind(&phases_by_hit);
    let explicit_kinds = phase_kind(&explicit_phases_by_hit);
    let confidence = |tests: &Reached, phases: &Reached, explicit: &Reached| {
        confidence_from(
            tests,
            phases,
            explicit,
            &test_traits,
            &phase_kinds,
            &explicit_kinds,
        )
    };
    // A decision's two outcomes are branch outcomes. Python, Ruby and Rust
    // declare them as a branch of their own, `<decision>:outcome`; for a
    // decision declared without one, the branch is made here from the
    // decision's vectors, so every language counts and lists outcomes the
    // same way and no decision is counted twice.
    let declared_branches = manifest
        .branches
        .iter()
        .map(|branch| branch.id.as_str())
        .collect::<HashSet<_>>();
    let mut outcome_branches = Vec::new();
    let mut decisions = Vec::with_capacity(decision_metadata.len());
    for meta in decision_metadata {
        let mutable = vectors_by_decision.remove(&meta.id).unwrap_or_default();
        let outcome_id = format!("{}:outcome", meta.id);
        if !declared_branches.contains(outcome_id.as_str()) {
            let alternatives = [true, false]
                .into_iter()
                .map(|outcome| {
                    let (mut tests, mut phases, mut explicit) =
                        (Vec::new(), Vec::new(), Vec::new());
                    for observation in mutable
                        .iter()
                        .filter(|observation| observation.vector.outcome == outcome)
                    {
                        tests.extend(&observation.tests);
                        phases.extend(&observation.phases);
                        explicit.extend(&observation.explicit_phases);
                    }
                    let tests = tests_by_hit.reached(tests);
                    let phases = phases_by_hit.reached(phases);
                    let explicit = explicit_phases_by_hit.reached(explicit);
                    AlternativeResult {
                        id: format!("{outcome_id}:{outcome}"),
                        label: outcome.to_string(),
                        covered: !tests.names.is_empty(),
                        confidence: confidence(&tests, &phases, &explicit),
                        tests: tests.names,
                        phases: phases.names,
                    }
                })
                .collect::<Vec<_>>();
            outcome_branches.push(BranchResult {
                covered: alternatives.iter().all(|alternative| alternative.covered),
                meta: BranchMeta {
                    id: outcome_id,
                    kind: meta.kind.clone(),
                    file: meta.file.clone(),
                    line: meta.line,
                    column: meta.column,
                    source: meta.source.clone(),
                    alternatives: alternatives
                        .iter()
                        .map(|alternative| BranchAlternativeMeta {
                            id: alternative.id.clone(),
                            label: alternative.label.clone(),
                        })
                        .collect(),
                },
                alternatives,
            });
        }
        let mut observations = Vec::with_capacity(mutable.len());
        let mut observed_phases = Vec::new();
        let mut observed_explicit = Vec::new();
        for observation in mutable {
            let tests = tests_by_hit.reached(observation.tests);
            let phases = phases_by_hit.reached(observation.phases);
            let explicit = explicit_phases_by_hit.reached(observation.explicit_phases);
            observed_phases.extend(&phases.numbers);
            observed_explicit.extend(&explicit.numbers);
            observations.push(VectorObservation {
                vector: observation.vector,
                confidence: confidence(&tests, &phases, &explicit),
                tests: tests.names,
                phases: phases.names,
                explicit_phases: explicit.names,
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
            conditions.push(ConditionResult {
                index,
                source: source.clone(),
                covered: witness.is_some(),
                witness,
                witness_tests,
            });
        }
        let decision_tests =
            tests_by_hit.reached(tests_by_decision.remove(&meta.id).unwrap_or_default());
        let decision_confidence = confidence(
            &decision_tests,
            &phases_by_hit.reached(observed_phases),
            &explicit_phases_by_hit.reached(observed_explicit),
        );
        decisions.push(DecisionResult {
            executed: !vectors.is_empty(),
            covered: conditions.iter().all(|condition| condition.covered),
            meta,
            vectors,
            vector_observations: observations,
            conditions,
            tests: decision_tests.names,
            confidence: decision_confidence,
        });
    }

    let points = manifest
        .points
        .iter()
        .cloned()
        .map(|meta| {
            let tests = tests_by_hit.of(&meta.id);
            let phases = phases_by_hit.of(&meta.id);
            let explicit = explicit_phases_by_hit.of(&meta.id);
            PointResult {
                measured: !declined.contains(&meta.id),
                covered: tests_by_hit.by_hit.contains_key(&meta.id),
                confidence: confidence(tests, phases, explicit),
                meta,
                tests: tests.names.clone(),
                phases: phases.names.clone(),
            }
        })
        .collect::<Vec<_>>();

    let branches = manifest
        .branches
        .iter()
        .cloned()
        .map(|mut meta| {
            // A declined outcome leaves the denominator; the others stay.
            meta.alternatives
                .retain(|alternative| !declined.contains(&alternative.id));
            let alternatives = meta
                .alternatives
                .iter()
                .map(|alternative| {
                    let tests = tests_by_hit.of(&alternative.id);
                    let phases = phases_by_hit.of(&alternative.id);
                    let explicit = explicit_phases_by_hit.of(&alternative.id);
                    AlternativeResult {
                        id: alternative.id.clone(),
                        label: alternative.label.clone(),
                        covered: tests_by_hit.by_hit.contains_key(&alternative.id),
                        tests: tests.names.clone(),
                        phases: phases.names.clone(),
                        confidence: confidence(tests, phases, explicit),
                    }
                })
                .collect::<Vec<_>>();
            BranchResult {
                covered: alternatives.iter().all(|alternative| alternative.covered),
                meta,
                alternatives,
            }
        })
        .chain(outcome_branches)
        .collect::<Vec<_>>();

    // Obligations the frontend declined to measure leave the covered/uncovered
    // denominator entirely. Counting them as uncovered would report a
    // measurement gap as a coverage gap — a wrong number, and wrong numbers get
    // trusted. They are reported separately instead, alongside the share of
    // obligations that were measured exactly.
    // Lines are folded from every point, declined ones included, so a file
    // Supercov could not measure still has addressable lines to report a
    // limitation against. Only what was measured decides the line's state and
    // whether it counts. A line gathers its points' test and phase numbers,
    // which sort and deduplicate as numbers.
    let mut line_aggregates = BTreeMap::<SourceLine, LineAggregate>::new();
    for point in &points {
        let aggregate = line_aggregates
            .entry(SourceLine {
                file: ids.id(&point.meta.file),
                line: point.meta.line,
            })
            .or_default();
        if declined.contains(&point.meta.id) {
            continue;
        }
        aggregate.measured = true;
        aggregate.covered |= point.covered;
        aggregate
            .tests
            .extend(&tests_by_hit.of(&point.meta.id).numbers);
        aggregate
            .phases
            .extend(&phases_by_hit.of(&point.meta.id).numbers);
        aggregate
            .explicit_phases
            .extend(&explicit_phases_by_hit.of(&point.meta.id).numbers);
    }
    let lines = line_aggregates
        .into_iter()
        .map(|(location, aggregate)| {
            let LineAggregate {
                covered,
                measured,
                tests,
                phases,
                explicit_phases,
            } = aggregate;
            let tests = tests_by_hit.reached(tests);
            let phases = phases_by_hit.reached(phases);
            let explicit = explicit_phases_by_hit.reached(explicit_phases);
            let traits = tests
                .numbers
                .iter()
                .filter_map(|number| test_traits[*number as usize].as_ref())
                .collect::<Vec<_>>();
            let runners = traits
                .iter()
                .map(|traits| traits.runner)
                .collect::<BTreeSet<_>>();
            let kinds = traits
                .iter()
                .map(|traits| traits.kind)
                .collect::<BTreeSet<_>>();
            LineResult {
                file: location.file.to_string(),
                line: location.line,
                covered,
                measured,
                confidence: confidence(&tests, &phases, &explicit),
                tests: tests.names,
                runners: runners.iter().map(|runner| (*runner).to_owned()).collect(),
                exclusive_kind: (kinds.len() == 1).then(|| (*kinds.first().unwrap()).to_owned()),
                phases: phases.names,
                kinds: kinds.iter().map(|kind| (*kind).to_owned()).collect(),
            }
        })
        .collect::<Vec<_>>();
    drop(test_traits);

    // Where each hit sits, by hit number, as (file rank, line): a test's or
    // phase's lines sort and deduplicate as numbers, in the order the
    // `SourceLine`s they stand for sort in.
    let hit_ranks = hit_ids.ranks();
    let mut point_files = manifest
        .points
        .iter()
        .map(|point| ids.id(&point.file))
        .collect::<Vec<_>>();
    point_files.sort_unstable();
    point_files.dedup();
    let mut hit_locations = vec![None; hit_ids.names.len()];
    for point in &manifest.points {
        if let Some(number) = hit_ids.numbers.get(point.id.as_str()) {
            let file = point_files
                .binary_search_by(|file| file.as_str().cmp(&point.file))
                .expect("every point file is listed") as u32;
            hit_locations[*number] = Some((file, point.line));
        }
    }
    let lines_of = |hits: &[usize]| {
        let mut located = hits
            .iter()
            .filter_map(|hit| hit_locations[*hit])
            .collect::<Vec<_>>();
        located.sort_unstable();
        located.dedup();
        located
            .into_iter()
            .map(|(file, line)| SourceLine {
                file: point_files[file as usize].clone(),
                line,
            })
            .collect::<Vec<_>>()
    };
    test_order.sort_by(|left, right| tests_by_id[left].name.cmp(&tests_by_id[right].name));
    let tests = test_order
        .into_iter()
        .map(|id| {
            let test = tests_by_id.remove(&id).expect("test order references test");
            let outcome = test_outcome(&test);
            let lines = lines_of(&test.hits);
            let hits = hit_ids.sorted_by(test.hits, &hit_ranks);
            TestCoverageResult {
                id: test.id,
                name: test.name,
                file: test.file,
                title: test.title,
                retries: test.retries.into_iter().collect(),
                attempts: test.attempts.into_values().collect(),
                outcome,
                provenance: test.provenance,
                role: test.role,
                attribution: test.attribution,
                hits,
                decisions: test
                    .decisions
                    .into_iter()
                    .map(|(id, vectors)| TestDecisionResult {
                        id,
                        vectors: vectors.values,
                    })
                    .collect(),
                lines,
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
        aggregate.3.extend(test.lines.iter().cloned());
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

    let mut phases = phases_by_id.into_values().collect::<Vec<_>>();
    phases.sort_by(|left, right| {
        left.phase
            .started_at_ms
            .cmp(&right.phase.started_at_ms)
            .then(left.phase.id.cmp(&right.phase.id))
    });
    let phases = phases
        .into_iter()
        .map(|phase| {
            let lines = lines_of(&phase.hits);
            let hits = hit_ids.sorted_by(phase.hits, &hit_ranks);
            PhaseResult {
                phase: phase.phase,
                test: phase.test,
                hits,
                decisions: phase
                    .decisions
                    .into_iter()
                    .map(|(id, vectors)| TestDecisionResult {
                        id,
                        vectors: vectors.values,
                    })
                    .collect(),
                lines,
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

    let total_obligations = decisions.len() + points.len() + branches.len();
    let (decisions, points, branches) = if declined.is_empty() {
        (decisions, points, branches)
    } else {
        (
            decisions
                .into_iter()
                .filter(|result| !declined.contains(&result.meta.id))
                .collect::<Vec<_>>(),
            points
                .into_iter()
                .filter(|result| !declined.contains(&result.meta.id))
                .collect::<Vec<_>>(),
            branches
                .into_iter()
                .filter(|result| !declined.contains(&result.meta.id))
                .collect::<Vec<_>>(),
        )
    };
    let measured_obligations = decisions.len() + points.len() + branches.len();
    let mut summary = summary_for_results(&decisions, &points, &branches, &lines, None)?;
    if total_obligations > measured_obligations {
        summary.unmeasured_obligations = Some(total_obligations - measured_obligations);
        // More in total than measured: the total is not zero.
        summary.exact_fraction_pct =
            Some((measured_obligations as f64) * 100.0 / (total_obligations as f64));
    }
    // A limitation that says `"blocking": false` is a declared boundary of
    // the denominator -- a macro the compiler expands, a const context no
    // probe can run in -- not a failure to measure what is inside it. Only a
    // blocking one makes the run incomplete. A limitation that says nothing
    // is blocking, so a frontend that has not been taught the difference
    // keeps its behaviour.
    if manifest.limitations.iter().any(blocking_limitation) {
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
                let in_dimension = |test: &&TestCoverageResult| {
                    if field == "kind" {
                        test.provenance.kind == value
                    } else {
                        test.provenance.runner == value
                    }
                };
                // Coverage is summarised over the tests that have coverage of
                // their own. A Go test that called `t.Parallel()` reached real
                // code and contributes no hits, because nothing can say which
                // code -- so counting it here reported a package covered
                // entirely by parallel tests as covered 0%, which is a wrong
                // number rather than a missing one.
                let selected = tests
                    .iter()
                    .filter(in_dimension)
                    .filter(|test| coverage_is_its_own(&test.attribution))
                    .map(|test| test.id.clone())
                    .collect::<BTreeSet<_>>();
                let counted = tests
                    .iter()
                    .filter(in_dimension)
                    .map(|test| test.id.clone())
                    .collect::<BTreeSet<_>>();
                Ok(DimensionCoverage {
                    kind: (field == "kind").then(|| value.clone()),
                    runner: (field == "runner").then(|| value.clone()),
                    // Counted whether or not anything could be attributed to
                    // them: they ran.
                    tests: tests
                        .iter()
                        .filter(|test| counted.contains(&test.id) && test.role == "test")
                        .count(),
                    setups: tests
                        .iter()
                        .filter(|test| counted.contains(&test.id) && test.role == "setup")
                        .count(),
                    attributed: tests
                        .iter()
                        .filter(|test| selected.contains(&test.id) && test.role == "test")
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

/// A run's views one at a time, each built when asked for: what the index
/// and a reader of one view need without the three held at once, which was
/// 1.7 GB of a 1,100-test run's re-index.
pub fn coverage_views(
    request: &CoverageReportRequest,
    views: &[crate::coverage_index::CoverageViewId],
    transport: Option<TransportStats>,
) -> Result<
    impl Iterator<Item = Result<(crate::coverage_index::CoverageViewId, CoverageView), ReportError>>,
    ReportError,
> {
    check_results(request)?;
    let coverage_model = request
        .coverage_model
        .clone()
        .unwrap_or_else(javascript_coverage_model);
    Ok(views.iter().copied().map(move |id| {
        use crate::coverage_index::CoverageViewId;
        let results = match id {
            CoverageViewId::All => request.raw_results.iter().collect::<Vec<_>>(),
            CoverageViewId::Passed => passing_coverage_results(&request.raw_results),
            CoverageViewId::Failed => failed_coverage_results(&request.raw_results),
        };
        let mut view = create_coverage_view_with_model(
            &request.manifest,
            &results,
            &request.generated_at,
            &coverage_model,
            &mut Interner::default(),
        )?;
        view.integrity.clone_from(&request.integrity);
        view.transport.clone_from(&transport);
        Ok((id, view))
    }))
}

/// What every analysis of a request first establishes.
fn check_results(request: &CoverageReportRequest) -> Result<(), ReportError> {
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
    Ok(())
}

pub fn analyze_coverage_results(
    request: &CoverageReportRequest,
) -> Result<CoverageReport, ReportError> {
    check_results(request)?;
    let default_model = javascript_coverage_model();
    let coverage_model = request.coverage_model.as_ref().unwrap_or(&default_model);
    // The three views are built side by side: each reads the same evidence
    // and nothing the others write. Each holds one copy of each id it names.
    // They do not share copies: two threads cloning the same ids contend on
    // their counts, which made each view twice as slow as building it alone.
    let all = request.raw_results.iter().collect::<Vec<_>>();
    let passing = passing_coverage_results(&request.raw_results);
    let failing = failed_coverage_results(&request.raw_results);
    let build = |results: &[&RawTestResult]| {
        create_coverage_view_with_model(
            &request.manifest,
            results,
            &request.generated_at,
            coverage_model,
            &mut Interner::default(),
        )
    };
    let (view, passed, failed) = std::thread::scope(|scope| {
        let passed = scope.spawn(|| build(&passing));
        let failed = scope.spawn(|| build(&failing));
        let view = build(&all);
        (
            view,
            passed
                .join()
                .unwrap_or_else(|panic| std::panic::resume_unwind(panic)),
            failed
                .join()
                .unwrap_or_else(|panic| std::panic::resume_unwind(panic)),
        )
    });
    let (view, passed, failed) = (view?, passed?, failed?);
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

/// Parse each entry on its own, a share of them on each core, keeping their
/// order. Every test's record is a JSON document of its own: one after another
/// they were a fifth of re-indexing a 1,100-test run.
fn parse_in_parallel<T: Send>(
    entries: &[&EvidenceArchiveEntry],
    parse: impl Fn(&&EvidenceArchiveEntry) -> Result<Vec<T>, ReportError> + Sync,
) -> Result<Vec<T>, ReportError> {
    let threads = std::thread::available_parallelism()
        .map_or(1, std::num::NonZeroUsize::get)
        .min(entries.len().div_ceil(64))
        .max(1);
    if threads == 1 {
        return entries.iter().try_fold(Vec::new(), |mut parsed, entry| {
            parsed.extend(parse(entry)?);
            Ok(parsed)
        });
    }
    let chunk = entries.len().div_ceil(threads);
    let parse = &parse;
    let parts = std::thread::scope(|scope| {
        let handles = entries
            .chunks(chunk)
            .map(|part| {
                scope.spawn(move || {
                    part.iter().try_fold(Vec::new(), |mut parsed, entry| {
                        parsed.extend(parse(entry)?);
                        Ok::<_, ReportError>(parsed)
                    })
                })
            })
            .collect::<Vec<_>>();
        handles
            .into_iter()
            .map(|handle| {
                handle
                    .join()
                    .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
            })
            .collect::<Vec<_>>()
    });
    let mut parsed = Vec::new();
    for part in parts {
        parsed.extend(part?);
    }
    Ok(parsed)
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

/// Server evidence is appended by application processes Supercov does not
/// control — including pool VMs restored from one snapshot, whose clones can
/// tear a shared shard. A torn line is one lost record, not a lost run: it is
/// skipped and counted, and the report says so through the existing
/// CORRUPT_EVIDENCE_RECORDS diagnostic and the blocking-limitation total.
struct TolerantJsonLines<T> {
    records: Vec<T>,
    corrupt_records: usize,
    corrupt_files: usize,
}

fn parse_server_json_lines<'a, T: for<'de> Deserialize<'de>>(
    entries: impl Iterator<Item = &'a EvidenceArchiveEntry>,
) -> TolerantJsonLines<T> {
    let mut parsed = TolerantJsonLines {
        records: Vec::new(),
        corrupt_records: 0,
        corrupt_files: 0,
    };
    for entry in entries {
        let mut corrupt_here = 0;
        for line in entry.contents.split(|byte| *byte == b'\n') {
            if line.is_empty() {
                continue;
            }
            match serde_json::from_slice(line) {
                Ok(record) => parsed.records.push(record),
                Err(_) => corrupt_here += 1,
            }
        }
        if corrupt_here > 0 {
            parsed.corrupt_records += corrupt_here;
            parsed.corrupt_files += 1;
        }
    }
    parsed
}

fn is_mcdc_result(path: &str) -> bool {
    path == "mcdc.json" || path.ends_with("/mcdc.json")
}

fn is_mcdc_journal(path: &str) -> bool {
    path == "mcdc.jsonl" || path.ends_with(".mcdc.jsonl")
}

pub fn analyze_coverage_archive(
    request: &ArchiveReportRequest,
) -> Result<CoverageReport, ReportError> {
    let (_, normalized, transport) = archive_request(request)?;
    let report = analyze_coverage_results(&normalized).map(|mut report| {
        report.view.transport = Some(transport.clone());
        report.filters.passed.transport = Some(transport.clone());
        report.filters.failed.transport = Some(transport);
        report
    });
    // Every test's records, gigabytes of small allocations on a large run:
    // the views hold what they need, and freeing the rest is nobody's wait.
    // It took 2 to 5 s of a 1,100-test run's re-index.
    std::thread::spawn(move || drop(normalized));
    report
}

/// A stored run's evidence as the request its views are built from, with the
/// frontend that declared it and what its processes reported.
pub fn archive_request(
    request: &ArchiveReportRequest,
) -> Result<
    (
        FrontendRunDeclaration,
        CoverageReportRequest,
        TransportStats,
    ),
    ReportError,
> {
    // Every test's record is parsed as it is read, a batch at a time, and its
    // bytes dropped: holding the decompressed archive whole beside what was
    // parsed from it was a gigabyte of a 1,100-test run's 3.6 GB.
    const BATCH: usize = 256;
    let mut entries = Vec::new();
    let mut raw_results = Vec::new();
    let mut journal_results = Vec::new();
    let mut results = Vec::new();
    let mut journals = Vec::new();
    let mut failure = None;
    let mut flush = |results: &mut Vec<EvidenceArchiveEntry>,
                     journals: &mut Vec<EvidenceArchiveEntry>,
                     failure: &mut Option<ReportError>| {
        if failure.is_some() {
            results.clear();
            journals.clear();
            return;
        }
        let parsed = parse_in_parallel(&results.iter().collect::<Vec<_>>(), |entry| {
            Ok(vec![parse_entry::<RawTestResult>(entry)?])
        })
        .and_then(|parsed| {
            raw_results.extend(parsed);
            parse_in_parallel(&journals.iter().collect::<Vec<_>>(), |entry| {
                parse_json_lines::<RawTestResult>(std::iter::once(*entry))
            })
        });
        match parsed {
            Ok(parsed) => journal_results.extend(parsed),
            Err(error) => *failure = Some(error),
        }
        results.clear();
        journals.clear();
    };
    crate::evidence_archive::read_archive_each(
        Path::new(&request.archive_path),
        |_| true,
        |entry| {
            if is_mcdc_result(&entry.path) {
                results.push(entry);
            } else if is_mcdc_journal(&entry.path) {
                journals.push(entry);
            } else {
                entries.push(entry);
            }
            if results.len() + journals.len() >= BATCH {
                flush(&mut results, &mut journals, &mut failure);
            }
            Ok(())
        },
    )
    .map_err(|error| ReportError::InvalidArchive(error.to_string()))?;
    flush(&mut results, &mut journals, &mut failure);
    if let Some(error) = failure {
        return Err(error);
    }
    raw_results.extend(journal_results);
    let manifest = entries
        .iter()
        .find(|entry| entry.path == "manifest.json")
        .ok_or(ReportError::MissingManifest)
        .and_then(parse_entry::<CoverageManifest>)?;

    let scoped = parse_server_json_lines::<ServerRecord>(entries.iter().filter(|entry| {
        entry.path.starts_with("server/")
            && !entry.path.starts_with("server/background/")
            && entry.path.ends_with(".jsonl")
    }));
    let scoped_records = scoped.records;
    // Each server record joins the first result with its scope, once. A
    // linear search per record and a linear duplicate check per result made
    // this quadratic: supergateway's e2e servers spent most of a 20 s report
    // preparation, and of publication, comparing scopes.
    let mut by_scope = std::collections::HashMap::<&ExecutionScope, usize>::new();
    for (index, raw) in raw_results.iter().enumerate() {
        if let Some(scope) = &raw.scope {
            by_scope.entry(scope).or_insert(index);
        }
    }
    let by_scope: std::collections::HashMap<ExecutionScope, usize> = by_scope
        .into_iter()
        .map(|(scope, index)| (scope.clone(), index))
        .collect();
    let mut seen = std::collections::HashMap::<usize, std::collections::HashSet<Vec<u8>>>::new();
    for record in &scoped_records {
        let Some(scope) = &record.scope else { continue };
        let Some(&index) = by_scope.get(scope) else {
            continue;
        };
        let raw = &mut raw_results[index];
        let known = seen.entry(index).or_insert_with(|| {
            raw.server
                .iter()
                .filter_map(|existing| serde_json::to_vec(existing).ok())
                .collect()
        });
        let Ok(key) = serde_json::to_vec(record) else {
            if !raw.server.contains(record) {
                raw.server.push(record.clone());
            }
            continue;
        };
        if known.insert(key) {
            raw.server.push(record.clone());
        }
    }

    // Background evidence by the process that wrote it: `<writer>-<n>.jsonl`.
    let mut writers = BTreeMap::<&str, Vec<&EvidenceArchiveEntry>>::new();
    for entry in entries.iter().filter(|entry| {
        entry.path.starts_with("server/background/") && entry.path.ends_with(".jsonl")
    }) {
        let name = &entry.path["server/background/".len()..entry.path.len() - ".jsonl".len()];
        let writer = name
            .rsplit_once('-')
            .filter(|(_, sequence)| sequence.chars().all(|c| c.is_ascii_digit()))
            .map_or(name, |(writer, _)| writer);
        writers.entry(writer).or_default().push(entry);
    }
    let mut background = TolerantJsonLines::<ServerRecord> {
        records: Vec::new(),
        corrupt_records: 0,
        corrupt_files: 0,
    };
    let mut processes = Vec::new();
    for files in writers.into_values() {
        let parsed = parse_server_json_lines::<ServerRecord>(files.into_iter());
        background.corrupt_records += parsed.corrupt_records;
        background.corrupt_files += parsed.corrupt_files;
        processes.push(parsed.records);
    }
    // The run declares its background runner whenever a process wrote
    // background evidence, so the runner stays though everything in it was
    // credited to a test or was only a note of what a server served.
    let had_background = !processes.is_empty();
    let background_records = credit_by_time(&mut raw_results, processes);
    background.records.clone_from(&background_records);
    if had_background {
        raw_results.push(RawTestResult {
            span: None,
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
                tags: Vec::new(),
            },
            role: "background".into(),
            attribution: ATTRIBUTION_EXACT.into(),
            phases: vec![],
            runtime: vec![],
            browser: vec![],
            server: background_records.clone(),
        });
    }

    // Execution traces are appended by the launch observer inside application
    // processes, so they share the clone hazard of server evidence.
    let execution =
        parse_server_json_lines::<ExecutionTraceEvent>(entries.iter().filter(|entry| {
            entry.path.starts_with("execution.") && entry.path.ends_with(".jsonl")
        }));
    let execution_events = execution.records;
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
        guest_processes: count_event("guest-process"),
        scoped_server_records: scoped_records.len(),
        background_server_records: background_records.len(),
        corrupt_records: scoped.corrupt_records
            + background.corrupt_records
            + execution.corrupt_records,
        corrupt_files: scoped.corrupt_files + background.corrupt_files + execution.corrupt_files,
    };
    let mut frontend = entries
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
    separate_repeated_executions(&mut raw_results);
    release_leaked_phases(&mut raw_results);
    // After server work was credited to its tests by time, so a picked test
    // keeps what its requests did.
    if let Some(filter) = request
        .test_filter
        .as_ref()
        .filter(|filter| !filter.is_empty())
    {
        raw_results = filter.select(raw_results)?;
        // The runners the picked tests ran under: one whose tests were all
        // left out, background included, is not in this view.
        frontend.runners.retain(|declared| {
            raw_results
                .iter()
                .any(|raw| raw.provenance.runner == declared.runner)
        });
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
    // Everything is parsed out of the archive's bytes -- 385 MB on a
    // 3,900-test run -- and nothing reads them again, so they need not stay
    // resident while the views are built beside what was parsed from them.
    drop(entries);
    crate::frontend_protocol::validate_frontend_report_request(&frontend, &normalized)
        .map_err(|error| ReportError::InvalidArchive(error.to_string()))?;
    Ok((frontend, normalized, transport))
}

/// Give a test that ran again under the same identity phases of its own.
///
/// A runner's attempt counter lives in its process, so a command that runs the
/// same tests twice -- ms runs Jest once in Node's environment and once in the
/// edge runtime's -- mints the same attempt, and the same phase ids, twice.
/// Every such run was refused whole ("duplicate frontend phase ID"). A record
/// that declares a phase an earlier record already declared is another
/// execution: its phases, and its own references to them, are renamed apart.
/// Both executions stay one attempt, whose outcome folds as any attempt's does.
fn separate_repeated_executions(raw_results: &mut [RawTestResult]) {
    let mut declared = BTreeSet::<String>::new();
    let mut executions = BTreeMap::<String, usize>::new();
    for raw in raw_results.iter_mut() {
        let repeated = raw.phases.iter().any(|phase| declared.contains(&phase.id));
        if !repeated {
            declared.extend(raw.phases.iter().map(|phase| phase.id.clone()));
            continue;
        }
        let mut renamed = BTreeMap::<String, String>::new();
        for phase in &raw.phases {
            let count = executions.entry(phase.id.clone()).or_insert(1);
            let fresh = loop {
                *count += 1;
                let candidate = format!("{}~{}", phase.id, count);
                if !declared.contains(&candidate) {
                    break candidate;
                }
            };
            renamed.insert(phase.id.clone(), fresh);
        }
        let rename = |phase: &mut Option<String>| {
            if let Some(fresh) = phase.as_ref().and_then(|id| renamed.get(id)) {
                *phase = Some(fresh.clone());
            }
        };
        for phase in &mut raw.phases {
            phase.id = renamed[&phase.id].clone();
            rename(&mut phase.caused_by_phase_id);
        }
        for snapshot in raw.runtime.iter_mut().chain(raw.browser.iter_mut()) {
            rename(&mut snapshot.phase_id);
            for event in &mut snapshot.events {
                rename(&mut event.phase_id);
            }
        }
        for record in &mut raw.server {
            rename(&mut record.phase_id);
        }
        declared.extend(renamed.into_values());
    }
}

/// Untag evidence that names another test's phase.
///
/// Work a test starts can finish after it: axios's HTTP/2 upload tests leave a
/// stream settling, and 2.0.1's runtime tagged what it ran with the ended
/// test's assertion phase, inside the next test's record. Three such events
/// in 2,201 records made the whole run unreadable ("unknown frontend phase
/// reference"). A phase another record of the same run declares is not
/// corruption: the event keeps its hit in the record that holds it and loses
/// the phase it cannot belong to. A phase no record declares is still refused.
fn release_leaked_phases(raw_results: &mut [RawTestResult]) {
    let declared = raw_results
        .iter()
        .flat_map(|raw| raw.phases.iter().map(|phase| phase.id.clone()))
        .collect::<BTreeSet<_>>();
    for raw in raw_results.iter_mut() {
        let own = raw
            .phases
            .iter()
            .map(|phase| phase.id.clone())
            .collect::<BTreeSet<_>>();
        let leaked = |phase: &mut Option<String>| {
            if phase
                .as_ref()
                .is_some_and(|id| !own.contains(id) && declared.contains(id))
            {
                *phase = None;
            }
        };
        for snapshot in raw.runtime.iter_mut().chain(raw.browser.iter_mut()) {
            leaked(&mut snapshot.phase_id);
            for event in &mut snapshot.events {
                leaked(&mut event.phase_id);
            }
        }
        for record in &mut raw.server {
            leaked(&mut record.phase_id);
        }
    }
}

/// The analysis of a frontend run, from its request as an archive holds it:
/// what `analyze_coverage_archive` does once it has parsed the archive, and
/// what a run that still holds the request it archived calls instead of
/// reading the archive back.
pub fn analyze_frontend_request(
    frontend: &FrontendRunDeclaration,
    normalized: &CoverageReportRequest,
    transport: TransportStats,
) -> Result<CoverageReport, ReportError> {
    let mut report = crate::frontend_protocol::analyze_frontend_results(frontend, normalized)
        .map_err(|error| ReportError::InvalidArchive(error.to_string()))?;
    report.view.transport = Some(transport.clone());
    report.filters.passed.transport = Some(transport.clone());
    report.filters.failed.transport = Some(transport);
    Ok(report)
}

impl TransportStats {
    /// An archive with no launch-observer or server evidence: every frontend
    /// but JavaScript's.
    pub fn none() -> Self {
        Self {
            processes: 0,
            child_launches: 0,
            remote_launches: 0,
            workspace_capabilities: 0,
            guest_processes: 0,
            scoped_server_records: 0,
            background_server_records: 0,
            corrupt_records: 0,
            corrupt_files: 0,
        }
    }
}

#[cfg(test)]
mod tests {

    #[test]
    fn a_test_filter_picks_by_any_value_of_each_criterion_and_every_criterion_given() {
        let provenance = |tags: &[&str]| TestProvenance {
            runner: "playwright".into(),
            kind: "e2e".into(),
            tags: tags.iter().map(|tag| tag.to_string()).collect(),
            ..TestProvenance::default()
        };
        let api = provenance(&["api", "smoke"]);
        let filter = |tags: &[&str], titles: &[&str], files: &[&str]| TestFilter {
            tags: tags.iter().map(|value| value.to_string()).collect(),
            titles: titles.iter().map(|value| value.to_string()).collect(),
            files: files.iter().map(|value| value.to_string()).collect(),
            kind: None,
            runner: None,
        };
        let name = "checkout > pays by card @api";
        let file = Some("tests/e2e/checkout.spec.ts");
        assert!(filter(&["@API"], &[], &[]).matches(name, None, file, &api));
        assert!(filter(&["ui", "smoke"], &[], &[]).matches(name, None, file, &api));
        assert!(!filter(&["ui"], &[], &[]).matches(name, None, file, &api));
        assert!(filter(&[], &["PAYS BY"], &[]).matches(name, None, file, &api));
        // A positional name matches by its title.
        assert!(filter(&[], &["adds an item"], &[]).matches(
            "spec/cart_spec.rb[1:2]",
            Some("Cart adds an item"),
            None,
            &api
        ));
        assert!(filter(&[], &[], &["tests/e2e"]).matches(name, None, file, &api));
        assert!(filter(&[], &[], &["./tests/e2e/"]).matches(name, None, file, &api));
        assert!(filter(&[], &[], &["tests/e2e/checkout.spec.ts"]).matches(name, None, file, &api));
        // A directory is a whole segment, not a prefix of a name.
        assert!(!filter(&[], &[], &["tests/e"]).matches(name, None, file, &api));
        assert!(!filter(&["api"], &[], &["tests/unit"]).matches(name, None, file, &api));
        // A leading `!` excludes; with only exclusions, every other test.
        assert!(!filter(&["!api"], &[], &[]).matches(name, None, file, &api));
        assert!(filter(&["!ui", "!cron"], &[], &[]).matches(name, None, file, &api));
        assert!(!filter(&["smoke", "!API"], &[], &[]).matches(name, None, file, &api));
        assert!(filter(&[], &["!refund"], &["!tests/unit"]).matches(name, None, file, &api));
        assert!(!filter(&[], &[], &["!tests/e2e"]).matches(name, None, file, &api));
        let kinded = TestFilter {
            kind: Some("unit".into()),
            ..TestFilter::default()
        };
        assert!(!kinded.matches(name, None, file, &api));
        assert_eq!(
            filter(&["api"], &["pays"], &["tests"]).describe(),
            "--tag api --title pays --test-file tests"
        );
    }
    use std::{
        fs,
        sync::atomic::{AtomicU64, Ordering},
        time::SystemTime,
    };

    use crate::evidence_archive::{EvidenceArchiveEntry, write_archive};

    use super::*;

    #[test]
    fn a_view_holds_one_copy_of_each_id_it_names() {
        // What made analysis fit in memory: a view names each test at every
        // point, line and confidence it reached, and must share one copy of
        // the name rather than hold one per mention. A change that copies ids
        // again fails here before it shows up as gigabytes. The full and the
        // passing view are built side by side, each with its own copy.
        let root = std::env::temp_dir().join(format!(
            "supercov-shared-ids-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let directory = crate::run_store::create_analyzable_test_run(&root, "shared");
        let run = crate::run_store::discover_runs(&root)
            .unwrap()
            .runs
            .remove(0);
        let report = crate::run_store::analyze_stored_run(&run).unwrap();
        let _ = directory;
        for view in [&report.view, &report.filters.passed] {
            let mentions = view
                .points
                .iter()
                .flat_map(|point| point.tests.iter().chain(&point.confidence.tests))
                .chain(view.lines.iter().flat_map(|line| &line.tests))
                .collect::<Vec<_>>();
            assert!(
                mentions.len() >= 2,
                "the fixture names its test in several places"
            );
            let first = mentions[0];
            for mention in &mentions {
                assert_eq!(*mention, first);
                assert!(
                    mention.same_allocation(first),
                    "every mention shares one copy"
                );
            }
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn references_numbered_as_they_arrive_come_out_as_the_sets_inserted_directly() {
        // Out of order, repeated, interleaved across obligations, and sharing
        // long prefixes, as test and phase ids do.
        let arrivals = [
            ("py:statement:b", "h11/tests/test_x.py::test_b[2-50]"),
            ("py:statement:a", "h11/tests/test_x.py::test_b[2-50]"),
            ("py:statement:b", "h11/tests/test_x.py::test_a[1-50]"),
            ("py:statement:b", "h11/tests/test_x.py::test_b[2-50]"),
            ("py:statement:a", "h11/tests/test_x.py::test_b[10-50]"),
            ("py:statement:b", "h11/tests/test_x.py::test_a[1-50]"),
            ("py:statement:c", "python-phase:9"),
            ("py:statement:c", "python-phase:10"),
            ("py:statement:b", "h11/tests/test_x.py::test_b[2-50]"),
        ];
        let mut ids = Interner::default();
        let mut hits = HitIds::default();
        let mut numbered = References::default();
        // A hit known to another relation must not become covered here.
        hits.number("unused", &mut ids);
        let mut direct = HashMap::<String, BTreeSet<String>>::new();
        for (obligation, name) in arrivals {
            let hit = hits.number(obligation, &mut ids);
            numbered.add(hit, name, &mut ids);
            direct
                .entry(obligation.to_owned())
                .or_default()
                .insert(name.to_owned());
        }
        let numbered = numbered
            .into_relation(&hits)
            .by_hit
            .into_iter()
            .map(|(obligation, reached)| {
                let names = reached
                    .names
                    .into_iter()
                    .map(String::from)
                    .collect::<Vec<_>>();
                let mut sorted_names = names.clone();
                sorted_names.sort();
                sorted_names.dedup();
                assert_eq!(
                    names, sorted_names,
                    "a relation lists its ids once, in order"
                );
                (obligation, names.into_iter().collect())
            })
            .collect::<HashMap<_, BTreeSet<String>>>();
        assert_eq!(numbered, direct);
        assert_eq!(
            hits.sorted((0..hits.names.len()).collect()),
            [
                "py:statement:a",
                "py:statement:b",
                "py:statement:c",
                "unused"
            ]
            .map(Id::from)
        );
    }

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
            span: None,
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
                tags: Vec::new(),
            },
            role: "test".into(),
            attribution: crate::coverage_report::ATTRIBUTION_EXACT.into(),
            phases: vec![],
            runtime: vec![RuntimeSnapshot {
                decisions: vec![],
                hits: hits.iter().map(|hit| (*hit).into()).collect(),
                events: vec![],
                logicals: vec![],
                phase_id: None,
            }],
            browser: vec![],
            server: vec![],
        }
    }

    /// A filtered view keeps the tests it picks and the setup that ran in
    /// their files; setup in another file, and work outside every test, is
    /// no picked test's.
    #[test]
    fn a_filter_keeps_the_setup_of_the_files_it_picks_and_nothing_else() {
        let in_file = |id: &str, role: &str, file: &str| RawTestResult {
            span: None,
            role: role.into(),
            test_file: Some(file.into()),
            ..raw(id, 0, "passed", &[])
        };
        let results = vec![
            in_file("app", "test", "tests/app.test.js"),
            in_file("other", "test", "tests/other.test.js"),
            in_file("app setup", "setup", "tests/app.test.js"),
            in_file("other setup", "setup", "tests/other.test.js"),
            in_file("background", "background", "tests/app.test.js"),
        ];
        let filter = TestFilter {
            files: vec!["tests/app.test.js".into()],
            runner: Some("node:test".into()),
            ..TestFilter::default()
        };
        assert!(!filter.is_empty());
        let kept = filter.select(results.clone()).unwrap();
        assert_eq!(
            kept.iter().map(|raw| raw.test.as_str()).collect::<Vec<_>>(),
            ["app", "app setup"]
        );
        let jest = TestFilter {
            runner: Some("jest".into()),
            ..TestFilter::default()
        };
        assert!(matches!(
            jest.select(results),
            Err(ReportError::NoTestMatches(described)) if described == "--runner jest"
        ));
    }

    #[test]
    fn server_work_no_request_named_is_credited_to_the_only_test_running() {
        let phase = |start: i64, end: i64| CoveragePhase {
            id: format!("phase-{start}"),
            kind: "action".into(),
            operation: "get".into(),
            source: None,
            caused_by_phase_id: None,
            started_at_ms: start,
            ended_at_ms: Some(end),
            status: None,
            error: None,
        };
        let at = |timestamp: i64| ServerRecord {
            record_type: "hit".into(),
            meta: None,
            vector: None,
            id: Some(format!("hit-{timestamp}")),
            timestamp_ms: Some(timestamp),
            phase_id: None,
            scope: None,
            request: Some(timestamp as u64),
        };
        // Module loading in a test process: no request, never credited.
        let mut loading = at(170);
        loading.request = None;
        let mut first = raw("checkout", 0, "passed", &[]);
        first.phases = vec![phase(100, 150), phase(160, 200)];
        let mut second = raw("refund", 0, "passed", &[]);
        second.phases = vec![phase(300, 400)];
        // A third that overlaps the second: they ran at once.
        let mut beside = raw("report", 0, "passed", &[]);
        beside.phases = vec![phase(350, 450)];
        let mut results = vec![first, second, beside];
        let left = credit_by_time(
            &mut results,
            vec![vec![at(50), at(155), loading, at(320), at(360), at(500)]],
        );
        // During the first test, between two of its phases: its own.
        assert_eq!(results[0].server.len(), 1);
        assert_eq!(results[0].attribution, ATTRIBUTION_BY_TIME);
        // While the second ran alone: its own.
        assert_eq!(results[1].server.len(), 1);
        assert_eq!(results[1].attribution, ATTRIBUTION_BY_TIME);
        // Before any test, while two ran at once, and after all: no test's.
        assert_eq!(results[2].server.len(), 0);
        assert_eq!(results[2].attribution, ATTRIBUTION_EXACT);
        let left = left
            .iter()
            .filter_map(|record| record.timestamp_ms)
            .collect::<Vec<_>>();
        assert_eq!(left, [50, 170, 360, 500]);
        // Still complete: the credit adds to the test's reach, never hides it.
        assert!(coverage_is_complete(ATTRIBUTION_BY_TIME));
        assert_eq!(
            least_exact(ATTRIBUTION_BY_TIME, ATTRIBUTION_PARTIAL),
            ATTRIBUTION_PARTIAL
        );
        assert_eq!(
            least_exact(ATTRIBUTION_EXACT, ATTRIBUTION_BY_TIME),
            ATTRIBUTION_BY_TIME
        );
    }

    #[test]
    fn with_a_server_per_worker_its_requests_are_credited_among_its_own_tests() {
        let scoped = |id: &str, start: i64, end: i64| {
            let mut result = raw(id, 0, "passed", &[]);
            result.scope = Some(ExecutionScope {
                version: 1,
                run_id: "run".into(),
                worker_id: format!("worker-{id}"),
                test_id: id.into(),
                test_key: id.into(),
                retry: 0,
                attempt_id: format!("attempt-{id}"),
            });
            result.phases = vec![CoveragePhase {
                id: format!("phase-{id}"),
                kind: "action".into(),
                operation: "get".into(),
                source: None,
                caused_by_phase_id: None,
                started_at_ms: start,
                ended_at_ms: Some(end),
                status: None,
                error: None,
            }];
            result
        };
        let record = |kind: &str, id: &str, at: i64, request: Option<u64>| ServerRecord {
            record_type: kind.into(),
            meta: None,
            vector: None,
            id: Some(id.into()),
            timestamp_ms: Some(at),
            phase_id: None,
            scope: None,
            request,
        };
        // Two workers' tests overlap from 150 to 200; each has its own server.
        let mut results = vec![scoped("first", 100, 200), scoped("second", 150, 300)];
        let first_server = vec![
            record("served", "attempt-first", 101, None),
            // During the overlap: the first worker's server, so its test.
            record("hit", "webhook", 170, Some(1)),
            // While only the second worker's test runs: not this server's.
            record("hit", "late", 250, Some(2)),
        ];
        let second_server = vec![record("served", "attempt-second", 151, None)];
        let left = credit_by_time(&mut results, vec![first_server, second_server]);
        assert_eq!(results[0].server.len(), 1, "{:?}", results[0].server);
        assert_eq!(results[0].server[0].id.as_deref(), Some("webhook"));
        assert!(results[1].server.is_empty());
        assert_eq!(
            left.iter().filter_map(|r| r.id.clone()).collect::<Vec<_>>(),
            ["late"],
            "served markers are not coverage"
        );
    }

    #[test]
    fn declined_obligations_are_unmeasured_never_uncovered() {
        // Two statements, neither executed. One of them Supercov declined to
        // measure. The declined one must not appear as a coverage gap: it
        // leaves the denominator and is reported as unmeasured instead.
        let point = |id: &str, line: usize| PointMeta {
            id: id.into(),
            kind: PointKind::Statement,
            file: "src/app.js".into(),
            line,
            column: 0,
            source: "work();".into(),
            label: None,
        };
        let measured_only = CoverageManifest {
            decisions: vec![],
            points: vec![point("measured", 1)],
            branches: vec![],
            limitations: vec![],
            unmeasured: Vec::new(),
            scope: None,
            assertion_sites: Vec::new(),
        };
        let with_declined = CoverageManifest {
            decisions: vec![],
            points: vec![point("measured", 1), point("declined", 2)],
            branches: vec![],
            limitations: vec![],
            unmeasured: vec!["declined".into()],
            scope: None,
            assertion_sites: Vec::new(),
        };

        let baseline =
            create_coverage_view(&measured_only, &[raw("test", 0, "passed", &[])], "time").unwrap();
        let view =
            create_coverage_view(&with_declined, &[raw("test", 0, "passed", &[])], "time").unwrap();

        // The declined statement never inflates the uncovered count.
        assert_eq!(
            view.summary.statements.total, baseline.summary.statements.total,
            "a declined obligation stayed in the covered/uncovered denominator"
        );
        assert_eq!(view.summary.unmeasured_obligations, Some(1));
        assert_eq!(view.summary.exact_fraction_pct, Some(50.0));

        // Its line leaves the line total too, but not the report: the line is
        // still there to hang a limitation on, and it is neither covered nor
        // uncovered.
        assert_eq!(
            view.summary.lines.total, baseline.summary.lines.total,
            "a declined obligation kept its line in the line denominator"
        );
        let declined_line = view
            .lines
            .iter()
            .find(|line| line.line == 2)
            .expect("the declined line stays addressable");
        assert!(!declined_line.measured);
        assert!(!declined_line.covered);

        // A fully measured run keeps its previous output exactly: the new
        // fields are absent, not zero, so existing consumers see no change.
        assert_eq!(baseline.summary.unmeasured_obligations, None);
        assert_eq!(baseline.summary.exact_fraction_pct, None);
        let encoded = serde_json::to_string(&baseline.summary).unwrap();
        assert!(
            !encoded.contains("unmeasured") && !encoded.contains("exactFraction"),
            "a fully measured summary must serialize unchanged: {encoded}"
        );
    }

    #[test]
    fn a_declined_line_that_executed_is_neither_covered_nor_uncovered() {
        // The subtle case: the statement ran, but Supercov could not measure
        // it, so its line has no coverage question to answer. Counting it as
        // covered would inflate the ratio with a line nothing verified.
        let point = |id: &str| PointMeta {
            id: id.into(),
            kind: PointKind::Statement,
            file: "src/app.js".into(),
            line: 1,
            column: 0,
            source: "work();".into(),
            label: None,
        };
        let manifest = CoverageManifest {
            decisions: vec![],
            points: vec![point("declined")],
            branches: vec![],
            limitations: vec![],
            unmeasured: vec!["declined".into()],
            scope: None,
            assertion_sites: Vec::new(),
        };
        let view = create_coverage_view(
            &manifest,
            &[raw("test", 0, "passed", &["declined"])],
            "time",
        )
        .unwrap();

        assert_eq!(view.summary.lines.total, 0);
        assert_eq!(view.summary.lines.covered, 0);
        let line = view.lines.first().expect("the line stays addressable");
        assert!(!line.measured);
        assert!(!line.covered, "an unmeasured line is not covered either");
    }

    #[test]
    fn report_retains_unexecuted_manifest_conditions() {
        let manifest = CoverageManifest {
            unmeasured: Vec::new(),
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
            assertion_sites: Vec::new(),
        };
        let view =
            create_coverage_view(&manifest, &[raw("test", 0, "passed", &[])], "time").unwrap();
        assert_eq!(view.summary.conditions, 2);
        assert_eq!(view.summary.covered_conditions, 0);
        assert_eq!(view.decisions[0].conditions.len(), 2);
    }

    /// A server's decision record names a decision of the frozen manifest
    /// as the manifest has it, with a vector; one of a file outside it is
    /// passed over.
    #[test]
    fn a_server_decision_record_is_refused_for_each_way_it_breaks_the_manifest() {
        let meta = |id: &str, file: &str, source: &str| DecisionMeta {
            id: id.into(),
            file: file.into(),
            line: 1,
            column: 0,
            source: source.into(),
            conditions: vec!["left".into(), "right".into()],
            kind: "if".into(),
        };
        let manifest = CoverageManifest {
            unmeasured: Vec::new(),
            decisions: vec![meta("decision", "src/app.js", "left && right")],
            points: vec![],
            branches: vec![],
            limitations: vec![],
            scope: None,
            assertion_sites: Vec::new(),
        };
        let vector = McdcVector {
            values: vec![Some(true), Some(true)],
            outcome: true,
        };
        let record = |meta: Option<DecisionMeta>, vector: Option<McdcVector>| ServerRecord {
            record_type: "decision".into(),
            meta,
            vector,
            id: None,
            timestamp_ms: None,
            phase_id: None,
            scope: None,
            request: None,
        };
        let read = |record: ServerRecord| {
            let mut attempt = raw("test", 0, "passed", &[]);
            attempt.server.push(record);
            match create_coverage_view(&manifest, &[attempt], "time") {
                Ok(view) => format!("ok {}", view.summary.covered_conditions),
                Err(ReportError::InvalidServerRecord(reason)) => reason,
                Err(error) => format!("{error:?}"),
            }
        };
        assert_eq!(read(record(None, Some(vector.clone()))), "missing meta");
        assert_eq!(
            read(record(
                Some(meta("decision", "src/app.js", "left && right")),
                None
            )),
            "missing vector"
        );
        assert!(
            read(record(
                Some(meta("other", "src/app.js", "a && b")),
                Some(vector.clone())
            ))
            .contains("absent from the frozen manifest")
        );
        assert!(
            read(record(
                Some(meta("decision", "src/app.js", "left || right")),
                Some(vector.clone())
            ))
            .contains("metadata differs from the frozen manifest")
        );
        assert!(
            read(record(
                Some(meta("other", "lib/vendor.js", "a && b")),
                Some(vector.clone())
            ))
            .starts_with("ok")
        );
        assert!(
            read(record(
                Some(meta("decision", "src/app.js", "left && right")),
                Some(vector)
            ))
            .starts_with("ok")
        );
    }

    /// A branch whose every outcome a frontend declined is declined itself;
    /// one with an outcome left is still counted by that outcome.
    #[test]
    fn a_branch_with_every_outcome_declined_is_declined() {
        let branch = |id: &str, outcomes: &[&str]| BranchMeta {
            id: id.into(),
            kind: "if".into(),
            file: "src/app.js".into(),
            line: 1,
            column: 0,
            source: "x".into(),
            alternatives: outcomes
                .iter()
                .map(|outcome| BranchAlternativeMeta {
                    id: (*outcome).into(),
                    label: (*outcome).into(),
                })
                .collect(),
        };
        let manifest = CoverageManifest {
            unmeasured: vec!["a1".into(), "a2".into(), "a3".into()],
            decisions: vec![],
            points: vec![],
            branches: vec![branch("b1", &["a1", "a2"]), branch("b2", &["a3", "a4"])],
            limitations: vec![],
            scope: None,
            assertion_sites: Vec::new(),
        };
        let view =
            create_coverage_view(&manifest, &[raw("test", 0, "passed", &[])], "time").unwrap();
        assert_eq!(view.summary.branches.total, 1);
        assert_eq!(view.branches.len(), 1);
        assert_eq!(view.branches[0].meta.id, "b2");
        assert_eq!(view.branches[0].alternatives.len(), 1);
        assert_eq!(view.branches[0].alternatives[0].id, "a4");
    }

    #[test]
    fn frozen_manifest_ignores_out_of_scope_synthetic_decisions() {
        let manifest = CoverageManifest {
            unmeasured: Vec::new(),
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
            assertion_sites: Vec::new(),
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
            unmeasured: Vec::new(),
            decisions: vec![expected.clone()],
            points: vec![],
            branches: vec![],
            limitations: vec![],
            scope: Some(serde_json::json!({
                "entries": [{ "file": "src/empty.js", "status": "included" }]
            })),
            assertion_sites: Vec::new(),
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
    fn neither_timestamp_nor_explicit_phase_links_award_assertion_credit() {
        let manifest = CoverageManifest {
            unmeasured: Vec::new(),
            decisions: vec![],
            points: vec![point("hit", 1)],
            branches: vec![],
            limitations: vec![],
            scope: None,
            assertion_sites: Vec::new(),
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

        attempt.runtime[0].events[0].phase_id = Some(phase.id);
        let explicit = create_coverage_view(&manifest, &[attempt], "time").unwrap();
        assert_eq!(explicit.points[0].confidence.level, "executed");
    }

    /// Each event counts for its phase as a browser's or a server's, named
    /// or placed by its time; one in no phase, or in a phase the test does
    /// not have, counts for none. A snapshot that names its phase counts each
    /// of its hits and vectors as that phase's. A decision without a vector
    /// and an event of no known type are refused.
    #[test]
    fn events_count_for_their_phase_by_environment_and_by_how_they_were_placed() {
        let decision = DecisionMeta {
            id: "d".into(),
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
            points: vec![point("hit", 1)],
            branches: vec![],
            limitations: vec![],
            scope: None,
            assertion_sites: Vec::new(),
        };
        let both = McdcVector {
            values: vec![Some(true), Some(true)],
            outcome: true,
        };
        let neither = McdcVector {
            values: vec![Some(false), Some(false)],
            outcome: false,
        };
        let phase = |id: &str, started_at_ms: i64| CoveragePhase {
            id: id.into(),
            kind: "action".into(),
            operation: id.into(),
            source: None,
            caused_by_phase_id: None,
            started_at_ms,
            ended_at_ms: Some(started_at_ms + 20),
            status: Some("passed".into()),
            error: None,
        };
        let event = |event_type: &str,
                     id: &str,
                     vector: Option<&McdcVector>,
                     timestamp_ms: i64,
                     phase_id: Option<&str>,
                     environment: &str| RuntimeEvent {
            event_type: event_type.into(),
            id: id.into(),
            vector: vector.cloned(),
            timestamp_ms,
            phase_id: phase_id.map(str::to_owned),
            environment: environment.into(),
        };
        let mut attempt = raw("test", 0, "passed", &["hit"]);
        attempt.phases = vec![phase("a", 100), phase("b", 200)];
        attempt.runtime[0].decisions.push(DecisionSnapshot {
            meta: decision.clone(),
            vectors: vec![both.clone()],
        });
        attempt.runtime[0].events = vec![
            event("hit", "hit", None, 0, Some("a"), "browser"),
            event("hit", "hit", None, 210, None, "server"),
            event("decision", "d", Some(&both), 105, None, "browser"),
            event("decision", "d", Some(&neither), 0, Some("b"), "server"),
            event("hit", "hit", None, 0, Some("ghost"), "server"),
            event("hit", "hit", None, 50, None, "server"),
        ];
        attempt.browser.push(RuntimeSnapshot {
            decisions: vec![DecisionSnapshot {
                meta: decision.clone(),
                vectors: vec![both.clone()],
            }],
            hits: vec!["hit".into()],
            events: vec![],
            logicals: vec![],
            phase_id: Some("b".into()),
        });
        attempt.runtime.push(RuntimeSnapshot {
            decisions: vec![],
            hits: vec!["hit".into()],
            events: vec![],
            logicals: vec![],
            phase_id: Some("ghost".into()),
        });
        let view = create_coverage_view(&manifest, &[attempt.clone()], "time").unwrap();
        let counts = |id: &str| {
            let phase = view
                .phases
                .iter()
                .find(|phase| phase.phase.id == id)
                .unwrap();
            [
                phase.browser_events,
                phase.server_events,
                phase.explicit_events,
                phase.inferred_events,
                phase.explicit_browser_events,
                phase.inferred_browser_events,
                phase.explicit_server_events,
                phase.inferred_server_events,
            ]
        };
        assert_eq!(counts("a"), [2, 0, 1, 1, 1, 1, 0, 0]);
        assert_eq!(counts("b"), [2, 2, 3, 1, 2, 0, 1, 1]);

        let mut vectorless = attempt.clone();
        vectorless.runtime[0].events = vec![event("decision", "d", None, 0, Some("a"), "server")];
        assert!(matches!(
            create_coverage_view(&manifest, &[vectorless], "time"),
            Err(ReportError::InvalidEvent(id)) if id == "d"
        ));
        let mut unknown = attempt;
        unknown.runtime[0].events = vec![event("logical", "hit", None, 0, Some("a"), "server")];
        assert!(matches!(
            create_coverage_view(&manifest, &[unknown], "time"),
            Err(ReportError::InvalidEvent(kind)) if kind == "logical"
        ));
    }

    #[test]
    fn an_event_in_the_millisecond_one_phase_ended_and_the_next_began_is_the_first_ones() {
        let phase = |id: &str, started_at_ms: i64, ended_at_ms: i64| CoveragePhase {
            id: id.into(),
            kind: "action".into(),
            operation: id.into(),
            source: None,
            caused_by_phase_id: None,
            started_at_ms,
            ended_at_ms: Some(ended_at_ms),
            status: Some("passed".into()),
            error: None,
        };
        // `page.goto` resolved and the test's `expect` began in millisecond
        // 200. What the page ran while loading carried that time and no phase,
        // and all of it was the `expect`'s: the action that loaded the page
        // read as having run nothing.
        let handed_over = [phase("goto", 100, 200), phase("expect", 200, 214)];
        assert_eq!(phase_at(&handed_over, 199), Some("goto"));
        assert_eq!(phase_at(&handed_over, 200), Some("goto"));
        assert_eq!(phase_at(&handed_over, 201), Some("expect"));
        assert_eq!(phase_at(&handed_over, 99), None);
        // A phase that begins after a gap has its first millisecond to itself.
        let apart = [phase("goto", 100, 150), phase("click", 200, 214)];
        assert_eq!(phase_at(&apart, 200), Some("click"));
        // One inside another, still running, keeps what happens as it begins.
        let inside = [phase("step", 100, 300), phase("expect", 200, 220)];
        assert_eq!(phase_at(&inside, 200), Some("expect"));
        // Two that both lie within the millisecond: the later, as before.
        let instant = [phase("first", 200, 200), phase("second", 200, 210)];
        assert_eq!(phase_at(&instant, 200), Some("second"));
    }

    #[test]
    fn verified_view_uses_only_the_terminal_successful_attempt() {
        let manifest = CoverageManifest {
            unmeasured: Vec::new(),
            decisions: vec![],
            points: vec![point("failed", 1), point("passed", 2), point("expected", 3)],
            branches: vec![],
            limitations: vec![],
            scope: None,
            assertion_sites: Vec::new(),
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

    /// A request with no evidence, or evidence of another run, is refused
    /// before anything is analysed.
    #[test]
    fn a_request_without_its_own_evidence_is_refused() {
        let manifest = CoverageManifest {
            unmeasured: Vec::new(),
            decisions: vec![],
            points: vec![point("a", 1)],
            branches: vec![],
            limitations: vec![],
            scope: None,
            assertion_sites: Vec::new(),
        };
        let request = |raw_results: Vec<RawTestResult>| CoverageReportRequest {
            run_id: "run".into(),
            manifest: manifest.clone(),
            raw_results,
            generated_at: "time".into(),
            coverage_model: None,
            integrity: None,
            test_exit_code: ExitCodeInput::Missing,
        };
        assert!(matches!(
            analyze_coverage_results(&request(Vec::new())),
            Err(ReportError::NoEvidence(run)) if run == "run"
        ));
        let mut foreign = raw("a", 0, "passed", &["a"]);
        foreign.scope = Some(ExecutionScope {
            version: 1,
            run_id: "other".into(),
            worker_id: "w".into(),
            test_id: "a".into(),
            test_key: "a".into(),
            retry: 0,
            attempt_id: "a:0".into(),
        });
        assert!(matches!(
            analyze_coverage_results(&request(vec![foreign])),
            Err(ReportError::ScopeMismatch { actual, .. }) if actual == "other"
        ));
        assert!(analyze_coverage_results(&request(vec![raw("a", 0, "passed", &["a"])])).is_ok());
    }

    #[test]
    fn expected_failure_is_a_green_outcome_but_not_verified_or_failed_coverage() {
        let mut expected = raw("expected-failure", 0, "failed", &["expected"]);
        expected.expected_status = Some("failed".into());
        let companion = raw("expected-failure", 0, "failed", &["expected"]);
        let request = CoverageReportRequest {
            run_id: "run".into(),
            manifest: CoverageManifest {
                unmeasured: Vec::new(),
                decisions: vec![],
                points: vec![point("expected", 1)],
                branches: vec![],
                limitations: vec![],
                scope: None,
                assertion_sites: Vec::new(),
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
                unmeasured: Vec::new(),
                decisions: vec![],
                points: vec![],
                branches: vec![],
                limitations: vec![],
                scope: None,
                assertion_sites: Vec::new(),
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
            unmeasured: Vec::new(),
            decisions: vec![],
            points: vec![point("background-hit", 1), point("test-hit", 2)],
            branches: vec![],
            limitations: vec![],
            scope: None,
            assertion_sites: Vec::new(),
        };
        // Declare exactly the observed runners: the journal's node:test and the
        // synthesized background runner, shaped as the JavaScript run declares it.
        let unattributed = |axis: &str| {
            serde_json::json!({
                "id": format!("background-no-{axis}"),
                "scopes": [axis],
                "reason": format!("Runner background did not expose exact {axis} identity for every result")
            })
        };
        let frontend = serde_json::json!({
            "protocolVersion": 2,
            "frontendId": "javascript",
            "frontendVersion": "fixture-v1",
            "language": "javascript",
            "structuralSource": "owned-probes",
            "runners": [
                {
                    "runner": "node:test",
                    "executionModel": "serial-in-process",
                    "attribution": {
                        "run": "exact", "worker": "unavailable", "test": "exact", "retry": "exact",
                        "phase": "exact", "action": "exact", "assertion": "exact"
                    },
                    "limitations": [{
                        "id": "fixture-worker-unavailable",
                        "scopes": ["worker"],
                        "reason": "The fixture does not require worker identity"
                    }]
                },
                {
                    "runner": "background",
                    "executionModel": "parallel-unattributed",
                    "attribution": {
                        "run": "exact", "worker": "unavailable", "test": "unavailable", "retry": "unavailable",
                        "phase": "unavailable", "action": "unavailable", "assertion": "unavailable"
                    },
                    "limitations": [
                        unattributed("worker"), unattributed("test"), unattributed("retry"),
                        unattributed("phase"), unattributed("action"), unattributed("assertion")
                    ]
                }
            ],
            "structuralLimitations": []
        });
        let path = archive(vec![
            EvidenceArchiveEntry {
                path: "manifest.json".into(),
                contents: serde_json::to_vec(&manifest).unwrap(),
            },
            EvidenceArchiveEntry {
                path: "frontend.json".into(),
                contents: serde_json::to_vec(&frontend).unwrap(),
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
            test_filter: None,
        });
        assert!(matches!(
            result,
            Err(ReportError::InvalidJson { path, .. })
                if path == "playwright-worker-1.mcdc.jsonl"
        ));
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn a_torn_background_record_is_counted_not_fatal() {
        // Pool VMs restored from one snapshot are clones with the same pid and
        // cached shard path; their appends over a shared mount tear lines. One
        // torn line is one lost record: the report must still build, count it,
        // and surface it through the corrupt-evidence accounting.
        let manifest = CoverageManifest {
            unmeasured: Vec::new(),
            decisions: vec![],
            points: vec![],
            branches: vec![],
            limitations: vec![],
            scope: None,
            assertion_sites: Vec::new(),
        };
        // Declare exactly the observed runners: the journal's node:test and the
        // synthesized background runner, shaped as the JavaScript run declares it.
        let unattributed = |axis: &str| {
            serde_json::json!({
                "id": format!("background-no-{axis}"),
                "scopes": [axis],
                "reason": format!("Runner background did not expose exact {axis} identity for every result")
            })
        };
        let frontend = serde_json::json!({
            "protocolVersion": 2,
            "frontendId": "javascript",
            "frontendVersion": "fixture-v1",
            "language": "javascript",
            "structuralSource": "owned-probes",
            "runners": [
                {
                    "runner": "node:test",
                    "executionModel": "serial-in-process",
                    "attribution": {
                        "run": "exact", "worker": "unavailable", "test": "exact", "retry": "exact",
                        "phase": "exact", "action": "exact", "assertion": "exact"
                    },
                    "limitations": [{
                        "id": "fixture-worker-unavailable",
                        "scopes": ["worker"],
                        "reason": "The fixture does not require worker identity"
                    }]
                },
                {
                    "runner": "background",
                    "executionModel": "parallel-unattributed",
                    "attribution": {
                        "run": "exact", "worker": "unavailable", "test": "unavailable", "retry": "unavailable",
                        "phase": "unavailable", "action": "unavailable", "assertion": "unavailable"
                    },
                    "limitations": [
                        unattributed("worker"), unattributed("test"), unattributed("retry"),
                        unattributed("phase"), unattributed("action"), unattributed("assertion")
                    ]
                }
            ],
            "structuralLimitations": []
        });
        let path = archive(vec![
            EvidenceArchiveEntry {
                path: "manifest.json".into(),
                contents: serde_json::to_vec(&manifest).unwrap(),
            },
            EvidenceArchiveEntry {
                path: "frontend.json".into(),
                contents: serde_json::to_vec(&frontend).unwrap(),
            },
            EvidenceArchiveEntry {
                path: "playwright-worker-1.mcdc.jsonl".into(),
                contents: {
                    let mut contents =
                        serde_json::to_vec(&raw("journal-test", 0, "passed", &["test-hit"]))
                            .unwrap();
                    contents.push(b'\n');
                    contents
                },
            },
            EvidenceArchiveEntry {
                path: "execution.37360-1.168.jsonl".into(),
                contents: b"418836ceb3ac9b6277f\"}}\n".to_vec(),
            },
            EvidenceArchiveEntry {
                path: "server/background/process-5118-a1b2c3d4-0.jsonl".into(),
                contents: concat!(
                    "{\"type\":\"hit\",\"id\":\"background-hit\"}\n",
                    "{\"type\":\"decision\",\"meta\":{\"id\":\"torn\",\"file\":\"app/rou{\"type\":\"hit\",\"id\":\"other-clone\"}\n",
                    "{\"type\":\"hit\",\"id\":\"after-tear\"}\n",
                )
                .as_bytes()
                .to_vec(),
            },
            // Files beside the evidence that are not its records are not
            // read as records: these would be three more corrupt ones.
            EvidenceArchiveEntry {
                path: "server/notes.txt".into(),
                contents: b"not a record\n".to_vec(),
            },
            EvidenceArchiveEntry {
                path: "server/background/process-1.log".into(),
                contents: b"not a record\n".to_vec(),
            },
            EvidenceArchiveEntry {
                path: "execution.37360-1.txt".into(),
                contents: b"not a record\n".to_vec(),
            },
        ]);
        let report = analyze_coverage_archive(&ArchiveReportRequest {
            archive_path: path.clone(),
            run_id: "run".into(),
            generated_at: "time".into(),
            integrity: None,
            test_exit_code: ExitCodeInput::Missing,
            test_filter: None,
        })
        .expect("a torn evidence line must not fail the whole run");
        let transport = report.view.transport.expect("transport stats");
        assert_eq!(transport.corrupt_records, 2);
        assert_eq!(transport.corrupt_files, 2);
        assert_eq!(transport.background_server_records, 2);
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn archive_analysis_rejects_cross_run_evidence() {
        let manifest = CoverageManifest {
            unmeasured: Vec::new(),
            decisions: vec![],
            points: vec![],
            branches: vec![],
            limitations: vec![],
            scope: None,
            assertion_sites: Vec::new(),
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
                test_filter: None,
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
            unmeasured: Vec::new(),
            decisions: vec![],
            points: vec![],
            branches: vec![],
            limitations: vec![],
            scope: None,
            assertion_sites: Vec::new(),
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
                test_filter: None,
            }),
            Err(ReportError::InvalidArchive(reason))
                if reason.contains("javascript differs from coverage model language rust")
        ));
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
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
    #[test]
    fn a_phase_leaked_from_another_test_is_untagged_and_an_unknown_one_is_kept() {
        let record = |test: &str, phases: &[&str], events: &[(&str, &str)]| -> RawTestResult {
            serde_json::from_value(serde_json::json!({
                "test": test,
                "phases": phases.iter().map(|id| serde_json::json!({
                    "id": id, "kind": "assertion", "operation": "assert.ok", "startedAtMs": 0
                })).collect::<Vec<_>>(),
                "runtime": [{
                    "events": events.iter().map(|(id, phase)| serde_json::json!({
                        "type": "hit", "id": id, "timestampMs": 0, "phaseId": phase, "environment": "server"
                    })).collect::<Vec<_>>()
                }]
            }))
            .unwrap()
        };
        let mut results = vec![
            record(
                "upload",
                &["a-0:assertion:1"],
                &[("own", "a-0:assertion:1")],
            ),
            // The stream the upload left settling ran during the next test.
            record(
                "form data",
                &["b-0:assertion:1"],
                &[
                    ("late", "a-0:assertion:1"),
                    ("mine", "b-0:assertion:1"),
                    ("corrupt", "z-0:assertion:9"),
                ],
            ),
        ];
        release_leaked_phases(&mut results);
        let phases = |raw: &RawTestResult| {
            raw.runtime[0]
                .events
                .iter()
                .map(|event| (event.id.clone(), event.phase_id.clone()))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            phases(&results[0]),
            [("own".into(), Some("a-0:assertion:1".into()))]
        );
        assert_eq!(
            phases(&results[1]),
            [
                ("late".into(), None),
                ("mine".into(), Some("b-0:assertion:1".into())),
                // No record declares it: that is corruption, and it is refused later.
                ("corrupt".into(), Some("z-0:assertion:9".into())),
            ]
        );
    }

    #[test]
    fn a_test_run_twice_under_one_identity_keeps_both_executions_apart() {
        // ms runs its Jest suite twice, in Node's environment and the edge
        // runtime's; each process numbers the same test's attempt 0.
        let record = |status: &str, events: &[(&str, &str)]| -> RawTestResult {
            serde_json::from_value(serde_json::json!({
                "test": "parse",
                "testId": "parse",
                "retry": 0,
                "status": status,
                "phases": [
                    { "id": "t-0:assertion:1", "kind": "assertion", "operation": "expect", "startedAtMs": 0 },
                    { "id": "t-0:assertion:2", "kind": "assertion", "operation": "expect", "startedAtMs": 0, "causedByPhaseId": "t-0:assertion:1" }
                ],
                "runtime": [{
                    "events": events.iter().map(|(id, phase)| serde_json::json!({
                        "type": "hit", "id": id, "timestampMs": 0, "phaseId": phase, "environment": "server"
                    })).collect::<Vec<_>>()
                }]
            }))
            .unwrap()
        };
        let mut results = vec![
            record("failed", &[("node", "t-0:assertion:1")]),
            record("passed", &[("edge", "t-0:assertion:2")]),
            record("passed", &[("third", "t-0:assertion:1")]),
        ];
        separate_repeated_executions(&mut results);
        let ids = |raw: &RawTestResult| {
            raw.phases
                .iter()
                .map(|phase| (phase.id.clone(), phase.caused_by_phase_id.clone()))
                .collect::<Vec<_>>()
        };
        let events = |raw: &RawTestResult| {
            raw.runtime[0]
                .events
                .iter()
                .map(|event| event.phase_id.clone().unwrap())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            ids(&results[0]),
            [
                ("t-0:assertion:1".into(), None),
                ("t-0:assertion:2".into(), Some("t-0:assertion:1".into()))
            ]
        );
        assert_eq!(
            ids(&results[1]),
            [
                ("t-0:assertion:1~2".into(), None),
                ("t-0:assertion:2~2".into(), Some("t-0:assertion:1~2".into()))
            ]
        );
        assert_eq!(events(&results[1]), ["t-0:assertion:2~2"]);
        assert_eq!(events(&results[2]), ["t-0:assertion:1~3"]);

        let mut test = MutableTest {
            id: "parse".into(),
            name: "parse".into(),
            file: None,
            title: None,
            retries: BTreeSet::new(),
            attempts: BTreeMap::new(),
            unstarted: false,
            runner_reported_flaky: false,
            provenance: TestProvenance::default(),
            role: "test".into(),
            attribution: "exact".into(),
            hits: Vec::new(),
            decisions: BTreeMap::new(),
        };
        for raw in &results {
            record_attempt(&mut test, raw);
        }
        assert_eq!(
            test_outcome(&test),
            "failed",
            "a failed execution fails the attempt"
        );
    }

    #[test]
    fn a_dimension_is_summarised_over_the_tests_it_can_describe() {
        // A test whose reach nothing recorded contributes no hits. Counting it
        // in the denominator of a coverage percentage reported a package
        // covered entirely by parallel tests as covered 0.00% -- a wrong
        // number, which is worse than a missing one, because it reads as a
        // suite that tests nothing.
        let exact = |id: &str, kind: &str| TestCoverageResult {
            id: id.into(),
            name: id.into(),
            file: None,
            title: None,
            retries: vec![],
            attempts: vec![],
            outcome: "passed".into(),
            provenance: TestProvenance {
                runner: "go-test".into(),
                kind: kind.into(),
                project: None,
                source: String::new(),
                tags: Vec::new(),
            },
            role: "test".into(),
            attribution: ATTRIBUTION_EXACT.into(),
            hits: vec![],
            decisions: vec![],
            lines: vec![],
        };
        let mut run_wide = exact("parallel", "unit");
        run_wide.attribution = ATTRIBUTION_RUN_WIDE.into();

        // Whose coverage it is, and whether all of it is there, are separate
        // questions, and conflating them gets one of them wrong.
        //
        // `partial` coverage is the test's own -- Ruby credits a line to the
        // first test that reaches it, and that crediting is true -- so it
        // counts in a percentage, and a union over such tests is a union of
        // things that happened. What it cannot support is the opposite
        // inference: that a test not recorded against a line did not run it.
        assert!(coverage_is_its_own(&exact("serial", "unit").attribution));
        assert!(coverage_is_its_own(ATTRIBUTION_PARTIAL));
        assert!(!coverage_is_its_own(&run_wide.attribution));

        assert!(coverage_is_complete(ATTRIBUTION_EXACT));
        assert!(!coverage_is_complete(ATTRIBUTION_PARTIAL));
        assert!(!coverage_is_complete(ATTRIBUTION_RUN_WIDE));

        // A test's records merge to the least exact claim among them, in
        // whichever order they arrive.
        for (left, right, merged) in [
            (ATTRIBUTION_EXACT, ATTRIBUTION_PARTIAL, ATTRIBUTION_PARTIAL),
            (ATTRIBUTION_PARTIAL, ATTRIBUTION_EXACT, ATTRIBUTION_PARTIAL),
            (
                ATTRIBUTION_PARTIAL,
                ATTRIBUTION_RUN_WIDE,
                ATTRIBUTION_RUN_WIDE,
            ),
            (ATTRIBUTION_EXACT, ATTRIBUTION_EXACT, ATTRIBUTION_EXACT),
        ] {
            assert_eq!(least_exact(left, right), merged);
        }

        // And a frontend that has never heard of the field keeps its meaning:
        // absent is exact, so nothing already measured changes shape.
        assert!(coverage_is_its_own(""));
        assert!(coverage_is_complete(""));
    }

    /// Recognized JSONL evidence ends with a newline, holds a record, and
    /// has no blank line; the MC/DC files are told by their names.
    #[test]
    fn jsonl_evidence_is_refused_for_each_way_it_can_be_malformed() {
        let entry = |contents: &str| EvidenceArchiveEntry {
            path: "probe.jsonl".into(),
            contents: contents.as_bytes().to_vec(),
        };
        let read = |contents: &str| match parse_json_lines::<serde_json::Value>(std::iter::once(
            &entry(contents),
        )) {
            Ok(records) => format!("{} records", records.len()),
            Err(ReportError::InvalidJson { reason, .. }) => reason,
            Err(error) => format!("{error:?}"),
        };
        assert_eq!(read("{}\n{}\n"), "2 records");
        assert!(read("{}").contains("must end with a newline"));
        assert!(read("\n").contains("at least one record"));
        assert!(read("{}\n\n{}\n").contains("blank JSONL record at line 2"));
        assert!(read("{}\nnope\n").contains("invalid JSONL record at line 2"));
        for (path, result, journal) in [
            ("mcdc.json", true, false),
            ("worker/mcdc.json", true, false),
            ("xmcdc.json", false, false),
            ("mcdc.jsonl", false, true),
            ("worker-1.mcdc.jsonl", false, true),
            ("mcdc.jsonl.gz", false, false),
        ] {
            assert_eq!(
                (is_mcdc_result(path), is_mcdc_journal(path)),
                (result, journal),
                "{path}"
            );
        }
    }
}
