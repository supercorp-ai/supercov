//! One agent-query implementation over an authenticated immutable query index.
//!
//! Opening, rebuilding and storing indexes belongs to `run_store`; this module
//! is deliberately unaware of paths. Both archive differential tests and the
//! persisted-run CLI therefore exercise exactly the same query operators.

use serde::Deserialize;

use crate::{
    agent_json,
    coverage_index::{CoverageDimension, CoverageIndex, CoverageViewId},
    coverage_query::{
        CoverageCoversQueryOptions, CoverageDecisionQueryOptions, CoverageDiffQueryOptions,
        CoverageDimensionQueryData, CoverageDimensionQueryOptions, CoverageFileDecisionsOptions,
        CoverageFileDetailOptions, CoverageFileQueryData, CoverageFileQueryOptions,
        CoverageMinimizeQueryOptions, CoverageQueryFilters, CoverageScopeQueryOptions,
        CoverageSummaryQueryOptions, CoverageTestQueryOptions, DecisionSort, MinimizeMetric,
        coverage_covers_query, coverage_decision_query, coverage_diff_query,
        coverage_dimension_query, coverage_file_decisions_query, coverage_file_detail_query,
        coverage_file_query, coverage_minimize_query, coverage_scope_query, coverage_summary_query,
        coverage_test_query,
    },
    coverage_report::CoverageReport,
    coverage_waivers::CoverageWaiverEvaluation,
};

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct IndexedQueryRequest {
    pub run_id: String,
    pub filter: String,
    pub command: String,
    #[serde(default = "default_metric")]
    pub metric: MinimizeMetric,
    pub kind: Option<String>,
    pub runner: Option<String>,
    pub file: Option<String>,
    pub line: Option<usize>,
    pub selector: Option<String>,
    pub sort: Option<DecisionSort>,
    pub valid: Option<bool>,
    pub stale: Option<bool>,
    pub stale_reasons: Option<Vec<String>>,
    #[serde(default)]
    pub offset: usize,
    #[serde(default = "default_limit")]
    pub limit: usize,
    pub target: Option<f64>,
    pub max_states: Option<usize>,
}

fn default_limit() -> usize {
    20
}

fn default_metric() -> MinimizeMetric {
    MinimizeMetric::All
}

impl IndexedQueryRequest {
    pub fn view(&self) -> Result<CoverageViewId, String> {
        match self.filter.as_str() {
            "all" => Ok(CoverageViewId::All),
            "passed" => Ok(CoverageViewId::Passed),
            "failed" => Ok(CoverageViewId::Failed),
            _ => Err("invalid coverage filter".into()),
        }
    }
}

pub struct NewerQuery<'a> {
    pub run_id: &'a str,
    pub index: &'a CoverageIndex<'a>,
}

fn response<T: serde::Serialize>(
    command: &str,
    data: &T,
    page: Option<&supercov_contracts::AgentPagination>,
) -> Result<String, String> {
    agent_json::success(command, data, page)
        .map_err(|error| format!("response exceeds {} bytes", error.max_bytes))
}

/// Execute a frozen agent query against an already authenticated index.
///
/// Only minimization needs the reconstructed per-test report today. All other
/// commands are served directly from the mmap-backed index.
pub fn execute_indexed_query(
    index: &CoverageIndex<'_>,
    report: Option<&CoverageReport>,
    request: &IndexedQueryRequest,
    newer: Option<NewerQuery<'_>>,
) -> Result<String, String> {
    execute_indexed_query_with_waivers(index, report, request, newer, None)
}

pub fn execute_indexed_query_with_waivers(
    index: &CoverageIndex<'_>,
    report: Option<&CoverageReport>,
    request: &IndexedQueryRequest,
    newer: Option<NewerQuery<'_>>,
    waivers: Option<&CoverageWaiverEvaluation>,
) -> Result<String, String> {
    let view = request.view()?;
    let gaps_only = match request.command.as_str() {
        "files" => Some(false),
        "gaps" => Some(true),
        "file-decisions" | "kinds" | "runners" | "summary" | "scope" | "covers" | "test"
        | "decision" | "file-detail" | "minimize" | "diff" => None,
        _ => return Err("unsupported indexed query".into()),
    };

    if request.command == "diff" {
        let newer = newer.ok_or_else(|| "indexed diff requires a newer run".to_owned())?;
        let (data, page) = coverage_diff_query(
            index,
            newer.index,
            CoverageDiffQueryOptions {
                older_run: &request.run_id,
                newer_run: newer.run_id,
                view,
                kind: request.kind.as_deref(),
                runner: request.runner.as_deref(),
                offset: request.offset,
                limit: request.limit,
            },
        )
        .map_err(|error| format!("{error:?}"))?;
        return response("diff", &data, Some(&page));
    }

    if request.command == "minimize" {
        let report = report.ok_or_else(|| {
            "coverage minimization requires reconstructed per-test evidence".to_owned()
        })?;
        let coverage_view = match view {
            CoverageViewId::All => &report.view,
            CoverageViewId::Passed => &report.filters.passed,
            CoverageViewId::Failed => &report.filters.failed,
        };
        let (data, page) = coverage_minimize_query(
            coverage_view,
            CoverageMinimizeQueryOptions {
                run: &request.run_id,
                view_id: view,
                kind: request.kind.as_deref(),
                runner: request.runner.as_deref(),
                target: request.target.unwrap_or(100.0),
                metric: request.metric,
                max_states: request.max_states.unwrap_or(5_000),
                offset: request.offset,
                limit: request.limit,
            },
        )
        .map_err(|error| format!("{error:?}"))?;
        return response("coverage.minimize", &data, Some(&page));
    }

    if request.command == "summary" {
        let mut data = coverage_summary_query(
            index,
            CoverageSummaryQueryOptions {
                run: &request.run_id,
                view,
                kind: request.kind.as_deref(),
                runner: request.runner.as_deref(),
                valid: request.valid.unwrap_or(false),
                stale: request.stale.unwrap_or(false),
                stale_reasons: request.stale_reasons.clone().unwrap_or_default(),
            },
        )
        .map_err(|error| format!("{error:?}"))?;
        if let Some(waivers) = waivers {
            data.waivers =
                Some(waivers.summary(data.coverage.covered_conditions, data.coverage.conditions));
        }
        return response("coverage.summary", &data, None);
    }

    if request.command == "scope" {
        let (data, page) = coverage_scope_query(
            index,
            CoverageScopeQueryOptions {
                run: &request.run_id,
                view,
                kind: request.kind.as_deref(),
                runner: request.runner.as_deref(),
                offset: request.offset,
                limit: request.limit,
            },
        )
        .map_err(|error| format!("{error:?}"))?;
        return response("coverage.scope", &data, Some(&page));
    }

    if request.command == "covers" {
        let file = request
            .file
            .as_deref()
            .ok_or_else(|| "indexed covers query requires a file".to_owned())?;
        let line = request
            .line
            .ok_or_else(|| "indexed covers query requires a line".to_owned())?;
        let (data, page) = coverage_covers_query(
            index,
            CoverageCoversQueryOptions {
                run: &request.run_id,
                view,
                kind: request.kind.as_deref(),
                runner: request.runner.as_deref(),
                file,
                line,
                offset: request.offset,
                limit: request.limit,
            },
        )
        .map_err(|error| format!("{error:?}"))?;
        return response("coverage.covers", &data, Some(&page));
    }

    if request.command == "test" {
        let selector = request
            .selector
            .as_deref()
            .ok_or_else(|| "indexed test query requires a selector".to_owned())?;
        let (data, page) = coverage_test_query(
            index,
            CoverageTestQueryOptions {
                run: &request.run_id,
                view,
                kind: request.kind.as_deref(),
                runner: request.runner.as_deref(),
                selector,
                offset: request.offset,
                limit: request.limit,
            },
        )
        .map_err(|error| format!("{error:?}"))?;
        return response("coverage.test", &data, Some(&page));
    }

    if request.command == "decision" {
        let selector = request
            .selector
            .as_deref()
            .ok_or_else(|| "indexed decision query requires a selector".to_owned())?;
        let (mut data, page) = coverage_decision_query(
            index,
            CoverageDecisionQueryOptions {
                run: &request.run_id,
                view,
                kind: request.kind.as_deref(),
                runner: request.runner.as_deref(),
                selector,
                offset: request.offset,
                limit: request.limit,
            },
        )
        .map_err(|error| format!("{error:?}"))?;
        if let Some(waivers) = waivers
            && let crate::coverage_query::CoverageDecisionData::Detail(detail) = &mut data
        {
            for decision in &mut detail.decisions {
                if let Some(conditions) = waivers.waived_by_decision.get(&decision.meta.id) {
                    for condition in &mut decision.conditions {
                        if let Some(waiver) = conditions.get(&condition.index) {
                            condition.waived = Some(true);
                            condition.waiver_reason = Some(waiver.reason.clone());
                        }
                    }
                }
            }
        }
        return response("coverage.decision", &data, Some(&page));
    }

    if request.command == "file-detail" {
        let selector = request
            .file
            .as_deref()
            .ok_or_else(|| "indexed file query requires a file".to_owned())?;
        let (mut data, page) = coverage_file_detail_query(
            index,
            CoverageFileDetailOptions {
                run: &request.run_id,
                view,
                kind: request.kind.as_deref(),
                runner: request.runner.as_deref(),
                selector,
                metric: request.metric,
                offset: request.offset,
                limit: request.limit,
            },
        )
        .map_err(|error| format!("{error:?}"))?;
        if let Some(waivers) = waivers {
            data.counts.waived_mcdc_conditions = waivers
                .applied_by_file
                .get(&data.file)
                .copied()
                .unwrap_or(0);
            for obligation in &mut data.obligations {
                if let crate::coverage_query::CoverageFileObligation::Mcdc(obligation) = obligation
                    && let Some(waiver) = waivers
                        .waived_by_decision
                        .get(&obligation.id)
                        .and_then(|conditions| conditions.get(&obligation.condition_index))
                {
                    obligation.waived = Some(true);
                    obligation.waiver_reason = Some(waiver.reason.clone());
                }
            }
        }
        return response("coverage.file", &data, Some(&page));
    }

    if request.command == "kinds" || request.command == "runners" {
        let dimension = if request.command == "kinds" {
            CoverageDimension::Kind
        } else {
            CoverageDimension::Runner
        };
        let filters = CoverageQueryFilters {
            outcome: request.filter.clone(),
            kind: request.kind.clone(),
            runner: request.runner.clone(),
        };
        let (data, page) = coverage_dimension_query(
            index,
            CoverageDimensionQueryOptions {
                run: &request.run_id,
                view,
                dimension,
                filters,
                offset: request.offset,
                limit: request.limit,
            },
        )
        .map_err(|error| format!("{error:?}"))?;
        let command = if request.command == "kinds" {
            "coverage.kinds"
        } else {
            "coverage.runners"
        };
        return match data {
            CoverageDimensionQueryData::Kinds(data) => response(command, &data, Some(&page)),
            CoverageDimensionQueryData::Runners(data) => response(command, &data, Some(&page)),
        };
    }

    if request.command == "file-decisions" {
        let file = request
            .file
            .as_deref()
            .ok_or_else(|| "indexed file-decision query requires a file".to_owned())?;
        let (data, page) = coverage_file_decisions_query(
            index,
            CoverageFileDecisionsOptions {
                run: &request.run_id,
                view,
                kind: request.kind.as_deref(),
                runner: request.runner.as_deref(),
                file,
                waived_by_decision: waivers.map(|waivers| &waivers.waived_by_decision),
                sort: request.sort.unwrap_or(DecisionSort::Location),
                offset: request.offset,
                limit: request.limit,
            },
        )
        .map_err(|error| format!("{error:?}"))?;
        return response("coverage.file", &data, Some(&page));
    }

    let mut query = coverage_file_query(
        index,
        CoverageFileQueryOptions {
            run: &request.run_id,
            view,
            metric: request.metric,
            gaps_only: gaps_only.expect("files/gaps command"),
            kind: request.kind.as_deref(),
            runner: request.runner.as_deref(),
            offset: request.offset,
            limit: request.limit,
        },
    )
    .map_err(|error| format!("{error:?}"))?;
    if let Some(waivers) = waivers {
        let rows = match &mut query.data {
            CoverageFileQueryData::Files(data) => &mut data.files,
            CoverageFileQueryData::Gaps(data) => &mut data.gaps,
        };
        for row in rows {
            row.waived_mcdc_conditions =
                Some(waivers.applied_by_file.get(&row.file).copied().unwrap_or(0));
        }
    }
    let command = if gaps_only == Some(true) {
        "coverage.gaps"
    } else {
        "coverage.files"
    };
    match query.data {
        CoverageFileQueryData::Files(data) => response(command, &data, Some(&query.pagination)),
        CoverageFileQueryData::Gaps(data) => response(command, &data, Some(&query.pagination)),
    }
}
