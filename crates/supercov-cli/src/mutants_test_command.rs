//! `supercov runs <run-id> mutants test`: strong mutation over a measured
//! run (`docs/design/strong-mutation.md`). The run already says which
//! mutants no test infected and which tests infected the rest; this runs
//! each remaining mutant against those tests only, and saves the verdicts.
//!
//! `--plan` prints what would run and runs nothing.

use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
    process::ExitCode,
};

use serde_json::{Value, json};
use supercov_engine::{
    coverage_report::{ArchiveReportRequest, ExitCodeInput},
    js_strong::{StrongPlan, plan_for_archive},
    run_store::{StoredRun, discover_runs, select_run},
};

pub(crate) const HELP: &str = "\
Usage: supercov runs <run-id> mutants test [options] [-- <command>]

Runs the tests again against each mutant the run's tests infected, only the
tests that infected it, and saves the verdicts (killed, survived, timeout)
into the run. Mutants no test reached or infected are decided without
running anything.

Options:
  --plan            print what would run, and run nothing
  --file <path>     only this source file's mutants (repeatable)
  --diff <base>     only the mutants on lines the change since <base> touched
  --shard <k>/<n>   only the k-th of n even shares of the mutants to run (for
                    parallel jobs; decided mutants go to shard 1)
  --fail-on <s,...> exit 1 when an asked mutant ends survived, timeout,
                    no-coverage or error (accepted mutants excepted)
  --critical <glob> source files where every mutant must be killed or
                    accepted (repeatable); a survivor there fails the command
  --json            the answer as JSON

Accepted mutants: supercov.accepted.json at the project root, a list of
{ \"id\": <mutant id>, \"reason\": <why no test can tell it apart> }. An
accepted mutant still runs; it is shown as accepted and never fails the
command. An entry whose mutant is killed or gone is listed as stale.
  -- <command>      run this command instead of the run's own
";

#[derive(Debug, Default)]
struct Options {
    plan: bool,
    json: bool,
    files: Vec<String>,
    diff: Option<String>,
    fail_on: Vec<String>,
    critical: Vec<String>,
    shard: Option<(usize, usize)>,
    command: Option<Vec<String>>,
}

const UNDETECTED: &[&str] = &["survived", "timeout", "no-coverage", "error"];

fn parse(args: &[String]) -> Result<Options, String> {
    let mut options = Options::default();
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--plan" => options.plan = true,
            "--json" => options.json = true,
            "--file" => {
                index += 1;
                let file = args
                    .get(index)
                    .filter(|file| !file.is_empty())
                    .ok_or("--file needs a path")?;
                options.files.push(file.clone());
            }
            "--diff" => {
                index += 1;
                let base = args
                    .get(index)
                    .filter(|base| !base.is_empty())
                    .ok_or("--diff needs a base revision")?;
                options.diff = Some(base.clone());
            }
            "--fail-on" => {
                index += 1;
                let list = args.get(index).ok_or("--fail-on needs statuses")?;
                for status in list.split(',').filter(|s| !s.is_empty()) {
                    if !UNDETECTED.contains(&status) {
                        return Err(format!(
                            "--fail-on takes {}, not {status}",
                            UNDETECTED.join(", ")
                        ));
                    }
                    options.fail_on.push(status.to_owned());
                }
            }
            "--shard" => {
                index += 1;
                let value = args.get(index).ok_or("--shard needs k/n")?;
                let parsed = value
                    .split_once('/')
                    .and_then(|(k, n)| Some((k.parse::<usize>().ok()?, n.parse::<usize>().ok()?)))
                    .filter(|(k, n)| *n > 0 && (1..=*n).contains(k))
                    .ok_or_else(|| format!("--shard takes k/n with 1 <= k <= n, not {value}"))?;
                options.shard = Some(parsed);
            }
            "--critical" => {
                index += 1;
                let glob = args
                    .get(index)
                    .filter(|glob| !glob.is_empty())
                    .ok_or("--critical needs a glob")?;
                options.critical.push(glob.clone());
            }
            "--" => {
                let command = args[index + 1..].to_vec();
                if command.is_empty() {
                    return Err("-- needs a command after it".into());
                }
                options.command = Some(command);
                break;
            }
            other => return Err(format!("Unknown option: {other}")),
        }
        index += 1;
    }
    Ok(options)
}

/// `args` is `[<run-id>, "mutants", "test", ...]`.
pub fn command(args: &[String]) -> ExitCode {
    if args.iter().any(|a| matches!(a.as_str(), "--help" | "-h")) {
        print!("{HELP}");
        return ExitCode::SUCCESS;
    }
    let options = match parse(&args[3..]) {
        Ok(options) => options,
        Err(message) => {
            return crate::refuse(
                "runs.mutants.test",
                args.iter().any(|a| a == "--json"),
                message,
                HELP,
            );
        }
    };
    let result = (|| -> Result<Value, String> {
        let root = std::env::current_dir().map_err(|e| e.to_string())?;
        let inventory = discover_runs(&root).map_err(|e| e.to_string())?;
        let run = select_run(&inventory, Some(&args[0])).map_err(|e| e.to_string())?;
        let mut plan = run_plan(run)?;
        narrow(&mut plan, &root, run, &options)?;
        let saved = read_saved(run);
        let asked = plan
            .mutants
            .iter()
            .map(|mutant| (mutant.id.clone(), mutant.file.clone()))
            .collect::<Vec<_>>();
        // A second pass over the same run runs only what has no verdict.
        plan.mutants
            .retain(|mutant| !saved.contains_key(&mutant.id));
        // A shard: every n-th mutant to run, from the k-th; the decided ones
        // with the first shard.
        if let Some((k, n)) = options.shard {
            let mut position = 0usize;
            plan.mutants.retain(|mutant| {
                if mutant.decided.is_some() {
                    return k == 1;
                }
                position += 1;
                (position - 1) % n == k - 1
            });
        }
        if options.plan {
            return Ok(plan_json(run, &plan));
        }
        let mut data = execute(&root, run, plan, saved, &options)?;
        let verdicts = read_saved(run);
        data["gate"] = gate(&root, &asked, &verdicts, &options)?;
        Ok(data)
    })();
    match result {
        Ok(data) => {
            if options.json {
                println!("{}", serde_json::to_string(&data).expect("JSON"));
            } else {
                print!("{}", plan_text(&data));
                print!("{}", gate_text(&data["gate"]));
            }
            if data["gate"]["failed"].as_bool() == Some(true) {
                ExitCode::from(1)
            } else {
                ExitCode::SUCCESS
            }
        }
        Err(message) => {
            if options.json {
                crate::refuse("runs.mutants.test", true, message, HELP)
            } else {
                eprintln!("[supercov] {message}");
                ExitCode::from(1)
            }
        }
    }
}

fn run_plan(run: &StoredRun) -> Result<StrongPlan, String> {
    plan_for_archive(&ArchiveReportRequest {
        archive_path: run.evidence_path.clone(),
        run_id: run.id.clone(),
        generated_at: run.metadata.started_at.clone(),
        integrity: Some(serde_json::to_value(&run.metadata.integrity).expect("plain data")),
        test_exit_code: ExitCodeInput::Present(run.metadata.test_exit_code),
        test_filter: None,
    })
    .map_err(|error| format!("{error:?}"))?
    .ok_or_else(|| {
        format!(
            "run {} listed no mutants: measure it again without SUPERCOV_MUTANTS=0",
            run.id
        )
    })
}

/// Keeps the mutants `--file` and `--diff` ask about.
fn narrow(
    plan: &mut StrongPlan,
    root: &Path,
    run: &StoredRun,
    options: &Options,
) -> Result<(), String> {
    if !options.files.is_empty() {
        let files = options.files.iter().collect::<BTreeSet<_>>();
        plan.mutants.retain(|mutant| files.contains(&mutant.file));
    }
    if let Some(base) = &options.diff {
        let diff = crate::mutant_diff(root, base, run)?;
        plan.mutants.retain(|mutant| {
            if diff.untracked.contains(&mutant.file) {
                return true;
            }
            let Some(lines) = diff.changed.get(&mutant.file) else {
                return false;
            };
            let (start, end) = span_lines(&mutant.id);
            lines.range(start..=end).next().is_some()
        });
    }
    Ok(())
}

/// The first and last line of a mutant id's span
/// (`file:startLine:startCol-endLine:endCol:Mutator:descriptor`).
fn span_lines(id: &str) -> (u32, u32) {
    // The file may hold colons; the span is the first `L:C-L:C` after it.
    let parts = id.split(':').collect::<Vec<_>>();
    for window in 0..parts.len().saturating_sub(2) {
        if let (Ok(start), Some((_, end_line))) = (
            parts[window].parse::<u32>(),
            parts[window + 1].split_once('-'),
        ) && let Ok(end) = end_line.parse::<u32>()
        {
            return (start, end);
        }
    }
    (0, u32::MAX)
}

fn plan_json(run: &StoredRun, plan: &StrongPlan) -> Value {
    let mut counts = json!({"run": 0, "survived": 0, "noCoverage": 0, "equivalent": 0});
    let mut runs = 0u64;
    for mutant in &plan.mutants {
        let key = match mutant.decided {
            None => {
                runs += mutant.candidates.len() as u64;
                "run"
            }
            Some(supercov_engine::js_strong::Decided::Survived) => "survived",
            Some(supercov_engine::js_strong::Decided::NoCoverage) => "noCoverage",
            Some(supercov_engine::js_strong::Decided::Equivalent) => "equivalent",
        };
        counts[key] = json!(counts[key].as_u64().unwrap_or(0) + 1);
    }
    json!({
        "run": run.id,
        "mutantSet": plan.mutant_set,
        "summary": {
            "mutants": plan.mutants.len(),
            "toRun": counts["run"],
            "decided": {
                "survived": counts["survived"],
                "noCoverage": counts["noCoverage"],
                "equivalent": counts["equivalent"],
            },
            "candidateRuns": runs,
            "tests": plan.tests.len(),
        },
        "tests": plan.tests,
        "mutants": plan.mutants,
    })
}

fn plan_text(data: &Value) -> String {
    let summary = &data["summary"];
    if !summary["killed"].is_null() {
        let score = summary["mutationScore"]
            .as_f64()
            .map_or("-".to_owned(), |score| format!("{:.1}%", score * 100.0));
        return format!(
            "Strong mutation for {}: mutation score {score}\n  killed        {}\n  timeout       {}\n  survived      {}\n  no coverage   {}\n  equivalent    {}\n  error         {}\n  ran {} mutant(s) in {} ms\n",
            data["run"].as_str().unwrap_or(""),
            summary["killed"],
            summary["timeout"],
            summary["survived"],
            summary["noCoverage"],
            summary["equivalent"],
            summary["error"],
            summary["ran"],
            summary["durationMs"],
        );
    }
    format!(
        "Strong mutation plan for {}: {} mutants\n  to run        {} (at most {} test runs)\n  survived      {} (no test infected them)\n  no coverage   {}\n  equivalent    {}\n",
        data["run"].as_str().unwrap_or(""),
        summary["mutants"],
        summary["toRun"],
        summary["candidateRuns"],
        summary["decided"]["survived"],
        summary["decided"]["noCoverage"],
        summary["decided"]["equivalent"],
    )
}

/// `supercov.accepted.json`: mutants a person judged equivalent, by id,
/// with the reason.
fn read_accepted(root: &Path) -> Result<std::collections::BTreeMap<String, String>, String> {
    let path = root.join("supercov.accepted.json");
    let Ok(bytes) = std::fs::read(&path) else {
        return Ok(Default::default());
    };
    let entries = serde_json::from_slice::<Vec<Value>>(&bytes)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    let mut accepted = std::collections::BTreeMap::new();
    for entry in entries {
        let id = entry["id"]
            .as_str()
            .ok_or_else(|| format!("{}: an entry has no id", path.display()))?;
        let reason = entry["reason"].as_str().unwrap_or("").trim();
        if reason.is_empty() {
            return Err(format!(
                "{}: {id} is accepted without a reason; say why no test can tell it apart",
                path.display()
            ));
        }
        accepted.insert(id.to_owned(), reason.to_owned());
    }
    Ok(accepted)
}

/// Whether `file` matches a `--critical` glob (`*` within a segment, `**`
/// across them).
fn critical(file: &str, globs: &[String]) -> bool {
    fn matches(pattern: &[u8], text: &[u8]) -> bool {
        match (pattern.first(), text.first()) {
            (None, None) => true,
            (Some(b'*'), _) if pattern.get(1) == Some(&b'*') => {
                let rest = pattern[2..].strip_prefix(b"/").unwrap_or(&pattern[2..]);
                (0..=text.len()).any(|at| matches(rest, &text[at..]))
            }
            (Some(b'*'), _) => {
                matches(&pattern[1..], text)
                    || (!text.is_empty() && text[0] != b'/' && matches(pattern, &text[1..]))
            }
            (Some(&p), Some(&t)) if p == b'?' && t != b'/' || p == t => {
                matches(&pattern[1..], &text[1..])
            }
            _ => false,
        }
    }
    globs
        .iter()
        .any(|glob| matches(glob.as_bytes(), file.as_bytes()))
}

/// The gate over the mutants asked about.
fn gate(
    root: &Path,
    asked: &[(String, String)],
    verdicts: &std::collections::BTreeMap<String, Verdict>,
    options: &Options,
) -> Result<Value, String> {
    let accepted = read_accepted(root)?;
    let mut failing = Vec::new();
    let mut accepted_shown = Vec::new();
    for (id, file) in asked {
        let Some(verdict) = verdicts.get(id) else {
            continue;
        };
        if !UNDETECTED.contains(&verdict.status.as_str()) {
            continue;
        }
        if let Some(reason) = accepted.get(id) {
            accepted_shown.push(json!({"id": id, "status": verdict.status, "reason": reason}));
            continue;
        }
        let in_critical = critical(file, &options.critical);
        if in_critical || options.fail_on.contains(&verdict.status) {
            failing.push(json!({
                "id": id,
                "status": verdict.status,
                "critical": in_critical,
                "reason": verdict.reason,
            }));
        }
    }
    let stale = accepted
        .keys()
        .filter(|id| {
            verdicts
                .get(*id)
                .is_none_or(|verdict| !UNDETECTED.contains(&verdict.status.as_str()))
        })
        .cloned()
        .collect::<Vec<_>>();
    Ok(json!({
        "failed": !failing.is_empty(),
        "failing": failing,
        "accepted": accepted_shown,
        "staleAccepted": stale,
    }))
}

fn gate_text(gate: &Value) -> String {
    let mut text = String::new();
    for entry in gate["failing"].as_array().into_iter().flatten() {
        text.push_str(&format!(
            "  {} {}{}\n",
            entry["status"].as_str().unwrap_or(""),
            entry["id"].as_str().unwrap_or(""),
            if entry["critical"].as_bool() == Some(true) {
                " (critical)"
            } else {
                ""
            }
        ));
    }
    let stale = gate["staleAccepted"].as_array().map_or(0, Vec::len);
    if stale > 0 {
        text.push_str(&format!(
            "  {stale} accepted mutant(s) in supercov.accepted.json are killed or gone: remove them\n"
        ));
    }
    text
}

/// Where a run's strong verdicts are saved.
pub(crate) fn results_path(run: &StoredRun) -> PathBuf {
    run.directory.join("strong-mutation.json")
}

/// One mutant's strong verdict, as saved.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Verdict {
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub killed_by: Option<String>,
    #[serde(default)]
    pub tests: u64,
    #[serde(default)]
    pub duration_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// The run's saved verdicts, by mutant id; empty when none were saved.
pub(crate) fn read_saved(run: &StoredRun) -> std::collections::BTreeMap<String, Verdict> {
    std::fs::read(results_path(run))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
        .and_then(|value| serde_json::from_value(value["mutants"].clone()).ok())
        .unwrap_or_default()
}

/// The mutation score StrykerJS reports: killed and timed out over all
/// that are not equivalent or errors.
fn score(verdicts: &std::collections::BTreeMap<String, Verdict>) -> Option<f64> {
    let count = |status: &str| verdicts.values().filter(|v| v.status == status).count();
    let detected = count("killed") + count("timeout");
    let total = detected + count("survived") + count("no-coverage");
    (total > 0).then(|| detected as f64 / total as f64)
}

/// Runs the plan's mutants and saves every verdict into the run.
fn execute(
    root: &Path,
    run: &StoredRun,
    plan: StrongPlan,
    mut saved: std::collections::BTreeMap<String, Verdict>,
    options: &Options,
) -> Result<Value, String> {
    use supercov_engine::js_strong::Decided;
    let started = std::time::Instant::now();
    // Decided without running.
    for mutant in &plan.mutants {
        let status = match mutant.decided {
            Some(Decided::Survived) => "survived",
            Some(Decided::NoCoverage) => "no-coverage",
            Some(Decided::Equivalent) => "equivalent",
            None => continue,
        };
        saved.insert(
            mutant.id.clone(),
            Verdict {
                status: status.into(),
                killed_by: None,
                tests: 0,
                duration_ms: 0,
                reason: mutant.reason.clone(),
            },
        );
    }
    let to_run = plan.mutants.iter().filter(|m| m.decided.is_none()).count();
    let mut command_exit = None;
    if to_run > 0 {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let work = root
            .join(".supercov/strong")
            .join(format!("{}-{nonce}", run.id));
        std::fs::create_dir_all(&work).map_err(|e| format!("{}: {e}", work.display()))?;
        let plan_path = work.join("plan.json");
        let results = work.join("results.jsonl");
        std::fs::write(
            &plan_path,
            serde_json::to_vec(&plan).map_err(|e| e.to_string())?,
        )
        .map_err(|e| format!("{}: {e}", plan_path.display()))?;
        let command = options
            .command
            .clone()
            .unwrap_or_else(|| run.metadata.command.clone());
        if command.is_empty() {
            return Err("the run recorded no command: give one after --".into());
        }
        let request = supercov_engine::javascript_run::DirectJavascriptRunRequest {
            root: root.to_owned(),
            command,
            run_id: Some(format!("strong-{nonce}")),
            started_at: None,
            watchdog_program: std::env::current_exe()
                .ok()
                .and_then(|path| std::fs::canonicalize(path).ok()),
            strong: Some(supercov_engine::javascript_run::StrongRun {
                plan: plan_path,
                results: results.clone(),
            }),
        };
        let mut diagnostics = std::io::stderr();
        let exit =
            supercov_engine::javascript_run::run_strong_javascript(&request, &mut diagnostics)
                .map_err(|error| error.to_string())?;
        command_exit = Some(exit);
        let text = std::fs::read_to_string(&results).unwrap_or_default();
        for line in text.lines().filter(|line| !line.trim().is_empty()) {
            let Ok(record) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            let Some(id) = record["id"].as_str() else {
                continue;
            };
            if let Ok(verdict) = serde_json::from_value::<Verdict>(record.clone()) {
                saved.insert(id.to_owned(), verdict);
            }
        }
        // A mutant the driver gave no verdict did not run to its end.
        for mutant in plan.mutants.iter().filter(|m| m.decided.is_none()) {
            saved.entry(mutant.id.clone()).or_insert_with(|| Verdict {
                status: "error".into(),
                killed_by: None,
                tests: 0,
                duration_ms: 0,
                reason: Some(format!(
                    "the command ended (exit {exit}) before this mutant's verdict"
                )),
            });
        }
    }
    let document = json!({
        "schemaVersion": 1,
        "run": run.id,
        "mutantSet": plan.mutant_set,
        "mutants": saved,
    });
    let path = results_path(run);
    std::fs::write(
        &path,
        serde_json::to_vec_pretty(&document).map_err(|e| e.to_string())?,
    )
    .map_err(|e| format!("{}: {e}", path.display()))?;
    let count = |status: &str| saved.values().filter(|v| v.status == status).count();
    Ok(json!({
        "run": run.id,
        "mutantSet": plan.mutant_set,
        "summary": {
            "mutants": saved.len(),
            "ran": to_run,
            "killed": count("killed"),
            "timeout": count("timeout"),
            "survived": count("survived"),
            "noCoverage": count("no-coverage"),
            "equivalent": count("equivalent"),
            "error": count("error"),
            "mutationScore": score(&saved),
            "durationMs": started.elapsed().as_millis() as u64,
            "commandExitCode": command_exit,
        },
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn critical_globs_match_paths() {
        let globs = ["src/auth/**".to_owned(), "lib/*.ts".to_owned()];
        assert!(critical("src/auth/login.ts", &globs));
        assert!(critical("src/auth/deep/x.ts", &globs));
        assert!(critical("lib/money.ts", &globs));
        assert!(!critical("lib/deep/money.ts", &globs));
        assert!(!critical("src/other.ts", &globs));
    }

    #[test]
    fn a_span_s_lines_are_read_from_an_id_whose_file_has_colons() {
        assert_eq!(span_lines("src/a.ts:3:9-5:2:EqualityOperator:<="), (3, 5));
        assert_eq!(
            span_lines("c:/x/a.ts:12:1-12:7:BlockStatement:{}"),
            (12, 12)
        );
    }

    #[test]
    fn options_parse() {
        let args = [
            "--plan", "--file", "a.ts", "--diff", "main", "--", "npx", "vitest",
        ]
        .map(String::from);
        let options = parse(&args).unwrap();
        assert!(options.plan);
        assert!(parse(&["--fail-on".into(), "killed".into()]).is_err());
        assert_eq!(options.files, ["a.ts"]);
        assert_eq!(options.diff.as_deref(), Some("main"));
        assert_eq!(options.command.unwrap(), ["npx", "vitest"]);
        assert!(parse(&["--nope".into()]).is_err());
    }
}
