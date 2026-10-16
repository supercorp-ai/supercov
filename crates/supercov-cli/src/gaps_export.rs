//! `supercov runs <run-id> gaps --format jsonl`: every obligation the run
//! left uncovered, one JSON object per line, file by file. `gaps` and `file`
//! answer a page at a time, which suits a reader and not a tool that wants
//! them all: one user paged through a 64 KB reply limit for forty minutes to
//! collect the uncovered MC/DC conditions of one run.

use std::{io::Write, path::PathBuf, process::ExitCode};

use serde_json::Value;

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
        let invocation = super::parse_public_query("runs", &arguments)
            .map_err(|error| error.error.message.clone())?;
        match super::execute_public_query(&root, &invocation)
            .map_err(|failure| failure.error.message.clone())?
        {
            super::PublicQueryOutput::Coverage { output, .. } => serde_json::to_value(&output.data)
                .map(|data| serde_json::json!({ "data": data }))
                .map_err(|error| error.to_string()),
            super::PublicQueryOutput::Runs { .. } => Err("not a coverage query".into()),
        }
    };
    let mut gaps_arguments = passed.clone();
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
    let mut lines = String::new();
    let mut written = 0usize;
    for file in &files {
        let mut file_arguments = vec![run.clone(), "file".into(), file.clone()];
        file_arguments.extend(shared.iter().cloned());
        file_arguments.extend(["--json", "--limit", ALL].map(str::to_owned));
        let detail = match query(file_arguments) {
            Ok(detail) => detail,
            Err(error) => {
                eprintln!("[supercov] {file}: {error}");
                return ExitCode::from(2);
            }
        };
        for gap_line in detail["data"]["gapLines"].as_array().into_iter().flatten() {
            for obligation in gap_line["obligations"].as_array().into_iter().flatten() {
                lines.push_str(&row(file, gap_line, obligation).to_string());
                lines.push('\n');
                written += 1;
            }
        }
    }
    match output {
        Some(path) => {
            if let Err(error) = std::fs::write(&path, &lines) {
                eprintln!("[supercov] {}: {error}", path.display());
                return ExitCode::from(2);
            }
            eprintln!(
                "[supercov] wrote {written} uncovered obligation(s) in {} file(s) to {}",
                files.len(),
                path.display()
            );
        }
        None => {
            let mut stdout = std::io::stdout().lock();
            if stdout.write_all(lines.as_bytes()).is_err() {
                return ExitCode::from(2);
            }
        }
    }
    ExitCode::SUCCESS
}

/// The options among `arguments` that `file` takes as well, with their values.
fn shared_options(arguments: &[String]) -> Vec<String> {
    let mut shared = Vec::new();
    let mut index = 0;
    while index < arguments.len() {
        if SHARED.contains(&arguments[index].as_str())
            && let Some(value) = arguments.get(index + 1)
        {
            shared.push(arguments[index].clone());
            shared.push(value.clone());
            index += 1;
        }
        index += 1;
    }
    shared
}

/// One exported obligation: what `file` says of it, with its file and the
/// source of its line.
fn row(file: &str, gap_line: &Value, obligation: &Value) -> Value {
    let mut row = serde_json::Map::new();
    row.insert("file".into(), Value::String(file.to_owned()));
    if let Some(fields) = obligation.as_object() {
        for (key, value) in fields {
            row.insert(key.clone(), value.clone());
        }
    }
    if let Some(source) = gap_line["source"].as_str() {
        row.insert("lineSource".into(), Value::String(source.to_owned()));
    }
    Value::Object(row)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_exported_obligation_names_its_file_and_line_source() {
        let gap_line = serde_json::json!({ "line": 28, "source": "if (!context) {" });
        let obligation =
            serde_json::json!({ "kind": "mcdc", "line": 28, "missingCondition": "!context" });
        let exported = row("src/chart.tsx", &gap_line, &obligation);
        assert_eq!(exported["file"], "src/chart.tsx");
        assert_eq!(exported["kind"], "mcdc");
        assert_eq!(exported["missingCondition"], "!context");
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
