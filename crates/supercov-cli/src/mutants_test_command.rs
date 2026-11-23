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
    run_store::{StoredRun, discover_runs, select_run},
    strong_plan::{StrongPlan, StrongVerdicts, Verdict, plan_for_run},
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
  --format markdown the verdicts as a table for a pull request's comment,
                    in place of the summary
  --output <file>   write that table to a file
  -- <command>      run this command instead of the run's own

It exits 2, after its answer, where the run's command failed (a failing test
kills nothing, so the verdicts cannot gate) or no mutant could be run, and
refuses a run that recorded no test.

Accepted mutants: supercov.accepted.json at the project root, a list of
{ \"id\": <mutant id>, \"reason\": <why no test can tell it apart> }. An
accepted mutant still runs; it is shown as accepted and never fails the
command. An entry whose mutant is killed or gone is listed as stale.
";

#[derive(Debug, Default)]
struct Options {
    plan: bool,
    json: bool,
    /// Keep an earlier run's verdicts no change could move: `supercov
    /// mutants`, which measures the checkout again after an edit, does;
    /// `runs <id> mutants test` tests the run it names.
    carry: bool,
    /// `--format markdown`: the verdicts as a pull request's comment.
    markdown: bool,
    output: Option<PathBuf>,
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
            "--format" => {
                index += 1;
                match args.get(index).map(String::as_str) {
                    Some("markdown") => options.markdown = true,
                    other => {
                        return Err(format!(
                            "--format takes markdown, not {}",
                            other.unwrap_or("nothing")
                        ));
                    }
                }
            }
            "--output" => {
                index += 1;
                let path = args
                    .get(index)
                    .filter(|path| !path.is_empty())
                    .ok_or("--output needs a file")?;
                options.output = Some(PathBuf::from(path));
            }
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
    if options.output.is_some() && !options.markdown {
        return Err("--output writes the --format markdown table: give --format markdown".into());
    }
    if options.markdown && options.json {
        return Err("--format markdown and --json are two answers: give one".into());
    }
    if options.markdown && options.plan {
        return Err(
            "--format markdown lists a pass's verdicts, and --plan runs none: give one".into(),
        );
    }
    if options.markdown && options.files.len() > 1 {
        return Err("--format markdown takes one --file at most".into());
    }
    Ok(options)
}

pub(crate) const MUTANTS_HELP: &str = "\
Usage: supercov mutants [options] [-- <test command>]

Mutation-tests the project: lists the mutants of every source the tests ran,
as each language's established tool makes them (StrykerJS for JavaScript and
TypeScript; in preview, mutmut for Python, cargo-mutants for Rust, Gremlins
for Go and mutant for Ruby), runs each against only the tests that run its
code, and saves the verdicts (killed, survived, timeout) into the run.
It reads the latest run, and measures one first, with the latest run's test
command or the one after --, when there is none, the checkout changed since,
or it is a plain run (supercov -- <tests>), which does nothing for mutants
and costs nothing more for them. Its own run records the values the mutants'
expressions take, which spares the strong pass the tests that cannot tell a
mutant apart; while the checkout is unchanged, the next supercov mutants
reads that run again. After an edit, a mutant keeps its verdict where
neither its file nor any test that could kill it changed, and could reach
nothing that changed.

Options are those of `supercov runs <run-id> mutants test`; the ones asked
most:
  --diff <base>     only the mutants on lines the change since <base> touched
  --file <path>     only this source file's mutants (repeatable)
  --fail-on <s,...> exit 1 when an asked mutant ends survived, timeout,
                    no-coverage or error
  --format markdown the verdicts as a table for a pull request's comment
  --output <file>   write that table to a file
  --plan            print what would run, and run nothing
  --json            the answer as JSON

It exits 2, after its answer, where the measured command failed (a failing
test kills nothing) or no mutant could be run, and refuses a run that
recorded no test.
";

/// `supercov mutants [options] [-- <command>]`: the latest run's mutants
/// tested, measured first where there is no run of this checkout.
pub fn mutants_command(args: Vec<String>) -> ExitCode {
    if args.iter().any(|a| matches!(a.as_str(), "--help" | "-h")) {
        print!("{MUTANTS_HELP}");
        return ExitCode::SUCCESS;
    }
    // Every option is checked before anything runs: a mistake costs no
    // measured run.
    let options = match parse(&args) {
        Ok(options) => options,
        Err(message) => {
            return crate::refuse(
                "mutants",
                args.iter().any(|a| a == "--json"),
                message,
                MUTANTS_HELP,
            );
        }
    };
    let given = options.command.clone();
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
        // Failing tests still leave a run; the pass says what they cost.
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
    run_command(&forwarded, true)
}

/// Why `mutants test` stopped short of an answer.
enum Stop {
    /// Something went wrong: exit 1.
    Failed(String),
    /// The run cannot be mutation-tested as it is: exit 2.
    Refused(String),
    /// A signal ended the pass: its exit code. What it decided is saved.
    Interrupted(i32),
}

impl From<String> for Stop {
    fn from(message: String) -> Self {
        Self::Failed(message)
    }
}

/// `args` is `[<run-id>, "mutants", "test", ...]`.
pub fn command(args: &[String]) -> ExitCode {
    run_command(args, false)
}

/// `command`, keeping earlier verdicts where `carry`.
fn run_command(args: &[String], carry: bool) -> ExitCode {
    if args.iter().any(|a| matches!(a.as_str(), "--help" | "-h")) {
        print!("{HELP}");
        return ExitCode::SUCCESS;
    }
    let options = match parse(&args[3..]) {
        Ok(options) => Options { carry, ..options },
        Err(message) => {
            return crate::refuse(
                "runs.mutants.test",
                args.iter().any(|a| a == "--json"),
                message,
                HELP,
            );
        }
    };
    match answer(&args[0], &options) {
        Ok(data) => {
            let mut exit = if data["gate"]["failed"].as_bool() == Some(true) {
                ExitCode::from(1)
            } else {
                ExitCode::SUCCESS
            };
            let refused = data["result"] == "error";
            if options.json {
                println!("{}", serde_json::to_string(&data).expect("JSON"));
            } else if options.markdown {
                let printed = markdown(&data, &options);
                if printed != ExitCode::SUCCESS {
                    exit = printed;
                }
            } else {
                print!("{}", plan_text(&data));
                print!(
                    "{}",
                    survivors_text(data["run"].as_str().unwrap_or("latest"), &data["survivors"])
                );
                print!("{}", gate_text(&data["gate"]));
            }
            for reason in data["reasons"].as_array().into_iter().flatten() {
                eprintln!("[supercov] {}", reason.as_str().unwrap_or_default());
            }
            if refused { ExitCode::from(2) } else { exit }
        }
        Err(Stop::Refused(message)) => {
            crate::refuse("runs.mutants.test", options.json, message, "")
        }
        Err(Stop::Interrupted(code)) => {
            eprintln!(
                "[supercov] interrupted: the verdicts the pass reached are saved into the run, and the next pass runs the rest"
            );
            ExitCode::from(u8::try_from(code).unwrap_or(1))
        }
        Err(Stop::Failed(message)) => {
            if options.json {
                crate::refuse("runs.mutants.test", true, message, HELP)
            } else {
                eprintln!("[supercov] {message}");
                ExitCode::from(1)
            }
        }
    }
}

/// The pass over run `selector`, or `--plan`'s answer.
fn answer(selector: &str, options: &Options) -> Result<Value, Stop> {
    let root = std::env::current_dir().map_err(|e| e.to_string())?;
    let inventory = discover_runs(&root).map_err(|e| e.to_string())?;
    let run = select_run(&inventory, Some(selector)).map_err(|e| e.to_string())?;
    let mut plan = run_plan(&root, run)?;
    // A run with no test has nothing to run a mutant against: said, not
    // reported as a score of nothing killed.
    if plan.tests.is_empty() {
        return Err(Stop::Refused(no_test_message(
            &run.id,
            &run.metadata.command,
            run.metadata.test_exit_code,
            &plan,
        )));
    }
    let total = plan.mutants.len();
    narrow(&mut plan, &root, run, options)?;
    let narrowed = plan.clone();
    let mut saved = read_saved(run);
    let asked = plan
        .mutants
        .iter()
        .map(|mutant| (mutant.id.clone(), mutant.file.clone()))
        .collect::<Vec<_>>();
    // After an edit, the verdicts no change could have moved.
    let carried = if options.carry && !options.plan {
        carry_over(&root, &inventory.runs, run, &plan, &saved)
    } else {
        Vec::new()
    };
    // Saved with the pass's own verdicts.
    for (_, verdicts) in &carried {
        saved.extend(verdicts.clone());
    }
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
    let running = plan
        .mutants
        .iter()
        .filter(|m| m.decided.is_none())
        .map(|m| m.id.clone())
        .collect::<Vec<_>>();
    let to_run = running.len();
    let started = std::time::Instant::now();
    let executed = execute(&root, run, plan.clone(), &mut saved, options)?;
    let verdicts = read_saved(run);
    let mut data = summary(run, &narrowed, &asked, &verdicts, to_run, &executed);
    data["summary"]["durationMs"] = json!(started.elapsed().as_millis() as u64);
    data["scope"] = json!({
        "diff": options.diff,
        "files": options.files,
        "mutants": asked.len(),
        "of": total,
    });
    if !carried.is_empty() {
        data["carried"] = json!({
            "from": carried.iter().map(|(from, _)| from).collect::<Vec<_>>(),
            "mutants": carried.iter().map(|(_, verdicts)| verdicts.len()).sum::<usize>(),
        });
    }
    data["gate"] = gate(&root, &asked, &verdicts, options)?;
    data["survivors"] = survivors(run, &asked, &verdicts);
    // What keeps this pass from gating, as `runs patch` says of a failed
    // suite: the answer stands, and the command exits 2.
    let mut reasons = Vec::new();
    if let Some(failed) = failed_command(&run.id, run.metadata.test_exit_code, &narrowed) {
        reasons.push(failed);
    }
    // A pass that had mutants to run and ran none of them.
    let ran_any = running.iter().any(|id| {
        verdicts
            .get(id)
            .is_some_and(|verdict| verdict.status != "error")
    });
    if to_run > 0 && !ran_any {
        reasons.push(format!(
            "no mutant of the {to_run} this pass had to run could be run: each is an error, said above"
        ));
    }
    if !reasons.is_empty() {
        data["result"] = json!("error");
        data["reasons"] = json!(reasons);
    }
    Ok(data)
}

/// Why a run that recorded no test is refused.
fn no_test_message(
    run: &str,
    command: &[String],
    exit_code: Option<i32>,
    plan: &StrongPlan,
) -> String {
    let reached = plan
        .mutants
        .iter()
        .filter(|mutant| mutant.weak != "not-reached" && mutant.weak != "equivalent")
        .count();
    let runner = runner_of_command(command);
    let command = supercov_engine::javascript_run::shell_words(command);
    let exited = match exit_code {
        Some(0) | None => String::new(),
        Some(code) => format!(", and exited {code}"),
    };
    if reached == 0 {
        return format!(
            "run {run} recorded no test: its command ({command}) ran none{exited}, so there is nothing to run its {} mutant(s) against",
            plan.mutants.len()
        );
    }
    format!(
        "run {run} recorded no test: its command ({command}) ran the code of {reached} mutant(s){exited}, but no runner Supercov drives reported a test there, so none can be run against one\n  {reached} mutant(s) not run: no strong driver for {runner}"
    )
}

/// The runner a command starts, by name, for a run no known runner
/// reported: `npx tape 'test/**/*.js'` is tape's.
fn runner_of_command(command: &[String]) -> String {
    let mut words = command.iter().map(String::as_str);
    while let Some(word) = words.next() {
        let name = word.rsplit(['/', '\\']).next().unwrap_or(word);
        let name = name.strip_suffix(".cmd").unwrap_or(name);
        match name {
            // An option, or a variable set for the command.
            _ if word.starts_with('-') || word.contains('=') => {}
            "npx" | "bunx" | "pnpx" | "exec" | "env" | "node" | "bun" | "deno" | "dlx" => {}
            // A package script: the command is its name.
            "test" | "t" | "run" | "run-script" => return command.join(" "),
            "npm" | "pnpm" | "yarn" => {}
            "sh" | "bash" | "zsh" => {
                let script = words
                    .find(|word| !word.starts_with('-'))
                    .unwrap_or_default();
                let inner = script
                    .split_whitespace()
                    .map(str::to_owned)
                    .collect::<Vec<_>>();
                if inner.is_empty() {
                    break;
                }
                return runner_of_command(&inner);
            }
            _ => return name.to_owned(),
        }
    }
    command.join(" ")
}

/// Where the run's command failed: what it cost, in one sentence. A test that
/// failed is no candidate, so its mutants ran against the others, and a
/// pass over a failing suite cannot gate.
fn failed_command(run: &str, code: Option<i32>, plan: &StrongPlan) -> Option<String> {
    if code == Some(0) {
        return None;
    }
    let not_passed = plan
        .tests
        .iter()
        .filter(|test| !test.runnable)
        .map(|test| test.name.as_str())
        .collect::<Vec<_>>();
    let exited = code.map_or_else(
        || "ended without an exit status".to_owned(),
        |code| format!("exited {code}"),
    );
    let tests = match not_passed.len() {
        0 => "no test it reported failed".to_owned(),
        count => format!(
            "{count} of its {} test(s) did not pass ({}{})",
            plan.tests.len(),
            not_passed
                .iter()
                .take(3)
                .copied()
                .collect::<Vec<_>>()
                .join(", "),
            if count > 3 { ", ..." } else { "" }
        ),
    };
    Some(format!(
        "run {run}'s command {exited}: {tests}. A test that does not pass kills nothing, so these verdicts are the passing tests' alone, and cannot gate; fix the suite and measure it again"
    ))
}

/// The answer as a pull request's comment, `runs <id> mutants --format
/// markdown`'s table over the mutants asked about.
fn markdown(data: &Value, options: &Options) -> ExitCode {
    let mut arguments = vec![
        data["run"].as_str().unwrap_or("latest").to_owned(),
        "mutants".to_owned(),
    ];
    arguments.extend(options.files.iter().cloned());
    if let Some(base) = &options.diff {
        arguments.extend(["--diff".to_owned(), base.clone()]);
    }
    arguments.extend(["--format".to_owned(), "markdown".to_owned()]);
    if let Some(output) = &options.output {
        arguments.extend(["--output".to_owned(), output.display().to_string()]);
    }
    crate::mutants_export::command(&arguments)
}

fn run_plan(root: &Path, run: &StoredRun) -> Result<StrongPlan, String> {
    let planned = plan_for_run(run, root).map_err(|error| format!("{error:?}"))?;
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
        // Mutants that did not compile, and those no driver could run.
        let mut extra = match summary["unviable"].as_u64() {
            Some(unviable) if unviable > 0 => format!("  unviable      {unviable}\n"),
            _ => String::new(),
        };
        for (runner, count) in summary["notRun"].as_object().into_iter().flatten() {
            extra.push_str(&format!(
                "  {count} mutant(s) not run: no strong driver for {runner}{}\n",
                supercov_engine::strong_driver::platform_note(runner)
            ));
        }
        // The mutants asked about, where they are not the run's all.
        let scope = &data["scope"];
        let of = scope["of"].as_u64().unwrap_or(0);
        let asked = scope["mutants"].as_u64().unwrap_or(of);
        let mut narrowed = Vec::new();
        if let Some(base) = scope["diff"].as_str() {
            narrowed.push(format!("on lines changed since {base}"));
        }
        let files = scope["files"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>();
        if !files.is_empty() {
            narrowed.push(format!("in {}", files.join(", ")));
        }
        let header = if narrowed.is_empty() {
            String::new()
        } else {
            format!(" ({asked} of its {of} mutants, {})", narrowed.join(" and "))
        };
        if let Some(carried) = data["carried"]["mutants"].as_u64().filter(|n| *n > 0) {
            let from = data["carried"]["from"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>();
            extra.push_str(&format!(
                "  {carried} verdict(s) kept from run{} {}: nothing they depend on changed\n",
                if from.len() == 1 { "" } else { "s" },
                from.join(", ")
            ));
        }
        return format!(
            "Strong mutation for {}{header}: mutation score {score}\n  killed        {}\n  timeout       {}\n  survived      {}\n  no coverage   {}\n  equivalent    {}\n  error         {}\n{}  ran {} mutant(s) in {} ms\n",
            data["run"].as_str().unwrap_or(""),
            summary["killed"],
            summary["timeout"],
            summary["survived"],
            summary["noCoverage"],
            summary["equivalent"],
            summary["error"],
            extra,
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
    saved_document(run)
        .map(|saved| saved.mutants)
        .unwrap_or_default()
}

/// The run's saved verdicts as written, where it has any that read.
fn saved_document(run: &StoredRun) -> Option<StrongVerdicts> {
    std::fs::read(results_path(run))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<StrongVerdicts>(&bytes).ok())
}

/// The mutation score StrykerJS reports: killed and timed out over all
/// that are not equivalent or errors.
/// How many surviving mutants the pass summary names.
const SURVIVORS_SHOWN: usize = 10;

/// The asked mutants the tests let survive, as the weak listing shows them
/// (`file:line`, the change), the first `SURVIVORS_SHOWN`, and how many in
/// all. Empty where the run's index cannot be read.
fn survivors(
    run: &StoredRun,
    asked: &[(String, String)],
    verdicts: &std::collections::BTreeMap<String, Verdict>,
) -> Value {
    let survived = asked
        .iter()
        .filter(|(id, _)| {
            verdicts
                .get(id)
                .is_some_and(|verdict| verdict.status == "survived")
        })
        .map(|(id, _)| id.as_str())
        .collect::<BTreeSet<_>>();
    let options = supercov_engine::mutants_query::MutantsOptions {
        strong_statuses: vec!["survived".into()],
        strong: Some(
            verdicts
                .iter()
                .map(|(id, verdict)| {
                    (
                        id.clone(),
                        serde_json::to_value(verdict).expect("plain data"),
                    )
                })
                .collect(),
        ),
        ..Default::default()
    };
    // Shown as the listing shows them, where the run's index reads; else
    // only counted.
    let listed = crate::open_query_index_for(run, None)
        .ok()
        .and_then(|container| {
            supercov_engine::coverage_index::CoverageIndex::new(&container)
                .ok()
                .and_then(|index| {
                    supercov_engine::mutants_query::coverage_mutants_query(
                        &index,
                        supercov_engine::mutants_query::CoverageMutantsQueryOptions {
                            run: &run.id,
                            file: None,
                            line: None,
                            options: &options,
                            offset: 0,
                            limit: usize::MAX,
                        },
                    )
                    .ok()
                })
        })
        .map(|(data, _)| data.mutants)
        .unwrap_or_default();
    let shown = listed
        .iter()
        .filter(|mutant| survived.contains(mutant.id.as_str()))
        .take(SURVIVORS_SHOWN)
        .map(|mutant| {
            json!({
                "id": mutant.id,
                "place": format!("{}:{}", mutant.file, mutant.span.start[0]),
                "mutator": mutant.mutator,
                "change": crate::human_query::mutant_change(mutant),
            })
        })
        .collect::<Vec<_>>();
    json!({"total": survived.len(), "shown": shown})
}

/// The pass summary's last lines: the survivors, capped.
fn survivors_text(run: &str, survivors: &Value) -> String {
    let shown = survivors["shown"].as_array().cloned().unwrap_or_default();
    if shown.is_empty() {
        return String::new();
    }
    let mut text = String::from("Survived:\n");
    text.extend(shown.iter().map(|mutant| {
        format!(
            "  {}  {}  {}\n",
            mutant["place"].as_str().unwrap_or(""),
            mutant["mutator"].as_str().unwrap_or(""),
            mutant["change"].as_str().unwrap_or("")
        )
    }));
    let total = survivors["total"].as_u64().unwrap_or(0) as usize;
    if total > shown.len() {
        text.push_str(&format!(
            "  and {} more: supercov runs {run} mutants --status survived\n",
            total - shown.len()
        ));
    }
    text
}

/// The mutants a pass could not run because no driver runs their tests, by
/// the runners they ran under: an `error` none of whose candidates' runners
/// its driver runs.
fn not_run(
    plan: &StrongPlan,
    verdicts: &std::collections::BTreeMap<String, Verdict>,
) -> std::collections::BTreeMap<String, usize> {
    let mut not_run = std::collections::BTreeMap::new();
    for mutant in &plan.mutants {
        if verdicts
            .get(&mutant.id)
            .is_none_or(|verdict| verdict.status != "error")
        {
            continue;
        }
        let driver = supercov_engine::strong_driver::driver_of(mutant, &plan.tests);
        let runners = mutant
            .candidates
            .iter()
            .filter_map(|candidate| plan.tests.get(candidate.test))
            .map(|test| test.runner.as_str())
            .collect::<BTreeSet<_>>();
        if runners.is_empty()
            || runners
                .iter()
                .any(|runner| supercov_engine::strong_driver::drives(driver, runner))
        {
            continue;
        }
        let runners = runners.into_iter().collect::<Vec<_>>().join(", ");
        *not_run.entry(runners).or_insert(0) += 1;
    }
    not_run
}

fn score(verdicts: &std::collections::BTreeMap<String, Verdict>) -> Option<f64> {
    let count = |status: &str| verdicts.values().filter(|v| v.status == status).count();
    let detected = count("killed") + count("timeout");
    let total = detected + count("survived") + count("no-coverage");
    (total > 0).then(|| detected as f64 / total as f64)
}

/// How often a running pass says how far it got.
const PROGRESS_INTERVAL: std::time::Duration = std::time::Duration::from_secs(15);

/// How often a running pass's verdicts so far are saved into the run.
const SAVE_INTERVAL: std::time::Duration = std::time::Duration::from_secs(1);

/// Verdicts saved as a pass makes them: what it had decided before, and
/// where they go.
struct Saving {
    path: PathBuf,
    run: String,
    mutant_set: String,
    before: std::collections::BTreeMap<String, Verdict>,
}

/// While a pass runs: every second, the verdicts its driver wrote so far
/// saved into the run, so a pass that is interrupted or killed keeps them
/// and the next runs the rest; and every `PROGRESS_INTERVAL` (or
/// `SUPERCOV_PROGRESS_INTERVAL_MS`), a line on stderr with the verdicts so
/// far and the time left at this pace. Whatever the driver.
struct Progress {
    stop: std::sync::mpsc::Sender<()>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Progress {
    fn start(results: PathBuf, total: usize, saving: Saving) -> Self {
        let interval = std::env::var("SUPERCOV_PROGRESS_INTERVAL_MS")
            .ok()
            .and_then(|value| value.parse().ok())
            .map_or(PROGRESS_INTERVAL, std::time::Duration::from_millis);
        let (stop, stopped) = std::sync::mpsc::channel::<()>();
        let started = std::time::Instant::now();
        let thread = std::thread::spawn(move || {
            let mut said = std::time::Instant::now();
            let mut seen = 0;
            while let Err(std::sync::mpsc::RecvTimeoutError::Timeout) =
                stopped.recv_timeout(SAVE_INTERVAL.min(interval))
            {
                let verdicts = read_results(&results).0;
                if verdicts.len() > seen {
                    seen = verdicts.len();
                    let mut all = saving.before.clone();
                    all.extend(verdicts);
                    // A save that fails here is made again when the pass ends.
                    let _ = save_to(&saving.path, &saving.run, &saving.mutant_set, &all);
                }
                if said.elapsed() >= interval {
                    said = std::time::Instant::now();
                    eprintln!("{}", progress_line(&results, total, started.elapsed()));
                }
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

/// A pass's results so far: each mutant's verdict, and what its driver said
/// of itself.
fn read_results(results: &Path) -> (std::collections::BTreeMap<String, Verdict>, Vec<Value>) {
    let text = std::fs::read_to_string(results).unwrap_or_default();
    let mut verdicts = std::collections::BTreeMap::new();
    let mut passes = Vec::new();
    for line in text.lines() {
        // A line cut short by a driver still writing it says nothing yet.
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
            verdicts.insert(id.to_owned(), verdict);
        }
    }
    (verdicts, passes)
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
    save_to(&results_path(run), &run.id, mutant_set, saved)
}

/// Writes verdicts to `path` whole, then renames them into place: a pass
/// killed while it saves leaves the last whole save.
fn save_to(
    path: &Path,
    run: &str,
    mutant_set: &str,
    saved: &std::collections::BTreeMap<String, Verdict>,
) -> Result<(), String> {
    supercov_engine::strong_verdicts::save(
        path,
        &StrongVerdicts {
            schema_version: supercov_engine::strong_plan::STRONG_VERDICTS_SCHEMA_VERSION,
            run: run.to_owned(),
            mutant_set: mutant_set.to_owned(),
            mutants: saved.clone(),
        },
    )
}

/// What running a plan's mutants did, beside the verdicts it saved.
struct Executed {
    command_exit: Option<i32>,
    /// What each invocation's driver said of itself.
    passes: Vec<Value>,
}

/// A pass's exit codes that mean a signal ended it: Ctrl-C, a hang-up, a
/// termination.
fn interrupted(exit: i32) -> bool {
    matches!(exit, 129 | 130 | 143)
}

/// Runs the plan's mutants and saves every verdict into the run, as it
/// comes.
fn execute(
    root: &Path,
    run: &StoredRun,
    plan: StrongPlan,
    saved: &mut std::collections::BTreeMap<String, Verdict>,
    options: &Options,
) -> Result<Executed, Stop> {
    // Decided without running.
    for mutant in &plan.mutants {
        saved.extend(
            supercov_engine::strong_verdicts::decided_verdict(mutant)
                .map(|verdict| (mutant.id.clone(), verdict)),
        );
    }
    let to_run = plan.mutants.iter().filter(|m| m.decided.is_none()).count();
    let mut executed = Executed {
        command_exit: None,
        passes: Vec::new(),
    };
    if to_run > 0 {
        let command = options
            .command
            .clone()
            .unwrap_or_else(|| run.metadata.command.clone());
        if command.is_empty() {
            return Err(Stop::Failed(
                "the run recorded no command: give one after --".into(),
            ));
        }
        // What earlier passes left behind, the ones killed included.
        supercov_engine::strong_driver::prune_work_directories(root);
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
            // Each language's mutants go to its own driver, one after another
            // (Rust compiled to wasm under JavaScript tests to JavaScript's).
            let driver_of = |mutant: &supercov_engine::strong_plan::PlannedMutant| {
                supercov_engine::strong_driver::driver_of(mutant, &plan.tests).to_owned()
            };
            let languages = remaining.iter().map(driver_of).collect::<BTreeSet<_>>();
            // A pass with mutants left has a language of them at least.
            languages
                .into_iter()
                .try_for_each(|language| -> Result<(), Stop> {
                    let nonce = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map_or(0, |d| d.as_nanos());
                    let work = supercov_engine::strong_driver::work_directory(root, &run.id, nonce);
                    supercov_engine::strong_driver::claim_work_directory(&work)
                        .map_err(|e| format!("{}: {e}", work.display()))?;
                    let results = work.join("results.jsonl");
                    let ran = run_language(
                        root, run, &plan, &remaining, &language, &command, &work, saved,
                    );
                    let (verdicts, passes) = read_results(&results);
                    // Success or failure, the pass keeps only its logs.
                    if !supercov_engine::strong_driver::keeps_work_directories(std::env::var_os(
                        supercov_engine::strong_driver::KEEP,
                    )) {
                        supercov_engine::strong_driver::finish_work_directory(&work);
                    }
                    saved.extend(verdicts);
                    executed.passes.extend(passes);
                    remaining.retain(|mutant| !saved.contains_key(&mutant.id));
                    save(run, &plan.mutant_set, saved)?;
                    exit = ran?;
                    executed.command_exit = Some(exit);
                    if interrupted(exit) {
                        return Err(Stop::Interrupted(exit));
                    }
                    Ok(())
                })?;
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
    save(run, &plan.mutant_set, saved)?;
    Ok(executed)
}

/// One driver's share of the plan, run in `work`, its verdicts saved into
/// the run as they come.
#[allow(clippy::too_many_arguments)]
fn run_language(
    root: &Path,
    run: &StoredRun,
    plan: &StrongPlan,
    remaining: &[supercov_engine::strong_plan::PlannedMutant],
    language: &str,
    command: &[String],
    work: &Path,
    saved: &std::collections::BTreeMap<String, Verdict>,
) -> Result<i32, String> {
    let plan_path = work.join("plan.json");
    let results = work.join("results.jsonl");
    let mutants = remaining
        .iter()
        .filter(|mutant| supercov_engine::strong_driver::driver_of(mutant, &plan.tests) == language)
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
    let nonce = work
        .file_name()
        .and_then(|name| name.to_str())
        .and_then(|name| name.rsplit('-').next())
        .unwrap_or("0");
    let pass = supercov_engine::strong_driver::StrongPass {
        root: root.to_owned(),
        command: command.to_owned(),
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
    let progress = Progress::start(
        results,
        pass_plan.mutants.len(),
        Saving {
            path: results_path(run),
            run: run.id.clone(),
            mutant_set: plan.mutant_set.clone(),
            before: saved.clone(),
        },
    );
    let exit = supercov_engine::strong_driver::run_strong_pass(language, &pass, &mut diagnostics);
    drop(progress);
    exit
}

/// The answer's summary: the verdicts of the mutants asked about, not every
/// verdict the run holds (`--diff`'s change, `--file`'s files).
fn summary(
    run: &StoredRun,
    plan: &StrongPlan,
    asked: &[(String, String)],
    verdicts: &std::collections::BTreeMap<String, Verdict>,
    ran: usize,
    executed: &Executed,
) -> Value {
    let asked_verdicts = asked
        .iter()
        .filter_map(|(id, _)| Some((id.clone(), verdicts.get(id)?.clone())))
        .collect::<std::collections::BTreeMap<_, _>>();
    let count = |status: &str| {
        asked_verdicts
            .values()
            .filter(|v| v.status == status)
            .count()
    };
    json!({
        "run": run.id,
        "mutantSet": plan.mutant_set,
        "summary": {
            "mutants": asked_verdicts.len(),
            "ran": ran,
            "killed": count("killed"),
            "timeout": count("timeout"),
            "survived": count("survived"),
            "noCoverage": count("no-coverage"),
            "equivalent": count("equivalent"),
            "error": count("error"),
            // A mutant that does not compile (cargo-mutants' `unviable`):
            // counted apart, and left out of the score.
            "unviable": count("unviable"),
            "mutationScore": score(&asked_verdicts),
            // Errors because no driver runs their tests, by runner.
            "notRun": not_run(plan, &asked_verdicts),
            "commandExitCode": executed.command_exit,
        },
        "passes": executed.passes,
    })
}

/// Verdicts earlier runs of this checkout's project still give `run`'s
/// mutants, by mutant id, with each run's id, newest first: of the earlier
/// runs of the same command, dependencies, configuration and mutant set,
/// newest first, each gives the mutants still undecided what
/// `kept_verdicts` keeps of its verdicts. A strong pass split across
/// machines leaves each part's verdicts in its own run.
fn carry_over(
    root: &Path,
    runs: &[StoredRun],
    run: &StoredRun,
    plan: &StrongPlan,
    saved: &std::collections::BTreeMap<String, Verdict>,
) -> Vec<(String, std::collections::BTreeMap<String, Verdict>)> {
    let mut undecided = plan
        .mutants
        .iter()
        .filter(|mutant| mutant.decided.is_none() && !saved.contains_key(&mutant.id))
        .collect::<Vec<_>>();
    // The same command, Supercov and setup: what else a verdict rests on.
    let key = |of: &StoredRun| {
        let integrity = &of.metadata.integrity;
        (
            of.metadata.command.clone(),
            integrity.instrumenter_version.clone(),
            integrity.fingerprint.dependencies.clone(),
            integrity.fingerprint.configuration.clone(),
        )
    };
    let mut carried = Vec::new();
    // Newest first, as runs are listed. Once every mutant has a verdict,
    // the rest are not read: `supercov mutants` measures a run where the
    // checkout is not current, and an unchanged run decides nothing new.
    for earlier in runs.iter().filter(|earlier| {
        earlier.metadata.started_at < run.metadata.started_at && key(earlier) == key(run)
    }) {
        if undecided.is_empty() {
            break;
        }
        let Some(document) =
            saved_document(earlier).filter(|document| document.mutant_set == plan.mutant_set)
        else {
            continue;
        };
        // A run whose record of what each test ran is gone says nothing of
        // what a change since could reach.
        let Ok(affected) = supercov_engine::source_manifest::affected_tests(root, earlier) else {
            continue;
        };
        let kept = kept_verdicts(&undecided, &plan.tests, &document.mutants, &affected);
        if kept.is_empty() {
            continue;
        }
        undecided.retain(|mutant| !kept.contains_key(&mutant.id));
        carried.push((earlier.id.clone(), kept));
    }
    carried
}

/// The verdicts of `earlier` that no change since could move, by mutant id,
/// given what `tests affected` says of that run (`affected`): a mutant keeps
/// its verdict (killed, survived or timeout; an error runs again) where its
/// file is byte for byte as it was, and every candidate test it has now is
/// one the earlier run had and found unaffected, as is every test of their
/// files (they share the file's state) and of the files it runs whole. A
/// test file's edit reaches its tests, a source edit the tests that ran the
/// changed code; a new test that could kill it is a candidate the earlier
/// run did not have.
fn kept_verdicts(
    undecided: &[&supercov_engine::strong_plan::PlannedMutant],
    tests: &[supercov_engine::strong_plan::PlannedTest],
    earlier: &std::collections::BTreeMap<String, Verdict>,
    affected: &Value,
) -> std::collections::BTreeMap<String, Verdict> {
    let entries = |key: &str| {
        affected[key]
            .as_array()
            .into_iter()
            .flatten()
            .map(|entry| {
                (
                    entry["file"].as_str().unwrap_or_default().to_owned(),
                    entry["name"].as_str().unwrap_or_default().to_owned(),
                )
            })
            .collect::<Vec<_>>()
    };
    // A test file holding any test a change may have reached.
    let reached_files = entries("affected")
        .into_iter()
        .chain(entries("undetermined"))
        .map(|(file, _)| file)
        .collect::<BTreeSet<_>>();
    let unaffected = entries("unaffected").into_iter().collect::<BTreeSet<_>>();
    let changed = affected["changedFiles"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|entry| entry["file"].as_str().unwrap_or_default())
        .collect::<BTreeSet<_>>();
    let unchanged_test = |index: usize| {
        let test = &tests[index];
        let file = test.file.clone().unwrap_or_default();
        !reached_files.contains(&file) && unaffected.contains(&(file, test.name.clone()))
    };
    let mut kept = std::collections::BTreeMap::new();
    for mutant in undecided {
        let Some(verdict) = earlier.get(&mutant.id) else {
            continue;
        };
        if !matches!(verdict.status.as_str(), "killed" | "survived" | "timeout") {
            continue;
        }
        if changed.contains(mutant.file.as_str()) {
            continue;
        }
        if mutant
            .whole_files
            .iter()
            .any(|file| reached_files.contains(file))
        {
            continue;
        }
        if !mutant
            .candidates
            .iter()
            .all(|candidate| unchanged_test(candidate.test))
        {
            continue;
        }
        kept.insert(mutant.id.clone(), verdict.clone());
    }
    kept
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

    /// Mutants in `error` because no driver runs their tests are counted by
    /// runner, said in one line each, and left out of the score as every
    /// error is.
    #[test]
    fn mutants_no_driver_runs_are_said_and_left_out_of_the_score() {
        use supercov_engine::strong_plan::{
            Candidate, CandidateReason, PlannedMutant, PlannedTest,
        };
        let tests = ["jest", "phpunit", "vitest-browser"]
            .map(|runner| PlannedTest {
                id: runner.into(),
                file: None,
                name: runner.into(),
                title: None,
                runner: runner.into(),
                duration_ms: None,
                span: None,
                runnable: true,
            })
            .to_vec();
        let mutant = |id: &str, language: &str, candidates: &[usize]| PlannedMutant {
            id: id.into(),
            file: "a".into(),
            language: language.into(),
            index: 0,
            mutator: "M".into(),
            weak: "unknown",
            decided: None,
            reason: None,
            candidates: candidates
                .iter()
                .map(|test| Candidate {
                    test: *test,
                    reason: CandidateReason::Unknown,
                })
                .collect(),
            whole_files: Vec::new(),
            server_wide: None,
            covering: 0,
            statement: None,
            function: None,
            function_hit_by: Default::default(),
        };
        let plan = StrongPlan {
            mutant_set: "set".into(),
            tests,
            mutants: vec![
                mutant("php-1", "php", &[1]),
                mutant("php-2", "php", &[1]),
                mutant("wasm", "rust", &[2]),
                // An error of a mutant a driver runs, one with no test, and
                // a kill: none is a mutant no driver runs.
                mutant("js-error", "javascript", &[0]),
                mutant("no-test", "php", &[]),
                mutant("killed", "javascript", &[0]),
            ],
        };
        let verdict = |status: &str| Verdict {
            status: status.into(),
            killed_by: None,
            tests: 0,
            duration_ms: 0,
            reason: None,
        };
        let verdicts = ["php-1", "php-2", "wasm", "js-error", "no-test"]
            .map(|id| (id.to_owned(), verdict("error")))
            .into_iter()
            .chain([("killed".to_owned(), verdict("killed"))])
            .collect::<std::collections::BTreeMap<_, _>>();
        let counted = not_run(&plan, &verdicts);
        assert_eq!(
            counted,
            std::collections::BTreeMap::from([
                ("phpunit".to_owned(), 2),
                ("vitest-browser".to_owned(), 1)
            ])
        );
        // The score is the kill's alone.
        assert_eq!(score(&verdicts), Some(1.0));
        let text = plan_text(&json!({
            "run": "r1",
            "summary": {"killed": 1, "timeout": 0, "survived": 0, "noCoverage": 0, "equivalent": 0,
                "error": 5, "unviable": 0, "ran": 6, "durationMs": 1, "mutationScore": 1.0,
                "notRun": counted},
        }));
        assert!(
            text.contains("  2 mutant(s) not run: no strong driver for phpunit\n")
                && text.contains("  1 mutant(s) not run: no strong driver for vitest-browser\n"),
            "{text}"
        );
    }

    /// The runner a command starts, named where no runner Supercov knows
    /// reported a test: past launchers, options, variables and shells; a
    /// package script by the command itself.
    #[test]
    fn a_command_names_its_runner() {
        let named = |words: &[&str]| {
            runner_of_command(&words.iter().map(|w| (*w).to_owned()).collect::<Vec<_>>())
        };
        assert_eq!(named(&["npx", "-y", "tape", "test/**/*.js"]), "tape");
        assert_eq!(named(&["FOO=1", "node_modules/.bin/tape.cmd", "x"]), "tape");
        assert_eq!(named(&["--require=x", "env", "pnpm", "dlx", "uvu"]), "uvu");
        assert_eq!(named(&["npm", "test"]), "npm test");
        assert_eq!(named(&["sh", "-c", "BABEL_ENV=x npx tape t/"]), "tape");
        assert_eq!(named(&["bash", "-c", " "]), "bash -c  ");
        assert_eq!(named(&["node"]), "node");
        assert_eq!(named(&[]), "");
    }

    fn plan_of(tests: &[(&str, bool)], weak: &[&'static str]) -> StrongPlan {
        use supercov_engine::strong_plan::{PlannedMutant, PlannedTest};
        StrongPlan {
            mutant_set: "set".into(),
            tests: tests
                .iter()
                .map(|(name, runnable)| PlannedTest {
                    id: (*name).into(),
                    file: None,
                    name: (*name).into(),
                    title: None,
                    runner: "jest".into(),
                    duration_ms: None,
                    span: None,
                    runnable: *runnable,
                })
                .collect(),
            mutants: weak
                .iter()
                .map(|weak| PlannedMutant {
                    id: (*weak).into(),
                    file: "a.js".into(),
                    language: "javascript".into(),
                    index: 0,
                    mutator: "M".into(),
                    weak,
                    decided: None,
                    reason: None,
                    candidates: Vec::new(),
                    whole_files: Vec::new(),
                    server_wide: None,
                    covering: 0,
                    statement: None,
                    function: None,
                    function_hit_by: Default::default(),
                })
                .collect(),
        }
    }

    /// A run with no test is refused: by how many mutants' code its command
    /// ran and the runner that ran it, or as having run none.
    #[test]
    fn a_run_with_no_test_says_what_its_command_ran() {
        let tape = ["npx".to_owned(), "tape".to_owned(), "t/**/*.js".to_owned()];
        let plan = plan_of(&[], &["infected", "not-reached", "equivalent", "unknown"]);
        let said = no_test_message("r1", &tape, Some(0), &plan);
        assert_eq!(
            said,
            "run r1 recorded no test: its command (npx tape 't/**/*.js') ran the code of 2 mutant(s), but no runner Supercov drives reported a test there, so none can be run against one\n  2 mutant(s) not run: no strong driver for tape"
        );
        let plan = plan_of(&[], &["not-reached"]);
        assert_eq!(
            no_test_message("r1", &tape, Some(1), &plan),
            "run r1 recorded no test: its command (npx tape 't/**/*.js') ran none, and exited 1, so there is nothing to run its 1 mutant(s) against"
        );
        assert!(no_test_message("r1", &tape, None, &plan).contains("ran none, so"));
    }

    /// A failed command is said with the tests that did not pass, the first
    /// three of them; a passing one is not.
    #[test]
    fn a_failed_command_names_the_tests_that_did_not_pass() {
        let plan = plan_of(&[("a", false), ("b", true)], &[]);
        assert_eq!(failed_command("r1", Some(0), &plan), None);
        assert_eq!(
            failed_command("r1", Some(1), &plan).unwrap(),
            "run r1's command exited 1: 1 of its 2 test(s) did not pass (a). A test that does not pass kills nothing, so these verdicts are the passing tests' alone, and cannot gate; fix the suite and measure it again"
        );
        let four = plan_of(
            &[("a", false), ("b", false), ("c", false), ("d", false)],
            &[],
        );
        assert!(
            failed_command("r1", None, &four)
                .unwrap()
                .starts_with("run r1's command ended without an exit status: 4 of its 4 test(s) did not pass (a, b, c, ...)")
        );
        let none = plan_of(&[("a", true)], &[]);
        assert!(
            failed_command("r1", Some(3), &none)
                .unwrap()
                .contains("exited 3: no test it reported failed.")
        );
    }

    /// The summary's first line names the mutants asked about where they
    /// are not all the run's, and the verdicts kept from an earlier run.
    #[test]
    fn the_summary_names_what_was_asked_and_what_was_kept() {
        let summary = json!({"killed": 1, "timeout": 0, "survived": 0, "noCoverage": 0,
            "equivalent": 0, "error": 0, "unviable": 2, "ran": 1, "durationMs": 5,
            "mutationScore": null});
        let text = plan_text(&json!({
            "run": "r1", "summary": summary,
            "scope": {"diff": "main", "files": ["a.js", "b.js"], "mutants": 3, "of": 9},
            "carried": {"from": ["r0"], "mutants": 4},
        }));
        assert!(
            text.starts_with(
                "Strong mutation for r1 (3 of its 9 mutants, on lines changed since main and in a.js, b.js): mutation score -\n"
            ),
            "{text}"
        );
        assert!(text.contains("  unviable      2\n"), "{text}");
        assert!(
            text.contains("  4 verdict(s) kept from run r0: nothing they depend on changed\n"),
            "{text}"
        );
        let shards = plan_text(&json!({
            "run": "r3", "summary": summary,
            "carried": {"from": ["r2", "r1"], "mutants": 193},
        }));
        assert!(
            shards.contains(
                "  193 verdict(s) kept from runs r2, r1: nothing they depend on changed\n"
            ),
            "{shards}"
        );
        let whole = plan_text(&json!({"run": "r1", "summary": summary, "carried": {"mutants": 0}}));
        assert!(whole.starts_with("Strong mutation for r1: "), "{whole}");
        assert!(!whole.contains("kept from"), "{whole}");
    }

    /// An earlier verdict is kept where its mutant's file and every test
    /// that could kill it are as they were: not an error's, not a changed
    /// file's, not where a candidate, its file's other tests, or a file run
    /// whole was reached, nor where a candidate is new.
    #[test]
    fn a_verdict_is_kept_where_nothing_it_rests_on_changed() {
        use supercov_engine::strong_plan::{Candidate, CandidateReason, PlannedTest};
        let test = |file: &str, name: &str| PlannedTest {
            id: name.into(),
            file: Some(file.into()),
            name: name.into(),
            title: None,
            runner: "jest".into(),
            duration_ms: None,
            span: None,
            runnable: true,
        };
        let tests = vec![
            test("t/a.test.js", "a1"),
            test("t/b.test.js", "b1"),
            test("t/c.test.js", "c1"),
            test("t/a.test.js", "new"),
            PlannedTest {
                file: None,
                ..test("", "fileless")
            },
        ];
        let mut plan = plan_of(&[], &[]);
        let mutant = |id: &str, file: &str, candidates: &[usize], whole: &[&str]| {
            let mut planned = plan_of(&[], &["infected"]).mutants.remove(0);
            planned.id = id.into();
            planned.file = file.into();
            planned.candidates = candidates
                .iter()
                .map(|test| Candidate {
                    test: *test,
                    reason: CandidateReason::Infected,
                })
                .collect();
            planned.whole_files = whole.iter().map(|file| (*file).to_owned()).collect();
            planned
        };
        plan.mutants = vec![
            mutant("kept", "src/a.js", &[0], &[]),
            mutant("timeout-kept", "src/a.js", &[0], &["t/a.test.js"]),
            mutant("never-run", "src/a.js", &[0], &[]),
            mutant("error", "src/a.js", &[0], &[]),
            mutant("file-changed", "src/b.js", &[0], &[]),
            mutant("whole-reached", "src/a.js", &[0], &["t/b.test.js"]),
            mutant("candidate-reached", "src/a.js", &[1], &[]),
            mutant("file-mate-reached", "src/a.js", &[2], &[]),
            mutant("new-candidate", "src/a.js", &[3], &[]),
            mutant("fileless", "src/a.js", &[4], &[]),
        ];
        let verdict = |status: &str| Verdict {
            status: status.into(),
            killed_by: None,
            tests: 1,
            duration_ms: 1,
            reason: None,
        };
        let earlier = plan
            .mutants
            .iter()
            .filter(|mutant| mutant.id != "never-run")
            .map(|mutant| {
                let status = match mutant.id.as_str() {
                    "error" => "error",
                    "timeout-kept" => "timeout",
                    _ => "killed",
                };
                (mutant.id.clone(), verdict(status))
            })
            .collect();
        let affected = json!({
            "affected": [{"file": "t/b.test.js", "name": "b1"}],
            "undetermined": [{"file": "t/c.test.js", "name": "c2"}],
            "unaffected": [
                {"file": "t/a.test.js", "name": "a1"},
                {"file": "t/c.test.js", "name": "c1"},
                {"file": "", "name": "fileless"},
                {"name": "unnamed file"}
            ],
            "changedFiles": [{"file": "src/b.js"}, {"change": "no file"}],
        });
        let undecided = plan.mutants.iter().collect::<Vec<_>>();
        let kept = kept_verdicts(&undecided, &tests, &earlier, &affected);
        assert_eq!(
            kept.keys().map(String::as_str).collect::<Vec<_>>(),
            ["fileless", "kept", "timeout-kept"]
        );
        assert!(kept_verdicts(&undecided, &tests, &earlier, &json!({})).is_empty());
        assert!(kept_verdicts(&[], &tests, &earlier, &affected).is_empty());
    }

    /// The summary names the first survivors and counts the rest.
    #[test]
    fn the_survivors_beyond_those_shown_are_counted() {
        let shown =
            json!({"total": 12, "shown": [{"place": "a.js:1", "mutator": "M", "change": "x → y"}]});
        assert_eq!(
            survivors_text("r1", &shown),
            "Survived:\n  a.js:1  M  x → y\n  and 11 more: supercov runs r1 mutants --status survived\n"
        );
        assert_eq!(survivors_text("r1", &json!({"total": 0, "shown": []})), "");
    }

    #[test]
    fn a_signal_s_exit_code_is_an_interruption() {
        assert!([129, 130, 143].into_iter().all(interrupted));
        assert!(!interrupted(0) && !interrupted(1) && !interrupted(137));
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
        let parsed =
            |words: &[&str]| parse(&words.iter().map(|w| (*w).to_owned()).collect::<Vec<_>>());
        let markdown =
            parsed(&["--format", "markdown", "--output", "m.md", "--file", "a"]).unwrap();
        assert!(markdown.markdown && markdown.output == Some(PathBuf::from("m.md")));
        for (words, said) in [
            (
                &["--format", "html"][..],
                "--format takes markdown, not html",
            ),
            (&["--format"], "--format takes markdown, not nothing"),
            (&["--output"], "--output needs a file"),
            (
                &["--output", "m.md"],
                "--output writes the --format markdown table",
            ),
            (&["--format", "markdown", "--json"], "two answers"),
            (&["--format", "markdown", "--plan"], "--plan runs none"),
            (
                &["--format", "markdown", "--file", "a", "--file", "b"],
                "one --file at most",
            ),
        ] {
            let error = parsed(words).unwrap_err();
            assert!(error.contains(said), "{words:?}: {error}");
        }
    }
}
