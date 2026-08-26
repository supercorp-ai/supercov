use supercov_contracts::AgentPagination;
use supercov_engine::{
    coverage_analysis::{CoverageSummary, McdcVector},
    coverage_index::{IndexedDimensionCoverage, IndexedFileGap},
    coverage_query::{
        CoverageCoversData, CoverageDecisionData, CoverageFileObligation, CoverageTestData,
        DecisionSort, MinimizeMetric,
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
        "npx supercov runs".into(),
        shell_quote(run),
        "coverage".into(),
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
                "complete".into()
            } else {
                format!(
                    "incomplete — {} blocking limitation(s) in {} file(s)",
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
                format!("Measurement  {measurement}"),
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
            if !data.diagnostics.is_empty() {
                lines.push(format!(
                    "Diagnostic   {}",
                    data.diagnostics
                        .iter()
                        .map(|item| format!("{}: {}", item.code, item.message))
                        .collect::<Vec<_>>()
                        .join("; ")
                ));
            }
            if let Some(confidence) = &data.confidence {
                lines.push(format!(
                    "Confidence   {} asserted lines, {} action-linked, {} execution-only; {} assertion-linked MC/DC conditions",
                    confidence.lines.asserted,
                    confidence.lines.action,
                    confidence.lines.executed,
                    confidence.assertion_covered_mcdc_conditions
                ));
            }
            let outcomes = [
                ("passed", data.test_outcomes.passed),
                ("failed", data.test_outcomes.failed),
                ("flaky", data.test_outcomes.flaky),
                ("skipped", data.test_outcomes.skipped),
                ("timedOut", data.test_outcomes.timed_out),
                ("interrupted", data.test_outcomes.interrupted),
                ("unknown", data.test_outcomes.unknown),
            ]
            .into_iter()
            .filter(|(_, count)| *count > 0)
            .map(|(outcome, count)| format!("{outcome}={count}"))
            .collect::<Vec<_>>();
            lines.push(format!(
                "Tests        {}{}; outcomes {}; {} file(s) have unresolved coverage or measurement gaps",
                data.tests,
                if data.setups == 0 {
                    String::new()
                } else {
                    format!(" + {} setup scope(s)", data.setups)
                },
                if outcomes.is_empty() {
                    "none".into()
                } else {
                    outcomes.join(", ")
                },
                data.files_with_gaps
            ));
            lines.extend([
                String::new(),
                "Commands".into(),
                format!("  npx supercov runs {} coverage files", data.run),
                format!("  npx supercov runs {} coverage gaps", data.run),
                format!("  npx supercov runs {} coverage kinds", data.run),
                format!("  npx supercov runs {} coverage runners", data.run),
                format!("  npx supercov runs {} coverage scope", data.run),
                format!("  npx supercov runs {} coverage --help", data.run),
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
                "mode {}; roots {}; included {}, excluded {}, ambiguous {}; measurement {}",
                data.mode,
                if data.roots.is_empty() {
                    "none".into()
                } else {
                    data.roots.join(", ")
                },
                data.counts.included,
                data.counts.excluded,
                data.counts.ambiguous,
                if data.measurement.complete {
                    "complete".into()
                } else {
                    format!("{} blocking limitation(s)", data.measurement.blocking)
                }
            )];
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
                format!(
                    "lines {}, statements {}, functions {}, branches {}, MC/DC {}, measurement limitations {}",
                    data.counts.uncovered_lines,
                    data.counts.uncovered_statements,
                    data.counts.uncovered_functions,
                    data.counts.missing_branches,
                    data.counts.missing_mcdc_conditions,
                    data.counts.measurement_limitations,
                ),
                format!("covered by {} test(s)", data.total_tests),
            ];
            let mut obligations = Vec::new();
            for obligation in &data.obligations {
                let text = match obligation {
                    CoverageFileObligation::Line(item) => format!(
                        "line {}: {}",
                        item.line,
                        if item.other_coverage.covered_elsewhere {
                            format!(
                                "covered only by {}/{}",
                                item.other_coverage.kinds.join(", "),
                                item.other_coverage.runners.join(", ")
                            )
                        } else {
                            "uncovered everywhere".into()
                        }
                    ),
                    CoverageFileObligation::Point(item) => format!(
                        "{} {}:{}: {}{}",
                        item.kind,
                        item.line,
                        item.column,
                        item.source,
                        if item.other_coverage.covered_elsewhere {
                            format!(
                                " [covered only by {}/{}]",
                                item.other_coverage.kinds.join(", "),
                                item.other_coverage.runners.join(", ")
                            )
                        } else {
                            String::new()
                        }
                    ),
                    CoverageFileObligation::Branch(item) => format!(
                        "branch {}:{}: missing {}{}",
                        item.line,
                        item.column,
                        item.missing,
                        if item.other_coverage.covered_elsewhere {
                            format!(
                                " [covered only by {}/{}]",
                                item.other_coverage.kinds.join(", "),
                                item.other_coverage.runners.join(", ")
                            )
                        } else {
                            String::new()
                        }
                    ),
                    CoverageFileObligation::Mcdc(item) => format!(
                        "MC/DC {}:{} [{}]: {}{}{}",
                        item.line,
                        item.column,
                        item.id,
                        item.missing_condition,
                        if item.waived == Some(true) {
                            " [waived]"
                        } else {
                            ""
                        },
                        if item.other_coverage.covered_elsewhere {
                            format!(
                                " [covered only by {}/{}]",
                                item.other_coverage.kinds.join(", "),
                                item.other_coverage.runners.join(", ")
                            )
                        } else {
                            String::new()
                        }
                    ),
                };
                obligations.push(text);
            }
            if !obligations.is_empty() {
                lines.push(obligations.join("\n"));
            } else if data.limitations.is_empty() {
                lines.push(String::new());
            }
            for limitation in &data.limitations {
                lines.push(format!(
                    "LIMITATION {} {}:{} [{}]\n  {}\n  source: {}\n  effect: outside measured denominator",
                    limitation.kind,
                    limitation.line,
                    limitation.column,
                    limitation.id,
                    limitation.reason,
                    limitation.source
                ));
            }
            lines.push(format!(
                "{} obligations/tests/limitations per category",
                page_label(page)
            ));
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
        IndexedQueryData::Covers(data) => {
            let page = page.expect("covers is paginated");
            match data.as_ref() {
                CoverageCoversData::Anchors(data) => {
                    let mut lines = vec![format!(
                        "{}:{} has no line obligation{}",
                        data.location.file,
                        data.location.line,
                        if data.total_anchored == 0 {
                            "; nothing is measured at this exact line".into()
                        } else {
                            format!("; {} obligation(s) anchor here", data.total_anchored)
                        }
                    )];
                    lines.extend(data.anchored.iter().map(|anchor| {
                        format!(
                            "{} {}:{} [{}] {}{}",
                            anchor.kind,
                            data.location.line,
                            anchor.column,
                            anchor.id,
                            if anchor.covered {
                                "covered"
                            } else {
                                "not fully covered"
                            },
                            anchor
                                .conditions
                                .map_or_else(String::new, |conditions| format!(
                                    " ({}/{conditions} conditions)",
                                    anchor.covered_conditions.unwrap_or(0)
                                ))
                        )
                    }));
                    lines.push(format!("{} anchored obligations", page_label(page)));
                    lines.join("\n")
                }
                CoverageCoversData::Line(data) => {
                    let mut lines = vec![format!(
                        "{}:{} {}; confidence {}{}",
                        data.location.file,
                        data.location.line,
                        if data.covered { "covered" } else { "uncovered" },
                        data.confidence.level,
                        if data.confidence.e2e {
                            "; E2E-covered"
                        } else {
                            ""
                        }
                    )];
                    if data.tests.is_empty() {
                        lines.push("no covering tests".into());
                    } else {
                        lines.extend(data.tests.iter().map(|test| {
                            format!(
                                "test: {} [{}] ({}/{})",
                                test.name, test.id, test.provenance.kind, test.provenance.runner
                            )
                        }));
                    }
                    lines.extend(data.phases.iter().map(|phase| {
                        format!(
                            "phase: {}{}{}",
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
                    lines.push(format!("{} tests/phases", page_label(page)));
                    let selector = format!("{}:{}", data.location.file, data.location.line);
                    let base = format!(
                        "{} {}",
                        coverage_command(&data.run, request, "covers"),
                        shell_quote(&selector)
                    );
                    if let Some(next) = next_page(&base, page) {
                        lines.push(format!("next page: {next}"));
                    }
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
