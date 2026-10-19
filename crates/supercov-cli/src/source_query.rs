//! `supercov runs <run> source <path>`: a project file as the run saw it,
//! read from the checkout once its bytes are verified against the run.

use serde_json::{Value, json};
use std::{collections::BTreeSet, fmt::Write, process::ExitCode};
use supercov_engine::{
    agent_json,
    run_store::{StoredRun, discover_runs, select_run},
    source_manifest,
};

const HELP: &str = "Usage: supercov runs <run> source <path> [--offset <n>] [--limit <n>] [--json]\n\nRead the current project file after verifying it matches the run. The path is project-relative.\n--offset is zero-based; --limit defaults to 20 (1..1000). --json returns line/text items.\n";

pub fn command(args: &[String]) -> ExitCode {
    if args.iter().any(|a| matches!(a.as_str(), "--help" | "-h")) {
        print!("{HELP}");
        return ExitCode::SUCCESS;
    }
    let json_output = args.iter().any(|a| a == "--json");
    let result = (|| -> Result<Value, String> {
        let (file, offset, limit) = parse(&args[2..])?;
        let root = std::env::current_dir().map_err(|e| e.to_string())?;
        let runs = discover_runs(&root).map_err(|e| e.to_string())?;
        let run = select_run(&runs, Some(&args[0])).map_err(|e| e.to_string())?;
        let working_tree = working_tree(&root, run);
        // Only this file has to match the run: another file changing does
        // not make it any less the file the run measured.
        let input = source_manifest::load_inputs_for(&root, run, |name| name == file)?;
        let text = input
            .inputs
            .files
            .get(&file)
            .ok_or_else(|| format!("File absent from the run's source manifest: {file}"))?;
        let lines = text
            .lines()
            .enumerate()
            .map(|(i, text)| json!({"line": i + 1, "text": text}))
            .collect::<Vec<_>>();
        let items = lines
            .iter()
            .skip(offset)
            .take(limit)
            .cloned()
            .collect::<Vec<_>>();
        let next = offset.saturating_add(items.len());
        Ok(json!({
            "view": "source", "run": run.id, "file": file, "revision": input.evidence_digest,
            "items": items,
            "pagination": {"offset": offset, "returned": items.len(), "total": lines.len(),
                "nextOffset": if next < lines.len() { Some(next) } else { None }},
            "workingTree": working_tree,
        }))
    })();
    match result {
        Ok(data) if json_output => match agent_json::success("coverage.source", &data, None) {
            Ok(output) => {
                print!("{output}");
                ExitCode::SUCCESS
            }
            Err(size) => {
                print!(
                    "{}",
                    agent_json::failure(
                        Some("coverage.source"),
                        &agent_json::AgentError {
                            code: agent_json::ErrorCode::ResponseTooLarge,
                            message: "Response too large; reduce --limit or use the text view (omit --json)".into(),
                            retryable: false,
                            details: Some(json!({"actualBytes": size.actual_bytes, "maxBytes": size.max_bytes})),
                        }
                    )
                );
                ExitCode::from(2)
            }
        },
        Ok(data) => {
            print!("{}", render(&data));
            ExitCode::SUCCESS
        }
        Err(message) => crate::refuse("coverage.source", json_output, message, ""),
    }
}

fn parse(args: &[String]) -> Result<(String, usize, usize), String> {
    let (file, flags) = args
        .split_first()
        .filter(|(s, _)| !s.starts_with('-'))
        .ok_or("source requires a project-relative path")?;
    let (mut offset, mut limit) = (0, 20);
    let mut flags = flags.iter();
    let mut seen = BTreeSet::new();
    while let Some(flag) = flags.next() {
        if !seen.insert(flag) {
            return Err(format!("Duplicate option: {flag}"));
        }
        match flag.as_str() {
            "--json" => (),
            "--offset" | "--limit" => {
                let value = flags
                    .next()
                    .ok_or_else(|| format!("{flag} requires a value"))?
                    .parse::<usize>()
                    .map_err(|_| format!("Invalid {flag}"))?;
                if flag == "--offset" {
                    offset = value;
                } else {
                    limit = value;
                }
            }
            _ => return Err(format!("Unexpected argument: {flag}")),
        }
    }
    if limit == 0 || limit > 1000 {
        return Err("limit must be 1..1000".into());
    }
    Ok((file.clone(), offset, limit))
}

fn working_tree(root: &std::path::Path, run: &StoredRun) -> Value {
    let comparison = super::checkout_comparison(root, run);
    json!({"stale": comparison.stale, "reasons": comparison.reasons})
}

fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn render(data: &Value) -> String {
    let text = |v: &Value| v.as_str().unwrap_or("").to_owned();
    let mut out = format!(
        "{} — matching current source, run {}\n\n",
        text(&data["file"]),
        text(&data["run"])
    );
    let width = data["pagination"]["total"].to_string().len();
    for line in data["items"].as_array().into_iter().flatten() {
        let number = line["line"].as_u64().unwrap_or(0);
        let _ = writeln!(out, "{number:>width$} │ {}", text(&line["text"]));
    }
    let page = &data["pagination"];
    let offset = page["offset"].as_u64().unwrap_or(0);
    let returned = page["returned"].as_u64().unwrap_or(0);
    let total = page["total"].as_u64().unwrap_or(0);
    if total == 0 {
        let _ = writeln!(out, "\nThe file is empty.");
    } else if returned == 0 {
        let _ = writeln!(
            out,
            "\nShowing none of {total}: --offset {offset} is past the end"
        );
    } else {
        let _ = writeln!(
            out,
            "\nShowing {}-{} of {total}",
            offset + 1,
            offset + returned
        );
    }
    if let Some(next) = page["nextOffset"].as_u64() {
        let _ = writeln!(
            out,
            "Next: supercov runs {} source {} --offset {next} --limit {returned}",
            quote(&text(&data["run"])),
            quote(&text(&data["file"]))
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_text_view_numbers_lines_and_names_the_next_page() {
        let data = json!({"view":"source","run":"run_one","file":"tests/a b's.ts",
            "items":[{"line":34,"text":"  const x = '🧪';"},{"line":35,"text":""},{"line":36,"text":"\tassert(x);"}],
            "pagination":{"offset":33,"returned":3,"total":100,"nextOffset":36}});
        let rendered = render(&data);
        assert!(rendered.contains(" 34 │   const x = '🧪';\n 35 │ \n 36 │ \tassert(x);\n"));
        assert!(rendered.contains("Showing 34-36 of 100"));
        assert!(rendered.contains("source 'tests/a b'\\''s.ts' --offset 36 --limit 3"));
    }

    #[test]
    fn a_page_past_the_last_line_says_so() {
        let data = json!({"view":"source","run":"run_one","file":"a.ts","items":[],
            "pagination":{"offset":9999,"returned":0,"total":21}});
        assert!(render(&data).contains("Showing none of 21: --offset 9999 is past the end"));
    }
}
