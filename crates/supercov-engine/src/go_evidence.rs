//! Turning what the Go runtime wrote into the model the report reads.
//!
//! The transport is deliberately dumb — sparse pairs of probe ids and packed
//! vector words — because everything it could have computed instead is cheaper
//! to compute here, once, than in a process that is trying to run tests. The
//! mapping from probe id back to obligation lives in the manifest this module
//! is handed, so the runtime never carries a string.

use std::collections::{BTreeMap, BTreeSet};

use supercov_contracts::{
    AttributionPrecision, ExecutionModel, FrontendAttribution, FrontendLimitation,
    FrontendLimitationScope, FrontendRunDeclaration, FrontendRunnerDeclaration,
};

use crate::coverage_analysis::McdcVector;
use crate::coverage_report::{
    CoverageManifest, CoverageReportRequest, DecisionSnapshot, ExitCodeInput, RawTestResult,
    RuntimeEvent, RuntimeSnapshot, TestProvenance,
};
use crate::go_instrumenter::{GoProbe, GoProbeTarget};

pub const FRONTEND_ID: &str = "supercov-go";
pub const FRONTEND_VERSION: &str = "go-owned-v1";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GoEvidenceError {
    Truncated(&'static str),
    NoTests,
}

impl std::fmt::Display for GoEvidenceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GoEvidenceError::Truncated(part) => {
                write!(f, "Go coverage evidence ended in the middle of its {part}")
            }
            GoEvidenceError::NoTests => write!(
                f,
                "the Go run produced coverage evidence but no test announced itself"
            ),
        }
    }
}

/// One evaluation of a decision, as the runtime packed it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PackedVector {
    pub decision: u32,
    pub key: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct GoTestEvidence {
    pub name: String,
    /// Probe id to the bitmask it was observed with.
    pub probes: BTreeMap<u32, u32>,
    pub vectors: Vec<PackedVector>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct GoEvidence {
    /// Run-wide probe totals, the union of every test's.
    pub global: Vec<u32>,
    pub tests: Vec<GoTestEvidence>,
    /// Conditions per decision, as the manifest ordered them.
    pub widths: Vec<u8>,
    /// Every distinct vector each decision produced, run-wide.
    pub decision_vectors: Vec<Vec<u64>>,
}

struct Cursor<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl Cursor<'_> {
    fn u64(&mut self, part: &'static str) -> Result<u64, GoEvidenceError> {
        let end = self.offset + 8;
        let slice = self
            .bytes
            .get(self.offset..end)
            .ok_or(GoEvidenceError::Truncated(part))?;
        self.offset = end;
        Ok(u64::from_le_bytes(slice.try_into().expect("eight bytes")))
    }

    fn text(&mut self, length: usize, part: &'static str) -> Result<String, GoEvidenceError> {
        let end = self.offset + length;
        let slice = self
            .bytes
            .get(self.offset..end)
            .ok_or(GoEvidenceError::Truncated(part))?;
        self.offset = end;
        String::from_utf8(slice.to_vec()).map_err(|_| GoEvidenceError::Truncated(part))
    }
}

/// Read the transport. A truncated file is an error rather than a short read:
/// a run that was killed half way through has partial evidence, and reporting
/// it as though it were complete would understate coverage as a fact.
pub fn read_evidence(bytes: &[u8]) -> Result<GoEvidence, GoEvidenceError> {
    let mut cursor = Cursor { bytes, offset: 0 };
    let probe_count = cursor.u64("probe totals")? as usize;
    let mut global = Vec::with_capacity(probe_count);
    for _ in 0..probe_count {
        global.push(cursor.u64("probe totals")? as u32);
    }
    let test_count = cursor.u64("test count")? as usize;
    let mut tests = Vec::with_capacity(test_count);
    for _ in 0..test_count {
        let length = cursor.u64("test name")? as usize;
        let name = cursor.text(length, "test name")?;
        let hits = cursor.u64("test probes")? as usize;
        let mut probes = BTreeMap::new();
        for _ in 0..hits {
            let index = cursor.u64("test probes")? as u32;
            probes.insert(index, cursor.u64("test probes")? as u32);
        }
        let vector_count = cursor.u64("test vectors")? as usize;
        let mut vectors = Vec::with_capacity(vector_count);
        for _ in 0..vector_count {
            let decision = cursor.u64("test vectors")? as u32;
            vectors.push(PackedVector {
                decision,
                key: cursor.u64("test vectors")?,
            });
        }
        tests.push(GoTestEvidence {
            name,
            probes,
            vectors,
        });
    }
    let decision_count = cursor.u64("decision table")? as usize;
    let mut widths = Vec::with_capacity(decision_count);
    let mut decision_vectors = Vec::with_capacity(decision_count);
    for _ in 0..decision_count {
        widths.push(cursor.u64("decision table")? as u8);
        let keys = cursor.u64("decision table")? as usize;
        let mut seen = Vec::with_capacity(keys);
        for _ in 0..keys {
            seen.push(cursor.u64("decision table")?);
        }
        decision_vectors.push(seen);
    }
    Ok(GoEvidence {
        global,
        tests,
        widths,
        decision_vectors,
    })
}

const PACKED_VALUE_SHIFT: u32 = 24;
const PACKED_OUTCOME_SHIFT: u32 = 48;

/// Unpack one evaluation into the vector MC/DC is judged from.
///
/// A condition the run never evaluated is `None`, not `false`. That is the
/// distinction the whole measurement rests on: `a && b` with `a` false says
/// nothing about `b`, and recording it as `false` would claim it had been
/// tested.
pub fn unpack_vector(key: u64, width: u8) -> McdcVector {
    let evaluated = key & ((1 << PACKED_VALUE_SHIFT) - 1);
    let values = (key >> PACKED_VALUE_SHIFT) & ((1 << PACKED_VALUE_SHIFT) - 1);
    McdcVector {
        values: (0..width)
            .map(|index| {
                let bit = 1_u64 << index;
                (evaluated & bit != 0).then_some(values & bit != 0)
            })
            .collect(),
        outcome: (key >> PACKED_OUTCOME_SHIFT) & 1 == 1,
    }
}

/// How a test's name and package became the identity the report uses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GoTestOutcome {
    pub name: String,
    pub package: String,
    pub file: Option<String>,
    /// `passed`, `failed` or `skipped`, as the runner reported it.
    pub status: String,
}

pub fn declaration() -> FrontendRunDeclaration {
    FrontendRunDeclaration {
        protocol_version: 1,
        frontend_id: FRONTEND_ID.into(),
        frontend_version: FRONTEND_VERSION.into(),
        language: "go".into(),
        // Supercov owns the probes: Go's own cover tool is a development
        // oracle here, not a product input.
        structural_source: supercov_contracts::StructuralSource::OwnedProbes,
        runners: vec![FrontendRunnerDeclaration {
            runner: "go test".into(),
            // Go runs a package's tests one after another unless a test opts
            // into parallelism, and Supercov runs one package at a time.
            execution_model: ExecutionModel::SerialInProcess,
            attribution: FrontendAttribution {
                run: AttributionPrecision::Exact,
                worker: AttributionPrecision::Exact,
                test: AttributionPrecision::Exact,
                retry: AttributionPrecision::Exact,
                phase: AttributionPrecision::Aggregate,
                action: AttributionPrecision::Unavailable,
                assertion: AttributionPrecision::Unavailable,
            },
            limitations: vec![FrontendLimitation {
                id: "go-parallel-tests".into(),
                scopes: vec![FrontendLimitationScope::Test],
                reason:
                    "a test that calls t.Parallel() runs alongside others, so work it does after that call cannot be attributed to it alone"
                        .into(),
            }],
        }],
        structural_limitations: Vec::new(),
    }
}

/// The obligation each probe answers for, by id.
fn obligations(probes: &BTreeMap<u64, GoProbe>) -> BTreeMap<u32, String> {
    probes
        .iter()
        .map(|(id, probe)| {
            let obligation = match &probe.target {
                GoProbeTarget::Statement { id } | GoProbeTarget::Function { id } => id.clone(),
                GoProbeTarget::Alternative { alternative, .. } => alternative.clone(),
            };
            (*id as u32, obligation)
        })
        .collect()
}

fn snapshot(
    evidence: &GoTestEvidence,
    manifest: &CoverageManifest,
    by_probe: &BTreeMap<u32, String>,
) -> RuntimeSnapshot {
    let mut events = Vec::new();
    let mut clock = 0_i64;
    let hits = evidence
        .probes
        .keys()
        .filter_map(|probe| by_probe.get(probe).cloned())
        .collect::<BTreeSet<_>>();
    for id in &hits {
        events.push(RuntimeEvent {
            event_type: "hit".into(),
            id: id.clone(),
            vector: None,
            timestamp_ms: clock,
            phase_id: Some("call".into()),
            statement_id: None,
            environment: "go".into(),
        });
        clock += 1;
    }
    let mut by_decision: BTreeMap<u32, Vec<u64>> = BTreeMap::new();
    for vector in &evidence.vectors {
        by_decision
            .entry(vector.decision)
            .or_default()
            .push(vector.key);
    }
    let mut decisions = Vec::new();
    for (index, meta) in manifest.decisions.iter().enumerate() {
        let Some(keys) = by_decision.get(&(index as u32)) else {
            continue;
        };
        let width = meta.conditions.len() as u8;
        let observed = keys
            .iter()
            .map(|key| unpack_vector(*key, width))
            .collect::<Vec<_>>();
        for vector in &observed {
            events.push(RuntimeEvent {
                event_type: "decision".into(),
                id: meta.id.clone(),
                vector: Some(vector.clone()),
                timestamp_ms: clock,
                phase_id: Some("call".into()),
                statement_id: None,
                environment: "go".into(),
            });
            clock += 1;
        }
        decisions.push(DecisionSnapshot {
            meta: meta.clone(),
            vectors: observed,
        });
    }
    RuntimeSnapshot {
        decisions,
        hits: hits.into_iter().collect(),
        events,
        logicals: Vec::new(),
    }
}

pub struct GoFrontendRun {
    pub declaration: FrontendRunDeclaration,
    pub request: CoverageReportRequest,
    pub tests: usize,
}

/// Build the report request from what the run recorded.
///
/// A test the runner reported but that announced nothing still becomes a
/// result, with no coverage. Dropping it would make a suite look smaller than
/// it is, and a test that ran without reaching any measured line is a fact
/// worth seeing rather than an absence worth hiding.
pub fn build_go_frontend_run(
    manifest: &CoverageManifest,
    probes: &BTreeMap<u64, GoProbe>,
    evidence: &GoEvidence,
    outcomes: &[GoTestOutcome],
    run_id: &str,
    generated_at: &str,
    test_exit_code: i32,
) -> Result<GoFrontendRun, GoEvidenceError> {
    if outcomes.is_empty() {
        return Err(GoEvidenceError::NoTests);
    }
    let by_probe = obligations(probes);
    let recorded = evidence
        .tests
        .iter()
        .map(|test| (test.name.clone(), test))
        .collect::<BTreeMap<_, _>>();
    let empty = GoTestEvidence::default();
    let raw_results = outcomes
        .iter()
        .map(|outcome| {
            let test = recorded.get(&outcome.name).copied().unwrap_or(&empty);
            RawTestResult {
                test_id: Some(format!("{}::{}", outcome.package, outcome.name)),
                scope: None,
                test: outcome.name.clone(),
                test_file: outcome.file.clone(),
                title: None,
                retry: Some(0),
                status: Some(outcome.status.clone()),
                expected_status: None,
                flaky: false,
                provenance: TestProvenance::default(),
                role: "test".into(),
                phases: Vec::new(),
                runtime: vec![snapshot(test, manifest, &by_probe)],
                browser: Vec::new(),
                server: Vec::new(),
            }
        })
        .collect::<Vec<_>>();
    Ok(GoFrontendRun {
        declaration: declaration(),
        tests: raw_results.len(),
        request: CoverageReportRequest {
            run_id: run_id.to_owned(),
            manifest: manifest.clone(),
            raw_results,
            generated_at: generated_at.to_owned(),
            coverage_model: None,
            integrity: None,
            test_exit_code: ExitCodeInput::Present(Some(test_exit_code)),
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(values: &[u64]) -> Vec<u8> {
        values.iter().flat_map(|v| v.to_le_bytes()).collect()
    }

    #[test]
    fn an_unevaluated_condition_is_absent_rather_than_false() {
        // The distinction the whole measurement rests on. `a && b` with `a`
        // false says nothing about `b`; recording it as false would claim it
        // had been tested, and independence would be judged from a fact that
        // never happened.
        let short_circuited = unpack_vector(0b01, 2);
        assert_eq!(short_circuited.values, [Some(false), None]);
        assert!(!short_circuited.outcome);

        let both = unpack_vector(0b11 | (0b11 << 24) | (1 << 48), 2);
        assert_eq!(both.values, [Some(true), Some(true)]);
        assert!(both.outcome);

        // A condition evaluated and false is Some(false), which is a different
        // vector from the one above.
        let first_false = unpack_vector(0b11 | (0b01 << 24), 2);
        assert_eq!(first_false.values, [Some(true), Some(false)]);
    }

    #[test]
    fn a_truncated_file_is_refused_rather_than_read_short() {
        // A run killed part way through has partial evidence. Reading what
        // survived and reporting it as complete would understate coverage as
        // though it were a fact about the tests.
        let full = write(&[2, 0, 2, 1, 4, u64::from_le_bytes(*b"Test\0\0\0\0"), 0, 0, 0]);
        for cut in [0, 8, 16, 24, 32] {
            assert!(
                read_evidence(&full[..cut.min(full.len())]).is_err(),
                "a file cut at {cut} bytes must not decode"
            );
        }
        assert!(matches!(
            read_evidence(&write(&[5, 1])),
            Err(GoEvidenceError::Truncated("probe totals"))
        ));
    }

    #[test]
    fn a_run_with_no_tests_is_an_error_not_an_empty_report() {
        let evidence = GoEvidence::default();
        let manifest = CoverageManifest {
            decisions: Vec::new(),
            points: Vec::new(),
            branches: Vec::new(),
            limitations: Vec::new(),
            unmeasured: Vec::new(),
            scope: None,
        };
        assert_eq!(
            build_go_frontend_run(&manifest, &BTreeMap::new(), &evidence, &[], "run", "now", 0)
                .err(),
            Some(GoEvidenceError::NoTests)
        );
    }

    #[test]
    fn the_declaration_says_what_go_can_and_cannot_attribute() {
        // A frontend that overstates its precision is worse than one that
        // admits a gap: the report would present guesses as measurements.
        let declared = declaration();
        let runner = &declared.runners[0];
        assert_eq!(runner.execution_model, ExecutionModel::SerialInProcess);
        assert_eq!(runner.attribution.test, AttributionPrecision::Exact);
        assert_eq!(
            runner.attribution.assertion,
            AttributionPrecision::Unavailable
        );
        assert_eq!(runner.limitations[0].id, "go-parallel-tests");
    }

    #[test]
    fn a_test_the_runner_saw_but_that_recorded_nothing_still_appears() {
        // Dropping it would make the suite look smaller than it is, and a test
        // that reached no measured line is a fact worth seeing.
        let manifest = CoverageManifest {
            decisions: Vec::new(),
            points: Vec::new(),
            branches: Vec::new(),
            limitations: Vec::new(),
            unmeasured: Vec::new(),
            scope: None,
        };
        let run = build_go_frontend_run(
            &manifest,
            &BTreeMap::new(),
            &GoEvidence::default(),
            &[GoTestOutcome {
                name: "TestSilent".into(),
                package: "example.com/p".into(),
                file: Some("p/x_test.go".into()),
                status: "passed".into(),
            }],
            "run",
            "now",
            0,
        )
        .expect("run");
        assert_eq!(run.tests, 1);
        let result = &run.request.raw_results[0];
        assert_eq!(result.test, "TestSilent");
        assert_eq!(result.test_id.as_deref(), Some("example.com/p::TestSilent"));
        assert!(result.runtime[0].hits.is_empty());
    }
}
