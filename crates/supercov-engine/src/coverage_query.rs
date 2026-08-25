//! Language-neutral coverage query operators.
//!
//! Querying is deliberately separated from the CLI and storage container.
//! This module accepts the frozen analyzed view and owns structural query
//! semantics shared by every language frontend.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use serde::{Deserialize, Serialize};

use crate::{
    agent_json::pagination,
    coverage_analysis::{CoverageSummary, is_independence_pair},
    coverage_index::{
        CoverageDimension, CoverageIndex, CoverageIndexError, CoverageViewId, IndexedDecisionGap,
        IndexedDimensionCoverage, IndexedFileGap, IndexedHitMetadata, IndexedMeasurement,
        IndexedOutcomeCounts, IndexedScopeEntry, IndexedSourceScope, IndexedSummaryConfidence,
        IndexedTestSummary,
    },
    coverage_report::{
        CoverageConfidence, CoverageReportRequest, CoverageView, DecisionMeta, ReportError,
        SourceLine, TestAttempt, TestProvenance, TransportStats, analyze_coverage_results,
        coverage_summary_for_tests,
    },
};
use supercov_contracts::AgentPagination;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MinimizeMetric {
    All,
    Lines,
    Statements,
    Functions,
    Branches,
    Mcdc,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MinimumTestSetResult {
    pub optimal: bool,
    pub target: f64,
    pub metric: MinimizeMetric,
    pub selected: Vec<String>,
    pub expanded: Vec<String>,
    pub summary: CoverageSummary,
    pub explored_states: usize,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MinimumTestSetRequest {
    pub coverage: CoverageReportRequest,
    #[serde(default = "default_target")]
    pub target: f64,
    #[serde(default = "default_metric")]
    pub metric: MinimizeMetric,
    #[serde(default = "default_max_states")]
    pub max_states: usize,
}

fn default_target() -> f64 {
    100.0
}

fn default_metric() -> MinimizeMetric {
    MinimizeMetric::All
}

fn default_max_states() -> usize {
    5_000
}

pub fn minimum_test_set_for_request(
    request: &MinimumTestSetRequest,
) -> Result<MinimumTestSetResult, QueryError> {
    let report = analyze_coverage_results(&request.coverage)?;
    minimum_test_set(
        &report.view,
        request.target,
        request.metric,
        request.max_states,
    )
}

#[derive(Debug)]
pub enum QueryError {
    InvalidTarget(f64),
    UnattributedEvidence,
    TargetUnreachable {
        metric: MinimizeMetric,
        target: f64,
        reachable: f64,
    },
    ComplexityLimit {
        candidate_tests: usize,
        obligations: usize,
        explored_states: usize,
        max_states: usize,
        target: f64,
        metric: MinimizeMetric,
    },
    Analysis(ReportError),
    Index(CoverageIndexError),
    InvalidPagination,
    InvalidRecordSelection,
    ScopeUnavailable,
}

impl From<ReportError> for QueryError {
    fn from(value: ReportError) -> Self {
        Self::Analysis(value)
    }
}

impl From<CoverageIndexError> for QueryError {
    fn from(value: CoverageIndexError) -> Self {
        Self::Index(value)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CoverageQueryFilters {
    pub outcome: String,
    pub kind: Option<String>,
    pub runner: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CoverageFilesData {
    pub run: String,
    pub filters: CoverageQueryFilters,
    pub metric: MinimizeMetric,
    pub files: Vec<IndexedFileGap>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CoverageGapsData {
    pub run: String,
    pub filters: CoverageQueryFilters,
    pub metric: MinimizeMetric,
    pub gaps: Vec<IndexedFileGap>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CoverageKindsData {
    pub run: String,
    pub filters: CoverageQueryFilters,
    pub kinds: Vec<IndexedDimensionCoverage>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CoverageRunnersData {
    pub run: String,
    pub filters: CoverageQueryFilters,
    pub runners: Vec<IndexedDimensionCoverage>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CoverageDiagnostic {
    pub code: String,
    pub severity: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CoverageSummaryData {
    pub run: String,
    pub filters: CoverageQueryFilters,
    pub generated_at: String,
    pub valid: bool,
    pub stale: bool,
    pub stale_reasons: Vec<String>,
    pub structurally_complete: bool,
    pub complete: bool,
    pub coverage: CoverageSummary,
    pub measurement: IndexedMeasurement,
    pub coverage_by_kind: Vec<IndexedDimensionCoverage>,
    pub coverage_by_runner: Vec<IndexedDimensionCoverage>,
    pub attribution: crate::coverage_index::IndexedAttribution,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transport: Option<TransportStats>,
    pub diagnostics: Vec<CoverageDiagnostic>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confidence: Option<IndexedSummaryConfidence>,
    pub files_with_gaps: usize,
    pub files_with_coverage_gaps: usize,
    pub files_with_measurement_limitations: usize,
    pub tests: usize,
    pub setups: usize,
    pub test_outcomes: IndexedOutcomeCounts,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_scope: Option<IndexedSourceScope>,
}

#[derive(Debug, Clone)]
pub struct CoverageSummaryQueryOptions<'a> {
    pub run: &'a str,
    pub view: CoverageViewId,
    pub kind: Option<&'a str>,
    pub runner: Option<&'a str>,
    pub valid: bool,
    pub stale: bool,
    pub stale_reasons: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ScopeCounts {
    pub included: usize,
    pub excluded: usize,
    pub ambiguous: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CoverageScopeData {
    pub run: String,
    pub filters: CoverageQueryFilters,
    pub mode: String,
    pub roots: Vec<String>,
    pub counts: ScopeCounts,
    pub measurement: IndexedMeasurement,
    pub entries: Vec<IndexedScopeEntry>,
}

#[derive(Debug, Clone, Copy)]
pub struct CoverageScopeQueryOptions<'a> {
    pub run: &'a str,
    pub view: CoverageViewId,
    pub kind: Option<&'a str>,
    pub runner: Option<&'a str>,
    pub offset: usize,
    pub limit: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CoverageLocation {
    pub file: String,
    pub line: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CoverageCoveringTest {
    pub id: String,
    pub name: String,
    pub provenance: TestProvenance,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CoverageCoveringPhase {
    pub id: String,
    pub kind: String,
    pub operation: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    pub test: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub caused_by_phase_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CoverageAnchor {
    pub kind: String,
    pub id: String,
    pub column: usize,
    pub covered: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub covered_conditions: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub conditions: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CoverageCoversLineData {
    pub run: String,
    pub filters: CoverageQueryFilters,
    pub location: CoverageLocation,
    pub covered: bool,
    pub confidence: CoverageConfidence,
    pub total_tests: usize,
    pub total_phases: usize,
    pub tests: Vec<CoverageCoveringTest>,
    pub phases: Vec<CoverageCoveringPhase>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CoverageCoversAnchorsData {
    pub run: String,
    pub filters: CoverageQueryFilters,
    pub location: CoverageLocation,
    pub line_obligation: bool,
    pub anchored: Vec<CoverageAnchor>,
    pub total_anchored: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(untagged)]
pub enum CoverageCoversData {
    Line(CoverageCoversLineData),
    Anchors(CoverageCoversAnchorsData),
}

#[derive(Debug, Clone, Copy)]
pub struct CoverageCoversQueryOptions<'a> {
    pub run: &'a str,
    pub view: CoverageViewId,
    pub kind: Option<&'a str>,
    pub runner: Option<&'a str>,
    pub file: &'a str,
    pub line: usize,
    pub offset: usize,
    pub limit: usize,
}

fn query_filters(
    view: CoverageViewId,
    kind: Option<&str>,
    runner: Option<&str>,
) -> CoverageQueryFilters {
    CoverageQueryFilters {
        outcome: match view {
            CoverageViewId::All => "all",
            CoverageViewId::Passed => "passed",
            CoverageViewId::Failed => "failed",
        }
        .into(),
        kind: kind.map(str::to_owned),
        runner: runner.map(str::to_owned),
    }
}

fn selected_test_ids(
    tests: &[IndexedTestSummary],
    kind: Option<&str>,
    runner: Option<&str>,
) -> Result<Option<BTreeSet<String>>, QueryError> {
    if kind.is_none() && runner.is_none() {
        return Ok(None);
    }
    let selected = tests
        .iter()
        .filter(|test| {
            kind.is_none_or(|kind| test.provenance.kind == kind)
                && runner.is_none_or(|runner| test.provenance.runner == runner)
        })
        .map(|test| test.id.clone())
        .collect::<BTreeSet<_>>();
    if selected.is_empty() {
        return Err(QueryError::InvalidRecordSelection);
    }
    Ok(Some(selected))
}

pub fn coverage_covers_query(
    index: &CoverageIndex<'_>,
    options: CoverageCoversQueryOptions<'_>,
) -> Result<(CoverageCoversData, AgentPagination), QueryError> {
    if options.limit == 0 {
        return Err(QueryError::InvalidPagination);
    }
    let tests = index.test_summaries(options.view)?;
    let selected = selected_test_ids(&tests, options.kind, options.runner)?;
    let selected_includes = |id: &str| selected.as_ref().is_none_or(|ids| ids.contains(id));
    let filters = query_filters(options.view, options.kind, options.runner);
    let location = CoverageLocation {
        file: options.file.into(),
        line: options.line,
    };
    let Some(line) = index.line(options.view, options.file, options.line)? else {
        let anchors = index.anchors(options.view, options.file, options.line)?;
        let total = anchors.len();
        let anchored = anchors
            .into_iter()
            .skip(options.offset)
            .take(options.limit)
            .map(|anchor| CoverageAnchor {
                kind: anchor.kind.clone(),
                id: anchor.id,
                column: anchor.column,
                covered: if matches!(anchor.kind.as_str(), "statement" | "function") {
                    anchor.tests.iter().any(|test| selected_includes(test))
                } else {
                    anchor.covered
                },
                covered_conditions: anchor.covered_conditions,
                conditions: anchor.conditions,
            })
            .collect::<Vec<_>>();
        let returned = anchored.len();
        return Ok((
            CoverageCoversData::Anchors(CoverageCoversAnchorsData {
                run: options.run.into(),
                filters,
                location,
                line_obligation: false,
                anchored,
                total_anchored: total,
            }),
            pagination(options.offset, options.limit, returned, total),
        ));
    };
    let tests_by_id = tests
        .into_iter()
        .map(|test| (test.id.clone(), test))
        .collect::<HashMap<_, _>>();
    let all_tests = line
        .tests
        .iter()
        .filter(|id| selected_includes(id))
        .map(|id| {
            let test = tests_by_id.get(id);
            CoverageCoveringTest {
                id: id.clone(),
                name: test.map_or_else(|| id.clone(), |test| test.name.clone()),
                provenance: test
                    .map_or_else(TestProvenance::default, |test| test.provenance.clone()),
            }
        })
        .collect::<Vec<_>>();
    let phases_by_id = index
        .phase_summaries(options.view)?
        .into_iter()
        .map(|phase| (phase.id.clone(), phase))
        .collect::<HashMap<_, _>>();
    let all_phases = line
        .phases
        .iter()
        .filter_map(|id| phases_by_id.get(id))
        .filter(|phase| selected_includes(&phase.test))
        .map(|phase| CoverageCoveringPhase {
            id: phase.id.clone(),
            kind: phase.kind.clone(),
            operation: phase.operation.clone(),
            source: phase.source.clone(),
            test: phase.test.clone(),
            status: phase.status.clone(),
            caused_by_phase_id: phase.caused_by_phase_id.clone(),
        })
        .collect::<Vec<_>>();
    let tests_page = all_tests
        .iter()
        .skip(options.offset)
        .take(options.limit)
        .cloned()
        .collect::<Vec<_>>();
    let phases_page = all_phases
        .iter()
        .skip(options.offset)
        .take(options.limit)
        .cloned()
        .collect::<Vec<_>>();
    let total = all_tests.len().max(all_phases.len());
    let returned = tests_page.len().max(phases_page.len());
    Ok((
        CoverageCoversData::Line(CoverageCoversLineData {
            run: options.run.into(),
            filters,
            location,
            covered: line.tests.iter().any(|test| selected_includes(test)),
            confidence: line.confidence,
            total_tests: all_tests.len(),
            total_phases: all_phases.len(),
            tests: tests_page,
            phases: phases_page,
        }),
        pagination(options.offset, options.limit, returned, total),
    ))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CoverageTestMatch {
    pub id: String,
    pub name: String,
    pub outcome: String,
    pub provenance: TestProvenance,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CoverageHitDetail {
    pub id: String,
    pub obligation: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch_kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub column: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub alternative: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CoverageTestDecision {
    pub id: String,
    pub vectors: Vec<crate::coverage_analysis::McdcVector>,
    pub meta: DecisionMeta,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CoverageTestPhase {
    pub id: String,
    pub kind: String,
    pub operation: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub caused_by_phase_id: Option<String>,
    pub lines: usize,
    pub decisions: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CoverageTestTotals {
    pub lines: usize,
    pub hits: usize,
    pub decisions: usize,
    pub phases: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CoverageSelectedTest {
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
    pub decisions: Vec<CoverageTestDecision>,
    pub lines: Vec<SourceLine>,
    pub hit_details: Vec<CoverageHitDetail>,
    pub phases: Vec<CoverageTestPhase>,
    pub totals: CoverageTestTotals,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CoverageTestMatchesData {
    pub run: String,
    pub filters: CoverageQueryFilters,
    pub tests: Vec<CoverageTestMatch>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CoverageTestDetailData {
    pub run: String,
    pub filters: CoverageQueryFilters,
    pub pagination_applies_to: String,
    pub tests: Vec<CoverageSelectedTest>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(untagged)]
pub enum CoverageTestData {
    Matches(CoverageTestMatchesData),
    Detail(CoverageTestDetailData),
}

#[derive(Debug, Clone, Copy)]
pub struct CoverageTestQueryOptions<'a> {
    pub run: &'a str,
    pub view: CoverageViewId,
    pub kind: Option<&'a str>,
    pub runner: Option<&'a str>,
    pub selector: &'a str,
    pub offset: usize,
    pub limit: usize,
}

fn hit_detail(id: &str, metadata: Option<&IndexedHitMetadata>) -> CoverageHitDetail {
    match metadata {
        Some(metadata) => CoverageHitDetail {
            id: id.into(),
            obligation: metadata.obligation.clone(),
            branch_kind: metadata.branch_kind.clone(),
            file: Some(metadata.file.clone()),
            line: Some(metadata.line),
            column: Some(metadata.column),
            label: metadata.label.clone(),
            alternative: metadata.alternative.clone(),
        },
        None => CoverageHitDetail {
            id: id.into(),
            obligation: "unknown".into(),
            branch_kind: None,
            file: None,
            line: None,
            column: None,
            label: None,
            alternative: None,
        },
    }
}

pub fn coverage_test_query(
    index: &CoverageIndex<'_>,
    options: CoverageTestQueryOptions<'_>,
) -> Result<(CoverageTestData, AgentPagination), QueryError> {
    if options.limit == 0 {
        return Err(QueryError::InvalidPagination);
    }
    let tests = index.test_details(options.view)?;
    let summaries = tests
        .iter()
        .map(|test| test.summary.clone())
        .collect::<Vec<_>>();
    let selected = selected_test_ids(&summaries, options.kind, options.runner)?;
    let selector = options.selector.to_lowercase();
    let matches = tests
        .into_iter()
        .filter(|test| {
            selected
                .as_ref()
                .is_none_or(|ids| ids.contains(&test.summary.id))
        })
        .filter(|test| {
            test.summary.id == selector || test.summary.name.to_lowercase().contains(&selector)
        })
        .collect::<Vec<_>>();
    if matches.is_empty() {
        return Err(QueryError::InvalidRecordSelection);
    }
    let filters = query_filters(options.view, options.kind, options.runner);
    if matches.len() > 1 {
        let total = matches.len();
        let page = matches
            .into_iter()
            .skip(options.offset)
            .take(options.limit)
            .map(|test| CoverageTestMatch {
                id: test.summary.id,
                name: test.summary.name,
                outcome: test.summary.outcome,
                provenance: test.summary.provenance,
            })
            .collect::<Vec<_>>();
        let returned = page.len();
        return Ok((
            CoverageTestData::Matches(CoverageTestMatchesData {
                run: options.run.into(),
                filters,
                tests: page,
            }),
            pagination(options.offset, options.limit, returned, total),
        ));
    }
    let test = matches.into_iter().next().expect("one test match");
    let metadata = index
        .hit_metadata(options.view)?
        .into_iter()
        .map(|metadata| (metadata.id.clone(), metadata))
        .collect::<HashMap<_, _>>();
    let decisions = index
        .decision_metadata(options.view)?
        .into_iter()
        .map(|decision| (decision.id.clone(), decision))
        .collect::<HashMap<_, _>>();
    let all_phases = index
        .phase_summaries(options.view)?
        .into_iter()
        .filter(|phase| phase.test == test.summary.id)
        .map(|phase| CoverageTestPhase {
            id: phase.id,
            kind: phase.kind,
            operation: phase.operation,
            source: phase.source,
            status: phase.status,
            caused_by_phase_id: phase.caused_by_phase_id,
            lines: phase.lines,
            decisions: phase.decisions,
        })
        .collect::<Vec<_>>();
    let totals = CoverageTestTotals {
        lines: test.lines.len(),
        hits: test.hits.len(),
        decisions: test.decisions.len(),
        phases: all_phases.len(),
    };
    let total = totals
        .lines
        .max(totals.hits)
        .max(totals.decisions)
        .max(totals.phases);
    let lines = test
        .lines
        .iter()
        .skip(options.offset)
        .take(options.limit)
        .cloned()
        .collect::<Vec<_>>();
    let hits = test
        .hits
        .iter()
        .skip(options.offset)
        .take(options.limit)
        .cloned()
        .collect::<Vec<_>>();
    let hit_details = test
        .hits
        .iter()
        .skip(options.offset)
        .take(options.limit)
        .map(|id| hit_detail(id, metadata.get(id)))
        .collect::<Vec<_>>();
    let test_decisions = test
        .decisions
        .iter()
        .skip(options.offset)
        .take(options.limit)
        .map(|decision| {
            Ok(CoverageTestDecision {
                id: decision.id.clone(),
                vectors: decision.vectors.clone(),
                meta: decisions
                    .get(&decision.id)
                    .cloned()
                    .ok_or(QueryError::InvalidRecordSelection)?,
            })
        })
        .collect::<Result<Vec<_>, QueryError>>()?;
    let phases = all_phases
        .into_iter()
        .skip(options.offset)
        .take(options.limit)
        .collect::<Vec<_>>();
    let returned = lines
        .len()
        .max(hits.len())
        .max(test_decisions.len())
        .max(phases.len());
    Ok((
        CoverageTestData::Detail(CoverageTestDetailData {
            run: options.run.into(),
            filters,
            pagination_applies_to:
                "lines, hits/hitDetails, decisions, and phases independently within the test".into(),
            tests: vec![CoverageSelectedTest {
                id: test.summary.id,
                name: test.summary.name,
                file: test.summary.file,
                title: test.summary.title,
                retries: test.retries,
                attempts: test.attempts,
                outcome: test.summary.outcome,
                provenance: test.summary.provenance,
                role: test.summary.role,
                hits,
                decisions: test_decisions,
                lines,
                hit_details,
                phases,
                totals,
            }],
        }),
        pagination(options.offset, options.limit, returned, total),
    ))
}

pub fn coverage_scope_query(
    index: &CoverageIndex<'_>,
    options: CoverageScopeQueryOptions<'_>,
) -> Result<(CoverageScopeData, AgentPagination), QueryError> {
    if options.limit == 0 {
        return Err(QueryError::InvalidPagination);
    }
    let projection = index.projection(options.view, options.kind, options.runner)?;
    let scope = projection
        .source_scope
        .ok_or(QueryError::ScopeUnavailable)?;
    let mut entries = index.scope_entries(options.view)?;
    entries.sort_by(|left, right| {
        let rank = |status: &str| match status {
            "ambiguous" => 0,
            "included" => 1,
            "excluded" => 2,
            _ => 3,
        };
        rank(&left.status)
            .cmp(&rank(&right.status))
            .then_with(|| left.file.cmp(&right.file))
    });
    let total = entries.len();
    let selected = entries
        .into_iter()
        .skip(options.offset)
        .take(options.limit)
        .collect::<Vec<_>>();
    let returned = selected.len();
    Ok((
        CoverageScopeData {
            run: options.run.into(),
            filters: CoverageQueryFilters {
                outcome: match options.view {
                    CoverageViewId::All => "all",
                    CoverageViewId::Passed => "passed",
                    CoverageViewId::Failed => "failed",
                }
                .into(),
                kind: options.kind.map(str::to_owned),
                runner: options.runner.map(str::to_owned),
            },
            mode: scope.mode,
            roots: scope.roots,
            counts: ScopeCounts {
                included: scope.included,
                excluded: scope.excluded,
                ambiguous: scope.ambiguous,
            },
            measurement: projection.measurement,
            entries: selected,
        },
        pagination(options.offset, options.limit, returned, total),
    ))
}

pub fn coverage_summary_query(
    index: &CoverageIndex<'_>,
    options: CoverageSummaryQueryOptions<'_>,
) -> Result<CoverageSummaryData, QueryError> {
    let projection = index.projection(options.view, options.kind, options.runner)?;
    let mut diagnostics = Vec::new();
    if projection.empty_evidence_tests > 0 {
        diagnostics.push(CoverageDiagnostic {
            code: "TEST_EVIDENCE_MISSING".into(),
            severity: "warning".into(),
            message: format!(
                "{} test(s) recorded assertion phases but attributed zero coverage evidence; this is valid for assertions over static or uninstrumented data, but may otherwise indicate missing probe transport. First: {}",
                projection.empty_evidence_tests,
                projection.first_empty_evidence_test.as_deref().unwrap_or("unknown")
            ),
        });
    }
    if let Some(transport) = &projection.transport {
        if transport.corrupt_records > 0 {
            diagnostics.push(CoverageDiagnostic {
                code: "CORRUPT_EVIDENCE_RECORDS".into(),
                severity: "error".into(),
                message: format!(
                    "{} malformed evidence record(s) in {} file(s) were excluded; coverage is incomplete.",
                    transport.corrupt_records, transport.corrupt_files
                ),
            });
        }
        if transport.remote_launches > 0
            && transport.scoped_server_records == 0
            && projection.attribution.server_explicit == 0
            && projection.attribution.server_fallback == 0
        {
            diagnostics.push(CoverageDiagnostic {
                code: "REMOTE_SERVER_EVIDENCE_MISSING".into(),
                severity: "warning".into(),
                message: "Remote launches were supervised, but no server evidence returned. Coverage may describe only browser/test processes; inspect how the application server is launched.".into(),
            });
        }
    }
    let coverage_by_kind = index.dimensions(options.view, CoverageDimension::Kind)?;
    let coverage_by_runner = index.dimensions(options.view, CoverageDimension::Runner)?;
    let filters = CoverageQueryFilters {
        outcome: match options.view {
            CoverageViewId::All => "all",
            CoverageViewId::Passed => "passed",
            CoverageViewId::Failed => "failed",
        }
        .into(),
        kind: options.kind.map(str::to_owned),
        runner: options.runner.map(str::to_owned),
    };
    let structurally_complete =
        projection.summary.coverage_complete && projection.measurement.complete;
    let complete = options.view == CoverageViewId::Passed
        && options.valid
        && !options.stale
        && structurally_complete;
    Ok(CoverageSummaryData {
        run: options.run.into(),
        filters,
        generated_at: projection.generated_at,
        valid: options.valid,
        stale: options.stale,
        stale_reasons: options.stale_reasons,
        structurally_complete,
        complete,
        coverage: projection.summary,
        measurement: projection.measurement.clone(),
        coverage_by_kind,
        coverage_by_runner,
        attribution: projection.attribution,
        transport: projection.transport,
        diagnostics,
        confidence: (options.kind.is_none() && options.runner.is_none())
            .then_some(projection.confidence),
        files_with_gaps: projection.files_with_gaps,
        files_with_coverage_gaps: projection.files_with_coverage_gaps,
        files_with_measurement_limitations: projection.measurement.files,
        tests: projection.tests,
        setups: projection.setups,
        test_outcomes: projection.test_outcomes,
        source_scope: projection.source_scope,
    })
}

#[derive(Debug, Clone, PartialEq)]
pub enum CoverageDimensionQueryData {
    Kinds(CoverageKindsData),
    Runners(CoverageRunnersData),
}

#[derive(Debug, Clone, PartialEq)]
pub enum CoverageFileQueryData {
    Files(CoverageFilesData),
    Gaps(CoverageGapsData),
}

#[derive(Debug, Clone, PartialEq)]
pub struct CoverageFileQueryResult {
    pub data: CoverageFileQueryData,
    pub pagination: AgentPagination,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DecisionSort {
    Location,
    Missing,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DecisionGapTotals {
    pub decisions: usize,
    pub decisions_with_missing_conditions: usize,
    pub conditions: usize,
    pub missing_conditions: usize,
    pub waived_conditions: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CoverageFileDecisionsData {
    pub run: String,
    pub filters: CoverageQueryFilters,
    pub file: String,
    pub group: String,
    pub sort: DecisionSort,
    pub totals: DecisionGapTotals,
    pub decisions: Vec<IndexedDecisionGap>,
}

#[derive(Debug, Clone, Copy)]
pub struct CoverageFileDecisionsOptions<'a> {
    pub run: &'a str,
    pub view: CoverageViewId,
    pub kind: Option<&'a str>,
    pub runner: Option<&'a str>,
    pub file: &'a str,
    pub sort: DecisionSort,
    pub offset: usize,
    pub limit: usize,
}

#[derive(Debug, Clone)]
pub struct CoverageDimensionQueryOptions<'a> {
    pub run: &'a str,
    pub view: CoverageViewId,
    pub dimension: CoverageDimension,
    pub filters: CoverageQueryFilters,
    pub offset: usize,
    pub limit: usize,
}

pub fn coverage_dimension_query(
    index: &CoverageIndex<'_>,
    options: CoverageDimensionQueryOptions<'_>,
) -> Result<(CoverageDimensionQueryData, AgentPagination), QueryError> {
    let CoverageDimensionQueryOptions {
        run,
        view,
        dimension,
        filters,
        offset,
        limit,
    } = options;
    if limit == 0 {
        return Err(QueryError::InvalidPagination);
    }
    let values = index.dimensions(view, dimension)?;
    let total = values.len();
    let selected = values
        .into_iter()
        .skip(offset)
        .take(limit)
        .collect::<Vec<_>>();
    let returned = selected.len();
    let data = match dimension {
        CoverageDimension::Kind => CoverageDimensionQueryData::Kinds(CoverageKindsData {
            run: run.into(),
            filters,
            kinds: selected,
        }),
        CoverageDimension::Runner => CoverageDimensionQueryData::Runners(CoverageRunnersData {
            run: run.into(),
            filters,
            runners: selected,
        }),
    };
    Ok((data, pagination(offset, limit, returned, total)))
}

#[derive(Debug, Clone, Copy)]
pub struct CoverageFileQueryOptions<'a> {
    pub run: &'a str,
    pub view: CoverageViewId,
    pub metric: MinimizeMetric,
    pub gaps_only: bool,
    pub kind: Option<&'a str>,
    pub runner: Option<&'a str>,
    pub offset: usize,
    pub limit: usize,
}

fn gap_metric_value(gap: &IndexedFileGap, metric: MinimizeMetric) -> usize {
    match metric {
        MinimizeMetric::All => gap.score,
        MinimizeMetric::Lines => gap.uncovered_lines,
        MinimizeMetric::Statements => gap.uncovered_statements,
        MinimizeMetric::Functions => gap.uncovered_functions,
        MinimizeMetric::Branches => gap.missing_branches,
        MinimizeMetric::Mcdc => gap.missing_mcdc_conditions,
    }
}

pub fn coverage_file_decisions_query(
    index: &CoverageIndex<'_>,
    options: CoverageFileDecisionsOptions<'_>,
) -> Result<(CoverageFileDecisionsData, AgentPagination), QueryError> {
    if options.limit == 0 {
        return Err(QueryError::InvalidPagination);
    }
    let all = index.decision_gaps(options.view, options.kind, options.runner, options.file)?;
    if (options.kind.is_some() || options.runner.is_some()) && all.is_empty() {
        // A file with no decisions is valid, so consult the file projection to
        // distinguish it from a nonexistent test provenance projection.
        if index
            .file_gaps(options.view, options.kind, options.runner)?
            .is_empty()
        {
            return Err(QueryError::InvalidRecordSelection);
        }
    }
    let totals = DecisionGapTotals {
        decisions: all.len(),
        decisions_with_missing_conditions: all
            .iter()
            .filter(|decision| decision.missing_conditions > 0)
            .count(),
        conditions: all.iter().map(|decision| decision.conditions).sum(),
        missing_conditions: all.iter().map(|decision| decision.missing_conditions).sum(),
        waived_conditions: all.iter().map(|decision| decision.waived_conditions).sum(),
    };
    let mut missing = all
        .into_iter()
        .filter(|decision| decision.missing_conditions > 0)
        .collect::<Vec<_>>();
    missing.sort_by(|left, right| match options.sort {
        DecisionSort::Missing => right
            .missing_conditions
            .saturating_sub(right.waived_conditions)
            .cmp(
                &left
                    .missing_conditions
                    .saturating_sub(left.waived_conditions),
            )
            .then_with(|| right.missing_conditions.cmp(&left.missing_conditions))
            .then_with(|| left.line.cmp(&right.line))
            .then_with(|| left.column.cmp(&right.column)),
        DecisionSort::Location => left
            .line
            .cmp(&right.line)
            .then_with(|| left.column.cmp(&right.column))
            .then_with(|| left.id.cmp(&right.id)),
    });
    let total = missing.len();
    let rows = missing
        .into_iter()
        .skip(options.offset)
        .take(options.limit)
        .collect::<Vec<_>>();
    let returned = rows.len();
    let filters = CoverageQueryFilters {
        outcome: match options.view {
            CoverageViewId::All => "all",
            CoverageViewId::Passed => "passed",
            CoverageViewId::Failed => "failed",
        }
        .into(),
        kind: options.kind.map(str::to_owned),
        runner: options.runner.map(str::to_owned),
    };
    Ok((
        CoverageFileDecisionsData {
            run: options.run.into(),
            filters,
            file: options.file.into(),
            group: "decision".into(),
            sort: options.sort,
            totals,
            decisions: rows,
        },
        pagination(options.offset, options.limit, returned, total),
    ))
}

pub fn coverage_file_query(
    index: &CoverageIndex<'_>,
    options: CoverageFileQueryOptions<'_>,
) -> Result<CoverageFileQueryResult, QueryError> {
    let CoverageFileQueryOptions {
        run,
        view,
        metric,
        gaps_only,
        kind,
        runner,
        offset,
        limit,
    } = options;
    if limit == 0 {
        return Err(QueryError::InvalidPagination);
    }
    let mut files = index.file_gaps(view, kind, runner)?;
    if (kind.is_some() || runner.is_some()) && files.is_empty() {
        return Err(QueryError::InvalidRecordSelection);
    }
    if gaps_only {
        files.retain(|gap| gap_metric_value(gap, metric) > 0 || gap.measurement_limitations > 0);
    }
    files.sort_by(|left, right| {
        gap_metric_value(right, metric)
            .cmp(&gap_metric_value(left, metric))
            .then_with(|| {
                right
                    .measurement_limitations
                    .cmp(&left.measurement_limitations)
            })
            .then_with(|| left.file.cmp(&right.file))
    });
    let total = files.len();
    let page = files
        .into_iter()
        .skip(offset)
        .take(limit)
        .collect::<Vec<_>>();
    let page_info = pagination(offset, limit, page.len(), total);
    let filters = CoverageQueryFilters {
        outcome: match view {
            CoverageViewId::All => "all",
            CoverageViewId::Passed => "passed",
            CoverageViewId::Failed => "failed",
        }
        .into(),
        kind: kind.map(str::to_owned),
        runner: runner.map(str::to_owned),
    };
    let data = if gaps_only {
        CoverageFileQueryData::Gaps(CoverageGapsData {
            run: run.into(),
            filters,
            metric,
            gaps: page,
        })
    } else {
        CoverageFileQueryData::Files(CoverageFilesData {
            run: run.into(),
            filters,
            metric,
            files: page,
        })
    };
    Ok(CoverageFileQueryResult {
        data,
        pagination: page_info,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum ObligationMetric {
    Lines,
    Statements,
    Functions,
    Branches,
    Mcdc,
}

impl ObligationMetric {
    fn selected(self, metric: MinimizeMetric) -> bool {
        metric == MinimizeMetric::All
            || matches!(
                (self, metric),
                (Self::Lines, MinimizeMetric::Lines)
                    | (Self::Statements, MinimizeMetric::Statements)
                    | (Self::Functions, MinimizeMetric::Functions)
                    | (Self::Branches, MinimizeMetric::Branches)
                    | (Self::Mcdc, MinimizeMetric::Mcdc)
            )
    }

    fn public(self) -> MinimizeMetric {
        match self {
            Self::Lines => MinimizeMetric::Lines,
            Self::Statements => MinimizeMetric::Statements,
            Self::Functions => MinimizeMetric::Functions,
            Self::Branches => MinimizeMetric::Branches,
            Self::Mcdc => MinimizeMetric::Mcdc,
        }
    }
}

#[derive(Clone)]
struct Obligation {
    id: String,
    metric: ObligationMetric,
    /// Any one complete option satisfies this obligation.
    options: Vec<Vec<String>>,
}

struct ObligationModel {
    obligations: Vec<Obligation>,
    setup_by_file: BTreeMap<String, Vec<String>>,
    tests_by_file: BTreeMap<String, Vec<String>>,
    background: Vec<String>,
}

impl ObligationModel {
    fn expand(&self, selected: &BTreeSet<String>) -> BTreeSet<String> {
        let mut expanded = selected.clone();
        expanded.extend(self.background.iter().cloned());
        for (file, setup_ids) in &self.setup_by_file {
            if self
                .tests_by_file
                .get(file)
                .is_some_and(|tests| tests.iter().any(|id| selected.contains(id)))
            {
                expanded.extend(setup_ids.iter().cloned());
            }
        }
        expanded
    }
}

fn deduplicate_options(options: impl IntoIterator<Item = Vec<String>>) -> Vec<Vec<String>> {
    let mut unique = BTreeMap::<String, Vec<String>>::new();
    for mut option in options {
        option.sort();
        option.dedup();
        let key = option.join("\0");
        unique.entry(key).or_insert(option);
    }
    unique.into_values().collect()
}

fn evidence_choices(
    ids: &[String],
    tests: &HashMap<&str, &crate::coverage_report::TestCoverageResult>,
    candidates: &BTreeSet<String>,
    tests_by_file: &BTreeMap<String, Vec<String>>,
) -> Vec<Vec<String>> {
    let mut choices = Vec::new();
    for id in ids {
        let Some(test) = tests.get(id.as_str()) else {
            continue;
        };
        if test.role == "background" {
            choices.push(Vec::new());
        } else if test.role == "setup" {
            if let Some(file) = &test.file {
                choices.extend(
                    tests_by_file
                        .get(file)
                        .into_iter()
                        .flatten()
                        .map(|candidate| vec![candidate.clone()]),
                );
            }
        } else if candidates.contains(id) {
            choices.push(vec![id.clone()]);
        }
    }
    deduplicate_options(choices)
}

fn build_obligations(view: &CoverageView, candidates: &BTreeSet<String>) -> ObligationModel {
    let tests = view
        .tests
        .iter()
        .map(|test| (test.id.as_str(), test))
        .collect::<HashMap<_, _>>();
    let mut tests_by_file = BTreeMap::<String, Vec<String>>::new();
    let mut setup_by_file = BTreeMap::<String, Vec<String>>::new();
    let mut background = Vec::new();
    for test in &view.tests {
        match test.role.as_str() {
            "test" if candidates.contains(&test.id) => {
                if let Some(file) = &test.file {
                    tests_by_file
                        .entry(file.clone())
                        .or_default()
                        .push(test.id.clone());
                }
            }
            "setup" => {
                if let Some(file) = &test.file {
                    setup_by_file
                        .entry(file.clone())
                        .or_default()
                        .push(test.id.clone());
                }
            }
            "background" => background.push(test.id.clone()),
            _ => {}
        }
    }
    let choices = |ids: &[String]| evidence_choices(ids, &tests, candidates, &tests_by_file);
    let mut obligations = Vec::new();
    let mut unique_lines = BTreeMap::new();
    for line in &view.lines {
        unique_lines.insert((line.file.as_str(), line.line), line);
    }
    for ((file, line), result) in unique_lines {
        obligations.push(Obligation {
            id: format!("line:{file}:{line}"),
            metric: ObligationMetric::Lines,
            options: choices(&result.tests),
        });
    }
    for point in &view.points {
        let (kind, metric) = match point.meta.kind {
            crate::coverage_analysis::PointKind::Statement => {
                ("statement", ObligationMetric::Statements)
            }
            crate::coverage_analysis::PointKind::Function => {
                ("function", ObligationMetric::Functions)
            }
        };
        obligations.push(Obligation {
            id: format!("{kind}:{}", point.meta.id),
            metric,
            options: choices(&point.tests),
        });
    }
    for branch in &view.branches {
        for alternative in &branch.alternatives {
            obligations.push(Obligation {
                id: format!("branch:{}:{}", branch.meta.id, alternative.id),
                metric: ObligationMetric::Branches,
                options: choices(&alternative.tests),
            });
        }
    }
    for decision in &view.decisions {
        for condition in 0..decision.meta.conditions.len() {
            let mut options = Vec::new();
            for left in 0..decision.vector_observations.len() {
                for right in (left + 1)..decision.vector_observations.len() {
                    let first = &decision.vector_observations[left];
                    let second = &decision.vector_observations[right];
                    if !is_independence_pair(&first.vector, &second.vector, condition) {
                        continue;
                    }
                    for first_choice in choices(&first.tests) {
                        for second_choice in choices(&second.tests) {
                            let mut combined = first_choice.clone();
                            combined.extend(second_choice);
                            options.push(combined);
                        }
                    }
                }
            }
            obligations.push(Obligation {
                id: format!("mcdc:{}:{condition}", decision.meta.id),
                metric: ObligationMetric::Mcdc,
                options: deduplicate_options(options),
            });
        }
    }
    ObligationModel {
        obligations,
        setup_by_file,
        tests_by_file,
        background,
    }
}

fn percentage(metric: ObligationMetric, summary: &CoverageSummary) -> f64 {
    match metric {
        ObligationMetric::Lines => summary.lines.percentage,
        ObligationMetric::Statements => summary.statements.percentage,
        ObligationMetric::Functions => summary.functions.percentage,
        ObligationMetric::Branches => summary.branches.percentage,
        ObligationMetric::Mcdc => summary.condition_coverage_pct,
    }
}

fn obligation_satisfied(obligation: &Obligation, selected: &BTreeSet<String>) -> bool {
    obligation
        .options
        .iter()
        .any(|option| option.iter().all(|test| selected.contains(test)))
}

struct Search<'a> {
    obligations: &'a [Obligation],
    skip_limits: BTreeMap<ObligationMetric, usize>,
    best: BTreeSet<String>,
    explored_states: usize,
    max_states: usize,
    seen: BTreeSet<String>,
    candidate_tests: usize,
    target: f64,
    metric: MinimizeMetric,
}

impl Search<'_> {
    fn visit(
        &mut self,
        selected: BTreeSet<String>,
        skipped: BTreeSet<String>,
        skipped_by_metric: BTreeMap<ObligationMetric, usize>,
    ) -> Result<(), QueryError> {
        self.explored_states += 1;
        if self.explored_states > self.max_states {
            return Err(QueryError::ComplexityLimit {
                candidate_tests: self.candidate_tests,
                obligations: self.obligations.len(),
                explored_states: self.explored_states,
                max_states: self.max_states,
                target: self.target,
                metric: self.metric,
            });
        }
        if selected.len() >= self.best.len() {
            return Ok(());
        }
        let state_key = format!(
            "{}|{}",
            selected.iter().cloned().collect::<Vec<_>>().join(","),
            skipped.iter().cloned().collect::<Vec<_>>().join(",")
        );
        if !self.seen.insert(state_key) {
            return Ok(());
        }
        let mut unmet = self
            .obligations
            .iter()
            .filter(|obligation| {
                !skipped.contains(&obligation.id) && !obligation_satisfied(obligation, &selected)
            })
            .collect::<Vec<_>>();
        if unmet.is_empty() {
            self.best = selected;
            return Ok(());
        }
        unmet.sort_by(|left, right| {
            let feasible = |obligation: &Obligation| {
                obligation
                    .options
                    .iter()
                    .filter(|option| option.iter().any(|test| !selected.contains(test)))
                    .count()
            };
            feasible(left)
                .cmp(&feasible(right))
                .then_with(|| left.id.cmp(&right.id))
        });
        let obligation = unmet[0];
        let mut additions = deduplicate_options(obligation.options.iter().filter_map(|option| {
            let addition = option
                .iter()
                .filter(|test| !selected.contains(*test))
                .cloned()
                .collect::<Vec<_>>();
            (!addition.is_empty()).then_some(addition)
        }));
        additions.sort_by(|left, right| {
            left.len()
                .cmp(&right.len())
                .then_with(|| left.join("\0").cmp(&right.join("\0")))
        });
        for addition in additions {
            if selected.len() + addition.len() >= self.best.len() {
                continue;
            }
            let mut next = selected.clone();
            next.extend(addition);
            self.visit(next, skipped.clone(), skipped_by_metric.clone())?;
        }
        let skipped_count = skipped_by_metric
            .get(&obligation.metric)
            .copied()
            .unwrap_or(0);
        if skipped_count
            < self
                .skip_limits
                .get(&obligation.metric)
                .copied()
                .unwrap_or(0)
        {
            let mut next_skipped = skipped;
            next_skipped.insert(obligation.id.clone());
            let mut next_counts = skipped_by_metric;
            next_counts.insert(obligation.metric, skipped_count + 1);
            self.visit(selected, next_skipped, next_counts)?;
        }
        Ok(())
    }
}

pub fn minimum_test_set(
    view: &CoverageView,
    target: f64,
    metric: MinimizeMetric,
    max_states: usize,
) -> Result<MinimumTestSetResult, QueryError> {
    if !target.is_finite() || !(0.0..=100.0).contains(&target) {
        return Err(QueryError::InvalidTarget(target));
    }
    if view.tests.iter().any(|test| {
        test.role == "background" && (!test.hits.is_empty() || !test.decisions.is_empty())
    }) {
        return Err(QueryError::UnattributedEvidence);
    }
    let candidate_tests = view
        .tests
        .iter()
        .filter(|test| test.role == "test")
        .map(|test| test.id.clone())
        .collect::<BTreeSet<_>>();
    let model = build_obligations(view, &candidate_tests);
    let obligations = model
        .obligations
        .iter()
        .filter(|obligation| obligation.metric.selected(metric))
        .cloned()
        .collect::<Vec<_>>();
    let mut totals = BTreeMap::<ObligationMetric, usize>::new();
    for obligation in &obligations {
        *totals.entry(obligation.metric).or_default() += 1;
    }
    let metrics = [
        ObligationMetric::Lines,
        ObligationMetric::Statements,
        ObligationMetric::Functions,
        ObligationMetric::Branches,
        ObligationMetric::Mcdc,
    ]
    .into_iter()
    .filter(|candidate| candidate.selected(metric))
    .collect::<Vec<_>>();
    let skip_limits = metrics
        .iter()
        .map(|selected_metric| {
            let total = totals.get(selected_metric).copied().unwrap_or(0);
            let required = ((total as f64 * target) / 100.0).ceil() as usize;
            (*selected_metric, total.saturating_sub(required))
        })
        .collect::<BTreeMap<_, _>>();
    let full_expanded = model.expand(&candidate_tests);
    let full_summary = coverage_summary_for_tests(view, &full_expanded)?;
    for selected_metric in &metrics {
        let reachable = percentage(*selected_metric, &full_summary);
        if reachable + 1e-9 < target {
            return Err(QueryError::TargetUnreachable {
                metric: selected_metric.public(),
                target,
                reachable,
            });
        }
    }
    let mut search = Search {
        obligations: &obligations,
        skip_limits,
        best: candidate_tests.clone(),
        explored_states: 0,
        max_states,
        seen: BTreeSet::new(),
        candidate_tests: candidate_tests.len(),
        target,
        metric,
    };
    search.visit(BTreeSet::new(), BTreeSet::new(), BTreeMap::new())?;
    let expanded = model.expand(&search.best);
    let summary = coverage_summary_for_tests(view, &expanded)?;
    Ok(MinimumTestSetResult {
        optimal: true,
        target,
        metric,
        selected: search.best.into_iter().collect(),
        expanded: expanded.into_iter().collect(),
        summary,
        explored_states: search.explored_states,
    })
}

#[cfg(test)]
mod tests {
    use crate::{
        coverage_analysis::McdcVector,
        coverage_report::{
            CoverageManifest, CoverageReport, CoverageReportRequest, DecisionMeta, ExitCodeInput,
            RawTestResult, RuntimeSnapshot, TestProvenance, analyze_coverage_results,
        },
    };

    use super::*;

    fn result(id: &str, vector: McdcVector) -> RawTestResult {
        RawTestResult {
            test_id: Some(id.into()),
            scope: None,
            test: id.into(),
            test_file: Some("tests/permission.test.js".into()),
            title: None,
            retry: Some(0),
            status: Some("passed".into()),
            expected_status: None,
            flaky: false,
            provenance: TestProvenance {
                runner: "node:test".into(),
                kind: "unit".into(),
                project: None,
                source: "runner-default".into(),
            },
            role: "test".into(),
            phases: Vec::new(),
            runtime: vec![RuntimeSnapshot {
                decisions: vec![crate::coverage_report::DecisionSnapshot {
                    meta: decision(),
                    vectors: vec![vector],
                }],
                hits: Vec::new(),
                events: Vec::new(),
            }],
            browser: Vec::new(),
            server: Vec::new(),
        }
    }

    fn decision() -> DecisionMeta {
        DecisionMeta {
            id: "decision".into(),
            file: "src/permission.js".into(),
            line: 1,
            column: 1,
            source: "admin || owner".into(),
            conditions: vec!["admin".into(), "owner".into()],
            kind: "if".into(),
        }
    }

    fn report(mut results: Vec<RawTestResult>) -> CoverageReport {
        analyze_coverage_results(&CoverageReportRequest {
            run_id: "run".into(),
            manifest: CoverageManifest {
                decisions: vec![decision()],
                points: Vec::new(),
                branches: Vec::new(),
                limitations: Vec::new(),
                scope: None,
            },
            raw_results: std::mem::take(&mut results),
            generated_at: "time".into(),
            integrity: None,
            test_exit_code: ExitCodeInput::Present(Some(0)),
        })
        .unwrap()
    }

    #[test]
    fn recomputes_mcdc_witnesses_and_removes_a_redundant_vector() {
        let report = report(vec![
            result(
                "admin",
                McdcVector {
                    values: vec![Some(true), None],
                    outcome: true,
                },
            ),
            result(
                "owner",
                McdcVector {
                    values: vec![Some(false), Some(true)],
                    outcome: true,
                },
            ),
            result(
                "both",
                McdcVector {
                    values: vec![Some(true), None],
                    outcome: true,
                },
            ),
            result(
                "neither",
                McdcVector {
                    values: vec![Some(false), Some(false)],
                    outcome: false,
                },
            ),
        ]);
        let minimized = minimum_test_set(&report.view, 100.0, MinimizeMetric::Mcdc, 5_000).unwrap();
        assert_eq!(minimized.selected.len(), 3);
        assert!(minimized.selected.contains(&"owner".into()));
        assert!(minimized.selected.contains(&"neither".into()));
        assert_eq!(minimized.summary.condition_coverage_pct, 100.0);
    }

    #[test]
    fn refuses_background_evidence() {
        let mut aggregate = result(
            "aggregate",
            McdcVector {
                values: vec![Some(false), Some(false)],
                outcome: false,
            },
        );
        aggregate.role = "background".into();
        assert!(matches!(
            minimum_test_set(
                &report(vec![aggregate]).view,
                100.0,
                MinimizeMetric::Mcdc,
                5_000,
            ),
            Err(QueryError::UnattributedEvidence)
        ));
    }

    #[test]
    fn bounds_the_exact_search() {
        let report = report(vec![
            result(
                "admin",
                McdcVector {
                    values: vec![Some(true), None],
                    outcome: true,
                },
            ),
            result(
                "owner",
                McdcVector {
                    values: vec![Some(false), Some(true)],
                    outcome: true,
                },
            ),
            result(
                "neither",
                McdcVector {
                    values: vec![Some(false), Some(false)],
                    outcome: false,
                },
            ),
        ]);
        assert!(matches!(
            minimum_test_set(&report.view, 100.0, MinimizeMetric::Mcdc, 1),
            Err(QueryError::ComplexityLimit { .. })
        ));
    }
}
