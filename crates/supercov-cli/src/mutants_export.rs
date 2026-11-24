//! `supercov runs <run-id> mutants --format jsonl`: every mutant asked
//! about, one JSON object per line, each as `mutants --json` lists it. A
//! reply holds a page, about 128 mutants under its 64 KB limit, and a large
//! app's run has tens of thousands.
//!
//! `--format markdown`: the mutants the tests leave alive as a table for a
//! pull request's comment, `--diff` the change's, capped at
//! `MARKDOWN_ROWS` with the command that lists the rest. Once a strong pass
//! ran, the mutation score and the verdicts too, and the survivors.
//!
//! `--format stryker`: StrykerJS's `mutation.json` (the mutation testing
//! report schema), and `--format pit`: PIT's `mutations.xml`, for the tools
//! that read those (`mutation_report`).

use std::{io::Write, path::PathBuf, process::ExitCode};

/// Everything one page holds: the export asks for all of it at once.
const ALL: &str = "1000000000";

pub fn command(arguments: &[String]) -> ExitCode {
    let mut passed = Vec::new();
    let mut output = None;
    let mut format = None;
    // Dispatch sends `<run> mutants ... --format ...` here: never nothing.
    let mut index = 0;
    loop {
        match arguments[index].as_str() {
            "--format" => {
                index += 1;
                format = arguments.get(index).cloned();
            }
            "--output" => {
                index += 1;
                output = arguments.get(index).map(PathBuf::from);
            }
            // Pages and the JSON envelope are the export's to choose.
            "--offset" | "--limit" => index += 1,
            "--json" => {}
            other => passed.push(other.to_owned()),
        }
        index += 1;
        if index >= arguments.len() {
            break;
        }
    }
    let format = match format.as_deref() {
        Some(format @ ("jsonl" | "markdown" | "stryker" | "pit")) => format.to_owned(),
        _ => {
            eprintln!("[supercov] mutants --format takes jsonl, markdown, stryker or pit");
            return ExitCode::from(2);
        }
    };
    let markdown = format == "markdown";
    // Dispatch sends `<run> mutants ...` here.
    let run = passed[0].clone();
    let mut mutants_arguments = passed[2..].to_vec();
    mutants_arguments.extend(["--json", "--limit", ALL].map(str::to_owned));
    let (request, _) =
        match super::public_query::coverage_request(run, "mutants", &mutants_arguments, true) {
            Ok(request) => request,
            Err(error) => {
                eprintln!("[supercov] {}", error.error.message);
                return ExitCode::from(2);
            }
        };
    let root = match std::env::current_dir() {
        Ok(root) => root,
        Err(error) => {
            eprintln!("[supercov] could not resolve the current directory: {error}");
            return ExitCode::from(2);
        }
    };
    // The query's data itself, not its reply, which stops at 64 KB.
    let (output_data, _) = match super::execute_coverage_query(&root, &request, None) {
        Ok(output) => output,
        Err(failure) => {
            eprintln!("[supercov] {}", failure.error.message);
            return ExitCode::from(2);
        }
    };
    let data = output_data
        .data
        .mutants()
        .expect("a mutants request is answered with mutants");
    if let Some(note) = &data.note {
        eprintln!("[supercov] {note}");
    }
    let mut lines = String::new();
    if markdown {
        lines = self::markdown(&request, data);
    } else if format == "stryker" {
        lines = stryker(&root, data);
    } else if format == "pit" {
        lines = supercov_engine::mutation_report::pit_xml(data);
    } else {
        for mutant in &data.mutants {
            lines.push_str(&serde_json::to_string(mutant).expect("a mutant is plain data"));
            lines.push('\n');
        }
    }
    let written = match output {
        Some(path) => {
            if let Err(error) = supercov_engine::fault_fs::write(&path, &lines) {
                eprintln!("[supercov] {}: {error}", path.display());
                return ExitCode::from(2);
            }
            if markdown {
                eprintln!("[supercov] wrote the mutants' table to {}", path.display());
            } else if format != "jsonl" {
                eprintln!(
                    "[supercov] wrote {} mutant(s) as a {format} report to {}",
                    data.mutants.len(),
                    path.display()
                );
            } else {
                eprintln!(
                    "[supercov] wrote {} mutant(s) to {}",
                    data.mutants.len(),
                    path.display()
                );
            }
            ExitCode::SUCCESS
        }
        None => printed(&mut std::io::stdout().lock(), &lines),
    };
    if written != ExitCode::SUCCESS {
        return written;
    }
    // As the query: a mutant said to be unkillable that StrykerJS killed.
    if let Some(stryker) = &data.stryker
        && stryker.soundness_violations.total > 0
    {
        eprintln!(
            "[supercov] {} soundness violation(s): mutants not infected or not reached here that StrykerJS killed",
            stryker.soundness_violations.total
        );
        return ExitCode::from(1);
    }
    if let Some(gate) = crate::human_query::gate_failure(data) {
        eprintln!("[supercov] {gate}");
        return ExitCode::from(1);
    }
    ExitCode::SUCCESS
}

/// The most mutants a markdown table lists.
const MARKDOWN_ROWS: usize = 25;

/// Text for a markdown table's cell: one line, its pipes escaped.
fn cell(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .replace('|', "\\|")
}

/// Code as a markdown code span, whatever backticks it holds.
fn code(text: &str) -> String {
    let longest = text
        .split(|character| character != '`')
        .map(str::len)
        .max()
        .unwrap_or(0);
    let fence = "`".repeat(longest + 1);
    let padded = if text.starts_with('`') || text.ends_with('`') {
        format!(" {text} ")
    } else {
        text.to_owned()
    };
    cell(&format!("{fence}{padded}{fence}"))
}

/// The mutants as StrykerJS's `mutation.json`, each file with its source as
/// the checkout holds it; a file it cannot read is left out and said so.
fn stryker(
    root: &std::path::Path,
    data: &supercov_engine::mutants_query::CoverageMutantsData,
) -> String {
    let mut sources = std::collections::BTreeMap::new();
    for mutant in &data.mutants {
        if !sources.contains_key(&mutant.file)
            && let Ok(text) = supercov_engine::fault_fs::read_to_string(root.join(&mutant.file))
        {
            sources.insert(mutant.file.clone(), text);
        }
    }
    let (report, missing) = supercov_engine::mutation_report::stryker_report(data, &sources);
    for file in missing {
        eprintln!("[supercov] {file} could not be read: its mutants are left out of the report");
    }
    let mut text = serde_json::to_string_pretty(&report).expect("a report is plain data");
    text.push('\n');
    text
}

/// The mutants the tests leave alive, or those `--status` asked for, as a
/// pull request's comment: one line of counts and a table. Once a strong
/// pass gave verdicts, the line has the mutation score and the verdicts, and
/// the table the survivors with their verdicts.
pub(crate) fn markdown(
    request: &supercov_engine::indexed_query::IndexedQueryRequest,
    data: &supercov_engine::mutants_query::CoverageMutantsData,
) -> String {
    use supercov_engine::coverage_index::MutantStatus;
    use supercov_engine::mutation_report::{self as report, Outcome};
    let summary = &data.summary;
    let strong = data.mutants.iter().any(|mutant| mutant.strong.is_some());
    // `--status survived` and the other verdicts pick by strong verdict.
    let asked_verdicts = &request.mutants.strong_statuses;
    let asked_statuses = !request.mutants.statuses.is_empty() || !asked_verdicts.is_empty();
    let wanted = if asked_statuses {
        request.mutants.statuses.clone()
    } else {
        vec![MutantStatus::NotInfected, MutantStatus::NotReached]
    };
    let mut rows = data
        .mutants
        .iter()
        .filter(|mutant| {
            if !asked_verdicts.is_empty() {
                report::verdict(mutant)
                    .is_some_and(|verdict| asked_verdicts.contains(&verdict.status))
            } else if strong && !asked_statuses {
                report::alive(mutant)
            } else {
                wanted.contains(&mutant.status)
            }
        })
        .collect::<Vec<_>>();
    // What the tests leave alive first, as `--diff` lists them: once a
    // strong pass ran, what survived it first.
    rows.sort_by_key(|mutant| {
        let verdict = match report::outcome(mutant) {
            Outcome::Survived => 0,
            Outcome::NoCoverage => 1,
            _ => 2,
        };
        (
            verdict,
            wanted.iter().position(|status| *status == mutant.status),
        )
    });
    let score = summary
        .weak_score
        .map_or_else(|| "—".to_owned(), |score| format!("{:.1}%", score * 100.0));
    let mut counts = MutantStatus::ALL
        .into_iter()
        .filter(|status| summary.status.of(*status) > 0)
        .map(|status| {
            format!(
                "{} {}",
                summary.status.of(status),
                status.name().replace('-', " ")
            )
        })
        .collect::<Vec<_>>()
        .join(" · ");
    if strong {
        let tally = report::Tally::of(&data.mutants);
        let verdicts = [
            (tally.strong.killed, "killed"),
            (tally.strong.timeout, "timed out"),
            (tally.strong.survived, "survived"),
            (tally.strong.no_coverage, "no coverage"),
            (tally.strong.unviable, "unviable"),
            (tally.strong.error, "error"),
            (tally.strong.not_run, "not run (no driver)"),
            (tally.strong.pending, "no strong verdict"),
        ]
        .into_iter()
        .filter(|(count, _)| *count > 0)
        .map(|(count, name)| format!("{count} {name}"))
        .collect::<Vec<_>>()
        .join(" · ");
        // The score with what it is over: a score of a few mutants is not
        // the run's, and no score says why.
        let strong = &tally.strong;
        let basis = strong.killed + strong.timeout + strong.survived + strong.no_coverage;
        let mutation = match tally.mutation_score {
            Some(score) => format!(
                "Mutation score **{:.1}%** (of {basis} of {})",
                score * 100.0,
                plural(tally.mutants, "mutant")
            ),
            None => {
                "No mutation score: no mutant was killed, timed out, survived or left uncovered"
                    .to_owned()
            }
        };
        counts = format!("{counts}\n\n{mutation} · {verdicts}");
    }
    let mut text = String::new();
    match &data.diff {
        Some(diff) => {
            text.push_str("### Mutants on this change\n\n");
            text.push_str(&format!(
                "**{}** on {} of {} changed lines since `{}` (from `{}`): weak score **{score}**{}{counts}\n",
                plural(summary.mutants, "mutant"),
                diff.lines_with_mutants,
                diff.changed_lines,
                diff.base,
                diff.merge_base.chars().take(12).collect::<String>(),
                if counts.is_empty() { "" } else { " · " },
            ));
        }
        None => {
            text.push_str("### Mutants\n\n");
            text.push_str(&format!(
                "**{}**: weak score **{score}**{}{counts}\n",
                plural(summary.mutants, "mutant"),
                if counts.is_empty() { "" } else { " · " },
            ));
        }
    }
    let names = if !asked_verdicts.is_empty() {
        asked_verdicts
            .iter()
            .map(|status| status.replace('-', " "))
            .collect::<Vec<_>>()
            .join(" or ")
    } else if strong && !asked_statuses {
        "left alive by the tests".to_owned()
    } else {
        wanted
            .iter()
            .map(|status| status.name().replace('-', " "))
            .collect::<Vec<_>>()
            .join(" or ")
    };
    if rows.is_empty() {
        // Nothing the tests leave alive, in the words a comment's reader uses.
        let said = if asked_statuses || strong {
            format!("No mutant is {names}.")
        } else {
            "No mutant is left alive: none is not infected or not reached.".to_owned()
        };
        text.push_str(&format!("\n{said}\n"));
        return text;
    }
    let weak_status = |mutant: &supercov_engine::mutants_query::MutantEntry| {
        format!(
            "{}{}",
            mutant.status.name().replace('-', " "),
            mutant
                .weak
                .untestable
                .map_or_else(String::new, |kind| format!(" ({} boundary)", kind.name()))
        )
    };
    let place = |mutant: &supercov_engine::mutants_query::MutantEntry| {
        code(&format!(
            "{}:{}:{}",
            mutant.file, mutant.span.start[0], mutant.span.start[1]
        ))
    };
    if strong {
        text.push_str(
            "\n| Where | Mutant | Weak | Strong | Why |\n| --- | --- | --- | --- | --- |\n",
        );
        text.extend(rows.iter().take(MARKDOWN_ROWS).map(|mutant| {
            let outcome = report::outcome(mutant);
            format!(
                "| {} | {} | {} | {} | {} |\n",
                place(mutant),
                code(&report::change(mutant)),
                cell(&weak_status(mutant)),
                cell(match outcome {
                    Outcome::Pending => "—",
                    other => other.name(),
                }),
                cell(&report::why(mutant, true)),
            )
        }));
    } else {
        text.push_str("\n| Where | Mutant | Status | Why |\n| --- | --- | --- | --- |\n");
        text.extend(rows.iter().take(MARKDOWN_ROWS).map(|mutant| {
            format!(
                "| {} | {} | {} | {} |\n",
                place(mutant),
                code(&report::change(mutant)),
                cell(&weak_status(mutant)),
                cell(&mutant.weak.reason),
            )
        }));
    }
    if rows.len() > MARKDOWN_ROWS {
        let mut asked = request.clone();
        if !strong {
            asked.mutants.statuses = wanted.clone();
        }
        text.push_str(&format!(
            "\n_…and {} more: `{}`_\n",
            rows.len() - MARKDOWN_ROWS,
            crate::human_query::mutants_command(&asked, data, data.file.as_deref())
        ));
    }
    text
}

fn plural(count: usize, noun: &str) -> String {
    format!("{count} {noun}{}", if count == 1 { "" } else { "s" })
}

/// The mutants written to standard output, `out`.
fn printed(out: &mut dyn Write, lines: &str) -> ExitCode {
    if let Err(error) = out.write_all(lines.as_bytes()).and_then(|()| out.flush()) {
        // A reader that stopped early, as `head` does, needs no word.
        if error.kind() != std::io::ErrorKind::BrokenPipe {
            eprintln!("[supercov] could not write the mutants to standard output: {error}");
        }
        return ExitCode::from(2);
    }
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A standard output whose every write fails with this kind of error.
    struct Failing(std::io::ErrorKind);

    impl Write for Failing {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            Err(self.0.into())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn request() -> supercov_engine::indexed_query::IndexedQueryRequest {
        supercov_engine::indexed_query::IndexedQueryRequest {
            run_id: "run_1".into(),
            filter: "all".into(),
            command: "coverage.mutants".into(),
            metric: supercov_engine::coverage_query::MinimizeMetric::All,
            kind: None,
            runner: None,
            file: None,
            line: None,
            more_lines: Vec::new(),
            selector: None,
            sort: None,
            valid: None,
            test_exit_code: None,
            stale: None,
            stale_reasons: None,
            offset: 0,
            limit: 20,
            target: None,
            max_states: None,
            group: None,
            depth: None,
            gained_only: false,
            tags: Vec::new(),
            titles: Vec::new(),
            test_files: Vec::new(),
            mutants: Default::default(),
        }
    }

    fn data(
        mutants: Vec<supercov_engine::mutants_query::MutantEntry>,
    ) -> supercov_engine::mutants_query::CoverageMutantsData {
        use supercov_engine::coverage_index::MutantCounts;
        use supercov_engine::mutants_query::{CoverageMutantsData, MutantLevels, MutantsSummary};
        let mut counts = MutantCounts::default();
        mutants.iter().for_each(|mutant| counts.add(mutant.status));
        CoverageMutantsData {
            run: "run_1".into(),
            mutant_set: Some("stryker-js@10.1.1".into()),
            levels: MutantLevels {
                weak: true,
                strong: true,
            },
            file: None,
            line: None,
            summary: MutantsSummary::of(counts, 0),
            note: None,
            mutants,
            stryker: None,
            files: Vec::new(),
            diff: None,
            fail_on: None,
        }
    }

    fn mutant(
        line: u32,
        status: supercov_engine::coverage_index::MutantStatus,
        strong: Option<serde_json::Value>,
    ) -> supercov_engine::mutants_query::MutantEntry {
        use supercov_engine::mutants_query::{MutantEntry, MutantSpan, WeakResult};
        MutantEntry {
            log_only: false,
            id: format!("src/a.py:{line}:3-{line}:9:ArithmeticOperator:-"),
            file: "src/a.py".into(),
            span: MutantSpan {
                start: [line, 3],
                end: [line, 9],
            },
            mutator: "ArithmeticOperator".into(),
            original: "a + b".into(),
            replacement: "a - b".into(),
            changed: None,
            status,
            weak: WeakResult {
                reach: "reached",
                infection: Some(status.name()),
                code: "c".into(),
                reason: "weak reason".into(),
                proxy: false,
                rule: None,
                untestable: None,
                untestable_reason: None,
            },
            strong,
            stryker: None,
        }
    }

    /// Once a strong pass ran, the table is of the survivors, with both
    /// levels, the mutation score and each verdict counted.
    #[test]
    fn a_strong_pass_s_survivors_are_the_table_with_their_verdicts() {
        use serde_json::json;
        use supercov_engine::coverage_index::MutantStatus::*;
        let mut mutants = (1..=28)
            .map(|line| {
                mutant(
                    line,
                    Infected,
                    Some(json!({"status": "survived", "tests": 2})),
                )
            })
            .collect::<Vec<_>>();
        mutants.push(mutant(
            29,
            NotReached,
            Some(json!({"status": "no-coverage"})),
        ));
        mutants.push(mutant(30, NotInfected, None));
        mutants.push(mutant(
            31,
            Infected,
            Some(json!({"status": "killed", "killedBy": "t"})),
        ));
        mutants.push(mutant(32, Infected, Some(json!({"status": "timeout"}))));
        mutants.push(mutant(33, Infected, Some(json!({"status": "unviable"}))));
        mutants.push(mutant(34, Infected, Some(json!({"status": "error"}))));
        mutants.push(mutant(
            35,
            Infected,
            Some(json!({"status": "error", "reason": "no strong mutation driver for python yet"})),
        ));
        mutants.push(mutant(36, Infected, None));
        let text = markdown(&request(), &data(mutants.clone()));
        assert!(
            text.contains("\n\nMutation score **6.5%** (of 31 of 36 mutants) · 1 killed · 1 timed out · 28 survived · 1 no coverage · 1 unviable · 1 error · 1 not run (no driver) · 2 no strong verdict\n"),
            "{text}"
        );
        assert!(
            text.contains("| Where | Mutant | Weak | Strong | Why |"),
            "{text}"
        );
        assert!(
            text.contains("| `src/a.py:1:3` | `a + b → a - b` | infected | survived | every test that reached it passed with it in place (2 tests ran) |"),
            "{text}"
        );
        assert_eq!(text.matches("| `src/a.py:").count(), 25, "{text}");
        // The rest of the survivors: two survived, the uncovered and the not infected.
        assert!(
            text.contains("_…and 5 more: `supercov runs 'run_1' mutants`_"),
            "{text}"
        );
        // Asked by weak status: those, with their verdicts.
        let mut asked = request();
        asked.mutants.statuses = vec![NotInfected];
        let text = markdown(&asked, &data(mutants.clone()));
        assert!(
            text.contains("| `src/a.py:30:3` | `a + b → a - b` | not infected | — | weak reason |"),
            "{text}"
        );
        assert_eq!(text.matches("| `src/a.py:").count(), 1, "{text}");
        // Asked by verdict: those alone, and a verdict none has said so.
        let mut survived = request();
        survived.mutants.strong_statuses = vec!["survived".into()];
        let text = markdown(&survived, &data(mutants.clone()));
        assert_eq!(text.matches(" | survived | ").count(), 25, "{text}");
        assert!(!text.contains("| no-coverage |"), "{text}");
        let mut none = request();
        none.mutants.strong_statuses = vec!["no-coverage".into(), "killed".into()];
        let text = markdown(
            &none,
            &data(vec![mutant(
                1,
                Infected,
                Some(json!({"status": "survived"})),
            )]),
        );
        assert!(
            text.ends_with("\nNo mutant is no coverage or killed.\n"),
            "{text}"
        );
        // Nothing left alive.
        let text = markdown(
            &request(),
            &data(vec![mutant(1, Infected, Some(json!({"status": "killed"})))]),
        );
        assert!(
            text.contains("Mutation score **100.0%** (of 1 of 1 mutant) · 1 killed\n"),
            "{text}"
        );
        assert!(
            text.ends_with("\nNo mutant is left alive by the tests.\n"),
            "{text}"
        );
        // Verdicts and none counted: no score.
        let text = markdown(
            &request(),
            &data(vec![mutant(1, Infected, Some(json!({"status": "error"})))]),
        );
        assert!(text.contains("No mutation score: no mutant was killed, timed out, survived or left uncovered · 1 error\n"), "{text}");
    }

    #[test]
    fn mutants_that_cannot_be_printed_end_the_export_with_two() {
        // A reader that stopped early, and a standard output that failed.
        for kind in [
            std::io::ErrorKind::BrokenPipe,
            std::io::ErrorKind::StorageFull,
        ] {
            assert_eq!(printed(&mut Failing(kind), "{}\n"), ExitCode::from(2));
        }
        let mut written = Vec::new();
        assert_eq!(printed(&mut written, "{}\n"), ExitCode::SUCCESS);
        assert_eq!(written, b"{}\n");
    }
}
