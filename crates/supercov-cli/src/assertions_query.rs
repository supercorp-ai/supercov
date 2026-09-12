use serde_json::{Value, json};
use std::{collections::BTreeSet, process::ExitCode};
use supercov_engine::{
    agent_json, assertion_store as maps,
    run_store::{discover_runs, select_run},
};

const HELP: &str = r#"Usage: supercov assertions schema [--json]
       supercov assertions validate --file <path> [--json]
       supercov runs <run> assertions [action] [options]

schema                      JSON Schema from Rust types; raw JSON without --json.
validate --file <path>       Standalone JSON syntax and field types (no run needed).

Run actions:
validate                    Check syntax, IDs, links, frozen references and state binding.
review --all|--flow <A/F>    Record agent review (repeat --flow); not semantic proof.
review --ack-scope          Acknowledge classified scope changes; combine with flows.
inventory [--file <path>]   Optional raw list of assertions found in saved test code.
files                       List frozen input paths and their byte sizes.
source --file <path>        Read source saved at run time, not today's checkout.
report [--view <view>]      summary (default), assertions, statements, tests,
                            creditedLines, unassertedLines; optional --file filter.
check                       Fail on invalid references, dirty flows, failed run,
                            pending scope review or a stale/unavailable working tree.
  --require-complete        Also require every inventoried assertion mapped.
  --require-observed        Also require passing runtime evidence for every mapped site/flow.
  --min <0..100>            Minimum agent-assessed statement percentage.
  --archived                Check archived evidence without checking today's working tree.

--offset <n> --limit <n>     Page inventory, source or report array views (limit 1..1000).
--json                      Standard structured result envelope.

Each new test run creates assertions.json and reuses the newest available map
for the same command and language. No init step is needed.
Edit the run's assertions.json directly, then validate, review and check.
Exit 0 means the requested check passed; exit 2 means invalid input or an unmet gate.
Pin a run ID while authoring. See docs/assertion-maps.md and docs/assertion-agent.md.
Supercov never authors semantic edges. MC/DC remains separate.
"#;
struct Options {
    action: String,
    require_complete: bool,
    require_observed: bool,
    archived: bool,
    minimum: Option<f64>,
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
        require_complete: false,
        require_observed: false,
        archived: false,
        minimum: None,
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
            "--require-complete" => o.require_complete = true,
            "--require-observed" => o.require_observed = true,
            "--archived" => o.archived = true,
            "--ack-scope" => o.ack = true,
            "--file" | "--view" | "--flow" | "--offset" | "--limit" | "--min" => {
                let value = args
                    .next()
                    .filter(|v| !v.starts_with('-'))
                    .ok_or_else(|| format!("{arg} requires a value"))?;
                match arg.as_str() {
                    "--min" => o.minimum = Some(value.parse().map_err(|_| "Invalid minimum")?),
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
        "report" | "review" | "validate" | "inventory" | "source" | "check" | "files"
    ) {
        return Err("Unknown assertions action".into());
    }
    if o.minimum
        .is_some_and(|n| !n.is_finite() || !(0.0..=100.0).contains(&n))
    {
        return Err("min must be a finite percentage from 0 to 100".into());
    }
    if o.limit == 0 || o.limit > 1000 {
        return Err("limit must be 1..1000".into());
    }
    for (flag, valid) in [
        ("--min", o.action == "check"),
        ("--require-complete", o.action == "check"),
        ("--require-observed", o.action == "check"),
        ("--archived", o.action == "check"),
        ("--all", o.action == "review"),
        ("--ack-scope", o.action == "review"),
        (
            "--file",
            matches!(o.action.as_str(), "source" | "inventory")
                || o.action == "report"
                    && matches!(
                        o.view.as_str(),
                        "assertions" | "statements" | "creditedLines" | "unassertedLines"
                    ),
        ),
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
        && !matches!(
            o.action.as_str(),
            "report" | "inventory" | "source" | "files"
        )
    {
        return Err("Pagination requires report, inventory, source or files".into());
    }
    if !matches!(
        o.view.as_str(),
        "summary" | "assertions" | "statements" | "tests" | "creditedLines" | "unassertedLines"
    ) {
        return Err("Unknown report view".into());
    }
    if o.action == "report"
        && o.view == "summary"
        && (seen.contains("--offset") || seen.contains("--limit"))
    {
        return Err("Pagination requires an array view, not summary".into());
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
        let mut check_report = None;
        let mut data = match o.action.as_str() {
            "review" => maps::acknowledge(&root, run, &o.flows, o.all, o.ack)?,
            "source" | "inventory" | "files" => {
                let input = maps::load_inputs(run)?;
                if let Some(file) = &o.file
                    && !input.inputs.files.contains_key(file)
                {
                    return Err("File absent from frozen run inputs".into());
                }
                let values = if o.action == "files" {
                    input
                        .inputs
                        .files
                        .iter()
                        .map(|(file, text)| json!({"file":file,"bytes":text.len()}))
                        .collect::<Vec<_>>()
                } else if o.action == "source" {
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
                        .filter(|s| o.file.as_ref().is_none_or(|file| s.at.file == *file))
                        .map(|s| serde_json::to_value(s).unwrap())
                        .collect()
                };
                let mut data = page(&values, o.offset, o.limit);
                data["limitations"] = json!(input.inputs.limitations);
                data["revision"] = json!(input.evidence_digest);
                data
            }
            "validate" => {
                let input = maps::load_inputs(run)?;
                let bytes =
                    std::fs::read(run.directory.join(maps::MAP_FILE)).map_err(|e| e.to_string())?;
                match supercov_engine::assertion_map::parse(&bytes) {
                    Err(error) => json!({"valid":false,"stage":"syntax","errors":[error]}),
                    Ok(_) => {
                        let (map, _) = maps::load(run, &input)?;
                        let errors = supercov_engine::assertion_map::validate(&map, &input.inputs);
                        json!({"valid":errors.is_empty(),"stage":"references","errors":errors,"meaning":"References only; semantic edges are agent-authored"})
                    }
                }
            }
            _ => {
                let report = maps::report(run)?;
                let mut data = if o.view == "summary" {
                    json!({})
                } else {
                    let values = report[&o.view]
                        .as_array()
                        .unwrap()
                        .iter()
                        .filter(|row| {
                            o.file.as_ref().is_none_or(|file| {
                                row["file"].as_str().or_else(|| row["at"]["file"].as_str())
                                    == Some(file.as_str())
                            })
                        })
                        .cloned()
                        .collect::<Vec<_>>();
                    page(&values, o.offset, o.limit)
                };
                for key in [
                    "summary",
                    "basis",
                    "validationErrors",
                    "limitations",
                    "revision",
                    "inheritance",
                ] {
                    data[key] = report[key].clone();
                }
                data["scope"] =
                    json!("summary covers the whole archived run; --file filters items only");
                if o.action == "check" {
                    check_report = Some(report);
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
        if let Some(report) = check_report {
            let failures = check_failures(&report, &data["workingTree"], &o);
            data["valid"] = json!(failures.is_empty());
            data["failures"] = json!(failures);
            data["requirements"] = json!({"complete":o.require_complete,"observed":o.require_observed,"minimum":o.minimum,"archived":o.archived});
        }
        data["run"] = json!(run.id);
        data["map"] = json!(run.directory.join(maps::MAP_FILE));
        Ok(data)
    })();
    emit(result, args.iter().any(|a| a == "--json"))
}
fn emit(result: Result<Value, String>, json_output: bool) -> ExitCode {
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

fn check_failures(report: &Value, working_tree: &Value, o: &Options) -> Vec<String> {
    let s = &report["summary"];
    let mut failures = Vec::new();
    if report["validationErrors"]
        .as_array()
        .is_none_or(|a| !a.is_empty())
    {
        failures.push("Map has invalid references".into());
    }
    if s["runPassed"] != true {
        failures.push("Run did not pass".into());
    }
    if s["dirtyFlows"].as_u64() != Some(0) {
        failures.push("Flows require review or reference repair".into());
    }
    if s["scopeReview"].as_array().is_none_or(|a| !a.is_empty()) {
        failures.push("Source scope changes require review".into());
    }
    if !o.archived && working_tree["stale"] != false {
        failures.push(
            "Working tree is stale or its integrity is unavailable; rerun tests to refresh the map automatically"
                .into(),
        );
    }
    if o.require_complete
        && (s["inventoryMappingComplete"] != true || s["inventoryFailures"].as_u64() != Some(0))
    {
        failures.push(
            "Recognized assertion inventory is incomplete, partial or could not be parsed".into(),
        );
    }
    if o.require_observed
        && (s["unobservedAssertions"].as_u64() != Some(0)
            || report["assertions"].as_array().is_none_or(|rows| {
                rows.iter().any(|a| {
                    a["flows"].as_array().is_none_or(|flows| {
                        flows
                            .iter()
                            .any(|f| f["matchingTests"].as_array().is_none_or(Vec::is_empty))
                    })
                })
            }))
    {
        failures.push("Assertions or appliesTo flows lack matching passing runtime evidence; inspect report --view assertions".into());
    }
    if let Some(minimum) = o.minimum
        && s["statements"]["percentage"]
            .as_f64()
            .is_none_or(|n| n < minimum)
    {
        failures.push(format!(
            "Statement assertion coverage is below {minimum}% or has no measured denominator"
        ));
    }
    failures
}

pub fn global_command(args: &[String]) -> ExitCode {
    if args.is_empty() || args.iter().any(|a| matches!(a.as_str(), "--help" | "-h")) {
        print!("{HELP}");
        return ExitCode::SUCCESS;
    }
    let json_output = args.iter().any(|a| a == "--json");
    let args = args
        .iter()
        .filter(|a| a.as_str() != "--json")
        .map(String::as_str)
        .collect::<Vec<_>>();
    let result = match args.as_slice() {
        ["schema"] => Ok(supercov_engine::assertion_map::schema()),
        ["validate", "--file", file] => std::fs::read(file).map_err(|e| format!("{file}: {e}")).map(|bytes| {
            match supercov_engine::assertion_map::parse(&bytes) {
                Ok(_) => json!({"valid":true,"stage":"syntax","errors":[],"meaning":"JSON shape only; use runs <run> assertions validate for IDs, links and frozen source references"}),
                Err(error) => json!({"valid":false,"stage":"syntax","errors":[error]}),
            }
        }),
        _ => Err("Use assertions schema or assertions validate --file <path>; run-owned commands need runs <run> assertions".into()),
    };
    emit(result, json_output)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_ignored_or_ambiguous_options() {
        for args in [
            vec!["review"],
            vec!["report", "--from", "old"],
            vec!["init"],
            vec!["review", "--all", "--flow", "a/f"],
            vec!["source"],
            vec!["report", "--limit", "0"],
            vec!["report", "--limit", "20"],
            vec!["validate", "--min", "50"],
            vec!["review", "--require-complete"],
            vec!["check", "--min", "NaN"],
            vec!["check", "--min", "101"],
        ] {
            assert!(parse(&args.iter().map(|s| s.to_string()).collect::<Vec<_>>()).is_err());
        }
    }

    #[test]
    fn check_distinguishes_completion_evidence_and_archived_freshness() {
        let mut report = json!({"validationErrors":[],"assertions":[],"summary":{
            "runPassed":true,"dirtyFlows":0,"scopeReview":[],"inventoryMappingComplete":false,
            "inventoryFailures":0,"unobservedAssertions":1,"statements":{"percentage":50}
        }});
        let mut options = parse(&["check".into()]).unwrap();
        assert!(check_failures(&report, &json!({"stale":false}), &options).is_empty());
        options.require_complete = true;
        options.require_observed = true;
        options.minimum = Some(60.0);
        assert_eq!(
            check_failures(&report, &json!({"stale":true}), &options).len(),
            4
        );
        options.archived = true;
        assert_eq!(
            check_failures(&report, &json!({"stale":null}), &options).len(),
            3
        );
        report["summary"]["inventoryMappingComplete"] = json!(true);
        report["summary"]["unobservedAssertions"] = json!(0);
        options.minimum = Some(50.0);
        assert!(check_failures(&report, &json!({"stale":null}), &options).is_empty());
        report["summary"]["statements"]["percentage"] = Value::Null;
        assert_eq!(
            check_failures(&report, &json!({"stale":false}), &options).len(),
            1
        );
    }
}
