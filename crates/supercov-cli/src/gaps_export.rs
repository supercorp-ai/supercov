//! `supercov runs <run-id> gaps --format jsonl`: every obligation the run
//! left uncovered, one JSON object per line, file by file. `gaps` and `file`
//! answer a page at a time, which suits a reader and not a tool that wants
//! them all: one user paged through a 64 KB reply limit for forty minutes to
//! collect the uncovered MC/DC conditions of one run.

use std::{io::Write, path::PathBuf, process::ExitCode};

use serde::Serialize;
use serde_json::Value;
use supercov_engine::coverage_query::{CoverageFileDetailData, CoverageFileObligation};

/// Options of `gaps` that `file` shares, passed on to each file.
const SHARED: &[&str] = &[
    "--metric",
    "--filter",
    "--kind",
    "--runner",
    "--tag",
    "--title",
    "--test-file",
];

/// Everything one page holds: the export asks for all of it at once.
const ALL: &str = "1000000000";

pub fn command(arguments: &[String]) -> ExitCode {
    let mut passed = Vec::new();
    let mut output = None;
    let mut format = None;
    let mut index = 0;
    // Dispatch sends `<run> gaps ...` here: never empty.
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
    if format.as_deref() != Some("jsonl") {
        eprintln!("[supercov] gaps --format takes jsonl");
        return ExitCode::from(2);
    }
    let Some(run) = passed.first().cloned() else {
        eprintln!(
            "[supercov] usage: supercov runs <run-id> gaps --format jsonl [--metric <metric>] [--output <file>]"
        );
        return ExitCode::from(2);
    };
    let shared = shared_options(&passed);
    let root = match std::env::current_dir() {
        Ok(root) => root,
        Err(error) => {
            eprintln!("[supercov] could not resolve the current directory: {error}");
            return ExitCode::from(2);
        }
    };
    // The query's data itself, not its reply: a reply stops at 64 KB, and the
    // export exists for the runs whose gaps do not fit in one. Read through
    // the reply, it refused exactly those.
    let query = |arguments: Vec<String>| -> Result<Value, String> {
        let (request, _) =
            super::public_query::coverage_request(run.clone(), "gaps", &arguments, true)
                .map_err(|error| error.error.message.clone())?;
        let (output, _) = super::execute_coverage_query(&root, &request, None)
            .map_err(|failure| failure.error.message.clone())?;
        Ok(serde_json::json!({
            "data": serde_json::to_value(&output.data).expect("plain data serializes")
        }))
    };
    // Past `<run> gaps`, as dispatch sends them.
    let mut gaps_arguments = passed[2..].to_vec();
    gaps_arguments.extend(["--json", "--limit", ALL].map(str::to_owned));
    let gaps = match query(gaps_arguments) {
        Ok(gaps) => gaps,
        Err(error) => {
            eprintln!("[supercov] {error}");
            return ExitCode::from(2);
        }
    };
    let files = gaps["data"]["gaps"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|gap| gap["file"].as_str().map(str::to_owned))
        .collect::<Vec<_>>();
    // Every file from one reading of the run. A query per file checked the
    // checkout and opened the index again each time: 129 s for 17,000 rows
    // of a 1,100-test run.
    let details = match file_details(&root, &run, &files, &shared) {
        Ok(details) => details,
        Err(error) => {
            eprintln!("[supercov] {error}");
            return ExitCode::from(2);
        }
    };
    let mut lines = String::new();
    let mut written = 0usize;
    for (file, detail) in files.iter().zip(&details) {
        // A file the gaps name has a gap line, and a gap line an obligation.
        detail
            .gap_lines
            .iter()
            .flat_map(|gap_line| gap_line.obligations.iter().map(move |o| (gap_line, o)))
            .for_each(|(gap_line, obligation)| {
                let row = Row {
                    file,
                    obligation,
                    line_source: gap_line.source.as_deref(),
                };
                lines.push_str(
                    &serde_json::to_string(&row).expect("a row is fields named by strings"),
                );
                lines.push('\n');
                written += 1;
            });
    }
    match output {
        Some(path) => {
            if let Err(error) = supercov_engine::fault_fs::write(&path, &lines) {
                eprintln!("[supercov] {}: {error}", path.display());
                return ExitCode::from(2);
            }
            eprintln!(
                "[supercov] wrote {written} uncovered obligation(s) in {} file(s) to {}",
                files.len(),
                path.display()
            );
        }
        None => return printed(&mut std::io::stdout().lock(), &lines),
    }
    ExitCode::SUCCESS
}

/// The gaps written to standard output, `out`.
fn printed(out: &mut dyn Write, lines: &str) -> ExitCode {
    if let Err(error) = out.write_all(lines.as_bytes()).and_then(|()| out.flush()) {
        // A reader that stopped early, as `head` does, needs no word.
        if error.kind() != std::io::ErrorKind::BrokenPipe {
            eprintln!("[supercov] could not write the gaps to standard output: {error}");
        }
        return ExitCode::from(2);
    }
    ExitCode::SUCCESS
}

/// Each file's detail as `runs <id> file <path> --json` gives its data, for
/// every file, from the run read once.
fn file_details(
    root: &std::path::Path,
    run: &str,
    files: &[String],
    shared: &[String],
) -> Result<Vec<CoverageFileDetailData>, String> {
    let Some(first) = files.first() else {
        return Ok(Vec::new());
    };
    // The options as `file` reads them, so the rows are the ones it gives.
    let mut arguments = vec![first.clone()];
    arguments.extend(shared.iter().cloned());
    arguments.extend(["--json", "--limit", ALL].map(str::to_owned));
    // The gaps query took these options already.
    let (request, _) =
        super::public_query::coverage_request(run.to_owned(), "file", &arguments, true)
            .expect("`file` takes the options `gaps` took");
    let inventory = super::public_run_inventory(root).map_err(|error| error.to_string())?;
    let stored = supercov_engine::run_store::select_run(&inventory, Some(&request.run_id))
        .map_err(|error| error.to_string())?;
    let container = super::open_query_index_for(stored, request.test_filter().as_ref())
        .map_err(|error| error.to_string())?;
    let index = supercov_engine::coverage_index::CoverageIndex::new(&container)
        .map_err(|error| error.to_string())?;
    let view = request.view().expect("the gaps query read this view");
    let read = supercov_engine::coverage_query::FileDetailSource::read(&index, view)
        .map_err(|error| format!("{error:?}"))?;
    files
        .iter()
        .map(|file| {
            let (detail, _) = supercov_engine::coverage_query::coverage_file_detail_from(
                &read,
                supercov_engine::coverage_query::CoverageFileDetailOptions {
                    run: &stored.id,
                    view,
                    kind: request.kind.as_deref(),
                    runner: request.runner.as_deref(),
                    selector: file,
                    metric: request.metric,
                    offset: 0,
                    limit: usize::MAX,
                },
            )
            .map_err(|error| format!("{file}: {error:?}"))?;
            Ok(detail)
        })
        .collect()
}

/// The options among `arguments` that `file` takes as well, with their values.
fn shared_options(arguments: &[String]) -> Vec<String> {
    let mut shared = Vec::new();
    let mut index = 0;
    // The run is the first argument: never empty.
    loop {
        if SHARED.contains(&arguments[index].as_str())
            && let Some(value) = arguments.get(index + 1)
        {
            shared.push(arguments[index].clone());
            shared.push(value.clone());
            index += 1;
        }
        index += 1;
        if index >= arguments.len() {
            break shared;
        }
    }
}

/// One exported obligation: what `file` says of it, with its file and the
/// source of its line.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Row<'a> {
    file: &'a str,
    #[serde(flatten)]
    obligation: &'a CoverageFileObligation,
    #[serde(skip_serializing_if = "Option::is_none")]
    line_source: Option<&'a str>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A writer that fails as `kind`.
    struct Failing(std::io::ErrorKind);

    impl Write for Failing {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            Err(self.0.into())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// Gaps that could not all be printed fail the command, whether the
    /// reader stopped early or the output failed otherwise.
    #[test]
    fn gaps_that_could_not_be_printed_fail_the_command() {
        let mut out = Vec::new();
        assert_eq!(printed(&mut out, "{}\n"), ExitCode::SUCCESS);
        assert_eq!(out, b"{}\n");
        for kind in [std::io::ErrorKind::BrokenPipe, std::io::ErrorKind::Other] {
            assert_eq!(printed(&mut Failing(kind), "{}\n"), ExitCode::from(2));
        }
    }

    #[test]
    fn an_exported_obligation_names_its_file_and_line_source() {
        use supercov_engine::coverage_query::{CoverageLineObligation, CoverageOtherCoverage};
        let obligation = CoverageFileObligation::Line(CoverageLineObligation {
            kind: "line".into(),
            id: "line:src/chart.tsx:28".into(),
            line: 28,
            other_coverage: CoverageOtherCoverage {
                covered_elsewhere: false,
                kinds: Vec::new(),
                runners: Vec::new(),
                tests: Vec::new(),
            },
        });
        let exported = serde_json::to_value(Row {
            file: "src/chart.tsx",
            obligation: &obligation,
            line_source: Some("if (!context) {"),
        })
        .unwrap();
        assert_eq!(exported["file"], "src/chart.tsx");
        assert_eq!(exported["kind"], "line");
        assert_eq!(exported["line"], 28);
        assert_eq!(exported["lineSource"], "if (!context) {");
        let options = shared_options(&[
            "run_1".into(),
            "--metric".into(),
            "mcdc".into(),
            "--kind".into(),
            "e2e".into(),
        ]);
        assert_eq!(options, ["--metric", "mcdc", "--kind", "e2e"]);
    }
}
