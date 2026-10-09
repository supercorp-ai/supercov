//! Language-neutral coverage query operators.
//!
//! Querying is deliberately separated from the CLI and storage container.
//! This module accepts the frozen analyzed view and owns structural query
//! semantics shared by every language frontend.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use serde::{Deserialize, Serialize};

use crate::interned::Id;
use crate::{
    agent_json::pagination,
    coverage_analysis::{CoverageSummary, is_independence_pair},
    coverage_index::{
        CoverageDimension, CoverageIndex, CoverageIndexError, CoverageViewId, IndexedCoverageModel,
        IndexedDecisionGap, IndexedDimensionCoverage, IndexedFileGap, IndexedGapDimensions,
        IndexedHitMetadata, IndexedMeasurement, IndexedOutcomeCounts, IndexedScopeEntry,
        IndexedSourceScope, IndexedSummaryConfidence, IndexedTestSummary,
    },
    coverage_report::{
        CoverageConfidence, CoverageView, DecisionMeta, ReportError, SourceLine, TestAttempt,
        TestProvenance, TransportStats, coverage_summary_for_tests,
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
    #[serde(serialize_with = "crate::coverage_analysis::serialize_javascript_number")]
    pub target: f64,
    pub metric: MinimizeMetric,
    pub selected: Vec<String>,
    pub expanded: Vec<String>,
    pub summary: CoverageSummary,
    pub explored_states: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CoverageMinimizedTest {
    pub id: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    pub runner: String,
    pub kind: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CoverageMinimizeData {
    pub run: String,
    pub filters: CoverageQueryFilters,
    pub optimal: bool,
    #[serde(serialize_with = "crate::coverage_analysis::serialize_javascript_number")]
    pub target: f64,
    pub metric: MinimizeMetric,
    pub selected: Vec<String>,
    pub expanded: Vec<String>,
    pub summary: CoverageSummary,
    pub explored_states: usize,
    pub selected_count: usize,
    pub total_candidate_tests: usize,
    pub tests: Vec<CoverageMinimizedTest>,
}

#[derive(Debug, Clone, Copy)]
pub struct CoverageMinimizeQueryOptions<'a> {
    pub run: &'a str,
    pub view_id: CoverageViewId,
    pub kind: Option<&'a str>,
    pub runner: Option<&'a str>,
    pub target: f64,
    pub metric: MinimizeMetric,
    pub max_states: usize,
    pub offset: usize,
    pub limit: usize,
}

pub fn coverage_minimize_query(
    view: &CoverageView,
    options: CoverageMinimizeQueryOptions<'_>,
) -> Result<(CoverageMinimizeData, AgentPagination), QueryError> {
    if options.limit == 0 {
        return Err(QueryError::InvalidPagination);
    }
    let selected_ids = if options.kind.is_none() && options.runner.is_none() {
        None
    } else {
        let ids = view
            .tests
            .iter()
            .filter(|test| {
                options.kind.is_none_or(|kind| test.provenance.kind == kind)
                    && options
                        .runner
                        .is_none_or(|runner| test.provenance.runner == runner)
            })
            .map(|test| test.id.clone())
            .collect::<BTreeSet<_>>();
        if ids.is_empty() {
            return Err(QueryError::TestFilterEmpty {
                kind: options.kind.map(str::to_owned),
                runner: options.runner.map(str::to_owned),
            });
        }
        Some(ids)
    };
    let mut solver_view = view.clone();
    if let Some(selected) = &selected_ids {
        solver_view.tests.retain(|test| selected.contains(&test.id));
    }
    let minimized = minimum_test_set(
        &solver_view,
        options.target,
        options.metric,
        options.max_states,
    )?;
    let selected_details = minimized
        .selected
        .iter()
        .map(|id| {
            let test = view
                .tests
                .iter()
                .find(|test| test.id == *id)
                .ok_or(QueryError::InvalidRecordSelection)?;
            Ok(CoverageMinimizedTest {
                id: id.clone(),
                name: test.name.clone(),
                file: test.file.clone(),
                runner: test.provenance.runner.clone(),
                kind: test.provenance.kind.clone(),
            })
        })
        .collect::<Result<Vec<_>, QueryError>>()?;
    let total = selected_details.len();
    let tests = selected_details
        .iter()
        .skip(options.offset)
        .take(options.limit)
        .cloned()
        .collect::<Vec<_>>();
    let returned = tests.len();
    let total_candidate_tests = solver_view
        .tests
        .iter()
        .filter(|test| test.role == "test")
        .count();
    Ok((
        CoverageMinimizeData {
            run: options.run.into(),
            filters: query_filters(options.view_id, options.kind, options.runner),
            optimal: minimized.optimal,
            target: minimized.target,
            metric: minimized.metric,
            selected: minimized.selected,
            expanded: minimized.expanded,
            summary: minimized.summary,
            explored_states: minimized.explored_states,
            selected_count: total,
            total_candidate_tests,
            tests,
        },
        pagination(options.offset, options.limit, returned, total),
    ))
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
    TestFilterEmpty {
        kind: Option<String>,
        runner: Option<String>,
    },
    TestNotFound(String),
    DecisionNotFound(String),
    SourceNotFound(String),
    AmbiguousSelector {
        selector: String,
        matches: Vec<String>,
    },
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

/// Tests by how completely their coverage is their own.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TestAttributionCounts {
    /// Credited with exactly what they reached.
    pub exact: usize,
    /// Credited with some of what they reached, and no way to say how much.
    pub partial: usize,
    /// Credited with none of it; what they reached is in the run's totals.
    pub run_wide: usize,
}

impl TestAttributionCounts {
    pub fn total(&self) -> usize {
        self.exact + self.partial + self.run_wide
    }
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
    pub test_kind_sources: BTreeMap<String, usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub assertion_coverage: Option<serde_json::Value>,
    pub run: String,
    #[serde(default)]
    pub command: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hints: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
    pub filters: CoverageQueryFilters,
    pub model: IndexedCoverageModel,
    pub generated_at: String,
    pub valid: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub test_exit_code: Option<i32>,
    pub stale: bool,
    pub stale_reasons: Vec<String>,
    pub structurally_complete: bool,
    pub complete: bool,
    pub coverage: CoverageSummary,
    pub measurement: IndexedMeasurement,
    pub coverage_by_kind: Vec<IndexedDimensionCoverage>,
    /// What the tests cover between them. `coverage` less this ran while no
    /// test was running, which is why a run with one kind of test can show a
    /// total above that kind's own.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub coverage_by_tests: Option<CoverageSummary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub e2e_gap_context: Option<CoverageKindGapContext>,
    pub coverage_by_runner: Vec<IndexedDimensionCoverage>,
    pub attribution: crate::coverage_index::IndexedAttribution,
    /// How completely each test's coverage could be credited to it.
    ///
    /// Counted here rather than stored, because the per-test answer is already
    /// in the run and a total is a read of it. A run written before the field
    /// existed reports every test as exact, which is what its records say.
    pub test_attribution: TestAttributionCounts,
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
    /// Where the files discovery could not classify are, and the roots that
    /// would settle them.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unclassified_source: Option<UnclassifiedSource>,
}

/// One row of the scope overview: the files of one place that share a status
/// and the reason for it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScopeGroup {
    pub status: String,
    pub directory: String,
    pub reason: String,
    pub files: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScopeDirectory {
    pub directory: String,
    pub files: usize,
}

/// The unclassified files of a run by where they are, and the source roots
/// to name if they are the project's own code: the roots discovery found
/// plus each of those places.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UnclassifiedSource {
    pub directories: Vec<ScopeDirectory>,
    pub roots: Vec<String>,
}

/// Where a reader would look for a file: the directory at the top of the
/// project, or of its package in a workspace. A file that sits at that top
/// is in `.`.
fn scope_directory(entry: &IndexedScopeEntry) -> String {
    let package = entry.package_root.as_deref().filter(|root| *root != ".");
    let local = package
        .and_then(|root| entry.file.strip_prefix(root))
        .and_then(|rest| rest.strip_prefix('/'))
        .unwrap_or(&entry.file);
    match (package, local.split_once('/')) {
        (Some(package), Some((first, _))) => format!("{package}/{first}"),
        (Some(package), None) => package.to_owned(),
        (None, Some((first, _))) => first.to_owned(),
        (None, None) => ".".to_owned(),
    }
}

fn scope_status_rank(status: &str) -> u8 {
    match status {
        "ambiguous" => 0,
        "included" => 1,
        "excluded" => 2,
        _ => 3,
    }
}

/// Every scope entry, grouped. 845 entries took five pages to read and an
/// `awk` to see that 79 of 89 unclassified files were one directory.
pub fn scope_groups(entries: &[IndexedScopeEntry]) -> Vec<ScopeGroup> {
    let mut groups = BTreeMap::<(u8, String, String, String), usize>::new();
    for entry in entries {
        *groups
            .entry((
                scope_status_rank(&entry.status),
                entry.status.clone(),
                scope_directory(entry),
                entry.reason.clone(),
            ))
            .or_default() += 1;
    }
    let mut groups = groups
        .into_iter()
        .map(|((rank, status, directory, reason), files)| {
            (
                rank,
                ScopeGroup {
                    status,
                    directory,
                    reason,
                    files,
                },
            )
        })
        .collect::<Vec<_>>();
    groups.sort_by(|(left_rank, left), (right_rank, right)| {
        left_rank
            .cmp(right_rank)
            .then_with(|| right.files.cmp(&left.files))
            .then_with(|| left.directory.cmp(&right.directory))
            .then_with(|| left.reason.cmp(&right.reason))
    });
    groups.into_iter().map(|(_, group)| group).collect()
}

pub fn unclassified_source(
    roots: &[String],
    entries: &[IndexedScopeEntry],
) -> Option<UnclassifiedSource> {
    let mut places = BTreeMap::<String, usize>::new();
    let mut named = Vec::<(String, usize)>::new();
    for entry in entries.iter().filter(|entry| entry.status == "ambiguous") {
        let directory = scope_directory(entry);
        *places.entry(directory.clone()).or_default() += 1;
        // A file at the top is named itself; naming `.` would take the tests
        // and the configuration with it.
        let root = if entry.file.contains('/') && directory != "." {
            directory
        } else {
            entry.file.clone()
        };
        match named.iter_mut().find(|(name, _)| *name == root) {
            Some((_, files)) => *files += 1,
            None => named.push((root, 1)),
        }
    }
    if places.is_empty() {
        return None;
    }
    let mut directories = places
        .into_iter()
        .map(|(directory, files)| ScopeDirectory { directory, files })
        .collect::<Vec<_>>();
    directories.sort_by(|left, right| {
        right
            .files
            .cmp(&left.files)
            .then_with(|| left.directory.cmp(&right.directory))
    });
    named.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(&right.0)));
    let mut all = roots.to_vec();
    for (root, _) in named {
        if !all.contains(&root) {
            all.push(root);
        }
    }
    Some(UnclassifiedSource {
        directories,
        roots: all,
    })
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CoverageKindGapContext {
    pub kind: String,
    pub other_kinds: Vec<String>,
    pub covered_elsewhere: IndexedGapDimensions,
    pub uncovered_everywhere: IndexedGapDimensions,
}

#[derive(Debug, Clone)]
pub struct CoverageSummaryQueryOptions<'a> {
    pub run: &'a str,
    pub view: CoverageViewId,
    pub kind: Option<&'a str>,
    pub runner: Option<&'a str>,
    pub valid: bool,
    pub test_exit_code: Option<i32>,
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
    pub kind: String,
    pub language: String,
    pub model: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    pub roots: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub measurement_complete: Option<bool>,
    pub counts: ScopeCounts,
    pub measurement: IndexedMeasurement,
    /// Every entry of the scope, grouped; `entries` is one page of them.
    pub groups: Vec<ScopeGroup>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unclassified_source: Option<UnclassifiedSource>,
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
    pub source: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub missing: Option<String>,
    pub covering_tests: usize,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_origin: Option<String>,
    pub covered: bool,
    pub confidence: CoverageConfidence,
    pub total_tests: usize,
    pub total_phases: usize,
    pub total_anchored: usize,
    pub covered_anchored: usize,
    pub total_limitations: usize,
    pub total_remaining: usize,
    pub tests: Vec<CoverageCoveringTest>,
    pub phases: Vec<CoverageCoveringPhase>,
    pub anchored: Vec<CoverageAnchor>,
    pub limitations: Vec<CoverageFileLimitation>,
    pub remaining: Vec<CoverageFileObligation>,
    /// The run's saved assertion assessment for this place, attached by the
    /// CLI when the run has one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub assertions: Option<serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CoverageCoversAnchorsData {
    pub run: String,
    pub filters: CoverageQueryFilters,
    pub location: CoverageLocation,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_origin: Option<String>,
    pub line_obligation: bool,
    pub anchored: Vec<CoverageAnchor>,
    pub total_anchored: usize,
    pub covered_anchored: usize,
    pub total_limitations: usize,
    pub limitations: Vec<CoverageFileLimitation>,
    pub total_remaining: usize,
    pub remaining: Vec<CoverageFileObligation>,
    pub total_tests: usize,
    pub tests: Vec<CoverageCoveringTest>,
    /// The run's saved assertion assessment for this place, attached by the
    /// CLI when the run has one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub assertions: Option<serde_json::Value>,
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
        return Err(QueryError::TestFilterEmpty {
            kind: kind.map(str::to_owned),
            runner: runner.map(str::to_owned),
        });
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
    let metadata = index
        .hit_metadata(options.view)?
        .into_iter()
        .map(|value| (value.id.clone(), value))
        .collect::<HashMap<_, _>>();
    let decisions = index
        .decision_details(options.view)?
        .into_iter()
        .map(|value| (value.meta.id.clone(), value))
        .collect::<HashMap<_, _>>();
    let anchors = index.anchors(options.view, options.file, options.line)?;
    let tests_by_id = tests
        .iter()
        .map(|test| (test.id.clone(), test.clone()))
        .collect::<HashMap<_, _>>();
    let mut anchor_test_ids = anchors
        .iter()
        .flat_map(|anchor| anchor.tests.iter())
        .filter(|id| selected_includes(id))
        .cloned()
        .collect::<BTreeSet<_>>();
    for anchor in anchors.iter().filter(|anchor| anchor.kind == "branch") {
        anchor_test_ids.extend(
            metadata
                .values()
                .filter(|detail| detail.parent_id.as_deref() == Some(anchor.id.as_str()))
                .flat_map(|detail| detail.tests.iter())
                .filter(|id| selected_includes(id))
                .cloned(),
        );
    }
    let all_anchor_tests = anchor_test_ids
        .iter()
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
    let total_anchor_tests = all_anchor_tests.len();
    let anchor_tests_page = all_anchor_tests
        .iter()
        .skip(options.offset)
        .take(options.limit)
        .cloned()
        .collect::<Vec<_>>();
    let render_anchor = |anchor: crate::coverage_index::IndexedAnchor| {
        let branch_alternatives = if anchor.kind == "branch" {
            metadata
                .values()
                .filter(|detail| {
                    detail.obligation == "branch"
                        && detail.parent_id.as_deref() == Some(anchor.id.as_str())
                })
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        };
        let branch_tests = branch_alternatives
            .iter()
            .flat_map(|detail| detail.tests.iter())
            .filter(|test| selected_includes(test))
            .cloned()
            .collect::<BTreeSet<_>>();
        let covering_tests = if anchor.kind == "branch" {
            branch_tests.len()
        } else {
            anchor
                .tests
                .iter()
                .filter(|test| selected_includes(test))
                .count()
        };
        let detail = metadata.get(&anchor.id);
        let decision = decisions
            .get(&anchor.id)
            .cloned()
            .map(|decision| selected_decision(decision, selected.as_ref()));
        let conditions = decision.as_ref().map_or(anchor.conditions, |decision| {
            Some(decision.conditions.len())
        });
        let covered_conditions = decision
            .as_ref()
            .map_or(anchor.covered_conditions, |decision| {
                Some(
                    decision
                        .conditions
                        .iter()
                        .filter(|condition| condition.covered)
                        .count(),
                )
            });
        let covered = match anchor.kind.as_str() {
            "decision" => conditions == covered_conditions,
            "branch" if !branch_alternatives.is_empty() => branch_alternatives
                .iter()
                .all(|detail| detail.tests.iter().any(|test| selected_includes(test))),
            "branch" => anchor.covered,
            _ => covering_tests > 0,
        };
        let branch_source = branch_alternatives
            .first()
            .map(|detail| detail.source.clone());
        let missing_branch_alternatives = branch_alternatives
            .iter()
            .filter(|detail| !detail.tests.iter().any(|test| selected_includes(test)))
            .filter_map(|detail| detail.alternative.clone())
            .collect::<Vec<_>>();
        CoverageAnchor {
            kind: anchor.kind,
            id: anchor.id,
            column: anchor.column,
            covered,
            source: decision
                .as_ref()
                .map(|decision| decision.meta.source.clone())
                .or(branch_source)
                .or_else(|| {
                    detail.map(|detail| {
                        detail
                            .label
                            .clone()
                            .unwrap_or_else(|| detail.source.clone())
                    })
                })
                .and_then(|source| compact_source(&source)),
            missing: if missing_branch_alternatives.is_empty() {
                detail.and_then(|detail| detail.alternative.clone())
            } else {
                Some(missing_branch_alternatives.join("; "))
            },
            covering_tests,
            covered_conditions,
            conditions,
        }
    };
    let rendered_anchors = anchors.into_iter().map(render_anchor).collect::<Vec<_>>();
    let total_anchored = rendered_anchors.len();
    let covered_anchored = rendered_anchors
        .iter()
        .filter(|anchor| anchor.covered)
        .count();
    let all_limitations = index
        .limitations(options.view)?
        .into_iter()
        .filter(|limitation| limitation.file == options.file && limitation.line == options.line)
        .map(|limitation| CoverageFileLimitation {
            id: limitation.id,
            kind: limitation.kind,
            file: limitation.file,
            line: limitation.line,
            column: limitation.column,
            source: limitation.source,
            reason: limitation.reason,
            blocking: limitation.blocking,
            effect: "outside-measured-denominator".into(),
        })
        .collect::<Vec<_>>();
    let total_limitations = all_limitations.len();
    let limitations_page = all_limitations
        .iter()
        .skip(options.offset)
        .take(options.limit)
        .cloned()
        .collect::<Vec<_>>();
    let (file_detail, _) = coverage_file_detail_query(
        index,
        CoverageFileDetailOptions {
            run: options.run,
            view: options.view,
            kind: options.kind,
            runner: options.runner,
            selector: options.file,
            metric: MinimizeMetric::All,
            offset: 0,
            limit: usize::MAX,
        },
    )?;
    let gap_line = file_detail
        .gap_lines
        .into_iter()
        .find(|gap| gap.line == options.line);
    // Covered lines are absent from the gap projection, but their anchored
    // statement/branch/decision metadata still carries the source snippet.
    // Prefer the exact gap-line text when present and otherwise retain that
    // anchored source so a successful line query never contradicts itself by
    // claiming the source is unavailable while printing it below.
    let source = gap_line
        .as_ref()
        .and_then(|gap| gap.source.clone())
        .or_else(|| {
            rendered_anchors
                .iter()
                .filter_map(|anchor| anchor.source.clone())
                .min_by_key(|source| source.len())
        });
    let all_remaining = gap_line.map_or_else(Vec::new, |gap| gap.obligations);
    let total_remaining = all_remaining.len();
    let remaining_page = all_remaining
        .iter()
        .skip(options.offset)
        .take(options.limit)
        .cloned()
        .collect::<Vec<_>>();
    let Some(line) = index.line(options.view, options.file, options.line)? else {
        let anchored = rendered_anchors
            .iter()
            .skip(options.offset)
            .take(options.limit)
            .cloned()
            .collect::<Vec<_>>();
        let total = total_anchored.max(total_limitations).max(total_remaining);
        let total = total.max(total_anchor_tests);
        let returned = anchored
            .len()
            .max(limitations_page.len())
            .max(remaining_page.len())
            .max(anchor_tests_page.len());
        return Ok((
            CoverageCoversData::Anchors(CoverageCoversAnchorsData {
                run: options.run.into(),
                filters,
                location,
                source,
                source_origin: None,
                line_obligation: false,
                anchored,
                total_anchored,
                covered_anchored,
                total_limitations,
                limitations: limitations_page,
                total_remaining,
                remaining: remaining_page,
                total_tests: total_anchor_tests,
                tests: anchor_tests_page,
                assertions: None,
            }),
            pagination(options.offset, options.limit, returned, total),
        ));
    };
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
    let anchored_page = rendered_anchors
        .into_iter()
        .skip(options.offset)
        .take(options.limit)
        .collect::<Vec<_>>();
    let total = all_tests
        .len()
        .max(all_phases.len())
        .max(total_anchored)
        .max(total_limitations)
        .max(total_remaining);
    let returned = tests_page
        .len()
        .max(phases_page.len())
        .max(anchored_page.len())
        .max(limitations_page.len())
        .max(remaining_page.len());
    Ok((
        CoverageCoversData::Line(CoverageCoversLineData {
            run: options.run.into(),
            filters,
            location,
            source,
            source_origin: None,
            covered: line.tests.iter().any(|test| selected_includes(test)),
            confidence: line.confidence,
            total_tests: all_tests.len(),
            total_phases: all_phases.len(),
            total_anchored,
            covered_anchored,
            total_limitations,
            total_remaining,
            tests: tests_page,
            phases: phases_page,
            anchored: anchored_page,
            limitations: limitations_page,
            remaining: remaining_page,
            assertions: None,
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
    /// `exact`, or `run-wide` when this test ran and nothing can say what it
    /// reached. Under `run-wide` the totals below are zero because the test
    /// has no coverage of its own, not because it reached nothing.
    pub attribution: String,
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
        return Err(QueryError::TestNotFound(options.selector.into()));
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
                attribution: test.summary.attribution,
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CoverageDecisionMatch {
    pub id: String,
    pub file: String,
    pub line: usize,
    pub column: usize,
    pub source: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CoverageDecisionCondition {
    pub index: usize,
    pub source: String,
    pub covered: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub assertion_covered: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub witness: Option<[crate::coverage_analysis::McdcVector; 2]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub witness_tests: Option<[Vec<Id>; 2]>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CoverageDecisionTotals {
    pub conditions: usize,
    pub vector_observations: usize,
    pub tests: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CoverageSelectedDecision {
    pub meta: DecisionMeta,
    pub executed: bool,
    pub covered: bool,
    pub vectors: Vec<crate::coverage_analysis::McdcVector>,
    pub vector_observations: Vec<crate::coverage_report::VectorObservation>,
    pub conditions: Vec<CoverageDecisionCondition>,
    pub tests: Vec<Id>,
    pub confidence: CoverageConfidence,
    pub totals: CoverageDecisionTotals,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CoverageDecisionMatchesData {
    pub run: String,
    pub filters: CoverageQueryFilters,
    pub decisions: Vec<CoverageDecisionMatch>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CoverageDecisionDetailData {
    pub run: String,
    pub filters: CoverageQueryFilters,
    pub pagination_applies_to: String,
    pub decisions: Vec<CoverageSelectedDecision>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(untagged)]
pub enum CoverageDecisionData {
    Matches(CoverageDecisionMatchesData),
    Detail(CoverageDecisionDetailData),
}

#[derive(Debug, Clone, Copy)]
pub struct CoverageDecisionQueryOptions<'a> {
    pub run: &'a str,
    pub view: CoverageViewId,
    pub kind: Option<&'a str>,
    pub runner: Option<&'a str>,
    pub selector: &'a str,
    pub offset: usize,
    pub limit: usize,
}

fn selector_location(selector: &str) -> Option<(&str, usize)> {
    let (prefix, last) = selector.rsplit_once(':')?;
    let last = last.parse::<usize>().ok()?;
    if let Some((file, possible_line)) = prefix.rsplit_once(':')
        && let Ok(line) = possible_line.parse::<usize>()
    {
        return Some((file, line));
    }
    Some((prefix, last))
}

fn selected_decision(
    decision: crate::coverage_report::DecisionResult,
    selected: Option<&BTreeSet<String>>,
) -> crate::coverage_report::DecisionResult {
    let Some(selected) = selected else {
        return decision;
    };
    let vector_observations = decision
        .vector_observations
        .into_iter()
        .filter_map(|mut observation| {
            observation
                .tests
                .retain(|test| selected.contains(test.as_str()));
            (!observation.tests.is_empty()).then_some(observation)
        })
        .collect::<Vec<_>>();
    let vectors = vector_observations
        .iter()
        .map(|observation| observation.vector.clone())
        .collect::<Vec<_>>();
    let conditions = decision
        .meta
        .conditions
        .iter()
        .enumerate()
        .map(|(index, source)| {
            let mut witness = None;
            let mut witness_tests = None;
            'pairs: for left in 0..vector_observations.len() {
                for right in (left + 1)..vector_observations.len() {
                    let first = &vector_observations[left];
                    let second = &vector_observations[right];
                    if is_independence_pair(&first.vector, &second.vector, index) {
                        witness = Some([first.vector.clone(), second.vector.clone()]);
                        witness_tests = Some([first.tests.clone(), second.tests.clone()]);
                        break 'pairs;
                    }
                }
            }
            crate::coverage_report::ConditionResult {
                index,
                source: source.clone(),
                covered: witness.is_some(),
                assertion_covered: false,
                witness,
                witness_tests,
            }
        })
        .collect::<Vec<_>>();
    crate::coverage_report::DecisionResult {
        meta: decision.meta,
        executed: !vectors.is_empty(),
        covered: conditions.iter().all(|condition| condition.covered),
        vectors,
        vector_observations,
        conditions,
        tests: decision
            .tests
            .into_iter()
            .filter(|test| selected.contains(test.as_str()))
            .collect(),
        confidence: decision.confidence,
    }
}

pub fn coverage_decision_query(
    index: &CoverageIndex<'_>,
    options: CoverageDecisionQueryOptions<'_>,
) -> Result<(CoverageDecisionData, AgentPagination), QueryError> {
    if options.limit == 0 {
        return Err(QueryError::InvalidPagination);
    }
    let tests = index.test_summaries(options.view)?;
    let selected = selected_test_ids(&tests, options.kind, options.runner)?;
    let decisions = index.decision_details(options.view)?;
    let mut matches = decisions
        .into_iter()
        .filter(|decision| decision.meta.id == options.selector)
        .collect::<Vec<_>>();
    if matches.is_empty()
        && let Some((file, line)) = selector_location(options.selector)
    {
        matches = index
            .decision_details(options.view)?
            .into_iter()
            .filter(|decision| decision.meta.file == file && decision.meta.line == line)
            .collect();
    }
    if matches.is_empty() {
        return Err(QueryError::DecisionNotFound(options.selector.into()));
    }
    let filters = query_filters(options.view, options.kind, options.runner);
    if matches.len() > 1 {
        let total = matches.len();
        let page = matches
            .into_iter()
            .skip(options.offset)
            .take(options.limit)
            .map(|decision| CoverageDecisionMatch {
                id: decision.meta.id,
                file: decision.meta.file,
                line: decision.meta.line,
                column: decision.meta.column,
                source: decision.meta.source,
            })
            .collect::<Vec<_>>();
        let returned = page.len();
        return Ok((
            CoverageDecisionData::Matches(CoverageDecisionMatchesData {
                run: options.run.into(),
                filters,
                decisions: page,
            }),
            pagination(options.offset, options.limit, returned, total),
        ));
    }
    let filtered = selected_decision(
        matches.into_iter().next().expect("one decision match"),
        selected.as_ref(),
    );
    let totals = CoverageDecisionTotals {
        conditions: filtered.conditions.len(),
        vector_observations: filtered.vector_observations.len(),
        tests: filtered.tests.len(),
    };
    let total = totals
        .conditions
        .max(totals.vector_observations)
        .max(totals.tests);
    let vector_observations = filtered
        .vector_observations
        .iter()
        .skip(options.offset)
        .take(options.limit)
        .cloned()
        .collect::<Vec<_>>();
    let vectors = vector_observations
        .iter()
        .map(|observation| observation.vector.clone())
        .collect::<Vec<_>>();
    let conditions = filtered
        .conditions
        .iter()
        .skip(options.offset)
        .take(options.limit)
        .map(|condition| CoverageDecisionCondition {
            index: condition.index,
            source: condition.source.clone(),
            covered: condition.covered,
            assertion_covered: selected.is_none().then_some(condition.assertion_covered),
            witness: condition.witness.clone(),
            witness_tests: condition.witness_tests.clone(),
        })
        .collect::<Vec<_>>();
    let tests = filtered
        .tests
        .iter()
        .skip(options.offset)
        .take(options.limit)
        .cloned()
        .collect::<Vec<_>>();
    let returned = vector_observations
        .len()
        .max(conditions.len())
        .max(tests.len());
    Ok((
        CoverageDecisionData::Detail(CoverageDecisionDetailData {
            run: options.run.into(),
            filters,
            pagination_applies_to:
                "conditions, vectorObservations, and tests independently within each decision"
                    .into(),
            decisions: vec![CoverageSelectedDecision {
                meta: filtered.meta,
                executed: filtered.executed,
                covered: filtered.covered,
                vectors,
                vector_observations,
                conditions,
                tests,
                confidence: filtered.confidence,
                totals,
            }],
        }),
        pagination(options.offset, options.limit, returned, total),
    ))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CoverageOtherTest {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CoverageOtherCoverage {
    pub covered_elsewhere: bool,
    pub kinds: Vec<String>,
    pub runners: Vec<String>,
    pub tests: Vec<CoverageOtherTest>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CoverageLineObligation {
    pub kind: String,
    pub id: String,
    pub line: usize,
    pub other_coverage: CoverageOtherCoverage,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CoveragePointObligation {
    pub kind: String,
    pub id: String,
    pub line: usize,
    pub column: usize,
    pub source: String,
    pub other_coverage: CoverageOtherCoverage,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CoverageBranchObligation {
    pub kind: String,
    pub id: String,
    pub line: usize,
    pub column: usize,
    pub source: String,
    pub missing: String,
    pub other_coverage: CoverageOtherCoverage,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CoverageMcdcObligation {
    pub kind: String,
    pub id: String,
    pub line: usize,
    pub column: usize,
    pub decision: String,
    pub missing_condition: String,
    #[serde(skip)]
    pub condition_index: usize,
    pub observed_vectors: Vec<String>,
    pub other_coverage: CoverageOtherCoverage,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(untagged)]
pub enum CoverageFileObligation {
    Line(CoverageLineObligation),
    Point(CoveragePointObligation),
    Branch(CoverageBranchObligation),
    Mcdc(CoverageMcdcObligation),
}

impl CoverageFileObligation {
    fn line(&self) -> usize {
        match self {
            Self::Line(value) => value.line,
            Self::Point(value) => value.line,
            Self::Branch(value) => value.line,
            Self::Mcdc(value) => value.line,
        }
    }

    fn kind(&self) -> &str {
        match self {
            Self::Line(value) => &value.kind,
            Self::Point(value) => &value.kind,
            Self::Branch(value) => &value.kind,
            Self::Mcdc(value) => &value.kind,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CoverageFileLimitation {
    pub id: String,
    pub kind: String,
    pub file: String,
    pub line: usize,
    pub column: usize,
    pub source: String,
    pub reason: String,
    pub blocking: bool,
    pub effect: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CoverageFileCounts {
    /// Measured lines in the file, and how many the selected tests covered:
    /// the file's own line coverage, comparable with other tools'.
    pub total_lines: usize,
    pub covered_lines: usize,
    pub uncovered_lines: usize,
    pub uncovered_statements: usize,
    pub uncovered_functions: usize,
    pub missing_branches: usize,
    pub missing_mcdc_conditions: usize,
    pub measurement_limitations: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CoverageFileGapLine {
    pub line: usize,
    pub state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    pub obligations: Vec<CoverageFileObligation>,
    pub limitations: Vec<CoverageFileLimitation>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CoverageFileDetailData {
    pub run: String,
    pub filters: CoverageQueryFilters,
    pub file: String,
    pub metric: MinimizeMetric,
    pub counts: CoverageFileCounts,
    pub total_tests: usize,
    pub total_obligations: usize,
    pub total_gap_lines: usize,
    pub gap_lines: Vec<CoverageFileGapLine>,
    pub total_limitations: usize,
    /// The run's saved assertion assessment for this place, attached by the
    /// CLI when the run has one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub assertions: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Copy)]
pub struct CoverageFileDetailOptions<'a> {
    pub run: &'a str,
    pub view: CoverageViewId,
    pub kind: Option<&'a str>,
    pub runner: Option<&'a str>,
    pub selector: &'a str,
    pub metric: MinimizeMetric,
    pub offset: usize,
    pub limit: usize,
}

fn other_coverage<T: AsRef<str>>(
    test_ids: &[T],
    selected: Option<&BTreeSet<String>>,
    tests: &HashMap<String, IndexedTestSummary>,
) -> CoverageOtherCoverage {
    let covered = selected.map_or_else(Vec::new, |selected| {
        test_ids
            .iter()
            .filter(|id| !selected.contains(id.as_ref()))
            .filter_map(|id| tests.get(id.as_ref()))
            .collect::<Vec<_>>()
    });
    CoverageOtherCoverage {
        covered_elsewhere: !covered.is_empty(),
        kinds: covered
            .iter()
            .map(|test| test.provenance.kind.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect(),
        runners: covered
            .iter()
            .map(|test| test.provenance.runner.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect(),
        tests: covered
            .into_iter()
            .map(|test| CoverageOtherTest {
                id: test.id.clone(),
                name: test.name.clone(),
            })
            .collect(),
    }
}

fn vector_text(vector: &crate::coverage_analysis::McdcVector) -> String {
    let values = vector
        .values
        .iter()
        .map(|value| match value {
            None => '-',
            Some(false) => 'F',
            Some(true) => 'T',
        })
        .collect::<String>();
    format!("{values} -> {}", if vector.outcome { 'T' } else { 'F' })
}

fn obligation_matches_metric(obligation: &CoverageFileObligation, metric: MinimizeMetric) -> bool {
    metric == MinimizeMetric::All
        || matches!(
            (obligation.kind(), metric),
            ("line", MinimizeMetric::Lines)
                | ("statement", MinimizeMetric::Statements)
                | ("function", MinimizeMetric::Functions)
                | ("branch", MinimizeMetric::Branches)
                | ("mcdc", MinimizeMetric::Mcdc)
        )
}

fn compact_source(value: &str) -> Option<String> {
    let line = value.lines().find(|line| !line.trim().is_empty())?.trim();
    let compact = line.split_whitespace().collect::<Vec<_>>().join(" ");
    if compact.is_empty() {
        None
    } else if compact.chars().count() > 120 {
        Some(format!(
            "{}…",
            compact.chars().take(119).collect::<String>()
        ))
    } else {
        Some(compact)
    }
}

fn obligation_source(obligation: &CoverageFileObligation) -> Option<String> {
    match obligation {
        CoverageFileObligation::Line(_) => None,
        CoverageFileObligation::Point(value) => compact_source(&value.source),
        CoverageFileObligation::Branch(value) => compact_source(&value.source),
        CoverageFileObligation::Mcdc(value) => compact_source(&value.decision),
    }
}

pub fn coverage_file_detail_query(
    index: &CoverageIndex<'_>,
    options: CoverageFileDetailOptions<'_>,
) -> Result<(CoverageFileDetailData, AgentPagination), QueryError> {
    if options.limit == 0 {
        return Err(QueryError::InvalidPagination);
    }
    let test_details = index.test_details(options.view)?;
    let test_summaries = test_details
        .iter()
        .map(|test| test.summary.clone())
        .collect::<Vec<_>>();
    let selected = selected_test_ids(&test_summaries, options.kind, options.runner)?;
    let tests_by_id = test_summaries
        .into_iter()
        .map(|test| (test.id.clone(), test))
        .collect::<HashMap<_, _>>();
    let lines = index.lines(options.view)?;
    let limitation_records = index.limitations(options.view)?;
    let files = lines
        .iter()
        .map(|line| line.file.as_str())
        .chain(
            limitation_records
                .iter()
                .map(|limitation| limitation.file.as_str()),
        )
        .collect::<BTreeSet<_>>();
    let file = if files.contains(options.selector) {
        options.selector.to_owned()
    } else {
        let matches = files
            .into_iter()
            .filter(|file| file.contains(options.selector))
            .collect::<Vec<_>>();
        if matches.is_empty() {
            return Err(QueryError::SourceNotFound(options.selector.into()));
        }
        if matches.len() != 1 {
            return Err(QueryError::AmbiguousSelector {
                selector: options.selector.into(),
                matches: matches.into_iter().map(str::to_owned).collect(),
            });
        }
        matches[0].to_owned()
    };
    let selected_includes = |tests: &[String]| {
        selected.as_ref().map_or(!tests.is_empty(), |selected| {
            tests.iter().any(|test| selected.contains(test))
        })
    };
    let total_lines = lines
        .iter()
        .filter(|line| line.measured && line.file == file)
        .count();
    let uncovered_lines = lines
        .iter()
        .filter(|line| line.measured && line.file == file && !selected_includes(&line.tests))
        .map(|line| {
            CoverageFileObligation::Line(CoverageLineObligation {
                kind: "line".into(),
                id: format!("line:{}:{}", line.file, line.line),
                line: line.line,
                other_coverage: other_coverage(&line.tests, selected.as_ref(), &tests_by_id),
            })
        })
        .collect::<Vec<_>>();
    let metadata = index.hit_metadata(options.view)?;
    let statements = metadata
        .iter()
        .filter(|point| {
            point.file == file
                && point.obligation == "statement"
                && !selected_includes(&point.tests)
        })
        .map(|point| {
            CoverageFileObligation::Point(CoveragePointObligation {
                kind: "statement".into(),
                id: point.id.clone(),
                line: point.line,
                column: point.column,
                source: point.label.clone().unwrap_or_else(|| point.source.clone()),
                other_coverage: other_coverage(&point.tests, selected.as_ref(), &tests_by_id),
            })
        })
        .collect::<Vec<_>>();
    let functions = metadata
        .iter()
        .filter(|point| {
            point.file == file && point.obligation == "function" && !selected_includes(&point.tests)
        })
        .map(|point| {
            CoverageFileObligation::Point(CoveragePointObligation {
                kind: "function".into(),
                id: point.id.clone(),
                line: point.line,
                column: point.column,
                source: point.label.clone().unwrap_or_else(|| point.source.clone()),
                other_coverage: other_coverage(&point.tests, selected.as_ref(), &tests_by_id),
            })
        })
        .collect::<Vec<_>>();
    let branches = metadata
        .iter()
        .filter(|branch| {
            branch.file == file
                && branch.obligation == "branch"
                && !selected_includes(&branch.tests)
        })
        .map(|branch| {
            CoverageFileObligation::Branch(CoverageBranchObligation {
                kind: "branch".into(),
                id: branch.id.clone(),
                line: branch.line,
                column: branch.column,
                source: branch.source.clone(),
                missing: branch.alternative.clone().unwrap_or_default(),
                other_coverage: other_coverage(&branch.tests, selected.as_ref(), &tests_by_id),
            })
        })
        .collect::<Vec<_>>();
    // A line ran when anything anchored on it was observed. A line inside a
    // multi-line expression has no line obligation of its own, so this is
    // what tells one that never ran from one that ran part of the way.
    let observed_lines = metadata
        .iter()
        .filter(|point| point.file == file && selected_includes(&point.tests))
        .map(|point| point.line)
        .collect::<BTreeSet<_>>();
    let original_decisions = index.decision_details(options.view)?;
    let mut mcdc = Vec::new();
    for original in original_decisions
        .iter()
        .filter(|decision| decision.meta.file == file)
    {
        let filtered = selected_decision(original.clone(), selected.as_ref());
        for condition in filtered
            .conditions
            .iter()
            .filter(|condition| !condition.covered)
        {
            let original_tests = original.conditions[condition.index]
                .witness_tests
                .clone()
                .unwrap_or_default()
                .into_iter()
                .flatten()
                .collect::<Vec<_>>();
            mcdc.push(CoverageFileObligation::Mcdc(CoverageMcdcObligation {
                kind: "mcdc".into(),
                id: original.meta.id.clone(),
                line: original.meta.line,
                column: original.meta.column,
                decision: original.meta.source.clone(),
                missing_condition: condition.source.clone(),
                condition_index: condition.index,
                observed_vectors: filtered
                    .vector_observations
                    .iter()
                    .map(|observation| vector_text(&observation.vector))
                    .collect(),
                other_coverage: other_coverage(&original_tests, selected.as_ref(), &tests_by_id),
            }));
        }
    }
    let mut obligations = uncovered_lines
        .iter()
        .chain(statements.iter())
        .chain(functions.iter())
        .chain(branches.iter())
        .chain(mcdc.iter())
        .filter(|obligation| obligation_matches_metric(obligation, options.metric))
        .cloned()
        .collect::<Vec<_>>();
    obligations.sort_by(|left, right| {
        left.line()
            .cmp(&right.line())
            .then_with(|| left.kind().cmp(right.kind()))
    });
    let mut limitations = limitation_records
        .into_iter()
        .filter(|limitation| limitation.file == file)
        .map(|limitation| CoverageFileLimitation {
            id: limitation.id,
            kind: limitation.kind,
            file: limitation.file,
            line: limitation.line,
            column: limitation.column,
            source: limitation.source,
            reason: limitation.reason,
            blocking: limitation.blocking,
            effect: "outside-measured-denominator".into(),
        })
        .collect::<Vec<_>>();
    limitations.sort_by(|left, right| {
        left.line
            .cmp(&right.line)
            .then_with(|| left.column.cmp(&right.column))
            .then_with(|| left.id.cmp(&right.id))
    });
    let total_tests = test_details
        .iter()
        .filter(|test| {
            selected
                .as_ref()
                .is_none_or(|selected| selected.contains(&test.summary.id))
                && test.lines.iter().any(|line| line.file == file)
        })
        .count();
    let total_obligations = obligations.len();
    let total_limitations = limitations.len();
    let mut grouped =
        BTreeMap::<usize, (Vec<CoverageFileObligation>, Vec<CoverageFileLimitation>)>::new();
    for obligation in obligations {
        grouped
            .entry(obligation.line())
            .or_default()
            .0
            .push(obligation);
    }
    for limitation in limitations {
        grouped
            .entry(limitation.line)
            .or_default()
            .1
            .push(limitation);
    }
    let gap_lines = grouped
        .into_iter()
        .map(|(line, (obligations, limitations))| {
            let source = obligations.iter().find_map(obligation_source).or_else(|| {
                limitations
                    .iter()
                    .find_map(|value| compact_source(&value.source))
            });
            // An MC/DC condition can be evaluated without anything on its own
            // line being recorded, so only branch and point gaps can say a
            // line never ran.
            let never_ran = !observed_lines.contains(&line)
                && !obligations.is_empty()
                && obligations
                    .iter()
                    .all(|value| !matches!(value, CoverageFileObligation::Mcdc(_)));
            let state = if never_ran
                || obligations
                    .iter()
                    .any(|value| matches!(value, CoverageFileObligation::Line(_)))
            {
                "missing"
            } else if !obligations.is_empty() || observed_lines.contains(&line) {
                // A line that ran with part of it outside measurement ran part
                // of the way, as its line view says.
                "part"
            } else {
                "limited"
            };
            CoverageFileGapLine {
                line,
                state: state.into(),
                source,
                obligations,
                limitations,
            }
        })
        .collect::<Vec<_>>();
    let total_gap_lines = gap_lines.len();
    let selected_gap_lines = gap_lines
        .into_iter()
        .skip(options.offset)
        .take(options.limit)
        .collect::<Vec<_>>();
    let returned = selected_gap_lines.len();
    Ok((
        CoverageFileDetailData {
            run: options.run.into(),
            filters: query_filters(options.view, options.kind, options.runner),
            file,
            metric: options.metric,
            counts: CoverageFileCounts {
                total_lines,
                covered_lines: total_lines - uncovered_lines.len(),
                uncovered_lines: uncovered_lines.len(),
                uncovered_statements: statements.len(),
                uncovered_functions: functions.len(),
                missing_branches: branches.len(),
                missing_mcdc_conditions: mcdc.len(),
                measurement_limitations: total_limitations,
            },
            total_tests,
            total_obligations,
            total_gap_lines,
            gap_lines: selected_gap_lines,
            total_limitations,
            assertions: None,
        },
        pagination(options.offset, options.limit, returned, total_gap_lines),
    ))
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CoverageDiffDelta {
    #[serde(serialize_with = "crate::coverage_analysis::serialize_javascript_number")]
    pub lines: f64,
    #[serde(serialize_with = "crate::coverage_analysis::serialize_javascript_number")]
    pub branches: f64,
    #[serde(serialize_with = "crate::coverage_analysis::serialize_javascript_number")]
    pub mcdc: f64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CoverageDiffSide {
    pub line_count: usize,
    pub branch_count: usize,
    pub mcdc_count: usize,
    pub lines: Vec<String>,
    pub branches: Vec<String>,
    pub mcdc: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CoverageDiffData {
    pub filters: CoverageQueryFilters,
    pub older: String,
    pub newer: String,
    pub delta: CoverageDiffDelta,
    pub gained: CoverageDiffSide,
    pub lost: CoverageDiffSide,
    /// Whether what the two runs measured differs, from their fingerprints.
    /// Absent where a run has none.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inputs: Option<CoverageDiffInputs>,
}

/// What two runs of a comparison had in common. With the same source, a
/// difference between them is not a change in the code: two e2e runs of one
/// commit read "lost: 11 lines, 31 branches" with nothing to say so.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CoverageDiffInputs {
    /// The measured source, its dependencies or its configuration.
    pub source_changed: bool,
    pub tests_changed: bool,
}

#[derive(Debug, Clone, Copy)]
pub struct CoverageDiffQueryOptions<'a> {
    pub older_run: &'a str,
    pub newer_run: &'a str,
    pub view: CoverageViewId,
    pub kind: Option<&'a str>,
    pub runner: Option<&'a str>,
    pub offset: usize,
    pub limit: usize,
}

struct DiffSnapshot {
    summary: CoverageSummary,
    lines: BTreeSet<String>,
    branches: HashMap<String, String>,
    mcdc: HashMap<String, String>,
}

fn diff_snapshot(
    index: &CoverageIndex<'_>,
    view: CoverageViewId,
) -> Result<DiffSnapshot, QueryError> {
    let summary = index.summary(view)?;
    let lines = index
        .lines(view)?
        .into_iter()
        .filter(|line| line.covered)
        .map(|line| format!("{}:{}", line.file, line.line))
        .collect();
    let branches = index
        .hit_metadata(view)?
        .into_iter()
        .filter(|metadata| metadata.obligation == "branch" && !metadata.tests.is_empty())
        .map(|metadata| {
            let parent = metadata
                .parent_id
                .ok_or(QueryError::InvalidRecordSelection)?;
            Ok((
                format!("{parent}:{}", metadata.id),
                format!(
                    "{}:{} {}",
                    metadata.file,
                    metadata.line,
                    metadata.alternative.unwrap_or_default()
                ),
            ))
        })
        .collect::<Result<HashMap<_, _>, QueryError>>()?;
    let mcdc = index
        .decision_details(view)?
        .into_iter()
        .flat_map(|decision| {
            decision
                .conditions
                .into_iter()
                .filter(|condition| condition.covered)
                .map(move |condition| {
                    (
                        format!("{}:c{}", decision.meta.id, condition.index),
                        format!(
                            "{}:{} C{} {}",
                            decision.meta.file,
                            decision.meta.line,
                            condition.index + 1,
                            condition.source
                        ),
                    )
                })
        })
        .collect();
    Ok(DiffSnapshot {
        summary,
        lines,
        branches,
        mcdc,
    })
}

fn js_string_cmp(left: &str, right: &str) -> std::cmp::Ordering {
    left.encode_utf16().cmp(right.encode_utf16())
}

fn rounded_delta(newer: f64, older: f64) -> f64 {
    ((newer - older) * 100.0).round() / 100.0
}

pub fn coverage_diff_query(
    older: &CoverageIndex<'_>,
    newer: &CoverageIndex<'_>,
    options: CoverageDiffQueryOptions<'_>,
) -> Result<(CoverageDiffData, AgentPagination), QueryError> {
    if options.limit == 0 {
        return Err(QueryError::InvalidPagination);
    }
    let older = diff_snapshot(older, options.view)?;
    let newer = diff_snapshot(newer, options.view)?;
    let mut gained_lines = newer
        .lines
        .difference(&older.lines)
        .cloned()
        .collect::<Vec<_>>();
    let mut lost_lines = older
        .lines
        .difference(&newer.lines)
        .cloned()
        .collect::<Vec<_>>();
    let mut gained_branches = newer
        .branches
        .iter()
        .filter(|(id, _)| !older.branches.contains_key(*id))
        .map(|(_, label)| label.clone())
        .collect::<Vec<_>>();
    let mut lost_branches = older
        .branches
        .iter()
        .filter(|(id, _)| !newer.branches.contains_key(*id))
        .map(|(_, label)| label.clone())
        .collect::<Vec<_>>();
    let mut gained_mcdc = newer
        .mcdc
        .iter()
        .filter(|(id, _)| !older.mcdc.contains_key(*id))
        .map(|(_, label)| label.clone())
        .collect::<Vec<_>>();
    let mut lost_mcdc = older
        .mcdc
        .iter()
        .filter(|(id, _)| !newer.mcdc.contains_key(*id))
        .map(|(_, label)| label.clone())
        .collect::<Vec<_>>();
    for values in [
        &mut gained_lines,
        &mut lost_lines,
        &mut gained_branches,
        &mut lost_branches,
        &mut gained_mcdc,
        &mut lost_mcdc,
    ] {
        values.sort_by(|left, right| js_string_cmp(left, right));
    }
    let total = [
        gained_lines.len(),
        gained_branches.len(),
        gained_mcdc.len(),
        lost_lines.len(),
        lost_branches.len(),
        lost_mcdc.len(),
    ]
    .into_iter()
    .max()
    .unwrap_or(0);
    let page = |values: &[String]| {
        values
            .iter()
            .skip(options.offset)
            .take(options.limit)
            .cloned()
            .collect::<Vec<_>>()
    };
    let gained = CoverageDiffSide {
        line_count: gained_lines.len(),
        branch_count: gained_branches.len(),
        mcdc_count: gained_mcdc.len(),
        lines: page(&gained_lines),
        branches: page(&gained_branches),
        mcdc: page(&gained_mcdc),
    };
    let lost = CoverageDiffSide {
        line_count: lost_lines.len(),
        branch_count: lost_branches.len(),
        mcdc_count: lost_mcdc.len(),
        lines: page(&lost_lines),
        branches: page(&lost_branches),
        mcdc: page(&lost_mcdc),
    };
    let returned = [
        gained.lines.len(),
        gained.branches.len(),
        gained.mcdc.len(),
        lost.lines.len(),
        lost.branches.len(),
        lost.mcdc.len(),
    ]
    .into_iter()
    .max()
    .unwrap_or(0);
    Ok((
        CoverageDiffData {
            filters: query_filters(options.view, options.kind, options.runner),
            older: options.older_run.into(),
            newer: options.newer_run.into(),
            delta: CoverageDiffDelta {
                lines: rounded_delta(
                    newer.summary.lines.percentage,
                    older.summary.lines.percentage,
                ),
                branches: rounded_delta(
                    newer.summary.branches.percentage,
                    older.summary.branches.percentage,
                ),
                mcdc: rounded_delta(
                    newer.summary.condition_coverage_pct,
                    older.summary.condition_coverage_pct,
                ),
            },
            gained,
            lost,
            inputs: None,
        },
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
        scope_status_rank(&left.status)
            .cmp(&scope_status_rank(&right.status))
            .then_with(|| left.file.cmp(&right.file))
    });
    let groups = scope_groups(&entries);
    let unclassified = unclassified_source(&scope.roots, &entries);
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
            kind: scope.kind,
            language: scope.language,
            model: scope.model,
            mode: scope.mode,
            roots: scope.roots,
            unit: scope.unit,
            measurement_complete: scope.measurement_complete,
            counts: ScopeCounts {
                included: scope.included,
                excluded: scope.excluded,
                ambiguous: scope.ambiguous,
            },
            measurement: projection.measurement,
            groups,
            unclassified_source: unclassified,
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
    let mut test_kind_sources = BTreeMap::new();
    for test in index.test_summaries(options.view)?.iter().filter(|t| {
        t.role == "test"
            && options.kind.is_none_or(|k| t.provenance.kind == k)
            && options.runner.is_none_or(|r| t.provenance.runner == r)
    }) {
        *test_kind_sources
            .entry(test.provenance.source.clone())
            .or_insert(0) += 1;
    }
    let mut diagnostics = Vec::new();
    let mut transport_blockers = 0usize;
    if projection.empty_evidence_tests > 0 {
        diagnostics.push(CoverageDiagnostic {
            code: "TEST_EVIDENCE_MISSING".into(),
            severity: "warning".into(),
            message: format!(
                "{} test(s) recorded assertion phases but attributed zero coverage evidence; possible causes include checks of uninstrumented data, shared setup, lost async context or missing probe transport. First: {}",
                projection.empty_evidence_tests,
                projection.first_empty_evidence_test.as_deref().unwrap_or("unknown")
            ),
        });
    }
    if let Some(transport) = &projection.transport {
        if transport.corrupt_records > 0 {
            transport_blockers += 1;
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
            && transport.background_server_records == 0
            && projection.attribution.server_explicit == 0
            && projection.attribution.server_fallback == 0
        {
            transport_blockers += 1;
            diagnostics.push(CoverageDiagnostic {
                code: "REMOTE_SERVER_EVIDENCE_MISSING".into(),
                severity: "error".into(),
                message: "Remote launches were supervised, but neither scoped nor background server evidence returned. Supercov refuses to describe this measurement as complete.".into(),
            });
        }
    }
    let coverage_by_kind = index.dimensions(options.view, CoverageDimension::Kind)?;
    let coverage_by_runner = index.dimensions(options.view, CoverageDimension::Runner)?;
    let coverage_by_tests = if options.kind.is_none() && options.runner.is_none() {
        index
            .dimensions(options.view, CoverageDimension::Tests)?
            .into_iter()
            .next()
            .filter(|tests| tests.tests + tests.setups > 0)
            .map(|tests| tests.summary)
    } else {
        None
    };
    let other_e2e_kinds = coverage_by_kind
        .iter()
        .filter(|dimension| dimension.tests > 0)
        .filter_map(|dimension| dimension.kind.clone())
        .filter(|kind| kind != "e2e")
        .collect::<Vec<_>>();
    let e2e_observed = coverage_by_kind
        .iter()
        .any(|dimension| dimension.tests > 0 && dimension.kind.as_deref() == Some("e2e"));
    let e2e_gap_context = if options.kind.is_none()
        && options.runner.is_none()
        && e2e_observed
        && !other_e2e_kinds.is_empty()
    {
        let mut covered_elsewhere = IndexedGapDimensions {
            lines: 0,
            statements: 0,
            functions: 0,
            branches: 0,
            mcdc_conditions: 0,
        };
        let mut uncovered_everywhere = covered_elsewhere.clone();
        for gap in index.file_gaps(options.view, Some("e2e"), None)? {
            covered_elsewhere.lines += gap.covered_by_other_tests.lines;
            covered_elsewhere.statements += gap.covered_by_other_tests.statements;
            covered_elsewhere.functions += gap.covered_by_other_tests.functions;
            covered_elsewhere.branches += gap.covered_by_other_tests.branches;
            covered_elsewhere.mcdc_conditions += gap.covered_by_other_tests.mcdc_conditions;
            uncovered_everywhere.lines += gap.uncovered_everywhere.lines;
            uncovered_everywhere.statements += gap.uncovered_everywhere.statements;
            uncovered_everywhere.functions += gap.uncovered_everywhere.functions;
            uncovered_everywhere.branches += gap.uncovered_everywhere.branches;
            uncovered_everywhere.mcdc_conditions += gap.uncovered_everywhere.mcdc_conditions;
        }
        Some(CoverageKindGapContext {
            kind: "e2e".into(),
            other_kinds: other_e2e_kinds,
            covered_elsewhere,
            uncovered_everywhere,
        })
    } else {
        None
    };
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
    let mut measurement = projection.measurement.clone();
    if transport_blockers > 0 {
        measurement.complete = false;
        measurement.limitations = measurement.limitations.saturating_add(transport_blockers);
        measurement.blocking = measurement.blocking.saturating_add(transport_blockers);
    }
    let structurally_complete = projection.summary.coverage_complete && measurement.complete;
    let complete = options.view == CoverageViewId::Passed
        && options.valid
        && !options.stale
        && structurally_complete;
    Ok(CoverageSummaryData {
        test_kind_sources,
        assertion_coverage: None,
        run: options.run.into(),
        command: Vec::new(),
        hints: Vec::new(),
        workspace: None,
        filters,
        model: index.model()?,
        generated_at: projection.generated_at,
        valid: options.valid,
        test_exit_code: options.test_exit_code,
        stale: options.stale,
        stale_reasons: options.stale_reasons,
        structurally_complete,
        complete,
        coverage: projection.summary,
        measurement,
        coverage_by_kind,
        coverage_by_tests,
        e2e_gap_context,
        coverage_by_runner,
        attribution: projection.attribution,
        test_attribution: {
            let mut counts = TestAttributionCounts::default();
            for test in index
                .test_summaries(options.view)?
                .iter()
                .filter(|test| test.role == "test")
            {
                match test.attribution.as_str() {
                    crate::coverage_report::ATTRIBUTION_RUN_WIDE => counts.run_wide += 1,
                    crate::coverage_report::ATTRIBUTION_PARTIAL => counts.partial += 1,
                    _ => counts.exact += 1,
                }
            }
            counts
        },
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
        unclassified_source: match &projection.source_scope {
            Some(scope) if scope.ambiguous > 0 => {
                unclassified_source(&scope.roots, &index.scope_entries(options.view)?)
            }
            _ => None,
        },
        source_scope: projection.source_scope,
    })
}

#[derive(Debug, Clone, PartialEq)]
pub enum CoverageDimensionQueryData {
    Kinds(CoverageKindsData),
    Runners(CoverageRunnersData),
}

/// Coverage of one directory: what its files have to cover, how much of it
/// is covered, and how much of it each kind of test covers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CoverageArea {
    pub directory: String,
    pub files: usize,
    pub totals: IndexedGapDimensions,
    pub covered: IndexedGapDimensions,
    pub by_kind: Vec<CoverageAreaKind>,
    /// What the tests of every kind cover together. The rest of `covered`
    /// ran while no test was running. Absent where the query names a kind or
    /// a runner, and for a run stored before this was recorded.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub covered_by_tests: Option<IndexedGapDimensions>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CoverageAreaKind {
    pub kind: String,
    pub covered: IndexedGapDimensions,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CoverageAreasData {
    pub run: String,
    pub filters: CoverageQueryFilters,
    pub metric: MinimizeMetric,
    pub depth: usize,
    /// The kinds each area is split by, in the order of `byKind`. Empty when
    /// the query already names one kind or runner.
    pub kinds: Vec<String>,
    pub areas: Vec<CoverageArea>,
}

#[derive(Debug, Clone, Copy)]
pub struct CoverageAreasQueryOptions<'a> {
    pub run: &'a str,
    pub view: CoverageViewId,
    pub metric: MinimizeMetric,
    pub kind: Option<&'a str>,
    pub runner: Option<&'a str>,
    pub depth: usize,
    pub offset: usize,
    pub limit: usize,
}

/// The directory a file is counted under: its first `depth` folders, or all
/// of them when it has fewer. A file at the top of the project is in `.`.
fn area_directory(file: &str, depth: usize) -> String {
    let mut folders = file.split('/').collect::<Vec<_>>();
    folders.pop();
    if folders.is_empty() {
        ".".into()
    } else {
        folders[..folders.len().min(depth)].join("/")
    }
}

fn add_dimensions(sum: &mut IndexedGapDimensions, value: &IndexedGapDimensions) {
    sum.lines += value.lines;
    sum.statements += value.statements;
    sum.functions += value.functions;
    sum.branches += value.branches;
    sum.mcdc_conditions += value.mcdc_conditions;
}

const NO_DIMENSIONS: IndexedGapDimensions = IndexedGapDimensions {
    lines: 0,
    statements: 0,
    functions: 0,
    branches: 0,
    mcdc_conditions: 0,
};

/// One metric of a file's or a directory's counts; `all` reads as lines.
pub fn dimension_for_metric(dimensions: &IndexedGapDimensions, metric: MinimizeMetric) -> usize {
    match metric {
        MinimizeMetric::All | MinimizeMetric::Lines => dimensions.lines,
        MinimizeMetric::Statements => dimensions.statements,
        MinimizeMetric::Functions => dimensions.functions,
        MinimizeMetric::Branches => dimensions.branches,
        MinimizeMetric::Mcdc => dimensions.mcdc_conditions,
    }
}

/// Coverage by directory, split by test kind. "Where is the coverage" was the
/// first question after a full run, and answering it took an LCOV export and
/// a script: listings named files only and carried no totals.
pub fn coverage_areas_query(
    index: &CoverageIndex<'_>,
    options: CoverageAreasQueryOptions<'_>,
) -> Result<(CoverageAreasData, AgentPagination), QueryError> {
    if options.limit == 0 || options.depth == 0 {
        return Err(QueryError::InvalidPagination);
    }
    let files = index.file_gaps(options.view, options.kind, options.runner)?;
    if (options.kind.is_some() || options.runner.is_some()) && files.is_empty() {
        return Err(QueryError::TestFilterEmpty {
            kind: options.kind.map(str::to_owned),
            runner: options.runner.map(str::to_owned),
        });
    }
    let kinds = if options.kind.is_none() && options.runner.is_none() {
        index
            .dimensions(options.view, CoverageDimension::Kind)?
            .into_iter()
            .filter(|kind| kind.tests > 0 && kind.attributed > 0)
            .filter_map(|kind| kind.kind)
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };
    let mut areas = BTreeMap::<String, CoverageArea>::new();
    for file in &files {
        let directory = area_directory(&file.file, options.depth);
        let area = areas
            .entry(directory.clone())
            .or_insert_with(|| CoverageArea {
                directory,
                files: 0,
                totals: NO_DIMENSIONS,
                covered: NO_DIMENSIONS,
                by_kind: kinds
                    .iter()
                    .map(|kind| CoverageAreaKind {
                        kind: kind.clone(),
                        covered: NO_DIMENSIONS,
                    })
                    .collect(),
                covered_by_tests: None,
            });
        area.files += 1;
        add_dimensions(&mut area.totals, &file.totals);
        add_dimensions(&mut area.covered, &file.covered);
    }
    for (position, kind) in kinds.iter().enumerate() {
        for file in index.file_gaps(options.view, Some(kind), None)? {
            if let Some(area) = areas.get_mut(&area_directory(&file.file, options.depth)) {
                add_dimensions(&mut area.by_kind[position].covered, &file.covered);
            }
        }
    }
    if !kinds.is_empty() {
        let tested = index.file_gaps(options.view, Some(crate::coverage_index::ALL_TESTS), None)?;
        if !tested.is_empty() {
            for area in areas.values_mut() {
                area.covered_by_tests = Some(NO_DIMENSIONS);
            }
            for file in tested {
                if let Some(sum) = areas
                    .get_mut(&area_directory(&file.file, options.depth))
                    .and_then(|area| area.covered_by_tests.as_mut())
                {
                    add_dimensions(sum, &file.covered);
                }
            }
        }
    }
    let mut areas = areas.into_values().collect::<Vec<_>>();
    areas.sort_by(|left, right| {
        dimension_for_metric(&right.totals, options.metric)
            .cmp(&dimension_for_metric(&left.totals, options.metric))
            .then_with(|| left.directory.cmp(&right.directory))
    });
    let total = areas.len();
    let page = areas
        .into_iter()
        .skip(options.offset)
        .take(options.limit)
        .collect::<Vec<_>>();
    let returned = page.len();
    Ok((
        CoverageAreasData {
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
            metric: options.metric,
            depth: options.depth,
            kinds,
            areas: page,
        },
        pagination(options.offset, options.limit, returned, total),
    ))
}

/// A test that made assertions and has no line, hit or decision to its name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TestWithoutEvidence {
    pub id: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    pub kind: String,
    pub runner: String,
    pub outcome: String,
    pub assertions: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TestsWithoutEvidenceData {
    pub run: String,
    pub filters: CoverageQueryFilters,
    pub tests: Vec<TestWithoutEvidence>,
}

#[derive(Debug, Clone, Copy)]
pub struct TestsWithoutEvidenceOptions<'a> {
    pub run: &'a str,
    pub view: CoverageViewId,
    pub kind: Option<&'a str>,
    pub runner: Option<&'a str>,
    pub offset: usize,
    pub limit: usize,
}

/// Every test the run's summary counts under "made assertions, but Supercov
/// received no source-coverage evidence". The summary names one of them; 63
/// tests behind one example could not be followed up.
pub fn tests_without_evidence_query(
    index: &CoverageIndex<'_>,
    options: TestsWithoutEvidenceOptions<'_>,
) -> Result<(TestsWithoutEvidenceData, AgentPagination), QueryError> {
    if options.limit == 0 {
        return Err(QueryError::InvalidPagination);
    }
    let mut assertions = BTreeMap::<String, usize>::new();
    for phase in index.phase_summaries(options.view)? {
        *assertions.entry(phase.test).or_insert(0) += 1;
    }
    let mut tests = index
        .test_details(options.view)?
        .into_iter()
        .filter(|test| {
            test.summary.role == "test"
                && test.lines.is_empty()
                && test.hits.is_empty()
                && test.decisions.is_empty()
                && options
                    .kind
                    .is_none_or(|kind| test.summary.provenance.kind == kind)
                && options
                    .runner
                    .is_none_or(|runner| test.summary.provenance.runner == runner)
        })
        .filter_map(|test| {
            let assertions = *assertions.get(&test.summary.id)?;
            Some(TestWithoutEvidence {
                id: test.summary.id,
                name: test.summary.name,
                file: test.summary.file,
                kind: test.summary.provenance.kind,
                runner: test.summary.provenance.runner,
                outcome: test.summary.outcome,
                assertions,
            })
        })
        .collect::<Vec<_>>();
    tests.sort_by(|left, right| {
        (&left.file, &left.name, &left.id).cmp(&(&right.file, &right.name, &right.id))
    });
    let total = tests.len();
    let page = tests
        .into_iter()
        .skip(options.offset)
        .take(options.limit)
        .collect::<Vec<_>>();
    let returned = page.len();
    Ok((
        TestsWithoutEvidenceData {
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
            tests: page,
        },
        pagination(options.offset, options.limit, returned, total),
    ))
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
        // The tests together are one entry of the summary, never a listing.
        CoverageDimension::Kind | CoverageDimension::Tests => {
            CoverageDimensionQueryData::Kinds(CoverageKindsData {
                run: run.into(),
                filters,
                kinds: selected,
            })
        }
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

fn has_gap_for_metric(gap: &IndexedFileGap, metric: MinimizeMetric) -> bool {
    match metric {
        MinimizeMetric::All => {
            gap.uncovered_lines > 0
                || gap.uncovered_statements > 0
                || gap.uncovered_functions > 0
                || gap.missing_branches > 0
                || gap.missing_mcdc_conditions > 0
        }
        _ => gap_metric_value(gap, metric) > 0,
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
            return Err(QueryError::TestFilterEmpty {
                kind: options.kind.map(str::to_owned),
                runner: options.runner.map(str::to_owned),
            });
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
    };
    let mut missing = all
        .into_iter()
        .filter(|decision| decision.missing_conditions > 0)
        .collect::<Vec<_>>();
    missing.sort_by(|left, right| match options.sort {
        DecisionSort::Missing => right
            .missing_conditions
            .cmp(&left.missing_conditions)
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
        return Err(QueryError::TestFilterEmpty {
            kind: kind.map(str::to_owned),
            runner: runner.map(str::to_owned),
        });
    }
    if gaps_only {
        files.retain(|gap| has_gap_for_metric(gap, metric) || gap.measurement_limitations > 0);
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
    ids: &[Id],
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
        } else if candidates.contains(id.as_str()) {
            choices.push(vec![id.to_string()]);
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
    let choices = |ids: &[Id]| evidence_choices(ids, &tests, candidates, &tests_by_file);
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
    use crate::coverage_report::coverage_is_complete;
    if !target.is_finite() || !(0.0..=100.0).contains(&target) {
        return Err(QueryError::InvalidTarget(target));
    }
    if view.tests.iter().any(|test| {
        test.role == "background" && (!test.hits.is_empty() || !test.decisions.is_empty())
    }) {
        return Err(QueryError::UnattributedEvidence);
    }
    // And a test whose own coverage nothing recorded cannot be minimized
    // around either. Its empty hit set would read as a test that adds nothing
    // to any set, so the smallest set would drop it -- and what it covers is
    // precisely what nothing knows. The check above catches this whenever the
    // run-wide record holds anything, which is nearly always; this catches the
    // case where those tests happened to reach nothing measured.
    if view
        .tests
        .iter()
        .any(|test| test.role == "test" && !coverage_is_complete(&test.attribution))
    {
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

    fn scope_entry(
        file: &str,
        status: &str,
        reason: &str,
        package: Option<&str>,
    ) -> IndexedScopeEntry {
        IndexedScopeEntry {
            file: file.into(),
            status: status.into(),
            reason: reason.into(),
            package_root: package.map(str::to_owned),
            measurement_limitations: 0,
            limitation_kinds: Vec::new(),
        }
    }

    #[test]
    fn a_scope_is_grouped_by_place_and_names_the_roots_that_settle_it() {
        let unclassified = "unclassified first-party source";
        let entries = [
            scope_entry("components/a.tsx", "ambiguous", unclassified, None),
            scope_entry("components/ui/b.tsx", "ambiguous", unclassified, None),
            scope_entry("proxy.ts", "ambiguous", unclassified, None),
            scope_entry(
                "app/page.tsx",
                "included",
                "discovered package source root",
                None,
            ),
            scope_entry(
                "tests/a.test.ts",
                "excluded",
                "test or fixture source",
                None,
            ),
            scope_entry(
                "next.config.ts",
                "excluded",
                "build/test/tool configuration",
                None,
            ),
            scope_entry(
                "packages/ui/extra/c.ts",
                "ambiguous",
                unclassified,
                Some("packages/ui"),
            ),
            scope_entry(
                "packages/ui/src/d.ts",
                "included",
                "discovered package source root",
                Some("packages/ui"),
            ),
        ];
        let groups = scope_groups(&entries)
            .into_iter()
            .map(|group| (group.files, group.status, group.directory))
            .collect::<Vec<_>>();
        assert_eq!(
            groups,
            [
                (2, "ambiguous".to_owned(), "components".to_owned()),
                (1, "ambiguous".into(), ".".into()),
                (1, "ambiguous".into(), "packages/ui/extra".into()),
                (1, "included".into(), "app".into()),
                (1, "included".into(), "packages/ui/src".into()),
                (1, "excluded".into(), ".".into()),
                (1, "excluded".into(), "tests".into()),
            ]
        );
        // The roots discovery found, then each place: a directory by name,
        // and a file at the top by its own, never `.`.
        let hint =
            unclassified_source(&["app".into(), "packages/ui/src".into()], &entries).unwrap();
        assert_eq!(
            hint.roots,
            [
                "app",
                "packages/ui/src",
                "components",
                "packages/ui/extra",
                "proxy.ts"
            ]
        );
        assert_eq!(hint.directories[0].directory, "components");
        assert_eq!(hint.directories[0].files, 2);
        assert_eq!(unclassified_source(&[], &entries[3..6]), None);
    }

    #[test]
    fn a_file_is_counted_under_its_first_folders() {
        assert_eq!(area_directory("app/api/cron/route.ts", 1), "app");
        assert_eq!(area_directory("app/api/cron/route.ts", 2), "app/api");
        assert_eq!(area_directory("app/page.tsx", 2), "app");
        assert_eq!(area_directory("proxy.ts", 2), ".");
    }

    #[test]
    fn all_metric_keeps_statement_only_files_in_the_gap_set() {
        let gap = IndexedFileGap {
            view: CoverageViewId::All,
            file: "src/statement.js".into(),
            uncovered_lines: 0,
            uncovered_statements: 1,
            uncovered_functions: 0,
            missing_branches: 0,
            missing_mcdc_conditions: 0,
            measurement_limitations: 0,
            limitation_kinds: Vec::new(),
            covered_by_other_tests: crate::coverage_index::IndexedGapDimensions {
                lines: 0,
                statements: 0,
                functions: 0,
                branches: 0,
                mcdc_conditions: 0,
            },
            uncovered_everywhere: crate::coverage_index::IndexedGapDimensions {
                lines: 0,
                statements: 1,
                functions: 0,
                branches: 0,
                mcdc_conditions: 0,
            },
            totals: crate::coverage_index::IndexedGapDimensions {
                lines: 1,
                statements: 2,
                functions: 0,
                branches: 0,
                mcdc_conditions: 0,
            },
            covered: crate::coverage_index::IndexedGapDimensions {
                lines: 1,
                statements: 1,
                functions: 0,
                branches: 0,
                mcdc_conditions: 0,
            },
            score: 0,
        };
        assert!(has_gap_for_metric(&gap, MinimizeMetric::All));
        assert!(has_gap_for_metric(&gap, MinimizeMetric::Statements));
        assert!(!has_gap_for_metric(&gap, MinimizeMetric::Lines));
    }

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
            attribution: crate::coverage_report::ATTRIBUTION_EXACT.into(),
            phases: Vec::new(),
            runtime: vec![RuntimeSnapshot {
                decisions: vec![crate::coverage_report::DecisionSnapshot {
                    meta: decision(),
                    vectors: vec![vector],
                }],
                hits: Vec::new(),
                events: Vec::new(),
                logicals: Vec::new(),
                phase_id: None,
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
                unmeasured: Vec::new(),
                decisions: vec![decision()],
                points: Vec::new(),
                branches: Vec::new(),
                limitations: Vec::new(),
                scope: None,
                assertion_sites: Vec::new(),
            },
            raw_results: std::mem::take(&mut results),
            generated_at: "time".into(),
            coverage_model: None,
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
    fn a_test_that_belongs_to_no_test_is_not_minimized_away() {
        // The first guard catches this whenever the run-wide record holds
        // anything, which is nearly always -- so it is the rare run, where
        // those tests happened to reach nothing measured, that reaches this
        // one. Without it the smallest set would quietly drop a test whose
        // coverage is unknown rather than absent, and report the drop as a
        // saving.
        let mut parallel = result(
            "parallel",
            McdcVector {
                values: vec![Some(false), Some(false)],
                outcome: false,
            },
        );
        parallel.attribution = crate::coverage_report::ATTRIBUTION_RUN_WIDE.into();
        assert!(matches!(
            minimum_test_set(
                &report(vec![parallel]).view,
                100.0,
                MinimizeMetric::Mcdc,
                5_000,
            ),
            Err(QueryError::UnattributedEvidence)
        ));
    }

    #[test]
    fn the_summary_counts_each_test_under_the_attribution_it_carries() {
        // These three numbers are what the `Attribution` line reads out, and
        // nothing -- here or in the integration scripts -- had ever checked
        // that a test lands in the bucket it was written with.
        let vector = || McdcVector {
            values: vec![Some(false), Some(false)],
            outcome: false,
        };
        let mut serial = result("serial", vector());
        serial.attribution = crate::coverage_report::ATTRIBUTION_EXACT.into();
        let mut parallel = result("parallel", vector());
        parallel.attribution = crate::coverage_report::ATTRIBUTION_RUN_WIDE.into();
        let mut ruby = result("ruby", vector());
        ruby.attribution = crate::coverage_report::ATTRIBUTION_PARTIAL.into();
        let mut also_parallel = result("also-parallel", vector());
        also_parallel.attribution = crate::coverage_report::ATTRIBUTION_RUN_WIDE.into();
        let report = report(vec![serial, parallel, ruby, also_parallel]);

        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "supercov-attribution-counts-{}-{nonce}",
            std::process::id()
        ));
        std::fs::create_dir(&root).unwrap();
        let path = root.join("query-index.v1.bin");
        let identity = crate::query_index::QueryIndexIdentity {
            evidence_sha256: [1; 32],
            evidence_bytes: 100,
            analysis_sha256: [2; 32],
            producer_sha256: [3; 32],
            archive_schema_version: 2,
        };
        crate::query_index::write_query_index(
            &crate::coverage_index::coverage_index_sections(&report).unwrap(),
            &identity,
            &path,
        )
        .unwrap();
        let container = crate::query_index::QueryIndex::open(&path, &identity).unwrap();
        let index = crate::coverage_index::CoverageIndex::new(&container).unwrap();

        let summary = coverage_summary_query(
            &index,
            CoverageSummaryQueryOptions {
                run: "run",
                view: CoverageViewId::All,
                kind: None,
                runner: None,
                valid: true,
                test_exit_code: Some(0),
                stale: false,
                stale_reasons: Vec::new(),
            },
        )
        .unwrap();
        assert_eq!(summary.test_attribution.exact, 1);
        assert_eq!(summary.test_attribution.run_wide, 2);
        assert_eq!(summary.test_attribution.partial, 1);
        assert_eq!(summary.test_attribution.total(), 4);
        std::fs::remove_dir_all(&root).ok();
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
