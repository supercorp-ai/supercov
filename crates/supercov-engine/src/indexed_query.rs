//! One agent-query implementation over an authenticated immutable query index.
//!
//! Opening, rebuilding and storing indexes belongs to `run_store`; this module
//! is deliberately unaware of paths. Both archive differential tests and the
//! persisted-run CLI therefore exercise exactly the same query operators.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::{
    agent_json::{self, ResponseTooLarge},
    coverage_index::{CoverageDimension, CoverageIndex, CoverageViewId},
    coverage_query::{
        CoverageCoversData, CoverageCoversQueryOptions, CoverageDecisionData,
        CoverageDecisionQueryOptions, CoverageDiffData, CoverageDiffQueryOptions,
        CoverageDimensionQueryData, CoverageDimensionQueryOptions, CoverageFileDecisionsData,
        CoverageFileDecisionsOptions, CoverageFileDetailData, CoverageFileDetailOptions,
        CoverageFileQueryData, CoverageFileQueryOptions, CoverageFilesData, CoverageGapsData,
        CoverageKindsData, CoverageMinimizeData, CoverageMinimizeQueryOptions,
        CoverageQueryFilters, CoverageRunnersData, CoverageScopeData, CoverageScopeQueryOptions,
        CoverageSummaryData, CoverageSummaryQueryOptions, CoverageTestData,
        CoverageTestQueryOptions, DecisionSort, MinimizeMetric, QueryError, coverage_covers_query,
        coverage_decision_query, coverage_diff_query, coverage_dimension_query,
        coverage_file_decisions_query, coverage_file_detail_query, coverage_file_query,
        coverage_minimize_query, coverage_scope_query, coverage_summary_query, coverage_test_query,
    },
    coverage_report::CoverageReport,
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
    pub test_exit_code: Option<i32>,
    pub stale: Option<bool>,
    pub stale_reasons: Option<Vec<String>>,
    #[serde(default)]
    pub offset: usize,
    #[serde(default = "default_limit")]
    pub limit: usize,
    pub target: Option<f64>,
    pub max_states: Option<usize>,
    /// How a listing is grouped: `file` for a scope listed file by file,
    /// `dir` for files rolled up by directory.
    #[serde(default)]
    pub group: Option<String>,
    /// How many directory levels a rollup keeps.
    #[serde(default)]
    pub depth: Option<usize>,
    /// `diff --gained`: what the newer run covers that the older did not,
    /// and nothing it does not.
    #[serde(default)]
    pub gained_only: bool,
    /// `line` asked about more locations than `file:line`: these, answered
    /// from the same opened run.
    #[serde(default)]
    pub more_lines: Vec<(String, usize)>,
    /// `--tag`: only tests carrying one of these tags.
    #[serde(default)]
    pub tags: Vec<String>,
    /// `--title`: only tests whose full name contains one of these.
    #[serde(default)]
    pub titles: Vec<String>,
    /// `--test-file`: only tests in one of these files or directories.
    #[serde(default)]
    pub test_files: Vec<String>,
}

fn default_limit() -> usize {
    20
}

fn default_metric() -> MinimizeMetric {
    MinimizeMetric::All
}

impl IndexedQueryRequest {
    /// The tests `--tag`, `--title` and `--test-file` pick, when any is given.
    pub fn test_filter(&self) -> Option<crate::coverage_report::TestFilter> {
        let filter = crate::coverage_report::TestFilter {
            tags: self.tags.clone(),
            titles: self.titles.clone(),
            files: self.test_files.clone(),
            kind: None,
            runner: None,
        };
        (!filter.is_empty()).then_some(filter)
    }

    pub fn view(&self) -> Result<CoverageViewId, IndexedQueryError> {
        match self.filter.as_str() {
            "all" => Ok(CoverageViewId::All),
            "passed" => Ok(CoverageViewId::Passed),
            "failed" => Ok(CoverageViewId::Failed),
            _ => Err(IndexedQueryError::InvalidFilter(self.filter.clone())),
        }
    }
}

#[derive(Debug)]
pub enum IndexedQueryError {
    InvalidFilter(String),
    UnsupportedCommand(String),
    MissingArgument(&'static str),
    MissingNewerRun,
    MissingReport,
    Query(QueryError),
    ResponseTooLarge(ResponseTooLarge),
    /// A page that was too large, and how many of its entries would fit.
    PageTooLarge {
        size: ResponseTooLarge,
        asked: usize,
        fits: usize,
    },
}

impl From<QueryError> for IndexedQueryError {
    fn from(value: QueryError) -> Self {
        Self::Query(value)
    }
}

impl From<ResponseTooLarge> for IndexedQueryError {
    fn from(value: ResponseTooLarge) -> Self {
        Self::ResponseTooLarge(value)
    }
}

impl std::fmt::Display for IndexedQueryError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidFilter(filter) => write!(formatter, "invalid coverage filter: {filter}"),
            Self::UnsupportedCommand(command) => {
                write!(formatter, "unsupported indexed query: {command}")
            }
            Self::MissingArgument(argument) => {
                write!(formatter, "indexed query requires {argument}")
            }
            Self::MissingNewerRun => write!(formatter, "indexed diff requires a newer run"),
            Self::MissingReport => write!(
                formatter,
                "coverage minimization requires reconstructed per-test evidence"
            ),
            Self::Query(error) => write!(formatter, "{error:?}"),
            Self::ResponseTooLarge(error) | Self::PageTooLarge { size: error, .. } => write!(
                formatter,
                "response is {} bytes and exceeds the {}-byte limit",
                error.actual_bytes, error.max_bytes
            ),
        }
    }
}

impl std::error::Error for IndexedQueryError {}

fn grouped_decimal(value: usize) -> String {
    let digits = value.to_string();
    let mut grouped = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, digit) in digits.bytes().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            grouped.push(',');
        }
        grouped.push(char::from(digit));
    }
    grouped
}

fn metric_name(metric: MinimizeMetric) -> &'static str {
    match metric {
        MinimizeMetric::All => "all",
        MinimizeMetric::Lines => "lines",
        MinimizeMetric::Statements => "statements",
        MinimizeMetric::Functions => "functions",
        MinimizeMetric::Branches => "branches",
        MinimizeMetric::Mcdc => "mcdc",
    }
}

fn test_filter_details(kind: &Option<String>, runner: &Option<String>) -> (String, Value) {
    let mut labels = Vec::new();
    let mut details = Map::new();
    if let Some(kind) = kind {
        labels.push(format!("kind={kind}"));
        details.insert("kind".into(), Value::String(kind.clone()));
    }
    if let Some(runner) = runner {
        labels.push(format!("runner={runner}"));
        details.insert("runner".into(), Value::String(runner.clone()));
    }
    (labels.join(", "), Value::Object(details))
}

impl IndexedQueryError {
    pub fn agent_error(&self) -> agent_json::AgentError {
        use agent_json::ErrorCode;

        let (code, message, details) = match self {
            Self::InvalidFilter(_) => (
                ErrorCode::InvalidArgument,
                "--filter must be all, passed, or failed".into(),
                None,
            ),
            Self::UnsupportedCommand(command) => (
                ErrorCode::UnknownCommand,
                format!("Unknown coverage resource: {command}"),
                Some(json!({ "command": command })),
            ),
            Self::MissingArgument(argument) => (
                ErrorCode::InvalidArgument,
                format!("Coverage query requires {argument}"),
                None,
            ),
            Self::MissingNewerRun => (
                ErrorCode::InvalidArgument,
                "Diff requires an older and newer run ID".into(),
                None,
            ),
            Self::MissingReport => (
                ErrorCode::InternalError,
                "Coverage minimization requires reconstructed per-test evidence".into(),
                None,
            ),
            Self::ResponseTooLarge(error) => (
                ErrorCode::ResponseTooLarge,
                format!(
                    "JSON response is {} bytes; the maximum is {} bytes",
                    error.actual_bytes, error.max_bytes
                ),
                Some(json!({
                    "actualBytes": error.actual_bytes,
                    "maxBytes": error.max_bytes,
                    "hint": "Use --offset/--limit or a narrower coverage query."
                })),
            ),
            // A script that wanted every entry asked for a limit it had to
            // guess, and was told only that the guess was too large.
            Self::PageTooLarge { size, asked, fits } => (
                ErrorCode::ResponseTooLarge,
                format!(
                    "JSON response is {} bytes; the maximum is {} bytes. About {fits} of the {asked} entries asked for fit in one reply: use --limit {fits} and follow pagination.nextOffset with --offset until hasMore is false",
                    size.actual_bytes, size.max_bytes
                ),
                Some(json!({
                    "actualBytes": size.actual_bytes,
                    "maxBytes": size.max_bytes,
                    "suggestedLimit": fits,
                    "hint": format!("Use --limit {fits} and --offset, following pagination.nextOffset, or a narrower coverage query.")
                })),
            ),
            Self::Query(error) => match error {
                QueryError::InvalidTarget(_) => (
                    ErrorCode::InvalidArgument,
                    "--target must be between 0 and 100".into(),
                    None,
                ),
                QueryError::UnattributedEvidence => (
                    ErrorCode::UnattributedEvidence,
                    "Cannot minimize exactly: this coverage view contains background/unattributed evidence. Use a runner with exact test attribution or select a fully attributed coverage view.".into(),
                    None,
                ),
                QueryError::TargetUnreachable {
                    metric,
                    target,
                    reachable,
                } => (
                    ErrorCode::TargetUnreachable,
                    format!(
                        "The full selected test view reaches only {reachable:.2}% {}; target {target}% is impossible",
                        metric_name(*metric)
                    ),
                    Some(json!({ "metric": metric, "target": target, "reachable": reachable })),
                ),
                QueryError::ComplexityLimit {
                    candidate_tests,
                    obligations,
                    explored_states,
                    max_states,
                    target,
                    metric,
                } => (
                    ErrorCode::MinimizationComplexityLimit,
                    format!(
                        "Exact minimization exceeded its {}-state safety budget. Narrow the test view with --kind or --runner, or request a different target.",
                        grouped_decimal(*max_states)
                    ),
                    Some(json!({
                        "candidateTests": candidate_tests,
                        "obligations": obligations,
                        "exploredStates": explored_states,
                        "maxStates": max_states,
                        "target": target,
                        "metric": metric,
                    })),
                ),
                QueryError::InvalidPagination => (
                    ErrorCode::InvalidArgument,
                    "--limit must be a positive integer".into(),
                    None,
                ),
                QueryError::TestFilterEmpty { kind, runner } => {
                    let (filter, details) = test_filter_details(kind, runner);
                    (
                        ErrorCode::TestFilterEmpty,
                        format!("No tests match {filter}"),
                        Some(details),
                    )
                }
                QueryError::TestNotFound(selector) => (
                    ErrorCode::TestNotFound,
                    format!("Test not found: {selector}"),
                    Some(json!({ "selector": selector })),
                ),
                QueryError::DecisionNotFound(selector) => (
                    ErrorCode::DecisionNotFound,
                    format!("Decision not found: {selector}"),
                    Some(json!({ "selector": selector })),
                ),
                QueryError::SourceNotFound(selector) => (
                    ErrorCode::SourceNotFound,
                    format!("Source file not found: {selector}"),
                    Some(json!({ "selector": selector })),
                ),
                QueryError::AmbiguousSelector { selector, matches } => (
                    ErrorCode::AmbiguousSelector,
                    format!("Ambiguous file selector: {}", matches.join(", ")),
                    Some(json!({ "selector": selector, "matches": matches })),
                ),
                QueryError::ScopeUnavailable => (
                    ErrorCode::ScopeUnavailable,
                    "This run does not contain a source-scope inventory.".into(),
                    None,
                ),
                QueryError::Analysis(error) => (
                    ErrorCode::InternalError,
                    format!("Coverage analysis failed: {error:?}"),
                    None,
                ),
                QueryError::Index(error) => (
                    ErrorCode::InternalError,
                    format!("Coverage index query failed: {error}"),
                    None,
                ),
                QueryError::InvalidRecordSelection => (
                    ErrorCode::InternalError,
                    "Coverage index contains inconsistent references".into(),
                    None,
                ),
            },
        };
        agent_json::AgentError {
            code,
            message,
            retryable: false,
            details,
        }
    }
}

/// How many covering tests each line of several lists: enough to name who
/// reaches it, few enough that twenty lines fit in one reply.
const BATCH_LINE_TESTS: usize = 3;

/// A line of several: its state, gaps and counts, without the per-test and
/// per-phase detail that a line asked alone lists in full.
fn compact_line(mut data: CoverageCoversData) -> CoverageCoversData {
    if let CoverageCoversData::Line(line) = &mut data {
        line.phases.clear();
        line.confidence.tests.clear();
        line.confidence.asserted_tests.clear();
    }
    data
}

/// Whether a several-line query's error belongs to one location -- its file
/// is not in the run, or names more than one -- rather than to the run.
fn location_failure(error: &QueryError) -> bool {
    matches!(
        error,
        QueryError::SourceNotFound(_) | QueryError::AmbiguousSelector { .. }
    )
}

/// A location of a several-line query that could not be answered.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CoverageLineFailure {
    /// As asked: `file:line`.
    pub location: String,
    pub code: agent_json::ErrorCode,
    pub message: String,
}

/// Several lines asked in one call, each answered as `line` answers it, but
/// compactly: see [`compact_line`].
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CoverageLinesData {
    pub run: String,
    pub lines: Vec<CoverageCoversData>,
    /// Locations asked that did not fit in one reply, as `file:line`: ask
    /// them in another call.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub remaining: Vec<String>,
    /// Locations that could not be answered -- a file the run has no record
    /// of, a line past its end -- each with why. The others are answered:
    /// one mistyped path no longer costs every other location its reply.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub failed: Vec<CoverageLineFailure>,
    /// Each line's own page of tests and obligations, for a reader that
    /// renders them one by one.
    #[serde(skip)]
    pub pages: Vec<supercov_contracts::AgentPagination>,
}

pub struct NewerQuery<'a> {
    pub run_id: &'a str,
    pub index: &'a CoverageIndex<'a>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(untagged)]
pub enum IndexedQueryData {
    Summary(Box<CoverageSummaryData>),
    Scope(Box<CoverageScopeData>),
    Line(Box<CoverageCoversData>),
    Lines(Box<CoverageLinesData>),
    Test(Box<CoverageTestData>),
    Decision(Box<CoverageDecisionData>),
    FileDetail(Box<CoverageFileDetailData>),
    FileDecisions(Box<CoverageFileDecisionsData>),
    Kinds(Box<CoverageKindsData>),
    Runners(Box<CoverageRunnersData>),
    Files(Box<CoverageFilesData>),
    Areas(Box<crate::coverage_query::CoverageAreasData>),
    Gaps(Box<CoverageGapsData>),
    Minimize(Box<CoverageMinimizeData>),
    Diff(Box<CoverageDiffData>),
    TestsWithoutEvidence(Box<crate::coverage_query::TestsWithoutEvidenceData>),
}

#[derive(Debug, Clone, PartialEq)]
pub struct IndexedQueryOutput {
    pub command: &'static str,
    pub data: IndexedQueryData,
    pub pagination: Option<supercov_contracts::AgentPagination>,
}

/// How many of the entries of a page that was too large fit in a reply:
/// nine tenths of what the sizes say, since entries differ.
fn entries_that_fit(returned: usize, size: &ResponseTooLarge) -> usize {
    (returned * size.max_bytes * 9 / (size.actual_bytes * 10)).max(1)
}

impl IndexedQueryOutput {
    pub fn agent_json(&self) -> Result<String, IndexedQueryError> {
        agent_json::success(self.command, &self.data, self.pagination.as_ref()).map_err(|size| {
            match &self.pagination {
                Some(page) if page.returned > 1 => IndexedQueryError::PageTooLarge {
                    asked: page.returned,
                    fits: entries_that_fit(page.returned, &size),
                    size,
                },
                _ => IndexedQueryError::ResponseTooLarge(size),
            }
        })
    }
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
) -> Result<String, IndexedQueryError> {
    query_indexed(index, report, request, newer)?.agent_json()
}

pub fn query_indexed(
    index: &CoverageIndex<'_>,
    report: Option<&CoverageReport>,
    request: &IndexedQueryRequest,
    newer: Option<NewerQuery<'_>>,
) -> Result<IndexedQueryOutput, IndexedQueryError> {
    let view = request.view()?;
    let gaps_only = match request.command.as_str() {
        "files" => Some(false),
        "gaps" => Some(true),
        "file-decisions"
        | "kinds"
        | "runners"
        | "summary"
        | "scope"
        | "line"
        | "test"
        | "decision"
        | "file-detail"
        | "minimize"
        | "diff"
        | "tests-without-evidence" => None,
        _ => {
            return Err(IndexedQueryError::UnsupportedCommand(
                request.command.clone(),
            ));
        }
    };

    if request.command == "diff" {
        let newer = newer.ok_or(IndexedQueryError::MissingNewerRun)?;
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
                gained_only: request.gained_only,
            },
        )?;
        return Ok(IndexedQueryOutput {
            command: "diff",
            data: IndexedQueryData::Diff(Box::new(data)),
            pagination: Some(page),
        });
    }

    if request.command == "minimize" {
        let report = report.ok_or(IndexedQueryError::MissingReport)?;
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
        )?;
        return Ok(IndexedQueryOutput {
            command: "coverage.minimize",
            data: IndexedQueryData::Minimize(Box::new(data)),
            pagination: Some(page),
        });
    }

    if request.command == "summary" {
        let data = coverage_summary_query(
            index,
            CoverageSummaryQueryOptions {
                run: &request.run_id,
                view,
                kind: request.kind.as_deref(),
                runner: request.runner.as_deref(),
                valid: request.valid.unwrap_or(false),
                test_exit_code: request.test_exit_code,
                stale: request.stale.unwrap_or(false),
                stale_reasons: request.stale_reasons.clone().unwrap_or_default(),
            },
        )?;
        return Ok(IndexedQueryOutput {
            command: "coverage.summary",
            data: IndexedQueryData::Summary(Box::new(data)),
            pagination: None,
        });
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
        )?;
        return Ok(IndexedQueryOutput {
            command: "coverage.scope",
            data: IndexedQueryData::Scope(Box::new(data)),
            pagination: Some(page),
        });
    }

    if request.command == "line" && !request.more_lines.is_empty() {
        let first = (
            request
                .file
                .clone()
                .ok_or(IndexedQueryError::MissingArgument("a file"))?,
            request
                .line
                .ok_or(IndexedQueryError::MissingArgument("a line"))?,
        );
        // Each location once, in the order asked; `--offset` skips the
        // locations a reply before this one answered.
        let mut locations = Vec::new();
        for location in std::iter::once(first).chain(request.more_lines.iter().cloned()) {
            if !locations.contains(&location) {
                locations.push(location);
            }
        }
        let total = locations.len();
        let mut lines = Vec::new();
        let mut pages = Vec::new();
        let mut cache = crate::coverage_query::CoversCache::default();
        let mut failed = Vec::new();
        let mut first_failure = None;
        for (file, line) in locations.into_iter().skip(request.offset) {
            let answer = crate::coverage_query::coverage_covers_query_cached(
                index,
                CoverageCoversQueryOptions {
                    run: &request.run_id,
                    view,
                    kind: request.kind.as_deref(),
                    runner: request.runner.as_deref(),
                    file: &file,
                    line,
                    offset: 0,
                    limit: request.limit.min(BATCH_LINE_TESTS),
                },
                &mut cache,
            );
            match answer {
                Ok((data, page)) => {
                    lines.push(compact_line(data));
                    pages.push(page);
                }
                // A failure of the location itself: the others still count.
                Err(error) if location_failure(&error) => {
                    let wrapped = IndexedQueryError::Query(error);
                    let agent = wrapped.agent_error();
                    failed.push(CoverageLineFailure {
                        location: format!("{file}:{line}"),
                        code: agent.code,
                        message: agent.message,
                    });
                    first_failure.get_or_insert(wrapped);
                }
                Err(error) => return Err(error.into()),
            }
        }
        // Every location failed: the call failed, as one location's would.
        if lines.is_empty()
            && let Some(error) = first_failure
        {
            return Err(error);
        }
        let returned = lines.len();
        return Ok(IndexedQueryOutput {
            command: "coverage.lines",
            data: IndexedQueryData::Lines(Box::new(CoverageLinesData {
                run: request.run_id.clone(),
                lines,
                remaining: Vec::new(),
                failed,
                pages,
            })),
            pagination: Some(supercov_contracts::AgentPagination {
                offset: request.offset,
                limit: returned,
                returned,
                total,
                has_more: false,
                next_offset: None,
            }),
        });
    }

    if request.command == "line" {
        let file = request
            .file
            .as_deref()
            .ok_or(IndexedQueryError::MissingArgument("a file"))?;
        let line = request
            .line
            .ok_or(IndexedQueryError::MissingArgument("a line"))?;
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
        )?;
        return Ok(IndexedQueryOutput {
            command: "coverage.line",
            data: IndexedQueryData::Line(Box::new(data)),
            pagination: Some(page),
        });
    }

    if request.command == "test" {
        let selector = request
            .selector
            .as_deref()
            .ok_or(IndexedQueryError::MissingArgument("a test selector"))?;
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
        )?;
        return Ok(IndexedQueryOutput {
            command: "coverage.test",
            data: IndexedQueryData::Test(Box::new(data)),
            pagination: Some(page),
        });
    }

    if request.command == "decision" {
        let selector = request
            .selector
            .as_deref()
            .ok_or(IndexedQueryError::MissingArgument("a decision selector"))?;
        let (data, page) = coverage_decision_query(
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
        )?;
        return Ok(IndexedQueryOutput {
            command: "coverage.decision",
            data: IndexedQueryData::Decision(Box::new(data)),
            pagination: Some(page),
        });
    }

    if request.command == "file-detail" {
        let selector = request
            .file
            .as_deref()
            .ok_or(IndexedQueryError::MissingArgument("a file"))?;
        let (data, page) = coverage_file_detail_query(
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
        )?;
        return Ok(IndexedQueryOutput {
            command: "coverage.file",
            data: IndexedQueryData::FileDetail(Box::new(data)),
            pagination: Some(page),
        });
    }

    if request.command == "tests-without-evidence" {
        let (data, page) = crate::coverage_query::tests_without_evidence_query(
            index,
            crate::coverage_query::TestsWithoutEvidenceOptions {
                run: &request.run_id,
                view,
                kind: request.kind.as_deref(),
                runner: request.runner.as_deref(),
                offset: request.offset,
                limit: request.limit,
            },
        )?;
        return Ok(IndexedQueryOutput {
            command: "coverage.tests-without-evidence",
            data: IndexedQueryData::TestsWithoutEvidence(Box::new(data)),
            pagination: Some(page),
        });
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
        )?;
        let command = if request.command == "kinds" {
            "coverage.kinds"
        } else {
            "coverage.runners"
        };
        return Ok(match data {
            CoverageDimensionQueryData::Kinds(data) => IndexedQueryOutput {
                command,
                data: IndexedQueryData::Kinds(Box::new(data)),
                pagination: Some(page),
            },
            CoverageDimensionQueryData::Runners(data) => IndexedQueryOutput {
                command,
                data: IndexedQueryData::Runners(Box::new(data)),
                pagination: Some(page),
            },
        });
    }

    if request.command == "file-decisions" {
        let file = request
            .file
            .as_deref()
            .ok_or(IndexedQueryError::MissingArgument("a file"))?;
        let (data, page) = coverage_file_decisions_query(
            index,
            CoverageFileDecisionsOptions {
                run: &request.run_id,
                view,
                kind: request.kind.as_deref(),
                runner: request.runner.as_deref(),
                file,
                sort: request.sort.unwrap_or(DecisionSort::Location),
                offset: request.offset,
                limit: request.limit,
            },
        )?;
        return Ok(IndexedQueryOutput {
            command: "coverage.file",
            data: IndexedQueryData::FileDecisions(Box::new(data)),
            pagination: Some(page),
        });
    }

    if request.command == "files" && request.group.as_deref() == Some("dir") {
        let (data, page) = crate::coverage_query::coverage_areas_query(
            index,
            crate::coverage_query::CoverageAreasQueryOptions {
                run: &request.run_id,
                view,
                metric: request.metric,
                kind: request.kind.as_deref(),
                runner: request.runner.as_deref(),
                depth: request.depth.unwrap_or(1),
                offset: request.offset,
                limit: request.limit,
            },
        )?;
        return Ok(IndexedQueryOutput {
            command: "coverage.files",
            data: IndexedQueryData::Areas(Box::new(data)),
            pagination: Some(page),
        });
    }

    let query = coverage_file_query(
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
    )?;
    let command = if gaps_only == Some(true) {
        "coverage.gaps"
    } else {
        "coverage.files"
    };
    Ok(match query.data {
        CoverageFileQueryData::Files(data) => IndexedQueryOutput {
            command,
            data: IndexedQueryData::Files(Box::new(data)),
            pagination: Some(query.pagination),
        },
        CoverageFileQueryData::Gaps(data) => IndexedQueryOutput {
            command,
            data: IndexedQueryData::Gaps(Box::new(data)),
            pagination: Some(query.pagination),
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_page_too_large_says_how_many_entries_fit() {
        // `runs <id> files --limit 5000 --json` on a project of 300 files:
        // the reply said it was too large and not what limit would not be.
        let size = ResponseTooLarge {
            actual_bytes: 170_131,
            max_bytes: 65_536,
        };
        let fits = entries_that_fit(300, &size);
        assert_eq!(fits, 104);
        // One entry larger than a reply is still asked for one at a time.
        assert_eq!(
            entries_that_fit(
                2,
                &ResponseTooLarge {
                    actual_bytes: 9_000_000,
                    max_bytes: 65_536
                }
            ),
            1
        );
        let error = IndexedQueryError::PageTooLarge {
            size,
            asked: 300,
            fits,
        }
        .agent_error();
        assert_eq!(error.code, agent_json::ErrorCode::ResponseTooLarge);
        assert_eq!(error.details.as_ref().unwrap()["suggestedLimit"], 104);
        assert!(
            error
                .message
                .contains("About 104 of the 300 entries asked for fit in one reply: use --limit 104 and follow pagination.nextOffset"),
            "{}",
            error.message
        );
    }

    #[test]
    fn maps_typed_selection_failures_to_the_frozen_agent_contract() {
        let source =
            IndexedQueryError::Query(QueryError::SourceNotFound("missing.ts".into())).agent_error();
        assert_eq!(source.code, agent_json::ErrorCode::SourceNotFound);
        assert_eq!(source.message, "Source file not found: missing.ts");
        assert_eq!(source.details, Some(json!({ "selector": "missing.ts" })));

        let filtered = IndexedQueryError::Query(QueryError::TestFilterEmpty {
            kind: Some("e2e".into()),
            runner: Some("playwright".into()),
        })
        .agent_error();
        assert_eq!(filtered.code, agent_json::ErrorCode::TestFilterEmpty);
        assert_eq!(
            filtered.message,
            "No tests match kind=e2e, runner=playwright"
        );
        assert_eq!(
            filtered.details,
            Some(json!({ "kind": "e2e", "runner": "playwright" }))
        );
    }

    #[test]
    fn maps_solver_limits_without_losing_machine_readable_details() {
        let error = IndexedQueryError::Query(QueryError::ComplexityLimit {
            candidate_tests: 200,
            obligations: 900,
            explored_states: 5_001,
            max_states: 5_000,
            target: 100.0,
            metric: MinimizeMetric::All,
        })
        .agent_error();
        assert_eq!(
            error.code,
            agent_json::ErrorCode::MinimizationComplexityLimit
        );
        assert!(error.message.contains("5,000-state safety budget"));
        assert_eq!(error.details.as_ref().unwrap()["exploredStates"], 5_001);
    }
}
