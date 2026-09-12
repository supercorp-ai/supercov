use serde_json::{Value, json};
use std::{collections::BTreeSet, process::ExitCode};
use supercov_engine::{
    agent_json, assertion_store as maps,
    run_store::{discover_runs, select_run},
};

const HELP: &str = "Usage: supercov runs <run> assertions [report|init|review|validate|inventory|source] [options]\n\ninit [--from <older-run>]     Create assertions.json, or carry an older map.\nreview --all|--flow <A/F>     Acknowledge agent-reviewed flows (repeat --flow).\nreview --ack-scope           Acknowledge classified scope changes; combine with flows.\nvalidate                     Check format, IDs and source references only.\ninventory                    Page recognized assertion locations in frozen inputs.\nsource --file <path>         Page original source lines from the run.\nreport [--view <view>]       summary (default), assertions, creditedLines, unassertedLines.\n\n--offset <n> --limit <n>     Page array views; source uses zero-based line offsets.\n--json                      Structured result.\n\nEdit the run's assertions.json directly; Supercov never authors semantic edges.\nReview is your acknowledgement, not a mechanical proof. MC/DC remains separate.\n";
struct Options {
    action: String,
    from: Option<String>,
    file: Option<String>,
    view: String,
    flows: BTreeSet<String>,
    all: bool,
    ack: bool,
    offset: usize,
    limit: usize,
}
fn parse(args: &[String]) -> Result<Options, String> {
    let mut o = Options {
        action: "report".into(),
        from: None,
        file: None,
        view: "summary".into(),
        flows: BTreeSet::new(),
        all: false,
        ack: false,
        offset: 0,
        limit: 20,
    };
    let mut args = args.iter().peekable();
    if args.peek().is_some_and(|a| !a.starts_with('-')) {
        o.action = args.next().unwrap().clone();
    }
    let mut seen = BTreeSet::new();
    while let Some(arg) = args.next() {
        if arg != "--flow" && !seen.insert(arg.as_str()) {
            return Err(format!("Duplicate option: {arg}"));
        }
        match arg.as_str() {
            "--json" => (),
            "--all" => o.all = true,
            "--ack-scope" => o.ack = true,
            "--from" | "--file" | "--view" | "--flow" | "--offset" | "--limit" => {
                let value = args
                    .next()
                    .filter(|v| !v.starts_with('-'))
                    .ok_or_else(|| format!("{arg} requires a value"))?;
                match arg.as_str() {
                    "--from" => o.from = Some(value.clone()),
                    "--file" => o.file = Some(value.clone()),
                    "--view" => o.view = value.clone(),
                    "--flow" => {
                        o.flows.insert(value.clone());
                    }
                    "--offset" => o.offset = value.parse().map_err(|_| "Invalid offset")?,
                    "--limit" => o.limit = value.parse().map_err(|_| "Invalid limit")?,
                    _ => unreachable!(),
                }
            }
            _ => return Err(format!("Unknown option: {arg}")),
        }
    }
    if !matches!(
        o.action.as_str(),
        "report" | "init" | "review" | "validate" | "inventory" | "source"
    ) {
        return Err("Unknown assertions action".into());
    }
    if o.limit == 0 || o.limit > 1000 {
        return Err("limit must be 1..1000".into());
    }
    for (flag, valid) in [
        ("--from", o.action == "init"),
        ("--all", o.action == "review"),
        ("--ack-scope", o.action == "review"),
        ("--file", o.action == "source"),
        ("--view", o.action == "report"),
    ] {
        if seen.contains(flag) && !valid {
            return Err(format!("{flag} is not valid for {}", o.action));
        }
    }
    if !o.flows.is_empty() && o.action != "review" {
        return Err("--flow requires review".into());
    }
    if o.all && !o.flows.is_empty() {
        return Err("Choose --all or selected --flow values".into());
    }
    if o.action == "review" && !o.all && o.flows.is_empty() && !o.ack {
        return Err("Review requires --all, --flow or --ack-scope".into());
    }
    if o.action == "source" && o.file.is_none() {
        return Err("source requires --file".into());
    }
    if (seen.contains("--offset") || seen.contains("--limit"))
        && !matches!(o.action.as_str(), "report" | "inventory" | "source")
    {
        return Err("Pagination requires report, inventory or source".into());
    }
    if !matches!(
        o.view.as_str(),
        "summary" | "assertions" | "creditedLines" | "unassertedLines"
    ) {
        return Err("Unknown report view".into());
    }
    Ok(o)
}
fn page(values: &[Value], offset: usize, limit: usize) -> Value {
    let items = values
        .iter()
        .skip(offset)
        .take(limit)
        .cloned()
        .collect::<Vec<_>>();
    let next = offset.saturating_add(items.len());
    json!({"items":items,"pagination":{"offset":offset,"returned":items.len(),"total":values.len(),"nextOffset":if next<values.len(){Some(next)}else{None}}})
}
pub fn command(args: &[String]) -> ExitCode {
    if args.iter().any(|a| matches!(a.as_str(), "--help" | "-h")) {
        print!("{HELP}");
        return ExitCode::SUCCESS;
    }
    let result = (|| -> Result<Value, String> {
        let o = parse(&args[2..])?;
        let root = std::env::current_dir().map_err(|e| e.to_string())?;
        let inventory = discover_runs(&root).map_err(|e| e.to_string())?;
        let run = select_run(&inventory, Some(&args[0])).map_err(|e| e.to_string())?;
        let mut data = match o.action.as_str() {
            "init" => maps::initialize(
                &root,
                run,
                o.from
                    .as_ref()
                    .map(|s| select_run(&inventory, Some(s)).map_err(|e| e.to_string()))
                    .transpose()?,
            )?,
            "review" => maps::acknowledge(&root, run, &o.flows, o.all, o.ack)?,
            "source" | "inventory" => {
                let input = maps::load_inputs(run)?;
                let values = if o.action == "source" {
                    input
                        .inputs
                        .files
                        .get(o.file.as_ref().unwrap())
                        .ok_or("File absent from frozen run inputs")?
                        .lines()
                        .enumerate()
                        .map(|(i, s)| json!({"line":i+1,"text":s}))
                        .collect::<Vec<_>>()
                } else {
                    input
                        .inputs
                        .assertions
                        .iter()
                        .map(|s| serde_json::to_value(s).unwrap())
                        .collect()
                };
                let mut data = page(&values, o.offset, o.limit);
                data["limitations"] = json!(input.inputs.limitations);
                data
            }
            "validate" => {
                let input = maps::load_inputs(run)?;
                let (map, _) = maps::load(run, &input)?;
                let errors = supercov_engine::assertion_map::validate(&map, &input.inputs);
                json!({"valid":errors.is_empty(),"errors":errors,"meaning":"References only; semantic edges are agent-authored"})
            }
            _ => {
                let report = maps::report(run)?;
                let mut data = if o.view == "summary" {
                    json!({})
                } else {
                    page(report[&o.view].as_array().unwrap(), o.offset, o.limit)
                };
                for key in ["summary", "basis", "validationErrors", "limitations"] {
                    data[key] = report[key].clone();
                }
                data
            }
        };
        data["workingTree"] = match super::current_integrity_for_run(&root, run) {
            Some(current) => {
                let comparison = supercov_engine::run_store::compare_run_integrity(
                    Some(&run.metadata.integrity),
                    &current,
                );
                json!({"stale":comparison.stale,"reasons":comparison.reasons})
            }
            None => json!({"stale":null,"reason":"current source integrity unavailable"}),
        };
        data["run"] = json!(run.id);
        data["map"] = json!(run.directory.join(maps::MAP_FILE));
        Ok(data)
    })();
    let json_output = args.iter().any(|a| a == "--json");
    match result {
        Ok(data) => {
            let invalid = data.get("valid") == Some(&Value::Bool(false));
            if json_output {
                match agent_json::success("coverage.assertions", &data, None) {
                    Ok(output) => print!("{output}"),
                    Err(size) => {
                        print!("{}",agent_json::failure(Some("coverage.assertions"), &agent_json::AgentError {
                            code:agent_json::ErrorCode::ResponseTooLarge,
                            message:"Response too large; reduce --limit or read the assertions.json file".into(),
                            retryable:false,details:Some(json!({"actualBytes":size.actual_bytes,"maxBytes":size.max_bytes})),
                        }));
                        return ExitCode::from(2);
                    }
                }
            } else {
                println!("{}", serde_json::to_string_pretty(&data).unwrap());
            }
            if invalid {
                ExitCode::from(2)
            } else {
                ExitCode::SUCCESS
            }
        }
        Err(message) => {
            if json_output {
                print!(
                    "{}",
                    agent_json::failure(
                        Some("coverage.assertions"),
                        &agent_json::AgentError {
                            code: agent_json::ErrorCode::InvalidArgument,
                            message,
                            retryable: false,
                            details: None
                        }
                    )
                );
            } else {
                eprintln!("[supercov] {message}");
            }
            ExitCode::from(2)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_ignored_or_ambiguous_options() {
        for args in [
            vec!["review"],
            vec!["report", "--from", "old"],
            vec!["init", "--flow", "a/f"],
            vec!["review", "--all", "--flow", "a/f"],
            vec!["source"],
            vec!["report", "--limit", "0"],
        ] {
            assert!(parse(&args.iter().map(|s| s.to_string()).collect::<Vec<_>>()).is_err());
        }
    }
}
