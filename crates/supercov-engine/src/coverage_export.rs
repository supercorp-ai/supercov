//! LCOV and Cobertura exports, written from the shared run view.
//!
//! These exist for compatibility, not as a headline: they let Supercov feed
//! the viewers, hosted services and CI integrations a team already uses. Both
//! read the same view the CLI and the gates read, so a consumer's totals match
//! what `check` enforced.
//!
//! Two things are deliberately not done. Supercov records whether a line ran,
//! not how many times, so an export states `1` or `0` and never invents an
//! execution frequency a consumer would display as fact. And MC/DC conditions
//! are not flattened into ordinary branches to fit a simpler model: a consumer
//! would then report condition obligations as branch coverage, which is a
//! different measurement. LCOV has records of its own for them, which are
//! written; Cobertura has none, and the evidence stays in the JSON view.

use std::fmt::Write as _;

use crate::run_view::{ConditionRecord, Metric, RunView};

/// Escape text for XML character data and attribute values.
fn xml(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            // XML 1.0 cannot carry these at all; dropping them keeps the
            // document parseable rather than emitting bytes no reader accepts.
            c if (c < ' ' && c != '\t' && c != '\n' && c != '\r') => {}
            c => out.push(c),
        }
    }
    out
}

fn rate(covered: usize, total: usize) -> f64 {
    if total == 0 {
        // Cobertura has no "not applicable"; 1.0 is the convention for an
        // empty denominator and is what every reader expects.
        return 1.0;
    }
    covered as f64 / total as f64
}

fn counts(view: &RunView, metric: Metric) -> (usize, usize) {
    let metric = view.metric(metric);
    (metric.covered, metric.eligible)
}

/// The export formats, named once so the rest of the product never has to
/// spell them. Supercov emits these formats; it never invokes the tools they
/// are named after, and the packaging audit that enforces that reads this
/// module as the single place allowed to name them.
pub const FORMATS: &str = "lcov|cobertura|html";

/// Render a run in the named format.
pub fn export(view: &RunView, format: &str, timestamp: u64) -> Result<String, String> {
    match format {
        "lcov" => Ok(lcov(view)),
        "cobertura" => Ok(cobertura(view, timestamp)),
        // `html` is rendered by `coverage_html`, which also needs verified
        // source text; the CLI routes it before reaching here.
        other => Err(format!(
            "unknown format {other}; Supercov exports {FORMATS}"
        )),
    }
}

/// A file's conditions as LCOV 2's `MCDC` records, and how many of them it
/// counts found and hit.
///
/// LCOV asks of each condition whether it was shown to turn the outcome when
/// it went from false to true, and from true to false, and counts each as
/// one. Supercov counts a condition covered when a pair of evaluations shows
/// it turning the outcome, which is both at once: a covered condition is two
/// hit, one not covered is none, and hit over found is Supercov's own MC/DC.
///
/// A record names its condition by line, the size of its group and its place
/// in the group, so two decisions of one size on one line would be one
/// group to a reader. Where a line holds such decisions, all its conditions
/// are written as one group; otherwise a decision is a group.
fn mcdc_records(conditions: &[ConditionRecord], out: &mut String) -> (usize, usize) {
    let (mut found, mut hit) = (0, 0);
    for line in conditions.chunk_by(|left, right| left.line == right.line) {
        let decisions = line
            .chunk_by(|left, right| left.column == right.column)
            .collect::<Vec<_>>();
        let mut sizes = decisions.iter().map(|each| each.len()).collect::<Vec<_>>();
        sizes.sort_unstable();
        let apart = sizes.windows(2).all(|pair| pair[0] != pair[1]);
        let mut groups = if apart { decisions } else { vec![line] };
        groups.sort_by_key(|group| group.len());
        // A line chunk holds a condition, so each group does.
        groups.into_iter().for_each(|group| {
            group.iter().enumerate().for_each(|(index, condition)| {
                let named = condition
                    .source
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ");
                let named = if named.is_empty() {
                    format!("condition {index}")
                } else {
                    named
                };
                let shown = u8::from(condition.covered);
                found += 2;
                hit += 2 * usize::from(shown);
                for sense in ['t', 'f'] {
                    let _ = writeln!(
                        out,
                        "MCDC:{},{},{sense},{shown},{index},{named}",
                        condition.line,
                        group.len()
                    );
                }
            });
        });
    }
    (found, hit)
}

/// An LCOV tracefile: line, function and branch records, and LCOV 2's
/// `MCDC` records, which a reader of an older format passes over.
pub fn lcov(view: &RunView) -> String {
    let mut out = String::new();
    for file in &view.files {
        let _ = writeln!(out, "TN:");
        let _ = writeln!(out, "SF:{}", file.file);
        for function in &file.functions {
            let _ = writeln!(out, "FN:{},{}", function.line, function.name);
        }
        for function in &file.functions {
            // Hit or not hit. Supercov does not count invocations, and writing
            // a made-up frequency here would be displayed as one.
            let _ = writeln!(out, "FNDA:{},{}", u8::from(function.covered), function.name);
        }
        let (covered_functions, functions) = file
            .metric(Metric::Functions)
            .map(|m| (m.covered, m.eligible))
            .unwrap_or((0, 0));
        let _ = writeln!(out, "FNF:{functions}");
        let _ = writeln!(out, "FNH:{covered_functions}");
        for branch in &file.branches {
            let _ = writeln!(
                out,
                "BRDA:{},{},{},{}",
                branch.line,
                branch.block,
                branch.index,
                if branch.taken { "1" } else { "-" }
            );
        }
        let (covered_branches, branches) = file
            .metric(Metric::Branches)
            .map(|m| (m.covered, m.eligible))
            .unwrap_or((0, 0));
        let _ = writeln!(out, "BRF:{branches}");
        let _ = writeln!(out, "BRH:{covered_branches}");
        let (conditions, shown) = mcdc_records(&file.conditions, &mut out);
        if conditions > 0 {
            let _ = writeln!(out, "MCF:{conditions}");
            let _ = writeln!(out, "MCH:{shown}");
        }
        for (line, covered) in file.line_hits() {
            let _ = writeln!(out, "DA:{line},{}", u8::from(covered));
        }
        let (covered_lines, lines) = file
            .metric(Metric::Lines)
            .map(|m| (m.covered, m.eligible))
            .unwrap_or((0, 0));
        let _ = writeln!(out, "LF:{lines}");
        let _ = writeln!(out, "LH:{covered_lines}");
        let _ = writeln!(out, "end_of_record");
    }
    out
}

/// A Cobertura XML report.
pub fn cobertura(view: &RunView, timestamp: u64) -> String {
    let (covered_lines, lines) = counts(view, Metric::Lines);
    let (covered_branches, branches) = counts(view, Metric::Branches);
    let mut out = String::new();
    out.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    let _ = writeln!(
        out,
        "<coverage line-rate=\"{:.4}\" branch-rate=\"{:.4}\" lines-covered=\"{covered_lines}\" lines-valid=\"{lines}\" branches-covered=\"{covered_branches}\" branches-valid=\"{branches}\" complexity=\"0\" version=\"{}\" timestamp=\"{timestamp}\">",
        rate(covered_lines, lines),
        rate(covered_branches, branches),
        xml(env!("CARGO_PKG_VERSION")),
    );
    // A single relative source root keeps filenames resolvable by readers that
    // join them onto a checkout, and leaks no absolute path from this machine.
    out.push_str("  <sources>\n    <source>.</source>\n  </sources>\n  <packages>\n");
    let _ = writeln!(
        out,
        "    <package name=\"\" line-rate=\"{:.4}\" branch-rate=\"{:.4}\" complexity=\"0\">",
        rate(covered_lines, lines),
        rate(covered_branches, branches),
    );
    out.push_str("      <classes>\n");
    for file in &view.files {
        let (file_covered, file_lines) = file
            .metric(Metric::Lines)
            .map(|m| (m.covered, m.eligible))
            .unwrap_or((0, 0));
        let (branch_covered, branch_total) = file
            .metric(Metric::Branches)
            .map(|m| (m.covered, m.eligible))
            .unwrap_or((0, 0));
        let _ = writeln!(
            out,
            "        <class name=\"{}\" filename=\"{}\" line-rate=\"{:.4}\" branch-rate=\"{:.4}\" complexity=\"0\">",
            xml(&file.file),
            xml(&file.file),
            rate(file_covered, file_lines),
            rate(branch_covered, branch_total),
        );
        out.push_str("          <methods/>\n          <lines>\n");
        for (line, covered) in file.line_hits() {
            let alternatives = file
                .branches
                .iter()
                .filter(|branch| branch.line == line)
                .collect::<Vec<_>>();
            if alternatives.is_empty() {
                let _ = writeln!(
                    out,
                    "            <line number=\"{line}\" hits=\"{}\"/>",
                    u8::from(covered)
                );
                continue;
            }
            let taken = alternatives.iter().filter(|branch| branch.taken).count();
            let total = alternatives.len();
            let _ = writeln!(
                out,
                "            <line number=\"{line}\" hits=\"{}\" branch=\"true\" condition-coverage=\"{}% ({taken}/{total})\"/>",
                u8::from(covered),
                (rate(taken, total) * 100.0).round() as u64,
            );
        }
        out.push_str("          </lines>\n        </class>\n");
    }
    out.push_str("      </classes>\n    </package>\n  </packages>\n</coverage>\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::run_view::{
        Applicability, BranchRecord, FileView, FunctionRecord, MetricView, RUN_VIEW_SCHEMA_VERSION,
        RunView,
    };
    use std::collections::BTreeSet;

    #[test]
    fn xml_escapes_what_markup_reads_and_drops_what_it_cannot_carry() {
        assert_eq!(xml(""), "");
        assert_eq!(
            xml("a & b < c > d \" e ' f\u{1}\tg"),
            "a &amp; b &lt; c &gt; d &quot; e &apos; f\tg"
        );
    }

    /// A run with no file exports a report of nothing in each format.
    #[test]
    fn a_view_of_no_file_exports_nothing_measured() {
        let mut view = fixture();
        view.files.clear();
        assert_eq!(lcov(&view), "");
        assert!(cobertura(&view, 0).contains("<packages>"));
    }

    fn metric(metric: Metric, covered: usize, eligible: usize) -> MetricView {
        MetricView {
            metric,
            covered,
            eligible,
            applicability: if eligible == 0 {
                Applicability::NotApplicable
            } else {
                Applicability::Measured
            },
        }
    }

    fn fixture() -> RunView {
        RunView {
            schema_version: RUN_VIEW_SCHEMA_VERSION,
            run: "run_1".into(),
            generated_at: "now".into(),
            suite_passed: true,
            stale: false,
            stale_reasons: Vec::new(),
            complete: true,
            limitations: Vec::new(),
            totals: vec![
                metric(Metric::Lines, 3, 4),
                metric(Metric::Branches, 1, 2),
                metric(Metric::Functions, 1, 1),
            ],
            files: vec![FileView {
                file: "src/a & b.ts".into(),
                metrics: vec![
                    metric(Metric::Lines, 3, 4),
                    metric(Metric::Branches, 1, 2),
                    metric(Metric::Functions, 1, 1),
                ],
                measured_lines: vec![1, 2, 3, 4],
                uncovered_lines: vec![4],
                missing_branches: Vec::new(),
                missing_conditions: Vec::new(),
                functions: vec![FunctionRecord {
                    line: 1,
                    name: "run".into(),
                    covered: true,
                }],
                branches: vec![
                    BranchRecord {
                        line: 2,
                        block: 0,
                        index: 0,
                        taken: true,
                    },
                    BranchRecord {
                        line: 2,
                        block: 0,
                        index: 1,
                        taken: false,
                    },
                ],
                // Line 2 holds `a && b` with `a` shown and `b` not; line 3 two
                // decisions of two conditions each, which a reader would take
                // for one group.
                conditions: [
                    (2, 4, 0, true, "a"),
                    (2, 4, 1, false, "b ==\n  1"),
                    (3, 2, 0, true, "c"),
                    (3, 2, 1, true, "d"),
                    (3, 20, 0, false, "e"),
                    (3, 20, 1, true, "f(1, 2)"),
                ]
                .into_iter()
                .map(|(line, column, index, covered, source)| ConditionRecord {
                    line,
                    column,
                    index,
                    covered,
                    source: source.into(),
                })
                .collect(),
            }],
            source_neighbourhoods: BTreeSet::new(),
        }
    }

    #[test]
    fn an_lcov_tracefile_states_hit_or_not_hit_and_never_a_frequency() {
        // Supercov records that a line ran, not how often. A consumer displays
        // `DA:` counts as execution frequencies, so writing anything above 1
        // would publish a number no measurement supports.
        let text = lcov(&fixture());
        assert!(text.contains("SF:src/a & b.ts\n"), "{text}");
        assert!(
            text.contains("DA:1,1\n") && text.contains("DA:4,0\n"),
            "{text}"
        );
        assert!(
            text.lines().all(|line| !line.starts_with("DA:")
                || line.ends_with(",1")
                || line.ends_with(",0"))
        );
        assert!(text.contains("FNDA:1,run\n"), "{text}");
        // An untaken branch is `-`, which readers distinguish from zero.
        assert!(
            text.contains("BRDA:2,0,0,1\n") && text.contains("BRDA:2,0,1,-\n"),
            "{text}"
        );
        assert!(text.contains("LF:4\nLH:3\n"), "{text}");
        assert!(text.contains("BRF:2\nBRH:1\n"), "{text}");
        assert!(text.ends_with("end_of_record\n"));
    }

    #[test]
    fn lcov_names_each_condition_and_counts_as_supercov_counts() {
        let text = lcov(&fixture());
        // A decision is a group: its line, its size, then each condition
        // shown turning the outcome both ways or neither.
        assert!(
            text.contains(
                "MCDC:2,2,t,1,0,a\nMCDC:2,2,f,1,0,a\nMCDC:2,2,t,0,1,b == 1\nMCDC:2,2,f,0,1,b == 1\n"
            ),
            "{text}"
        );
        // Two decisions of one size on a line are one group of four, so that
        // no two conditions answer to one name.
        assert!(
            text.contains(
                "MCDC:3,4,t,1,0,c\nMCDC:3,4,f,1,0,c\nMCDC:3,4,t,1,1,d\nMCDC:3,4,f,1,1,d\nMCDC:3,4,t,0,2,e\nMCDC:3,4,f,0,2,e\nMCDC:3,4,t,1,3,f(1, 2)\nMCDC:3,4,f,1,3,f(1, 2)\n"
            ),
            "{text}"
        );
        // Four of six conditions shown is eight of twelve, the same share.
        assert!(text.contains("MCF:12\nMCH:8\n"), "{text}");
        // Every record is one LCOV 2 reads: its own pattern for the line.
        for record in text.lines().filter(|line| line.starts_with("MCDC:")) {
            let fields = record["MCDC:".len()..].splitn(6, ',').collect::<Vec<_>>();
            assert_eq!(fields.len(), 6, "{record}");
            assert!(fields[0].parse::<usize>().is_ok_and(|line| line > 0));
            assert!(fields[1].parse::<usize>().is_ok());
            assert!(matches!(fields[2], "t" | "f"));
            assert!(fields[3].parse::<usize>().is_ok());
            assert!(fields[4].parse::<usize>().unwrap() < fields[1].parse::<usize>().unwrap());
            assert!(!fields[5].is_empty() && !fields[5].contains('\n'));
        }
    }

    #[test]
    fn lcov_records_keep_the_order_readers_parse_them_in() {
        // geninfo defines the sequence, and readers rely on it: a tracefile
        // whose counters precede the records they summarise is accepted by
        // some parsers and silently mis-read by others.
        let text = lcov(&fixture());
        let rank = |line: &str| match line.split(':').next().unwrap_or("") {
            "TN" => 0,
            "SF" => 1,
            "FN" => 2,
            "FNDA" => 3,
            "FNF" => 4,
            "FNH" => 5,
            "BRDA" => 6,
            "BRF" => 7,
            "BRH" => 8,
            "MCDC" => 9,
            "MCF" => 10,
            "MCH" => 11,
            "DA" => 12,
            "LF" => 13,
            "LH" => 14,
            _ => 15,
        };
        for record in text
            .split("end_of_record\n")
            .filter(|r| !r.trim().is_empty())
        {
            let ranks = record
                .trim()
                .lines()
                .map(rank)
                .filter(|rank| *rank < 12)
                .collect::<Vec<_>>();
            let mut sorted = ranks.clone();
            sorted.sort_unstable();
            assert_eq!(ranks, sorted, "out of order:\n{record}");
        }
    }

    #[test]
    fn totals_in_an_export_match_the_run_view_they_came_from() {
        // The whole point of exporting from the shared view: a consumer's
        // percentage has to be the one the gate enforced.
        let view = fixture();
        let text = lcov(&view);
        let found: usize = text
            .lines()
            .filter_map(|line| line.strip_prefix("LF:")?.parse::<usize>().ok())
            .sum();
        let hit: usize = text
            .lines()
            .filter_map(|line| line.strip_prefix("LH:")?.parse::<usize>().ok())
            .sum();
        assert_eq!((hit, found), counts(&view, Metric::Lines));

        let xml = cobertura(&view, 0);
        assert!(
            xml.contains("lines-covered=\"3\" lines-valid=\"4\""),
            "{xml}"
        );
        assert!(
            xml.contains("branches-covered=\"1\" branches-valid=\"2\""),
            "{xml}"
        );
        assert!(xml.contains("line-rate=\"0.7500\""), "{xml}");
    }

    #[test]
    fn cobertura_escapes_markup_and_keeps_paths_relative() {
        // A filename carrying `&` or `<` would otherwise produce a document no
        // reader can parse, and an absolute path would leak this machine's
        // layout into a shared artifact.
        let xml_text = cobertura(&fixture(), 1_700_000_000);
        assert!(
            xml_text.contains("filename=\"src/a &amp; b.ts\""),
            "{xml_text}"
        );
        assert!(!xml_text.contains(" & "), "raw ampersand survived");
        assert!(xml_text.contains("<source>.</source>"));
        assert!(xml_text.starts_with("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n"));
        // A branching line carries its condition detail; a plain line does not.
        assert!(
            xml_text.contains(
                "<line number=\"2\" hits=\"1\" branch=\"true\" condition-coverage=\"50% (1/2)\"/>"
            ),
            "{xml_text}"
        );
        assert!(
            xml_text.contains("<line number=\"1\" hits=\"1\"/>"),
            "{xml_text}"
        );
        assert_eq!(
            xml("a\u{0}b"),
            "ab",
            "unencodable control bytes are dropped"
        );
    }

    #[test]
    fn xml_keeps_the_whitespace_controls_xml_allows() {
        // Tab, line feed and carriage return are legal XML characters; only
        // the other control characters are unencodable.
        assert_eq!(xml("a\tb\nc\rd\u{1}e"), "a\tb\nc\rde");
    }

    #[test]
    fn a_condition_without_source_is_named_by_its_place_in_the_group() {
        let conditions = [ConditionRecord {
            line: 7,
            column: 1,
            index: 0,
            covered: false,
            source: " \n ".into(),
        }];
        let mut out = String::new();
        assert_eq!(mcdc_records(&conditions, &mut out), (2, 0));
        assert_eq!(
            out,
            "MCDC:7,1,t,0,0,condition 0\nMCDC:7,1,f,0,0,condition 0\n"
        );
    }

    /// A file Supercov knows of but measured nothing in still has its record,
    /// its counters at zero.
    #[test]
    fn a_file_with_nothing_measured_exports_an_empty_record() {
        let mut view = fixture();
        let file = &mut view.files[0];
        file.metrics.clear();
        file.measured_lines.clear();
        file.uncovered_lines.clear();
        file.functions.clear();
        file.branches.clear();
        file.conditions.clear();
        assert_eq!(
            lcov(&view),
            "TN:\nSF:src/a & b.ts\nFNF:0\nFNH:0\nBRF:0\nBRH:0\nLF:0\nLH:0\nend_of_record\n"
        );
        let xml_text = cobertura(&view, 0);
        assert!(
            xml_text.contains("<methods/>\n          <lines>\n          </lines>"),
            "{xml_text}"
        );
    }

    #[test]
    fn an_empty_denominator_does_not_become_a_zero_rate() {
        // Cobertura has no "not applicable", and reporting 0% for a file with
        // no branches would look like a regression that never happened.
        assert_eq!(rate(0, 0), 1.0);
        let mut view = fixture();
        view.files[0].branches.clear();
        view.files[0].metrics = vec![metric(Metric::Lines, 3, 4), metric(Metric::Branches, 0, 0)];
        let xml_text = cobertura(&view, 0);
        assert!(xml_text.contains("branch-rate=\"1.0000\""), "{xml_text}");
    }
}
