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
        ("Timed out", outcomes.timed_out),
        ("Interrupted", outcomes.interrupted),
        ("Unknown", outcomes.unknown),
    ]
    .into_iter()
    .filter(|(_, value)| *value > 0)
    .map(|(label, value)| format!("  {label:<11} {}", count(value)))
    .collect()
}

fn diagnostic_lines(diagnostic: &CoverageDiagnostic) -> Vec<String> {
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
            return vec![
                format!(
                    "  {} {} made assertions, but Supercov received no source-coverage evidence:",
                    count(test_count),
                    if test_count == 1 { "test" } else { "tests" }
                ),
                format!("    {first}"),
                "  This is usually normal for environment or static-data checks. Investigate only if the test should execute instrumented application code.".into(),
            ];
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
    let mut values = vec!["npx supercov runs".into(), shell_quote(run), child.into()];
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
        percentage(summary.lines.percentage),
        percentage(summary.statements.percentage),
        percentage(summary.functions.percentage),
        percentage(summary.branches.percentage),
        percentage(summary.condition_coverage_pct),
    )
}

fn gap_dimensions_total(gap: &supercov_engine::coverage_index::IndexedGapDimensions) -> usize {
    gap.lines + gap.statements + gap.functions + gap.branches + gap.mcdc_conditions
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
                "coverage complete".into()
            } else {
                format!(
                    "missing: lines {}  stmts {}  funcs {}  branches {}  MC/DC {}{}",
                    gap.uncovered_lines,
                    gap.uncovered_statements,
                    gap.uncovered_functions,
                    gap.missing_branches,
                    gap.missing_mcdc_conditions,
                    gap.waived_mcdc_conditions
                        .filter(|count| *count > 0)
                        .map_or_else(String::new, |count| format!(" ({count} waived)")),
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
            format!("{}  {status}{limitations}{provenance}", gap.file)
        })
        .collect::<Vec<_>>()
        .join("\n");
    let mut output = format!("{body}\n{}", page_label(page));
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
            format!(
                "{name}  {} test(s){}  lines {}  branches {}  MC/DC {}",
                entry.tests,
                if entry.setups == 0 {
                    String::new()
                } else {
                    format!(" + {} setup scope(s)", entry.setups)
                },
                percentage(entry.summary.lines.percentage),
                percentage(entry.summary.branches.percentage),
                percentage(entry.summary.condition_coverage_pct),
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
        "default evaluated" => "use the default value".into(),
        "value provided" => "provide an explicit value".into(),
        "no matching case" => "run with no matching switch case".into(),
        "try completed without catch" => "complete the try block without entering catch".into(),
        "catch entered" => "enter the catch block".into(),
        "zero iterations" => "run with zero loop iterations".into(),
        "one or more iterations" => "run with one or more loop iterations".into(),
        "nullish / short-circuited" => "use a nullish value and short-circuit".into(),
        "non-nullish / continued" => "use a non-nullish value and continue".into(),
        "assignment skipped" => "skip the assignment".into(),
        "right evaluated / assigned" => "evaluate and assign the right-hand side".into(),
        "short-circuit / left selected" => "short-circuit and select the left-hand side".into(),
        "right evaluated / selected" => "evaluate and select the right-hand side".into(),
        other => other.into(),
    }
}

fn render_needed_obligation(obligation: &CoverageFileObligation) -> String {
    match obligation {
        CoverageFileObligation::Line(_) => "execute this line".into(),
        CoverageFileObligation::Point(item) if item.kind == "statement" => {
            "execute this statement".into()
        }
        CoverageFileObligation::Point(item) if item.kind == "function" => {
            "call this function".into()
        }
        CoverageFileObligation::Point(item) => format!("cover this {}", item.kind),
        CoverageFileObligation::Branch(item) => branch_need(&item.missing),
        CoverageFileObligation::Mcdc(item) => {
            let mut value = format!(
                "show that `{}` independently changes the decision result",
                item.missing_condition
            );
            if item.waived == Some(true) {
                value.push_str(" (waived)");
            }
            value
        }
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
        "  {} at column {} — {coverage} ({} covering tests)",
        title_case_kind(&anchor.kind),
        anchor.column,
        count(anchor.covering_tests)
    )];
    if let Some(source) = &anchor.source {
        lines.push(format!("    `{source}`"));
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
            if !data.valid {
                first.push_str(" [INVALID: test exit unknown]");
            }
            if data.stale {
                first.push_str(&format!(" [STALE: {}]", data.stale_reasons.join(", ")));
            }
            let measurement = if data.measurement.complete {
                "Complete — no blocking measurement limitations".into()
            } else {
                format!(
                    "Incomplete — {} blocking limitation(s) in {} file(s)",
                    data.measurement.blocking, data.measurement.files
                )
            };
            let mut lines = vec![
                first,
                String::new(),
                "Coverage".into(),
                format!(
                    "  Lines      {} ({}/{})",
                    percentage(data.coverage.lines.percentage),
                    data.coverage.lines.covered,
                    data.coverage.lines.total
                ),
                format!(
                    "  Branches   {} ({}/{})",
                    percentage(data.coverage.branches.percentage),
                    data.coverage.branches.covered,
                    data.coverage.branches.total
                ),
                format!(
                    "  MC/DC      {} ({}/{})",
                    percentage(data.coverage.condition_coverage_pct),
                    data.coverage.covered_conditions,
                    data.coverage.conditions
                ),
                String::new(),
                "Measurement".into(),
                format!("  Status      {measurement}"),
            ];
            if let Some(waivers) = &data.waivers {
                lines.push(format!(
                    "Waivers      {} applied, {} contradicted, {} unmatched; MC/DC excluding waived {} ({}/{})",
                    waivers.applied,
                    waivers.contradicted.len(),
                    waivers.unmatched.len(),
                    percentage(waivers.mcdc_excluding_waived.percentage),
                    waivers.mcdc_excluding_waived.covered,
                    waivers.mcdc_excluding_waived.total,
                ));
                for contradiction in &waivers.contradicted {
                    lines.push(format!(
                        "  contradicted (condition is covered): {}:{} {}",
                        contradiction.file, contradiction.line, contradiction.condition
                    ));
                }
                for waiver in &waivers.unmatched {
                    lines.push(format!(
                        "  unmatched (no such condition): {}{} {}",
                        waiver.file,
                        waiver
                            .line
                            .map_or_else(String::new, |line| format!(":{line}")),
                        waiver.condition
                    ));
                }
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
            if let Some(confidence) = &data.confidence {
                lines.extend([
                    String::new(),
                    "Evidence confidence".into(),
                    format!(
                        "  Linked to passing assertions      {} lines",
                        count(confidence.lines.asserted)
                    ),
                    format!(
                        "  Linked to test actions             {} lines",
                        count(confidence.lines.action)
                    ),
                    format!(
                        "  Execution only                     {} lines",
                        count(confidence.lines.executed)
                    ),
                    format!(
                        "  MC/DC linked to assertions         {} conditions",
                        count(confidence.assertion_covered_mcdc_conditions)
                    ),
                    "  Linkage indicates evidence strength; it does not prove that an assertion is correct.".into(),
                ]);
            }
            if !data.diagnostics.is_empty() {
                lines.extend([String::new(), "Warnings".into()]);
                for diagnostic in &data.diagnostics {
                    lines.extend(diagnostic_lines(diagnostic));
                }
            }
            lines.extend([
                String::new(),
                "Commands".into(),
                format!("  npx supercov runs {} files", data.run),
                format!("  npx supercov runs {} gaps", data.run),
                format!("  npx supercov runs {} kinds", data.run),
                format!("  npx supercov runs {} runners", data.run),
                format!("  npx supercov runs {} scope", data.run),
                format!("  npx supercov runs {} --help", data.run),
            ]);
            lines.join("\n")
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
                    "complete".into()
                } else {
                    format!("{} blocking limitation(s)", data.measurement.blocking)
                }
            ));
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
            if let Some(next) = next_page(&coverage_command(&data.run, request, "scope"), page) {
                lines.push(format!("next page: {next}"));
            }
            lines.join("\n")
        }
        IndexedQueryData::FileDecisions(data) => {
            let page = page.expect("file decisions are paginated");
            let mut lines = vec![
                format!("{}  MC/DC by decision", data.file),
                format!(
                    "decisions {}, with missing conditions {}; conditions missing {}/{}{}",
                    data.totals.decisions,
                    data.totals.decisions_with_missing_conditions,
                    data.totals.missing_conditions,
                    data.totals.conditions,
                    if data.totals.waived_conditions == 0 {
                        String::new()
                    } else {
                        format!(", waived {}", data.totals.waived_conditions)
                    }
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
                    "{}:{}  [{}]  missing {}/{}{}  {snippet}",
                    row.line,
                    row.column,
                    row.id,
                    row.missing_conditions,
                    row.conditions,
                    if row.waived_conditions == 0 {
                        String::new()
                    } else {
                        format!(" ({} waived)", row.waived_conditions)
                    }
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
                format!(
                    "Tests touching this file: {}",
                    count(data.total_tests)
                ),
                String::new(),
                "Gaps".into(),
                "  NOT COVERED = line never executed; PARTIAL = line executed but some behavior remains untested".into(),
                " LINE  STATUS        SOURCE".into(),
            ];
            for gap in &data.gap_lines {
                lines.push(format!(
                    "{:>5}  {:<12}  {}",
                    gap.line,
                    state_label(&gap.state),
                    gap.source.as_deref().unwrap_or("(source unavailable)")
                ));
                let needs = file_gap_needs(&gap.state, &gap.obligations);
                for need in needs {
                    lines.push(format!("       Needed: {need}"));
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
                                "C{} {}{}: {}{}",
                                condition.index + 1,
                                if condition.covered {
                                    "covered"
                                } else if condition.waived == Some(true) {
                                    "MISSING (waived)"
                                } else {
                                    "MISSING"
                                },
                                if condition.assertion_covered == Some(true) {
                                    " + asserted"
                                } else {
                                    ""
                                },
                                condition.source,
                                condition
                                    .waiver_reason
                                    .as_ref()
                                    .map_or_else(String::new, |reason| format!(
                                        "\n   waived: {reason}"
                                    ))
                            )
                        }));
                        lines.push(format!(
                            "confidence {}; asserted MC/DC {}/{}",
                            decision.confidence.level,
                            decision
                                .conditions
                                .iter()
                                .filter(|condition| condition.assertion_covered == Some(true))
                                .count(),
                            decision.conditions.len()
                        ));
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
                    let state = if data.total_anchored == 0
                        && data.total_limitations == 0
                        && data.total_remaining == 0
                    {
                        "NOT MEASURED"
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
                    if !data.remaining.is_empty() {
                        lines.extend([String::new(), "Needed".into()]);
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
                    if !data.remaining.is_empty() {
                        lines.extend([String::new(), "Needed".into()]);
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
                        format!(
                            "{} lines, {} hits, {} decisions, {} phases",
                            test.totals.lines,
                            test.totals.hits,
                            test.totals.decisions,
                            test.totals.phases
                        ),
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
                "npx supercov diff {} {}",
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
                format!(
                    "{:<id_width$}  {:>8}  {:>8}  {:>8}  {}{}",
                    run.id,
                    optional_percentage(run.lines),
                    optional_percentage(run.branches),
                    optional_percentage(run.mcdc),
                    readable_timestamp(&run.generated_at),
                    if run.coverage_error.is_some() {
                        "  INVALID COVERAGE".into()
                    } else if run.stale == Some(true) {
                        format!("  STALE ({})", run.reasons.join(", "))
                    } else {
                        String::new()
                    }
                )
            }));
            lines.push(page_label(pagination));
            let mut base = "npx supercov runs".to_owned();
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

#[cfg(test)]
mod tests {
    use super::*;

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
            stale: None,
            stale_reasons: None,
            offset: 0,
            limit: DEFAULT_LIMIT,
            target: None,
            max_states: None,
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
            Some("inspect file: npx supercov runs 'run_00b780f05c9ae324' file 'app/routes/app.articles.$articleId/route.tsx'".into())
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
    fn counts_are_grouped_for_human_output() {
        assert_eq!(count(0), "0");
        assert_eq!(count(999), "999");
        assert_eq!(count(1_130), "1,130");
        assert_eq!(count(1_000_000), "1,000,000");
    }

    #[test]
    fn missing_test_evidence_is_explained_without_internal_codes() {
        let lines = diagnostic_lines(&CoverageDiagnostic {
            code: "TEST_EVIDENCE_MISSING".into(),
            severity: "warning".into(),
            message: "1 test(s) recorded assertion phases but attributed zero coverage evidence; this is valid for assertions over static or uninstrumented data, but may otherwise indicate missing probe transport. First: safety.spec.ts > checks the VM".into(),
        });
        assert_eq!(
            lines,
            vec![
                "  1 test made assertions, but Supercov received no source-coverage evidence:",
                "    safety.spec.ts > checks the VM",
                "  This is usually normal for environment or static-data checks. Investigate only if the test should execute instrumented application code.",
            ]
        );
        assert!(
            lines
                .iter()
                .all(|line| !line.contains("TEST_EVIDENCE_MISSING"))
        );
    }

    #[test]
    fn branch_obligations_are_described_as_test_behaviors() {
        assert_eq!(branch_need("default evaluated"), "use the default value");
        assert_eq!(branch_need("value provided"), "provide an explicit value");
        assert_eq!(
            branch_need("zero iterations"),
            "run with zero loop iterations"
        );
    }
}
