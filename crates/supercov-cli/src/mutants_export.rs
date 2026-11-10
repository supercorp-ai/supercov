//! `supercov runs <run-id> mutants --format jsonl`: every mutant asked
//! about, one JSON object per line, each as `mutants --json` lists it. A
//! reply holds a page, about 128 mutants under its 64 KB limit, and a run of
//! a large app's size has 61,000.

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
    if format.as_deref() != Some("jsonl") {
        eprintln!("[supercov] mutants --format takes jsonl");
        return ExitCode::from(2);
    }
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
    for mutant in &data.mutants {
        lines.push_str(&serde_json::to_string(mutant).expect("a mutant is plain data"));
        lines.push('\n');
    }
    let written = match output {
        Some(path) => {
            if let Err(error) = supercov_engine::fault_fs::write(&path, &lines) {
                eprintln!("[supercov] {}: {error}", path.display());
                return ExitCode::from(2);
            }
            eprintln!(
                "[supercov] wrote {} mutant(s) to {}",
                data.mutants.len(),
                path.display()
            );
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
    ExitCode::SUCCESS
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
