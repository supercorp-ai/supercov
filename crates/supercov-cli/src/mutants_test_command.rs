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
    run_store::{StoredRun, discover_runs, select_run},
    strong_plan::{StrongPlan, StrongVerdicts, Verdict, plan_for_archive},
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

pub(crate) const MUTANTS_HELP: &str = "\
Usage: supercov mutants [options] [-- <test command>]

Mutation-tests the project: lists the mutants of every JavaScript and
TypeScript source the tests ran, runs each against only the tests that run
its code, and saves the verdicts (killed, survived, timeout) into the run.
It reads the latest run, and measures one first, with the latest run's test
command or the one after --, when there is none, the checkout changed since,
or it is a plain run (supercov -- <tests>), which does nothing for mutants
and costs nothing more for them. Its own run records the values the mutants'
expressions take, which spares the strong pass the tests that cannot tell a
mutant apart; while the checkout is unchanged, the next supercov mutants
reads that run again.

Options are those of `supercov runs <run-id> mutants test`; the ones asked
most:
  --diff <base>     only the mutants on lines the change since <base> touched
  --file <path>     only this source file's mutants (repeatable)
  --fail-on <s,...> exit 1 when an asked mutant ends survived, timeout,
                    no-coverage or error
  --plan            print what would run, and run nothing
  --json            the answer as JSON
";

/// `supercov mutants [options] [-- <command>]`: the latest run's mutants
/// tested, measured first where there is no run of this checkout.
pub fn mutants_command(args: Vec<String>) -> ExitCode {
    if args.iter().any(|a| matches!(a.as_str(), "--help" | "-h")) {
        print!("{MUTANTS_HELP}");
        return ExitCode::SUCCESS;
    }
    let given = args
        .iter()
        .position(|a| a == "--")
        .map(|at| args[at + 1..].to_vec())
        .filter(|command| !command.is_empty());
    let root = match std::env::current_dir() {
        Ok(root) => root,
        Err(error) => {
            eprintln!("[supercov] {error}");
            return ExitCode::from(2);
        }
    };
    let latest = discover_runs(&root)
        .ok()
        .and_then(|inventory| select_run(&inventory, Some("latest")).ok().cloned());
    // A run of this checkout that recorded its mutants' values plans as it
    // is; otherwise one is measured with them: its probes cost a few percent
    // of the run, and spare the strong pass a quarter of its tests where
    // they decide what reach alone cannot.
    let stale = latest
        .as_ref()
        .is_none_or(|run| crate::checkout_comparison(&root, run).stale);
    let valued = !stale
        && latest.as_ref().is_some_and(|run| {
            supercov_engine::strong_plan::run_recorded_mutant_values(&run.evidence_path)
        });
    if !valued {
        let Some(command) = given
            .clone()
            .or_else(|| latest.as_ref().map(|run| run.metadata.command.clone()))
            .filter(|command| !command.is_empty())
        else {
            eprintln!(
                "[supercov] No run to read mutants from: give the test command, supercov mutants -- <test command>"
            );
            return ExitCode::from(2);
        };
        eprintln!(
            "[supercov] measuring first ({}): {}",
            if latest.is_none() {
                "no run yet"
            } else if stale {
                "the checkout changed since the latest run"
            } else {
                "the latest run recorded no values for its mutants"
            },
            command.join(" ")
        );
        // Measured as `supercov -- <command>` would be, its report on the
        // standard error: the standard output is this command's answer.
        // Failing tests still leave a run; their tests kill nothing.
        let measured = std::env::current_exe().and_then(|program| {
            use supercov_engine::fault_process::Launch;
            std::process::Command::new(program)
                .arg("--")
                .args(&command)
                .env("SUPERCOV_MUTANTS", "1")
                .stdout(std::io::stderr())
                .launch_status()
        });
        if let Err(error) = measured {
            eprintln!("[supercov] could not measure: {error}");
            return ExitCode::from(2);
        }
    }
    let mut forwarded = vec!["latest".to_owned(), "mutants".to_owned(), "test".to_owned()];
    forwarded.extend(args);
    command(&forwarded)
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
        let mut plan = run_plan(&root, run)?;
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
        refuse_without_a_driver(run, &plan)?;
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

/// A pass with mutants to run needs a test it can run: one of a runner a
/// strong driver drives. Without one every mutant would end without a
/// verdict, and the pass would read as a score of nothing killed.
fn refuse_without_a_driver(run: &StoredRun, plan: &StrongPlan) -> Result<(), String> {
    use supercov_engine::strong_driver::STRONG_RUNNERS;
    let runs_any = plan.mutants.iter().any(|mutant| mutant.decided.is_none());
    let drivable = plan
        .tests
        .iter()
        .any(|test| STRONG_RUNNERS.contains(&test.runner.as_str()));
    if !runs_any || drivable {
        return Ok(());
    }
    let runners = plan
        .tests
        .iter()
        .map(|test| test.runner.as_str())
        .filter(|runner| *runner != "background")
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>()
        .join(", ");
    Err(format!(
        "The strong pass runs tests under {}, and run {}'s tests ran under {runners}: it cannot run their mutants. `supercov runs {} mutants` lists them with what the run's own tests tell of each.",
        STRONG_RUNNERS.join(", "),
        run.id,
        run.id,
    ))
}

/// The run's plan, as `mutants test` makes it.
pub(crate) fn plan_of(root: &Path, run: &StoredRun) -> Result<StrongPlan, String> {
    run_plan(root, run)
}

fn run_plan(root: &Path, run: &StoredRun) -> Result<StrongPlan, String> {
    let planned = plan_for_archive(
        &ArchiveReportRequest {
            archive_path: run.evidence_path.clone(),
            run_id: run.id.clone(),
            generated_at: run.metadata.started_at.clone(),
            integrity: Some(serde_json::to_value(&run.metadata.integrity).expect("plain data")),
            test_exit_code: ExitCodeInput::Present(run.metadata.test_exit_code),
            test_filter: None,
        },
        root,
    )
    .map_err(|error| format!("{error:?}"))?;
    if !planned.changed.is_empty() {
        eprintln!(
            "[supercov] {} file(s) changed since run {}; their mutants are left out until it is measured again: {}",
            planned.changed.len(),
            run.id,
            planned.changed.join(", ")
        );
    }
    planned.plan.ok_or_else(|| {
        format!(
            "run {} listed no mutants and measured no source to list them from",
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
            Some(supercov_engine::strong_plan::Decided::Survived) => "survived",
            Some(supercov_engine::strong_plan::Decided::NoCoverage) => "noCoverage",
            Some(supercov_engine::strong_plan::Decided::Equivalent) => "equivalent",
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
    run.directory
        .join(supercov_engine::strong_plan::STRONG_VERDICTS_FILE)
}

/// The run's saved verdicts, by mutant id; empty when none were saved.
pub(crate) fn read_saved(run: &StoredRun) -> std::collections::BTreeMap<String, Verdict> {
    std::fs::read(results_path(run))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<StrongVerdicts>(&bytes).ok())
        .map(|saved| saved.mutants)
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

/// How often a running pass says how far it got.
const PROGRESS_INTERVAL: std::time::Duration = std::time::Duration::from_secs(15);

/// While a pass runs: every `PROGRESS_INTERVAL` (or
/// `SUPERCOV_PROGRESS_INTERVAL_MS`), a line on stderr with the verdicts so
/// far, read from the pass's results as its driver writes them, and the time
/// left at this pace. Whatever the driver, Vitest's or Playwright's.
struct Progress {
    stop: std::sync::mpsc::Sender<()>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Progress {
    fn start(results: PathBuf, total: usize) -> Self {
        let interval = std::env::var("SUPERCOV_PROGRESS_INTERVAL_MS")
            .ok()
            .and_then(|value| value.parse().ok())
            .map_or(PROGRESS_INTERVAL, std::time::Duration::from_millis);
        let (stop, stopped) = std::sync::mpsc::channel::<()>();
        let started = std::time::Instant::now();
        let thread = std::thread::spawn(move || {
            while let Err(std::sync::mpsc::RecvTimeoutError::Timeout) =
                stopped.recv_timeout(interval)
            {
                eprintln!("{}", progress_line(&results, total, started.elapsed()));
            }
        });
        Self {
            stop,
            thread: Some(thread),
        }
    }
}

impl Drop for Progress {
    fn drop(&mut self) {
        let _ = self.stop.send(());
        let _ = self.thread.take().map(std::thread::JoinHandle::join);
    }
}

/// How far a pass got: its verdicts so far of the `total` it runs.
fn progress_line(results: &Path, total: usize, elapsed: std::time::Duration) -> String {
    let text = std::fs::read_to_string(results).unwrap_or_default();
    let statuses = text
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(|record| record["id"].is_string())
        .map(|record| record["status"].as_str().unwrap_or_default().to_owned())
        .collect::<Vec<_>>();
    let count = |status: &str| statuses.iter().filter(|seen| *seen == status).count();
    let done = statuses.len();
    let left = if done > 0 {
        let rest = elapsed.as_millis() * total.saturating_sub(done) as u128 / done as u128;
        format!(
            "; about {} left",
            supercov_engine::process_supervision::format_duration(rest)
        )
    } else {
        String::new()
    };
    format!(
        "[supercov] strong pass: {done} of {total} mutants ({} killed, {} timed out, {} survived, {} errors), {}{left}",
        count("killed"),
        count("timeout"),
        count("survived"),
        count("error"),
        supercov_engine::process_supervision::format_duration(elapsed.as_millis()),
    )
}

/// How many passes `mutants test` makes at most: a pass that stopped early
/// leaves the rest to the next while each gets further.
const PASSES: usize = 6;

/// Writes the verdicts into the run.
fn save(
    run: &StoredRun,
    mutant_set: &str,
    saved: &std::collections::BTreeMap<String, Verdict>,
) -> Result<(), String> {
    let document = StrongVerdicts {
        schema_version: supercov_engine::strong_plan::STRONG_VERDICTS_SCHEMA_VERSION,
        run: run.id.clone(),
        mutant_set: mutant_set.to_owned(),
        mutants: saved.clone(),
    };
    let path = results_path(run);
    std::fs::write(
        &path,
        serde_json::to_vec_pretty(&document).expect("plain data serializes"),
    )
    .map_err(|e| format!("{}: {e}", path.display()))
}

/// Runs the plan's mutants and saves every verdict into the run.
fn execute(
    root: &Path,
    run: &StoredRun,
    plan: StrongPlan,
    mut saved: std::collections::BTreeMap<String, Verdict>,
    options: &Options,
) -> Result<Value, String> {
    use supercov_engine::strong_plan::Decided;
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
    // What each invocation's driver said of itself: a Playwright pass's
    // canaries, and how many mutants ran alone.
    let mut passes = Vec::new();
    if to_run > 0 {
        let command = options
            .command
            .clone()
            .unwrap_or_else(|| run.metadata.command.clone());
        if command.is_empty() {
            return Err("the run recorded no command: give one after --".into());
        }
        let mut remaining = plan
            .mutants
            .iter()
            .filter(|m| m.decided.is_none())
            .cloned()
            .collect::<Vec<_>>();
        // A language no driver runs yet: its mutants are errors, said once.
        let mut undriven = BTreeSet::new();
        remaining.retain(|mutant| {
            if supercov_engine::strong_driver::has_driver(&mutant.language) {
                return true;
            }
            undriven.insert(mutant.language.clone());
            saved.insert(
                mutant.id.clone(),
                Verdict {
                    status: "error".into(),
                    killed_by: None,
                    tests: 0,
                    duration_ms: 0,
                    reason: Some(format!(
                        "no strong mutation driver for {} yet",
                        mutant.language
                    )),
                },
            );
            false
        });
        for language in undriven {
            eprintln!(
                "[supercov] no strong mutation driver for {language} yet: its mutants are errors"
            );
        }
        // A pass that stops before its end (a test worker that would not
        // stop even when forced) leaves its verdicts; the next pass runs
        // what has none, while each pass gets further.
        let mut exit = 0;
        for _ in 0..PASSES {
            if remaining.is_empty() {
                break;
            }
            let before = remaining.len();
            // Each language's mutants go to its own driver, one after another.
            let languages = remaining
                .iter()
                .map(|mutant| mutant.language.clone())
                .collect::<BTreeSet<_>>();
            for language in languages {
                let nonce = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |d| d.as_nanos());
                let work = supercov_engine::strong_driver::work_directory(root, &run.id, nonce);
                std::fs::create_dir_all(&work).map_err(|e| format!("{}: {e}", work.display()))?;
                let plan_path = work.join("plan.json");
                let results = work.join("results.jsonl");
                let mutants = remaining
                    .iter()
                    .filter(|mutant| mutant.language == language)
                    .cloned()
                    .collect::<Vec<_>>();
                let pass_plan = StrongPlan {
                    mutant_set: plan.mutant_set.clone(),
                    tests: plan.tests.clone(),
                    mutants,
                };
                supercov_engine::fault_fs::write(
                    &plan_path,
                    serde_json::to_vec(&pass_plan).expect("plain data serializes"),
                )
                .map_err(|e| format!("{}: {e}", plan_path.display()))?;
                let pass = supercov_engine::strong_driver::StrongPass {
                    root: root.to_owned(),
                    command: command.clone(),
                    run_id: format!("strong-{nonce}"),
                    watchdog_program: std::env::current_exe()
                        .ok()
                        .and_then(|path| std::fs::canonicalize(path).ok()),
                    strong: supercov_engine::strong_driver::StrongRun {
                        plan: plan_path,
                        results: results.clone(),
                    },
                };
                let mut diagnostics = std::io::stderr();
                let progress = Progress::start(results.clone(), pass_plan.mutants.len());
                exit = supercov_engine::strong_driver::run_strong_pass(
                    &language,
                    &pass,
                    &mut diagnostics,
                )?;
                drop(progress);
                command_exit = Some(exit);
                let text = std::fs::read_to_string(&results).unwrap_or_default();
                for line in text.lines().filter(|line| !line.trim().is_empty()) {
                    let Ok(record) = serde_json::from_str::<Value>(line) else {
                        continue;
                    };
                    if record["summary"] == true {
                        passes.push(record);
                        continue;
                    }
                    let Some(id) = record["id"].as_str() else {
                        continue;
                    };
                    if let Ok(verdict) = serde_json::from_value::<Verdict>(record.clone()) {
                        saved.insert(id.to_owned(), verdict);
                    }
                }
                remaining.retain(|mutant| !saved.contains_key(&mutant.id));
                save(run, &plan.mutant_set, &saved)?;
            }
            if remaining.len() == before {
                break;
            }
        }
        // A mutant no pass gave a verdict did not run to its end.
        for mutant in &remaining {
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
    save(run, &plan.mutant_set, &saved)?;
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
        "passes": passes,
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
        // `?` is one character of a name, never a `/`.
        assert!(critical("src/cart.js", &["src/cart.j?".to_owned()]));
        assert!(!critical("src/cart.js", &["src?cart.js".to_owned()]));
        // A `*` that reached the end of the name with the glob left over.
        assert!(!critical("src/cart.js", &["src/*z".to_owned()]));
    }

    #[test]
    fn a_span_s_lines_are_read_from_an_id_whose_file_has_colons() {
        assert_eq!(span_lines("src/a.ts:3:9-5:2:EqualityOperator:<="), (3, 5));
        assert_eq!(
            span_lines("c:/x/a.ts:12:1-12:7:BlockStatement:{}"),
            (12, 12)
        );
        // An id with no span is read as spanning every line.
        assert_eq!(span_lines("src/a.ts"), (0, u32::MAX));
        assert_eq!(span_lines("src/a.ts:3:Mutator"), (0, u32::MAX));
        assert_eq!(span_lines("src/a.ts:3:9-x:Mutator"), (0, u32::MAX));
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
