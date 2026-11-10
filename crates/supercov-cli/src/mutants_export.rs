//! `supercov runs <run-id> mutants --format jsonl`: every mutant asked
//! about, one JSON object per line, each as `mutants --json` lists it. A
//! reply holds a page, about 128 mutants under its 64 KB limit, and a large
//! app's run has tens of thousands.
//!
//! `--format markdown`: the mutants the tests leave alive as a table for a
//! pull request's comment, `--diff` the change's, capped at
//! `MARKDOWN_ROWS` with the command that lists the rest.

use std::{io::Write, path::PathBuf, process::ExitCode};

use supercov_engine::indexed_query::IndexedQueryData;

/// Everything one page holds: the export asks for all of it at once.
const ALL: &str = "1000000000";

pub fn command(arguments: &[String]) -> ExitCode {
    let mut passed = Vec::new();
    let mut output = None;
    let mut format = None;
    let mut index = 0;
    while index < arguments.len() {
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
    }
    let markdown = match format.as_deref() {
        Some("jsonl") => false,
        Some("markdown") => true,
        _ => {
            eprintln!("[supercov] mutants --format takes jsonl or markdown");
            return ExitCode::from(2);
        }
    };
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
    let IndexedQueryData::Mutants(data) = &output_data.data else {
        unreachable!("a mutants request is answered with mutants");
    };
    if let Some(note) = &data.note {
        eprintln!("[supercov] {note}");
    }
    let mut lines = String::new();
    if markdown {
        lines = self::markdown(&request, data);
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

/// The mutants the tests leave alive, or those `--status` asked for, as a
/// pull request's comment: one line of counts and a table.
pub(crate) fn markdown(
    request: &supercov_engine::indexed_query::IndexedQueryRequest,
    data: &supercov_engine::mutants_query::CoverageMutantsData,
) -> String {
    use supercov_engine::coverage_index::MutantStatus;
    let summary = &data.summary;
    let wanted = if request.mutants.statuses.is_empty() {
        vec![MutantStatus::NotInfected, MutantStatus::NotReached]
    } else {
        request.mutants.statuses.clone()
    };
    let mut rows = data
        .mutants
        .iter()
        .filter(|mutant| wanted.contains(&mutant.status))
        .collect::<Vec<_>>();
    // What the tests leave alive first, as `--diff` lists them.
    rows.sort_by_key(|mutant| wanted.iter().position(|status| *status == mutant.status));
    let score = summary
        .weak_score
        .map_or_else(|| "—".to_owned(), |score| format!("{:.1}%", score * 100.0));
    let counts = MutantStatus::ALL
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
    let names = wanted
        .iter()
        .map(|status| status.name().replace('-', " "))
        .collect::<Vec<_>>()
        .join(" or ");
    if rows.is_empty() {
        text.push_str(&format!("\nNo mutant is {names}.\n"));
        return text;
    }
    text.push_str("\n| Where | Mutant | Status | Why |\n| --- | --- | --- | --- |\n");
    for mutant in rows.iter().take(MARKDOWN_ROWS) {
        text.push_str(&format!(
            "| {} | {} | {} | {} |\n",
            code(&format!(
                "{}:{}:{}",
                mutant.file, mutant.span.start[0], mutant.span.start[1]
            )),
            code(&crate::human_query::mutant_change(mutant)),
            cell(&format!(
                "{}{}",
                mutant.status.name().replace('-', " "),
                mutant
                    .weak
                    .untestable
                    .map_or_else(String::new, |kind| format!(" ({} boundary)", kind.name()))
            )),
            cell(&mutant.weak.reason),
        ));
    }
    if rows.len() > MARKDOWN_ROWS {
        let mut asked = request.clone();
        asked.mutants.statuses = wanted.clone();
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
