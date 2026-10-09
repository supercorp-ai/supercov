use supercov_contracts::AgentPagination;
use supercov_engine::{
    coverage_analysis::{CoverageSummary, McdcVector},
    coverage_index::{IndexedDimensionCoverage, IndexedFileGap, IndexedOutcomeCounts},
    coverage_query::{
        CoverageCoversData, CoverageDecisionData, CoverageDiagnostic, CoverageFileObligation,
        CoverageTestData, DecisionSort, MinimizeMetric,
    },
    indexed_query::{IndexedQueryData, IndexedQueryOutput, IndexedQueryRequest},
};

use crate::{PublicQueryOutput, public_query::PublicQueryInvocation};

const DEFAULT_LIMIT: usize = 20;

fn percentage(value: f64) -> String {
    format!("{value:.2}%")
}

/// A metric's share with its counts, or that the run had nothing of it to
/// measure: a file with no branches read `Branches 100.00% (0/0)`.
fn measured(percent: f64, covered: u64, total: u64) -> String {
    if total == 0 {
        "nothing to measure (0/0)".into()
    } else {
        format!("{} ({covered}/{total})", percentage(percent))
    }
}

/// A percentage in a table column, or `—` where there was nothing to measure.
fn share(percent: f64, total: usize) -> String {
    if total == 0 {
        "—".into()
    } else {
        percentage(percent)
    }
}

fn optional_percentage(value: Option<f64>) -> String {
    value.map(percentage).unwrap_or_else(|| "—".into())
}

fn readable_timestamp(value: &str) -> String {
    if value.len() >= 20 && value.as_bytes().get(10) == Some(&b'T') {
        format!("{} {} UTC", &value[..10], &value[11..19])
    } else {
        value.to_owned()
    }
}

fn number(value: f64) -> String {
    if value == 0.0 {
        "0".into()
    } else if value.fract() == 0.0 {
        format!("{value:.0}")
    } else {
        value.to_string()
    }
}

fn count(value: usize) -> String {
    let digits = value.to_string();
    let mut output = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, character) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            output.push(',');
        }
        output.push(character);
    }
    output
}

fn outcome_lines(outcomes: &IndexedOutcomeCounts) -> Vec<String> {
    [
        ("Passed", outcomes.passed),
        ("Failed", outcomes.failed),
        ("Flaky", outcomes.flaky),
        ("Skipped", outcomes.skipped),
        ("Todo", outcomes.todo),
        ("Timed out", outcomes.timed_out),
        ("Interrupted", outcomes.interrupted),
        ("Unknown", outcomes.unknown),
    ]
    .into_iter()
    .filter(|(_, value)| *value > 0)
    .map(|(label, value)| format!("  {label:<11} {}", count(value)))
    .collect()
}

/// The scope as a table: how many files, their status, where they are and why.
fn scope_group_lines(groups: &[supercov_engine::coverage_query::ScopeGroup]) -> Vec<String> {
    let files = groups
        .iter()
        .map(|group| count(group.files).len())
        .max()
        .unwrap_or(0)
        .max("Files".len());
    let directory = groups
        .iter()
        .map(|group| group.directory.chars().count())
        .max()
        .unwrap_or(0)
        .max("Directory".len());
    let mut lines = vec![format!(
        "{:>files$}  {:<9}  {:<directory$}  Reason",
        "Files", "Status", "Directory"
    )];
    lines.extend(groups.iter().map(|group| {
        format!(
            "{:>files$}  {:<9}  {:<directory$}  {}",
            count(group.files),
            group.status.to_uppercase(),
            group.directory,
            group.reason
        )
    }));
    lines
}

/// The line that settles unclassified source, ready to copy.
fn source_roots_hint(unclassified: &supercov_engine::coverage_query::UnclassifiedSource) -> String {
    format!(
        "If the ambiguous files are your code, name the source roots: SUPERCOV_SOURCE_ROOTS={}",
        unclassified.roots.join(",")
    )
}

fn diagnostic_lines(diagnostic: &CoverageDiagnostic, list: Option<&str>) -> Vec<String> {
    if diagnostic.code == "TEST_EVIDENCE_MISSING" {
        let test_count = diagnostic
            .message
            .split_whitespace()
            .next()
            .and_then(|value| value.parse::<usize>().ok());
        let first = diagnostic
            .message
            .split_once("First: ")
            .map(|(_, value)| value.trim());
        if let (Some(test_count), Some(first)) = (test_count, first) {
            let mut lines = vec![
                format!(
                    "  {} {} made assertions, but Supercov received no source-coverage evidence:",
                    count(test_count),
                    if test_count == 1 { "test" } else { "tests" }
                ),
                format!("    Example: {first}"),
            ];
            // One example for 63 tests left nothing to follow up.
            if let Some(list) = list.filter(|_| test_count > 1) {
                lines.push(format!("    All of them: {list}"));
            }
            lines.extend([
                "  Possible causes: the code under test is outside the measured source (see `scope`), uninstrumented data, shared setup, lost async context, or missing probe transport. Missing evidence does not prove the code did not execute.".into(),
                "  Inspect the test's coverage and assertion details to distinguish missing execution from missing attribution.".into(),
            ]);
            return lines;
        }
    }
    vec![format!("  {}", diagnostic.message)]
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

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn page_label(page: &AgentPagination) -> String {
    let start = if page.total == 0 || page.returned == 0 {
        0
    } else {
        page.offset + 1
    };
    let end = (page.offset + page.returned).min(page.total);
    format!("showing {start}-{end} of {}", page.total)
}

fn next_page(base: &str, page: &AgentPagination) -> Option<String> {
    page.next_offset.map(|offset| {
        format!(
            "{base} --offset {offset}{}",
            if page.limit == DEFAULT_LIMIT {
                String::new()
            } else {
                format!(" --limit {}", page.limit)
            }
        )
    })
}

fn coverage_command(run: &str, request: &IndexedQueryRequest, child: &str) -> String {
    let mut values = vec![
        format!("{} runs", crate::launcher_command()),
        shell_quote(run),
        child.into(),
    ];
    if request.filter != "all" {
        values.push(format!("--filter {}", request.filter));
    }
    if let Some(kind) = &request.kind {
        values.push(format!("--kind {}", shell_quote(kind)));
    }
    if let Some(runner) = &request.runner {
        values.push(format!("--runner {}", shell_quote(runner)));
    }
    if request.metric != MinimizeMetric::All {
        values.push(format!("--metric {}", metric_name(request.metric)));
    }
    values.join(" ")
}

fn inspect_file_command(
    run: &str,
    request: &IndexedQueryRequest,
    file: Option<&str>,
) -> Option<String> {
    file.map(|file| {
        format!(
            "inspect file: {} {}",
            coverage_command(run, request, "file"),
            shell_quote(file)
        )
    })
}

fn filter_label(request: &IndexedQueryRequest) -> String {
    let mut labels = Vec::new();
    if request.filter != "all" {
        labels.push(format!("{} attempts only", request.filter));
    }
    if let Some(kind) = &request.kind {
        labels.push(format!("kind {kind}"));
    }
    if let Some(runner) = &request.runner {
        labels.push(format!("runner {runner}"));
    }
    labels.join(", ")
}

fn summary_line(summary: &CoverageSummary) -> String {
    format!(
        "lines {}, statements {}, functions {}, branches {}, MC/DC {}",
        share(summary.lines.percentage, summary.lines.total),
        share(summary.statements.percentage, summary.statements.total),
        share(summary.functions.percentage, summary.functions.total),
        share(summary.branches.percentage, summary.branches.total),
        share(summary.condition_coverage_pct, summary.conditions),
    )
}

fn gap_dimensions_total(gap: &supercov_engine::coverage_index::IndexedGapDimensions) -> usize {
    gap.lines + gap.statements + gap.functions + gap.branches + gap.mcdc_conditions
}

/// Coverage by directory: what each holds, how much of it every kind of test
/// covers, and how much all of them cover together.
fn render_areas(
    data: &supercov_engine::coverage_query::CoverageAreasData,
    request: &IndexedQueryRequest,
    page: &AgentPagination,
) -> String {
    use supercov_engine::coverage_query::dimension_for_metric;
    let (metric, heading) = match data.metric {
        MinimizeMetric::All | MinimizeMetric::Lines => (MinimizeMetric::Lines, "Lines"),
        MinimizeMetric::Statements => (MinimizeMetric::Statements, "Statements"),
        MinimizeMetric::Functions => (MinimizeMetric::Functions, "Functions"),
        MinimizeMetric::Branches => (MinimizeMetric::Branches, "Branches"),
        MinimizeMetric::Mcdc => (MinimizeMetric::Mcdc, "MC/DC"),
    };
    let percent = |covered: usize, total: usize| {
        share(
            if total == 0 {
                0.0
            } else {
                covered as f64 * 100.0 / total as f64
            },
            total,
        )
    };
    // `All` above every kind shown read as an error: `.` at e2e 18.18%, the
    // other kinds 0.00%, All 100.00%. The rest ran while no test was running,
    // and a column says so wherever some did.
    let outside = |area: &supercov_engine::coverage_query::CoverageArea| {
        area.covered_by_tests.as_ref().map(|tested| {
            dimension_for_metric(&area.covered, metric)
                .saturating_sub(dimension_for_metric(tested, metric))
        })
    };
    let no_test = data
        .areas
        .iter()
        .any(|area| outside(area).is_some_and(|only| only > 0));
    let mut table = vec![
        ["Directory".to_owned(), "Files".into(), heading.into()]
            .into_iter()
            .chain(data.kinds.iter().cloned())
            .chain(no_test.then(|| "no test".to_owned()))
            .chain(["All".to_owned()])
            .collect::<Vec<_>>(),
    ];
    for area in &data.areas {
        let total = dimension_for_metric(&area.totals, metric);
        table.push(
            [area.directory.clone(), count(area.files), count(total)]
                .into_iter()
                .chain(
                    area.by_kind
                        .iter()
                        .map(|kind| percent(dimension_for_metric(&kind.covered, metric), total)),
                )
                .chain(no_test.then(|| percent(outside(area).unwrap_or(0), total)))
                .chain([percent(dimension_for_metric(&area.covered, metric), total)])
                .collect(),
        );
    }
    let widths = (0..table[0].len())
        .map(|column| {
            table
                .iter()
                .map(|row| row[column].chars().count())
                .max()
                .unwrap_or(0)
        })
        .collect::<Vec<_>>();
    let label = filter_label(request);
    let mut lines = vec![format!(
        "Coverage by directory, {} deep{}",
        data.depth,
        if label.is_empty() {
            String::new()
        } else {
            format!(" — {label}")
        }
    )];
    lines.push(String::new());
    lines.extend(table.iter().map(|row| {
        row.iter()
            .enumerate()
            .map(|(column, cell)| {
                if column == 0 {
                    format!("{cell:<width$}", width = widths[column])
                } else {
                    format!("{cell:>width$}", width = widths[column])
                }
            })
            .collect::<Vec<_>>()
            .join("  ")
            .trim_end()
            .to_owned()
    }));
    if no_test {
        lines.push("\"no test\" is code that ran only while no test was running: start-up and setup, module loading, work between tests. It counts in All and in no kind.".into());
    }
    lines.push(page_label(page));
    let base = format!(
        "{} --group dir{}",
        coverage_command(&data.run, request, "files"),
        request
            .depth
            .map_or_else(String::new, |depth| format!(" --depth {depth}"))
    );
    if let Some(next) = next_page(&base, page) {
        lines.push(format!("next page: {next}"));
    }
    lines.join("\n")
}

fn render_files(
    rows: &[IndexedFileGap],
    request: &IndexedQueryRequest,
    page: &AgentPagination,
    child: &str,
    run: &str,
) -> String {
    let selected = request.kind.is_some() || request.runner.is_some();
    let body = rows
        .iter()
        .map(|gap| {
            let missing = gap.uncovered_lines
                + gap.uncovered_statements
                + gap.uncovered_functions
                + gap.missing_branches
                + gap.missing_mcdc_conditions;
            let status = if missing == 0 {
                "covered in this projection".into()
            } else {
                format!(
                    "uncovered: lines {}  statements {}  functions {}  branch outcomes {}  MC/DC conditions {}",
                    gap.uncovered_lines,
                    gap.uncovered_statements,
                    gap.uncovered_functions,
                    gap.missing_branches,
                    gap.missing_mcdc_conditions,
                )
            };
            let limitations = if gap.measurement_limitations == 0 {
                String::new()
            } else {
                format!(
                    "  measurement limitations {} ({})",
                    gap.measurement_limitations,
                    gap.limitation_kinds.join(", ")
                )
            };
            let provenance = if selected {
                format!(
                    "  [covered elsewhere: {}; nowhere: {}]",
                    gap_dimensions_total(&gap.covered_by_other_tests),
                    gap_dimensions_total(&gap.uncovered_everywhere)
                )
            } else {
                String::new()
            };
            format!("{}\n  {status}{limitations}{provenance}", gap.file)
        })
        .collect::<Vec<_>>()
        .join("\n\n");
    let title = if child == "gaps" {
        "Coverage gaps — only files with unresolved obligations"
    } else {
        "Coverage files — every included source file"
    };
    let label = filter_label(request);
    let projection = if label.is_empty() {
        String::new()
    } else {
        format!(
            "\nProjection: {label}. Uncovered counts are recalculated using only that evidence."
        )
    };
    let body = if page.total == 0 {
        if child == "gaps" {
            "No gaps: every included file is fully covered and measured.".into()
        } else {
            "No source files are included in this run.".into()
        }
    } else {
        body
    };
    let mut output = format!("{title}{projection}\n\n{body}\n\n{}", page_label(page));
    if let Some(inspect) =
        inspect_file_command(run, request, rows.first().map(|row| row.file.as_str()))
    {
        output.push_str(&format!("\n{inspect}"));
    }
    if let Some(next) = next_page(&coverage_command(run, request, child), page) {
        output.push_str(&format!("\nnext page: {next}"));
    }
    output
}

/// What a dimension's percentage leaves out, where it leaves anything out.
///
/// Silence would read as "these numbers describe every test here", and for a
/// Go suite that mixes parallel tests with serial ones they describe only some.
fn unattributed_note(entry: &IndexedDimensionCoverage) -> String {
    let unattributed = entry.tests.saturating_sub(entry.attributed);
    if unattributed == 0 {
        String::new()
    } else {
        format!(" ({unattributed} not attributable)")
    }
}

/// How completely the run could credit each test with what it reached.
///
/// Here whether or not anything is wrong, the way `Instrumentation` is: a
/// reader who only ever sees "Exact for every test" has still learned that the
/// question exists, and will recognise the other answer when a suite starts
/// running its tests at once.
fn attribution_line(counts: &supercov_engine::coverage_query::TestAttributionCounts) -> String {
    let total = counts.total();
    if total == 0 {
        return "No test recorded coverage".into();
    }
    if counts.exact == total {
        return format!("Exact for {} test(s)", count(total));
    }
    let mut parts = Vec::new();
    if counts.exact > 0 {
        parts.push(format!("{} exact", count(counts.exact)));
    }
    if counts.partial > 0 {
        // Ruby records a line for the first test that reaches it, so a later
        // test running the same line is credited with none of it.
        parts.push(format!("{} a lower bound", count(counts.partial)));
    }
    if counts.run_wide > 0 {
        parts.push(format!("{} counted run-wide", count(counts.run_wide)));
    }
    let detail = parts.join(", ");
    // The offer belongs only where it would change the answer. Running the
    // suite in order is what buys back a run-wide credit; it buys nothing for
    // a lower bound, which is how the language reports coverage at all.
    if counts.run_wide > 0 {
        format!(
            "{detail} — `--exact-attribution` credits them individually, running your suite in order"
        )
    } else {
        detail
    }
}

fn render_dimension(
    values: &[IndexedDimensionCoverage],
    request: &IndexedQueryRequest,
    page: &AgentPagination,
    child: &str,
    run: &str,
) -> String {
    let body = values
        .iter()
        .map(|entry| {
            let name = entry
                .kind
                .as_deref()
                .or(entry.runner.as_deref())
                .unwrap_or("unknown");
            let setups = if entry.setups == 0 {
                String::new()
            } else {
                format!(" + {} setup scope(s)", entry.setups)
            };
            // A percentage here describes the tests that have coverage of
            // their own. Where none of them do, there is no percentage to
            // print: the tests ran and reached real code, and what they
            // reached is in the run's totals rather than in theirs.
            if entry.tests > 0 && entry.attributed == 0 {
                return format!(
                    "{name}  {} test(s){setups}  coverage not attributable; counted run-wide",
                    entry.tests
                );
            }
            format!(
                "{name}  {} test(s){}{setups}  lines {}  branches {}  MC/DC {}",
                entry.tests,
                unattributed_note(entry),
                share(entry.summary.lines.percentage, entry.summary.lines.total),
                share(
                    entry.summary.branches.percentage,
                    entry.summary.branches.total
                ),
                share(
                    entry.summary.condition_coverage_pct,
                    entry.summary.conditions
                ),
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let mut output = format!("{body}\n{}", page_label(page));
    if let Some(next) = next_page(&coverage_command(run, request, child), page) {
        output.push_str(&format!("\nnext page: {next}"));
    }
    output
}

fn vector_text(vector: &McdcVector) -> String {
    format!(
        "{} -> {}",
        vector
            .values
            .iter()
            .map(|value| match value {
                None => '-',
                Some(true) => 'T',
                Some(false) => 'F',
            })
            .collect::<String>(),
        if vector.outcome { 'T' } else { 'F' }
    )
}

fn branch_need(value: &str) -> String {
    match value {
        "default evaluated" => "default-value branch not observed".into(),
        "value provided" => "explicit-value branch not observed".into(),
        "no matching case" => "switch no-match outcome not observed".into(),
        "try completed without catch" => "try-success outcome not observed".into(),
        "catch entered" => "catch outcome not observed".into(),
        "zero iterations" => "zero-iteration outcome not observed".into(),
        "one or more iterations" => "entered-loop outcome not observed".into(),
        "nullish / short-circuited" => "nullish short-circuit outcome not observed".into(),
        "non-nullish / continued" => "non-nullish continuation outcome not observed".into(),
        "assignment skipped" => "assignment-skipped outcome not observed".into(),
        "right evaluated / assigned" => "right-evaluated assignment outcome not observed".into(),
        "short-circuit / left selected" => {
            "left-selected short-circuit outcome not observed".into()
        }
        "right evaluated / selected" => "right-selected outcome not observed".into(),
        "true" => "decision never true".into(),
        "false" => "decision never false".into(),
        other => format!("branch outcome not observed: {other}"),
    }
}

fn render_needed_obligation(obligation: &CoverageFileObligation) -> String {
    match obligation {
        CoverageFileObligation::Line(_) => "line not executed".into(),
        CoverageFileObligation::Point(item) if item.kind == "statement" => {
            "statement not executed".into()
        }
        CoverageFileObligation::Point(item) if item.kind == "function" => {
            "function not called".into()
        }
        CoverageFileObligation::Point(item) => format!("{} not covered", item.kind),
        CoverageFileObligation::Branch(item) => branch_need(&item.missing),
        CoverageFileObligation::Mcdc(item) => format!(
            "no witness pair shows `{}` independently changing the decision result",
            item.missing_condition
        ),
    }
}

fn file_gap_needs<'a>(
    state: &str,
    obligations: impl IntoIterator<Item = &'a CoverageFileObligation>,
) -> Vec<String> {
    obligations
        .into_iter()
        .filter(|obligation| {
            state != "missing"
                || match obligation {
                    CoverageFileObligation::Line(_) => false,
                    CoverageFileObligation::Point(item) if item.kind == "statement" => false,
                    _ => true,
                }
        })
        .map(render_needed_obligation)
        .collect()
}

/// The saved assessment's word on a file, beside its coverage: nothing when
/// the run has not been assessed or the file has no assessed statement.
fn file_assertion_lines(assertions: Option<&serde_json::Value>) -> Vec<String> {
    let Some(a) = assertions.filter(|a| a["available"] == true) else {
        return Vec::new();
    };
    let statements = a["statements"].as_u64().unwrap_or(0);
    if statements == 0 {
        return Vec::new();
    }
    let mut lines = vec![
        String::new(),
        format!(
            "Assertions  {}% ({}/{statements} statements a test is judged to catch)",
            a["percentage"].as_f64().unwrap_or(0.0),
            a["asserted"]
        ),
    ];
    let not = a["notAssertedLines"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(serde_json::Value::as_u64)
        .collect::<Vec<_>>();
    if !not.is_empty() {
        let shown = not
            .iter()
            .take(12)
            .map(u64::to_string)
            .collect::<Vec<_>>()
            .join(", ");
        let more = if not.len() > 12 {
            format!(" and {} more", not.len() - 12)
        } else {
            String::new()
        };
        lines.push(format!("  Not asserted on line(s) {shown}{more}"));
        lines.push(format!(
            "  Inspect: {}",
            a["inspect"].as_str().unwrap_or("")
        ));
    }
    lines
}

/// The saved assessment's word on the statements of one line.
fn line_assertion_lines(assertions: Option<&serde_json::Value>) -> Vec<String> {
    let Some(a) = assertions.filter(|a| a["available"] == true) else {
        return Vec::new();
    };
    let statements = a["statements"].as_array().cloned().unwrap_or_default();
    if statements.is_empty() {
        return Vec::new();
    }
    let mut lines = vec![String::new(), "Assertions".into()];
    for statement in &statements {
        let test = statement["test"]["name"].as_str().unwrap_or("no test");
        let likely = statement["answer"]
            .as_f64()
            .map(|p| format!(", {:.0}% likely to fail", p * 100.0))
            .unwrap_or_default();
        lines.push(if statement["asserted"] == true {
            format!(
                "  Asserted: `{}`; {test} is judged to fail if it changes ({}{likely})",
                statement["text"].as_str().unwrap_or(""),
                statement["change"].as_str().unwrap_or("")
            )
        } else {
            format!(
                "  Not asserted: `{}`; no test that runs it is judged to fail if it changes ({}); closest: {test}{likely}",
                statement["text"].as_str().unwrap_or(""),
                statement["change"].as_str().unwrap_or("")
            )
        });
    }
    lines.push(format!(
        "  Inspect: {}",
        a["inspect"].as_str().unwrap_or("")
    ));
    lines
}

fn state_label(state: &str) -> &'static str {
    match state {
        "missing" => "NOT COVERED",
        "part" => "PARTIAL",
        "limited" => "NOT MEASURED",
        _ => "UNKNOWN",
    }
}

fn confidence_label(level: &str) -> &str {
    match level {
        "asserted" => "linked to a passing assertion",
        "action" => "linked to a test action",
        "executed" => "execution only",
        "unexecuted" => "not executed",
        other => other,
    }
}

fn title_case_kind(kind: &str) -> String {
    let mut characters = kind.chars();
    characters.next().map_or_else(String::new, |first| {
        first.to_uppercase().collect::<String>() + characters.as_str()
    })
}

fn render_anchor(anchor: &supercov_engine::coverage_query::CoverageAnchor) -> Vec<String> {
    let coverage = anchor.conditions.map_or_else(
        || {
            if anchor.covered {
                "covered".into()
            } else {
                "not covered".into()
            }
        },
        |conditions| {
            format!(
                "{}/{} MC/DC conditions covered",
                anchor.covered_conditions.unwrap_or(0),
                conditions
            )
        },
    );
    let mut lines = vec![format!(
        "  {} at column {} — {coverage} ({} covering test{})",
        title_case_kind(&anchor.kind),
        anchor.column,
        count(anchor.covering_tests),
        if anchor.covering_tests == 1 { "" } else { "s" }
    )];
    if let Some(source) = &anchor.source {
        lines.push(format!("    `{source}`"));
    }
    if !anchor.covered
        && let Some(missing) = &anchor.missing
    {
        lines.push(format!("    Unobserved: {missing}"));
    }
    lines
}

fn push_line_pagination(
    lines: &mut Vec<String>,
    page: &AgentPagination,
    categories: &str,
    next: Option<String>,
) {
    if page.offset > 0 || page.total > page.returned {
        lines.push(format!("{} per category ({categories})", page_label(page)));
    }
    if let Some(next) = next {
        lines.push(format!("next page: {next}"));
    }
}

fn render_coverage(request: &IndexedQueryRequest, output: &IndexedQueryOutput) -> String {
    let page = output.pagination.as_ref();
    match &output.data {
        IndexedQueryData::Summary(data) => {
            let label = filter_label(request);
            let mut first = format!("run {}", data.run);
            if !label.is_empty() {
                first.push_str(&format!(" ({label})"));
            }
            if data.stale {
                first.push_str(&format!(" [STALE: {}]", data.stale_reasons.join(", ")));
            }
            let measurement = if data.measurement.complete {
                if data.measurement.declared == 0 {
                    "Complete for the measured command and coverage model".into()
                } else {
                    // Nothing inside the denominator went unmeasured; what is
                    // declared is what the denominator excludes.
                    format!(
                        "Complete for the measured command and coverage model — {} declared boundary(ies) in {} file(s)",
                        data.measurement.declared, data.measurement.files
                    )
                }
            } else {
                if data.measurement.files > 0 {
                    format!(
                        "Incomplete — {} blocking limitation(s) in {} file(s)",
                        data.measurement.blocking, data.measurement.files
                    )
                } else {
                    format!(
                        "Incomplete — {} blocking transport or run limitation(s)",
                        data.measurement.blocking
                    )
                }
            };
            let coverage_heading = if data.valid {
                "Coverage"
            } else {
                "Coverage (diagnostic — the wrapped command did not pass)"
            };
            let mut lines = vec![first];
            if !data.command.is_empty() {
                lines.push(format!("command: {}", data.command.join(" ")));
            }
            if !data.valid {
                // Calm but unmissable: the run itself is fine, the wrapped
                // command failed, so the numbers below cannot gate anything.
                lines.push(match data.test_exit_code {
                    Some(code) => format!(
                        "status: wrapped command exited {code} — coverage below is diagnostic and cannot gate"
                    ),
                    None => "status: wrapped command exit status unavailable — coverage below is diagnostic and cannot gate".into(),
                });
            }
            lines.extend([
                String::new(),
                coverage_heading.into(),
                format!(
                    "  Lines      {}",
                    measured(
                        data.coverage.lines.percentage,
                        data.coverage.lines.covered as u64,
                        data.coverage.lines.total as u64
                    )
                ),
                format!(
                    "  Branches   {}",
                    measured(
                        data.coverage.branches.percentage,
                        data.coverage.branches.covered as u64,
                        data.coverage.branches.total as u64
                    )
                ),
                format!(
                    "  MC/DC      {}",
                    measured(
                        data.coverage.condition_coverage_pct,
                        data.coverage.covered_conditions as u64,
                        data.coverage.conditions as u64
                    )
                ),
            ]);
            if let Some(assertions) = &data.assertion_coverage {
                lines.extend(assertion_summary_lines(assertions));
            }
            if data.coverage_by_kind.iter().any(|kind| kind.tests > 0) {
                lines.extend([String::new(), "By test kind".into()]);
                for kind in data.coverage_by_kind.iter().filter(|kind| kind.tests > 0) {
                    if kind.attributed == 0 {
                        lines.push(format!(
                            "  {:<12} {:>4} test(s)  coverage not attributable; counted run-wide",
                            kind.kind.as_deref().unwrap_or("unknown"),
                            kind.tests,
                        ));
                        continue;
                    }
                    lines.push(format!(
                        "  {:<12} {:>4} test(s)  lines {:>7}  branches {:>7}  MC/DC {:>7}{}",
                        kind.kind.as_deref().unwrap_or("unknown"),
                        kind.tests,
                        share(kind.summary.lines.percentage, kind.summary.lines.total),
                        share(
                            kind.summary.branches.percentage,
                            kind.summary.branches.total
                        ),
                        share(kind.summary.condition_coverage_pct, kind.summary.conditions),
                        unattributed_note(kind),
                    ));
                }
                // One kind of test at 35.83% under a run total of 41.58%
                // read as two answers to one question. The rest ran while no
                // test was running, and the rows now say so.
                if let Some(tests) = &data.coverage_by_tests {
                    let outside = [
                        (
                            data.coverage.lines.covered,
                            tests.lines.covered,
                            data.coverage.lines.total,
                        ),
                        (
                            data.coverage.branches.covered,
                            tests.branches.covered,
                            data.coverage.branches.total,
                        ),
                        (
                            data.coverage.covered_conditions,
                            tests.covered_conditions,
                            data.coverage.conditions,
                        ),
                    ]
                    .map(|(all, tested, total)| (all.saturating_sub(tested), total));
                    if outside.iter().any(|(only, _)| *only > 0) {
                        let [lines_only, branches_only, conditions_only] =
                            outside.map(|(only, total)| {
                                share(
                                    if total == 0 {
                                        0.0
                                    } else {
                                        only as f64 * 100.0 / total as f64
                                    },
                                    total,
                                )
                            });
                        lines.push(format!(
                            "  {:<12} {:>4}          lines {:>7}  branches {:>7}  MC/DC {:>7}",
                            "no test", "", lines_only, branches_only, conditions_only,
                        ));
                        lines.push("  \"no test\" is code that ran only while no test was running: start-up and setup, module loading, work between tests. It counts in the run's total and in no test's.".into());
                    }
                }
                if let Some(defaults) = data
                    .test_kind_sources
                    .get("runner-default")
                    .filter(|n| **n > 0)
                {
                    lines.push(format!("  {defaults} test(s) use the runner's default kind; this is not inferred from their behavior."));
                    lines.push("  Set SUPERCOV_TEST_KIND=integration (or unit/component/e2e) when running a suite to override it.".into());
                }
            }
            if let Some(context) = &data.e2e_gap_context {
                let share = if data.coverage.lines.covered == 0 {
                    0.0
                } else {
                    context.covered_elsewhere.lines as f64 * 100.0
                        / data.coverage.lines.covered as f64
                };
                lines.extend([
                    String::new(),
                    "E2E gap context".into(),
                    format!(
                        "  Other test kinds only  {} lines ({} of all covered lines)",
                        count(context.covered_elsewhere.lines),
                        percentage(share),
                    ),
                    format!(
                        "  Uncovered everywhere   {} lines",
                        count(context.uncovered_everywhere.lines),
                    ),
                    format!(
                        "  Other kinds            {}",
                        context.other_kinds.join(", ")
                    ),
                    format!(
                        "  Inspect                {}",
                        coverage_command(&data.run, request, "gaps --kind e2e")
                    ),
                ]);
            }
            lines.extend([
                String::new(),
                "Measurement".into(),
                format!("  Instrumentation  {measurement}"),
            ]);
            // "89 blocking limitation(s) in 89 file(s)" did not say that 79
            // of them were one directory, or what would settle them.
            if let Some(unclassified) = &data.unclassified_source {
                const SHOWN: usize = 4;
                let mut places = unclassified
                    .directories
                    .iter()
                    .take(SHOWN)
                    .map(|place| {
                        let name = if place.directory == "." {
                            "the top level"
                        } else {
                            &place.directory
                        };
                        format!("{name} {}", count(place.files))
                    })
                    .collect::<Vec<_>>();
                if unclassified.directories.len() > SHOWN {
                    places.push(format!(
                        "{} elsewhere",
                        count(
                            unclassified.directories[SHOWN..]
                                .iter()
                                .map(|place| place.files)
                                .sum()
                        )
                    ));
                }
                lines.push(format!(
                    "                   Unclassified source: {}",
                    places.join(", ")
                ));
                lines.push(format!(
                    "                   {}",
                    source_roots_hint(unclassified)
                ));
            }
            lines.extend([
                format!("  Attribution      {}", attribution_line(&data.test_attribution)),
                "  Scope            Only code reached by the wrapped command is observed; this status does not prove every project test suite was run.".into(),
            ]);
            if let Some(guests) = data
                .transport
                .as_ref()
                .map(|transport| transport.guest_processes)
                .filter(|guests| *guests > 0)
            {
                lines.push(format!(
                    "  Guests           {guests} process(es) ran the workspace in a container or VM without Supercov's settings; their coverage counts for the run, credited to no test"
                ));
            }
            if let Some(workspace) = &data.workspace {
                lines.push(format!("  Command outputs  {workspace}"));
            }
            lines.extend([String::new(), "Tests".into()]);
            lines.push(format!("  Total       {}", count(data.tests)));
            lines.extend(outcome_lines(&data.test_outcomes));
            if data.setups > 0 {
                lines.push(format!("  Setup scopes {}", count(data.setups)));
            }
            lines.extend([
                String::new(),
                "Remaining".into(),
                format!(
                    "  Files with uncovered code         {}",
                    count(data.files_with_coverage_gaps)
                ),
                format!(
                    "  Files with measurement limits     {}",
                    count(data.files_with_measurement_limitations)
                ),
            ]);
            if !data.diagnostics.is_empty() {
                lines.extend([String::new(), "Warnings".into()]);
                for diagnostic in &data.diagnostics {
                    lines.extend(diagnostic_lines(
                        diagnostic,
                        Some(&coverage_command(
                            &data.run,
                            request,
                            "tests without-evidence",
                        )),
                    ));
                }
            }
            if !data.hints.is_empty() {
                lines.extend([String::new(), "Hints".into()]);
                lines.extend(data.hints.iter().map(|hint| format!("  {hint}")));
            }
            lines.extend([
                String::new(),
                "Commands".into(),
                format!("  {}", coverage_command(&data.run, request, "files")),
                format!("  {}", coverage_command(&data.run, request, "gaps")),
                format!("  {}", coverage_command(&data.run, request, "kinds")),
                format!("  {}", coverage_command(&data.run, request, "runners")),
                format!("  {}", coverage_command(&data.run, request, "scope")),
                format!("  {} runs {} --help", crate::launcher_command(), data.run),
            ]);
            lines.join("\n")
        }
        IndexedQueryData::Areas(data) => {
            render_areas(data, request, page.expect("areas are paginated"))
        }
        IndexedQueryData::Files(data) => render_files(
            &data.files,
            request,
            page.expect("files are paginated"),
            "files",
            &data.run,
        ),
        IndexedQueryData::Gaps(data) => render_files(
            &data.gaps,
            request,
            page.expect("gaps are paginated"),
            "gaps",
            &data.run,
        ),
        IndexedQueryData::Kinds(data) => render_dimension(
            &data.kinds,
            request,
            page.expect("kinds are paginated"),
            "kinds",
            &data.run,
        ),
        IndexedQueryData::Runners(data) => render_dimension(
            &data.runners,
            request,
            page.expect("runners are paginated"),
            "runners",
            &data.run,
        ),
        IndexedQueryData::Scope(data) => {
            let page = page.expect("scope is paginated");
            let mut lines = vec![format!(
                "{} scope; language {}; model {}",
                data.kind, data.language, data.model
            )];
            if let Some(mode) = &data.mode {
                lines.push(format!(
                    "mode {}; roots {}; included {}, excluded {}, ambiguous {}",
                    mode,
                    if data.roots.is_empty() {
                        "none".into()
                    } else {
                        data.roots.join(", ")
                    },
                    data.counts.included,
                    data.counts.excluded,
                    data.counts.ambiguous,
                ));
            }
            if let Some(unit) = &data.unit {
                lines.push(format!("unit {unit}"));
            }
            if let Some(complete) = data.measurement_complete {
                lines.push(format!(
                    "frontend measurement {}",
                    if complete { "complete" } else { "incomplete" }
                ));
            }
            lines.push(format!(
                "measurement {}",
                if data.measurement.complete {
                    if data.measurement.declared == 0 {
                        "complete".into()
                    } else {
                        format!(
                            "complete, {} declared boundary(ies)",
                            data.measurement.declared
                        )
                    }
                } else {
                    format!("{} blocking limitation(s)", data.measurement.blocking)
                }
            ));
            if request.group.as_deref() != Some("file") {
                lines.push(String::new());
                lines.extend(scope_group_lines(&data.groups));
                if let Some(unclassified) = &data.unclassified_source {
                    lines.push(String::new());
                    lines.push(source_roots_hint(unclassified));
                }
                lines.push(String::new());
                lines.push(format!(
                    "File by file: {} --files",
                    coverage_command(&data.run, request, "scope")
                ));
                return lines.join("\n");
            }
            lines.extend(data.entries.iter().map(|entry| {
                format!(
                    "{}  {}  {}{}{}",
                    entry.status.to_uppercase(),
                    entry.file,
                    entry.reason,
                    if entry.measurement_limitations == 0 {
                        String::new()
                    } else {
                        format!(
                            "  [measurement limitations: {} {}]",
                            entry.measurement_limitations,
                            entry.limitation_kinds.join(", ")
                        )
                    },
                    entry
                        .package_root
                        .as_ref()
                        .map_or_else(String::new, |root| format!("  [package {root}]"))
                )
            }));
            lines.push(page_label(page));
            if let Some(next) = next_page(
                &format!("{} --files", coverage_command(&data.run, request, "scope")),
                page,
            ) {
                lines.push(format!("next page: {next}"));
            }
            lines.join("\n")
        }
        IndexedQueryData::FileDecisions(data) => {
            let page = page.expect("file decisions are paginated");
            let mut lines = vec![
                format!("{}  MC/DC by decision", data.file),
                format!(
                    "decisions {}, with missing conditions {}; conditions missing {}/{}",
                    data.totals.decisions,
                    data.totals.decisions_with_missing_conditions,
                    data.totals.missing_conditions,
                    data.totals.conditions,
                ),
            ];
            lines.extend(data.decisions.iter().map(|row| {
                let compact = row.source.split_whitespace().collect::<Vec<_>>().join(" ");
                let snippet = if compact.chars().count() > 96 {
                    format!("{}…", compact.chars().take(95).collect::<String>())
                } else {
                    compact
                };
                format!(
                    "{}:{}  [{}]  missing {}/{}  {snippet}",
                    row.line, row.column, row.id, row.missing_conditions, row.conditions,
                )
            }));
            if data.decisions.is_empty() {
                lines.push(String::new());
            }
            lines.push(format!(
                "{} decisions with missing conditions",
                page_label(page)
            ));
            let mut base = format!(
                "{} {} --group decision",
                coverage_command(&data.run, request, "file"),
                shell_quote(&data.file)
            );
            if data.sort != DecisionSort::Location {
                base.push_str(" --sort missing");
            }
            if let Some(next) = next_page(&base, page) {
                lines.push(format!("next page: {next}"));
            }
            lines.join("\n")
        }
        IndexedQueryData::FileDetail(data) => {
            let page = page.expect("file detail is paginated");
            let mut lines = vec![
                data.file.clone(),
                String::new(),
                "Uncovered".into(),
                format!(
                    "  Lines not executed              {}",
                    count(data.counts.uncovered_lines)
                ),
                format!(
                    "  Statements not executed         {}",
                    count(data.counts.uncovered_statements)
                ),
                format!(
                    "  Functions not called            {}",
                    count(data.counts.uncovered_functions)
                ),
                format!(
                    "  Branch outcomes not taken       {}",
                    count(data.counts.missing_branches)
                ),
                format!(
                    "  MC/DC conditions not shown      {}",
                    count(data.counts.missing_mcdc_conditions)
                ),
                format!(
                    "  Measurement limitations         {}",
                    count(data.counts.measurement_limitations)
                ),
                String::new(),
                format!("Tests touching this file: {}", count(data.total_tests)),
                String::new(),
                "Gaps".into(),
                format!(
                    "  NOT COVERED = line never executed; PARTIAL = line executed but some behavior remains untested{}",
                    if data.gap_lines.iter().any(|gap| gap.state == "limited") {
                        "; NOT MEASURED = no record of the line, for the reason given"
                    } else {
                        ""
                    }
                ),
            ];
            if page.total == 0 {
                lines.truncate(lines.len() - 1);
                lines.push("  No gap lines: every line is covered and measured.".into());
            } else {
                lines.push(" LINE  STATUS        SOURCE".into());
            }
            if let Some(at) = lines
                .iter()
                .position(|line| line.starts_with("Tests touching this file"))
            {
                let assertions = file_assertion_lines(data.assertions.as_ref());
                lines.splice(at + 1..at + 1, assertions);
            }
            for gap in &data.gap_lines {
                lines.push(format!(
                    "{:>5}  {:<12}  {}",
                    gap.line,
                    state_label(&gap.state),
                    gap.source.as_deref().unwrap_or("(source unavailable)")
                ));
                let needs = file_gap_needs(&gap.state, &gap.obligations);
                for need in needs {
                    lines.push(format!("       Unobserved: {need}"));
                }
                for limitation in &gap.limitations {
                    lines.push(format!("       Cannot measure: {}", limitation.reason));
                }
                if gap.state == "part" {
                    let selector = format!("{}:{}", data.file, gap.line);
                    lines.push(format!(
                        "       Inspect: {} {}",
                        coverage_command(&data.run, request, "line"),
                        shell_quote(&selector)
                    ));
                }
            }
            lines.push(format!("{} gap lines", page_label(page)));
            let base = format!(
                "{} {}",
                coverage_command(&data.run, request, "file"),
                shell_quote(&data.file)
            );
            if let Some(next) = next_page(&base, page) {
                lines.push(format!("next page: {next}"));
            }
            lines.join("\n")
        }
        IndexedQueryData::Decision(data) => {
            let page = page.expect("decision is paginated");
            match data.as_ref() {
                CoverageDecisionData::Matches(data) => {
                    let mut lines = data
                        .decisions
                        .iter()
                        .map(|decision| {
                            format!(
                                "{}  {}:{}:{}  {}",
                                decision.id,
                                decision.file,
                                decision.line,
                                decision.column,
                                decision.source
                            )
                        })
                        .collect::<Vec<_>>();
                    lines.push(format!("{} matching decisions", page_label(page)));
                    let base = format!(
                        "{} {}",
                        coverage_command(&data.run, request, "decision"),
                        shell_quote(request.selector.as_deref().unwrap_or_default())
                    );
                    if let Some(next) = next_page(&base, page) {
                        lines.push(format!("next page: {next}"));
                    }
                    lines.join("\n")
                }
                CoverageDecisionData::Detail(data) => {
                    let mut decisions = Vec::new();
                    for decision in &data.decisions {
                        let mut lines = vec![
                            format!(
                                "{}  {}:{}:{}",
                                decision.meta.id,
                                decision.meta.file,
                                decision.meta.line,
                                decision.meta.column
                            ),
                            decision.meta.source.clone(),
                        ];
                        lines.extend(decision.conditions.iter().map(|condition| {
                            format!(
                                "C{} {}: {}",
                                condition.index + 1,
                                if condition.covered {
                                    "covered"
                                } else {
                                    "MISSING"
                                },
                                condition.source,
                            )
                        }));
                        lines.push(format!("confidence {}", decision.confidence.level));
                        lines.push("vectors:".into());
                        if decision.vector_observations.is_empty() {
                            lines.push("  none".into());
                        } else {
                            lines.extend(decision.vector_observations.iter().map(|observation| {
                                format!(
                                    "  {}  tests={} confidence={}",
                                    vector_text(&observation.vector),
                                    observation.tests.len(),
                                    observation.confidence.level
                                )
                            }));
                        }
                        decisions.push(lines.join("\n"));
                    }
                    let mut output = decisions.join("\n\n");
                    output.push_str(&format!(
                        "\n{} conditions/vectors/tests per decision",
                        page_label(page)
                    ));
                    let base = format!(
                        "{} {}",
                        coverage_command(&data.run, request, "decision"),
                        shell_quote(request.selector.as_deref().unwrap_or_default())
                    );
                    if let Some(next) = next_page(&base, page) {
                        output.push_str(&format!("\nnext page: {next}"));
                    }
                    output
                }
            }
        }
        IndexedQueryData::Line(data) => {
            let page = page.expect("line is paginated");
            match data.as_ref() {
                CoverageCoversData::Anchors(data) => {
                    let complete = data.covered_anchored;
                    // A line nothing measured is not measured, whatever
                    // limitation says why.
                    let state = if data.total_anchored == 0 && data.total_remaining == 0 {
                        "NOT MEASURED"
                    } else if data.total_tests == 0
                        && complete == 0
                        && data.total_anchored > 0
                        && data.total_limitations == 0
                    {
                        // Nothing anchored on the line was observed, by a test
                        // or in the background: it never ran.
                        "NOT COVERED"
                    } else if complete == data.total_anchored
                        && data.total_limitations == 0
                        && data.total_remaining == 0
                    {
                        "COVERED"
                    } else {
                        "PARTIAL"
                    };
                    let mut lines = vec![
                        format!("{}:{}", data.location.file, data.location.line),
                        String::new(),
                        "Source".into(),
                        format!(
                            "  {:>5} | {}",
                            data.location.line,
                            data.source
                                .as_deref()
                                .unwrap_or("(source unavailable in this run)")
                        ),
                        String::new(),
                        "Status".into(),
                        format!("  {state}"),
                    ];
                    if let Some(origin) = &data.source_origin {
                        lines.insert(
                            4,
                            if origin == "working-tree-stale" {
                                "  (read from the working tree, which changed since this run)"
                                    .into()
                            } else {
                                "  (read from the current working tree)".into()
                            },
                        );
                    }
                    if !data.remaining.is_empty() {
                        lines.extend([String::new(), "Unobserved coverage".into()]);
                        lines.extend(data.remaining.iter().map(|obligation| {
                            format!("  - {}", render_needed_obligation(obligation))
                        }));
                    }
                    if !data.anchored.is_empty() {
                        lines.extend([String::new(), "Coverage details".into()]);
                        for anchor in &data.anchored {
                            lines.extend(render_anchor(anchor));
                        }
                    }
                    if !data.limitations.is_empty() {
                        lines.extend([String::new(), "Measurement limits".into()]);
                        lines.extend(data.limitations.iter().map(|limitation| {
                            format!("  - {}: {}", limitation.kind, limitation.reason)
                        }));
                    }
                    lines.extend(line_assertion_lines(data.assertions.as_ref()));
                    lines.extend([String::new(), "Covering tests".into()]);
                    if data.tests.is_empty() {
                        lines.push("  None".into());
                    } else {
                        lines.extend(data.tests.iter().map(|test| {
                            format!(
                                "  {} [{}] — {}/{}",
                                test.name, test.id, test.provenance.kind, test.provenance.runner
                            )
                        }));
                    }
                    let selector = format!("{}:{}", data.location.file, data.location.line);
                    let base = format!(
                        "{} {}",
                        coverage_command(&data.run, request, "line"),
                        shell_quote(&selector)
                    );
                    push_line_pagination(
                        &mut lines,
                        page,
                        "tests/obligations/limitations",
                        next_page(&base, page),
                    );
                    lines.join("\n")
                }
                CoverageCoversData::Line(data) => {
                    let complete = data.covered_anchored;
                    let state = if !data.covered {
                        "NOT COVERED"
                    } else if complete < data.total_anchored
                        || data.total_limitations > 0
                        || data.total_remaining > 0
                    {
                        "PARTIAL"
                    } else {
                        "COVERED"
                    };
                    let mut lines = vec![
                        format!("{}:{}", data.location.file, data.location.line),
                        String::new(),
                        "Source".into(),
                        format!(
                            "  {:>5} | {}",
                            data.location.line,
                            data.source
                                .as_deref()
                                .unwrap_or("(source unavailable in this run)")
                        ),
                        String::new(),
                        "Status".into(),
                        format!("  {state}"),
                        format!(
                            "  Evidence: {}{}",
                            confidence_label(&data.confidence.level),
                            if data.confidence.e2e {
                                " through E2E"
                            } else {
                                ""
                            }
                        ),
                    ];
                    if let Some(origin) = &data.source_origin {
                        lines.insert(
                            4,
                            if origin == "working-tree-stale" {
                                "  (read from the working tree, which changed since this run)"
                                    .into()
                            } else {
                                "  (read from the current working tree)".into()
                            },
                        );
                    }
                    if !data.remaining.is_empty() {
                        lines.extend([String::new(), "Unobserved coverage".into()]);
                        lines.extend(data.remaining.iter().map(|obligation| {
                            format!("  - {}", render_needed_obligation(obligation))
                        }));
                    }
                    if !data.anchored.is_empty() {
                        lines.extend([String::new(), "Coverage details".into()]);
                        for anchor in &data.anchored {
                            lines.extend(render_anchor(anchor));
                        }
                    }
                    if !data.limitations.is_empty() {
                        lines.extend([String::new(), "Measurement limits".into()]);
                        lines.extend(data.limitations.iter().map(|limitation| {
                            format!("  - {}: {}", limitation.kind, limitation.reason)
                        }));
                    }
                    lines.extend(line_assertion_lines(data.assertions.as_ref()));
                    lines.extend([String::new(), "Covering tests".into()]);
                    if data.tests.is_empty() {
                        lines.push("  None".into());
                    } else {
                        lines.extend(data.tests.iter().map(|test| {
                            format!(
                                "  {} [{}] — {}/{}",
                                test.name, test.id, test.provenance.kind, test.provenance.runner
                            )
                        }));
                    }
                    if !data.phases.is_empty() {
                        lines.extend([String::new(), "Test phases".into()]);
                        lines.extend(data.phases.iter().map(|phase| {
                            format!(
                                "  {}{}{}",
                                phase.operation,
                                phase
                                    .status
                                    .as_ref()
                                    .map_or_else(String::new, |status| format!(" ({status})")),
                                phase
                                    .source
                                    .as_ref()
                                    .map_or_else(String::new, |source| format!(" at {source}"))
                            )
                        }));
                    }
                    let selector = format!("{}:{}", data.location.file, data.location.line);
                    let base = format!(
                        "{} {}",
                        coverage_command(&data.run, request, "line"),
                        shell_quote(&selector)
                    );
                    push_line_pagination(
                        &mut lines,
                        page,
                        "tests/phases/obligations/limitations",
                        next_page(&base, page),
                    );
                    lines.join("\n")
                }
            }
        }
        IndexedQueryData::Test(data) => {
            let page = page.expect("test is paginated");
            match data.as_ref() {
                CoverageTestData::Matches(data) => {
                    let mut lines = data
                        .tests
                        .iter()
                        .map(|test| format!("{} [{}] — {}", test.name, test.id, test.outcome))
                        .collect::<Vec<_>>();
                    lines.push(format!("{} matching tests", page_label(page)));
                    let base = format!(
                        "{} {}",
                        coverage_command(&data.run, request, "test"),
                        shell_quote(request.selector.as_deref().unwrap_or_default())
                    );
                    if let Some(next) = next_page(&base, page) {
                        lines.push(format!("next page: {next}"));
                    }
                    lines.join("\n")
                }
                CoverageTestData::Detail(data) => {
                    let test = data.tests.first().expect("test detail contains one test");
                    let mut lines = vec![
                        test.name.clone(),
                        format!(
                            "outcome {}{}",
                            test.outcome,
                            if test.attempts.is_empty() {
                                String::new()
                            } else {
                                format!(
                                    "; {}",
                                    test.attempts
                                        .iter()
                                        .map(|attempt| format!(
                                            "retry {}={}",
                                            attempt.retry, attempt.status
                                        ))
                                        .collect::<Vec<_>>()
                                        .join(", ")
                                )
                            }
                        ),
                        // Zero here means two different things, and only one
                        // of them is "this test reached nothing". A Go test
                        // that called t.Parallel() stores into the same probe
                        // array as the tests beside it, so what it reached was
                        // recorded against the run: reporting that as a test
                        // covering nothing would be a wrong number, and
                        // reporting nothing at all is what used to answer
                        // "Test not found" for a test that had just passed.
                        if test.attribution == "run-wide" {
                            "coverage not attributable to this test; it ran alongside others and what it reached is recorded run-wide".into()
                        } else if test.attribution == "partial" {
                            // The numbers are real and they are not all of it.
                            // Ruby credits a line to the first test that runs
                            // it, so a later test that runs the same line is
                            // recorded against none of it.
                            format!(
                                "at least {} lines, {} hits, {} decisions, {} phases (a line already recorded for an earlier test is not recorded again)",
                                test.totals.lines,
                                test.totals.hits,
                                test.totals.decisions,
                                test.totals.phases
                            )
                        } else {
                            format!(
                                "{} lines, {} hits, {} decisions, {} phases",
                                test.totals.lines,
                                test.totals.hits,
                                test.totals.decisions,
                                test.totals.phases
                            )
                        },
                    ];
                    lines.extend(
                        test.lines
                            .iter()
                            .map(|line| format!("line: {}:{}", line.file, line.line)),
                    );
                    lines.extend(test.phases.iter().map(|phase| {
                        format!(
                            "{}: {}{}",
                            phase.kind,
                            phase.operation,
                            phase
                                .source
                                .as_ref()
                                .map_or_else(String::new, |source| format!(" at {source}"))
                        )
                    }));
                    lines.push(format!("{} per evidence category", page_label(page)));
                    let base = format!(
                        "{} {}",
                        coverage_command(&data.run, request, "test"),
                        shell_quote(request.selector.as_deref().unwrap_or_default())
                    );
                    if let Some(next) = next_page(&base, page) {
                        lines.push(format!("next page: {next}"));
                    }
                    lines.join("\n")
                }
            }
        }
        IndexedQueryData::Minimize(data) => {
            let page = page.expect("minimize is paginated");
            let target_label = if data.metric == MinimizeMetric::All {
                "coverage across all measured metrics"
            } else {
                metric_name(data.metric)
            };
            let mut lines = vec![
                format!(
                    "exact minimum {}/{} test(s) for {}% {}; explored {} state(s)",
                    data.selected_count,
                    data.total_candidate_tests,
                    number(data.target),
                    target_label,
                    data.explored_states
                ),
                summary_line(&data.summary),
            ];
            lines.extend(data.tests.iter().map(|test| {
                format!(
                    "{}  {}/{}  {}  {}",
                    test.id,
                    test.kind,
                    test.runner,
                    test.file.as_deref().unwrap_or("unknown"),
                    test.name
                )
            }));
            lines.push(page_label(page));
            let base = format!(
                "{} --target {}",
                coverage_command(&data.run, request, "minimize"),
                number(data.target)
            );
            if let Some(next) = next_page(&base, page) {
                lines.push(format!("next page: {next}"));
            }
            lines.join("\n")
        }
        IndexedQueryData::TestsWithoutEvidence(data) => {
            let page = page.expect("tests are paginated");
            let label = filter_label(request);
            let mut lines = vec![format!(
                "Tests that made assertions and recorded no coverage: {}{}",
                count(page.total),
                if label.is_empty() {
                    String::new()
                } else {
                    format!(" — {label}")
                }
            )];
            if data.tests.is_empty() {
                return lines.join("\n");
            }
            lines.push(String::new());
            for test in &data.tests {
                // A runner that names a test by its titles alone leaves out
                // where it is.
                let place = test
                    .file
                    .as_deref()
                    .filter(|file| {
                        let name = file.rsplit('/').next().unwrap_or(file);
                        !test.name.contains(name)
                    })
                    .map_or_else(String::new, |file| format!("{file} > "));
                lines.push(format!(
                    "  {place}{} [{}] — {}/{}, {}, {} assertion{}",
                    test.name,
                    test.id,
                    test.kind,
                    test.runner,
                    test.outcome,
                    count(test.assertions),
                    if test.assertions == 1 { "" } else { "s" },
                ));
            }
            lines.push(page_label(page));
            let base = coverage_command(&data.run, request, "tests without-evidence");
            if page.offset + page.returned < page.total {
                lines.push(format!(
                    "next page: {base} --offset {} --limit {}",
                    page.offset + page.returned,
                    page.limit
                ));
            }
            lines.extend([
                String::new(),
                "Each ran and checked something, and no line of the measured source is linked to it: the code it checks is outside the measured source (see `scope`), ran in shared setup, or its evidence did not arrive. Missing evidence does not prove the code did not execute.".into(),
                format!(
                    "One test in detail: {}",
                    coverage_command(&data.run, request, "test <id>")
                ),
            ]);
            lines.join("\n")
        }
        IndexedQueryData::Diff(data) => {
            let page = page.expect("diff is paginated");
            let signed = |value: f64| format!("{}{value}", if value >= 0.0 { "+" } else { "" });
            let mut lines = vec![
                format!("{} -> {}", data.older, data.newer),
                format!(
                    "lines {}pp, branches {}pp, MC/DC {}pp",
                    signed(data.delta.lines),
                    signed(data.delta.branches),
                    signed(data.delta.mcdc)
                ),
                format!(
                    "gained: {} lines, {} branches, {} MC/DC conditions",
                    data.gained.line_count, data.gained.branch_count, data.gained.mcdc_count
                ),
                format!(
                    "lost: {} lines, {} branches, {} MC/DC conditions",
                    data.lost.line_count, data.lost.branch_count, data.lost.mcdc_count
                ),
            ];
            let gained = data
                .gained
                .lines
                .iter()
                .map(|line| format!("+ line {line}"))
                .chain(
                    data.gained
                        .branches
                        .iter()
                        .map(|item| format!("+ branch {item}")),
                )
                .chain(
                    data.gained
                        .mcdc
                        .iter()
                        .map(|item| format!("+ MC/DC {item}")),
                )
                .collect::<Vec<_>>();
            if gained.is_empty() {
                lines.push(String::new());
            } else {
                lines.extend(gained);
            }
            lines.push(format!("{} per category", page_label(page)));
            let mut base = format!(
                "{} diff {} {}",
                crate::launcher_command(),
                shell_quote(&data.older),
                shell_quote(&data.newer)
            );
            if request.filter != "all" {
                base.push_str(&format!(" --filter {}", request.filter));
            }
            if let Some(next) = next_page(&base, page) {
                lines.push(format!("next page: {next}"));
            }
            lines.join("\n")
        }
    }
}

pub fn render_human(invocation: &PublicQueryInvocation, output: &PublicQueryOutput) -> String {
    match (invocation, output) {
        (
            PublicQueryInvocation::Runs { filter, .. },
            PublicQueryOutput::Runs { data, pagination },
        ) => {
            let id_width = data
                .runs
                .iter()
                .map(|run| run.id.len())
                .max()
                .unwrap_or(2)
                .max(2);
            let mut lines = vec![format!(
                "{:<id_width$}  {:>8}  {:>8}  {:>8}  {}",
                "ID", "LINES", "BRANCH", "MC/DC", "STARTED"
            )];
            lines.extend(data.runs.iter().map(|run| {
                let mut status = Vec::new();
                if let Some(code) = run.test_exit_code {
                    if code != 0 {
                        status.push(format!("FAILED (exit {code})"));
                    }
                } else {
                    status.push("INVALID (exit status unavailable)".into());
                }
                if run.coverage_error.is_some() {
                    status.push("INVALID COVERAGE".into());
                }
                if run.tests == Some(0) {
                    status.push("NO TESTS RAN".into());
                }
                if run.stale == Some(true) {
                    status.push(format!("STALE ({})", run.reasons.join(", ")));
                }
                format!(
                    "{:<id_width$}  {:>8}  {:>8}  {:>8}  {}{}",
                    run.id,
                    optional_percentage(run.lines),
                    optional_percentage(run.branches),
                    optional_percentage(run.mcdc),
                    readable_timestamp(&run.generated_at),
                    if status.is_empty() {
                        String::new()
                    } else {
                        format!("  {}", status.join("; "))
                    }
                )
            }));
            lines.push(page_label(pagination));
            let mut base = format!("{} runs", crate::launcher_command());
            if filter != "all" {
                base.push_str(&format!(" --filter {filter}"));
            }
            if let Some(next) = next_page(&base, pagination) {
                lines.push(format!("next page: {next}"));
            }
            lines.join("\n")
        }
        (
            PublicQueryInvocation::Coverage { request, .. },
            PublicQueryOutput::Coverage { output, .. },
        ) => render_coverage(request, output),
        _ => unreachable!("query execution preserves invocation kind"),
    }
}

fn assertion_summary_lines(value: &serde_json::Value) -> Vec<String> {
    if value["available"] == false {
        return vec![format!(
            "Assertions   not assessed — {}",
            value["assess"]
                .as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| format!(
                    "{} runs latest assertions assess",
                    crate::launcher_command()
                ))
        )];
    }
    let s = &value["summary"];
    match s["percentage"].as_f64() {
        Some(pct) => vec![format!(
            "Assertions   {pct}% ({}/{} executed statements a test is judged to catch)",
            s["asserted"], s["statements"]
        )],
        None => vec!["Assertions   no executed statements to assess".into()],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn assertion_summary_reads_the_assessed_share_or_says_how_to_assess() {
        let report = serde_json::json!({"available":true,"summary":{"statements":4,"asserted":1,"percentage":25.0}});
        assert_eq!(
            assertion_summary_lines(&report),
            vec!["Assertions   25% (1/4 executed statements a test is judged to catch)"]
        );
        let none = serde_json::json!({"available":false,"assess":"npx supercov runs r1 assertions assess"});
        assert_eq!(
            assertion_summary_lines(&none),
            vec!["Assertions   not assessed — npx supercov runs r1 assertions assess"]
        );
    }

    fn request() -> IndexedQueryRequest {
        IndexedQueryRequest {
            run_id: "run_00b780f05c9ae324".into(),
            filter: "all".into(),
            command: "coverage.files".into(),
            metric: MinimizeMetric::All,
            kind: None,
            runner: None,
            file: None,
            line: None,
            selector: None,
            sort: None,
            valid: None,
            test_exit_code: None,
            stale: None,
            stale_reasons: None,
            offset: 0,
            limit: DEFAULT_LIMIT,
            target: None,
            max_states: None,
            group: None,
            depth: None,
        }
    }

    #[test]
    fn file_listing_offers_a_concrete_drill_down_command() {
        assert_eq!(
            inspect_file_command(
                "run_00b780f05c9ae324",
                &request(),
                Some("app/routes/app.articles.$articleId/route.tsx")
            ),
            Some(format!(
                "inspect file: {} runs 'run_00b780f05c9ae324' file 'app/routes/app.articles.$articleId/route.tsx'",
                crate::launcher_command()
            ))
        );
    }

    #[test]
    fn empty_file_listing_does_not_offer_a_drill_down_command() {
        assert_eq!(
            inspect_file_command("run_00b780f05c9ae324", &request(), None),
            None
        );
    }

    #[test]
    fn a_metric_with_nothing_to_measure_is_not_a_percentage() {
        assert_eq!(measured(100.0, 0, 0), "nothing to measure (0/0)");
        assert_eq!(measured(50.0, 1, 2), "50.00% (1/2)");
        assert_eq!(share(100.0, 0), "—");
        assert_eq!(share(100.0, 4), "100.00%");
    }

    #[test]
    fn a_run_without_gaps_says_so_instead_of_an_empty_list() {
        let page = AgentPagination {
            offset: 0,
            limit: DEFAULT_LIMIT,
            returned: 0,
            total: 0,
            has_more: false,
            next_offset: None,
        };
        let text = render_files(&[], &request(), &page, "gaps", "run_00b780f05c9ae324");
        assert!(
            text.contains("\n\nNo gaps: every included file is fully covered and measured.\n\n"),
            "{text}"
        );
    }

    #[test]
    fn counts_are_grouped_for_human_output() {
        assert_eq!(count(0), "0");
        assert_eq!(count(999), "999");
        assert_eq!(count(1_130), "1,130");
        assert_eq!(count(1_000_000), "1,000,000");
    }

    #[test]
    fn follow_up_commands_preserve_the_active_projection() {
        let mut request = request();
        request.filter = "failed".into();
        request.kind = Some("integration".into());
        request.runner = Some("playwright".into());
        assert_eq!(
            coverage_command("run_00b780f05c9ae324", &request, "files"),
            format!(
                "{} runs 'run_00b780f05c9ae324' files --filter failed --kind 'integration' --runner 'playwright'",
                crate::launcher_command()
            )
        );
    }

    #[test]
    fn missing_evidence_for_several_tests_names_the_query_that_lists_them() {
        let lines = diagnostic_lines(
            &CoverageDiagnostic {
                code: "TEST_EVIDENCE_MISSING".into(),
                severity: "warning".into(),
                message: "63 test(s) recorded assertion phases but attributed zero coverage evidence; possible causes follow. First: safety.spec.ts > checks the VM".into(),
            },
            Some("supercov runs 'run_1' tests without-evidence"),
        );
        assert_eq!(
            lines[..3],
            [
                "  63 tests made assertions, but Supercov received no source-coverage evidence:",
                "    Example: safety.spec.ts > checks the VM",
                "    All of them: supercov runs 'run_1' tests without-evidence",
            ]
        );
    }

    #[test]
    fn missing_test_evidence_is_explained_without_internal_codes() {
        let lines = diagnostic_lines(
            &CoverageDiagnostic {
                code: "TEST_EVIDENCE_MISSING".into(),
                severity: "warning".into(),
                message: "1 test(s) recorded assertion phases but attributed zero coverage evidence; this is valid for assertions over static or uninstrumented data, but may otherwise indicate missing probe transport. First: safety.spec.ts > checks the VM".into(),
            },
            Some("supercov runs 'run_1' tests without-evidence"),
        );
        assert_eq!(
            lines,
            vec![
                "  1 test made assertions, but Supercov received no source-coverage evidence:",
                "    Example: safety.spec.ts > checks the VM",
                "  Possible causes: the code under test is outside the measured source (see `scope`), uninstrumented data, shared setup, lost async context, or missing probe transport. Missing evidence does not prove the code did not execute.",
                "  Inspect the test's coverage and assertion details to distinguish missing execution from missing attribution.",
            ]
        );
        assert!(
            lines
                .iter()
                .all(|line| !line.contains("TEST_EVIDENCE_MISSING"))
        );
    }

    #[test]
    fn branch_obligations_are_described_as_observed_facts() {
        assert_eq!(
            branch_need("default evaluated"),
            "default-value branch not observed"
        );
        assert_eq!(
            branch_need("value provided"),
            "explicit-value branch not observed"
        );
        assert_eq!(
            branch_need("zero iterations"),
            "zero-iteration outcome not observed"
        );
    }

    #[test]
    fn the_attribution_line_offers_only_what_would_help() {
        use supercov_engine::coverage_query::TestAttributionCounts;

        let counts = |exact, partial, run_wide| TestAttributionCounts {
            exact,
            partial,
            run_wide,
        };

        // The ordinary answer, printed on every run so the question is
        // familiar before it matters.
        assert_eq!(attribution_line(&counts(16, 0, 0)), "Exact for 16 test(s)");

        // Where running the suite in order would buy the credit back, say so.
        let offered = attribution_line(&counts(0, 0, 2));
        assert!(offered.contains("2 counted run-wide"), "{offered}");
        assert!(offered.contains("--exact-attribution"), "{offered}");

        // Where it would not, do not: Ruby credits a line to the first test
        // that reaches it, and running in order changes nothing about that.
        // An offer that cannot be taken is worse than silence.
        let lower_bound = attribution_line(&counts(0, 3, 0));
        assert!(lower_bound.contains("3 a lower bound"), "{lower_bound}");
        assert!(
            !lower_bound.contains("--exact-attribution"),
            "nothing here is bought by running in order: {lower_bound}"
        );

        // A mixed run names each part rather than rounding to the worst.
        let mixed = attribution_line(&counts(5, 0, 2));
        assert!(mixed.contains("5 exact"), "{mixed}");
        assert!(mixed.contains("2 counted run-wide"), "{mixed}");

        // And a run with no tests says that, rather than claiming exactness
        // over nothing -- which is the shape every floor is satisfied by.
        assert_eq!(
            attribution_line(&counts(0, 0, 0)),
            "No test recorded coverage"
        );
    }
}
