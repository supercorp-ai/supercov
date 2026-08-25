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
        CoverageIndex, CoverageIndexError, CoverageViewId, IndexedDecisionGap, IndexedFileGap,
    },
    coverage_report::{
        CoverageReportRequest, CoverageView, ReportError, analyze_coverage_results,
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
