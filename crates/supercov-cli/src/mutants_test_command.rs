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
    strong_plan::{
        PlannedMutant, Progress as Passed, StrongPlan, StrongVerdicts, Verdict, plan_for_run,
    },
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
  --shard <k>/<n>   only the k-th of n shares of the mutants to run, of about
                    the same cost (for parallel jobs; decided mutants go to
                    shard 1)
  --budget <s>      stop s seconds after the command started (with
                    supercov mutants, measuring included), the verdicts
                    reached saved; the next pass runs the rest (the
                    cheapest mutants run first)
  --timeout-factor <x>
                    a Playwright test run with a mutant is stopped, and the
                    mutant is a timeout, past x times the test's baseline
                    + 5 s (default 1.5, as StrykerJS's timeoutFactor;
                    SUPERCOV_STRONG_TIMEOUT_FACTOR)
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
    /// `--budget <seconds>`: how long the command may run, measuring,
    /// planning and the pass, from `before.started`.
    budget: Option<std::time::Duration>,
    /// What the command did before its pass.
    before: Before,
    /// `--timeout-factor <x>`: a Playwright attempt's time limit, as a
    /// multiple of its test's baseline (`TIMEOUT_FACTOR_VARIABLE` where
    /// not given).
    timeout_factor: Option<f64>,
    command: Option<Vec<String>>,
}

const UNDETECTED: &[&str] = &["survived", "timeout", "no-coverage", "error"];

/// What sets a Playwright attempt's time limit where `--timeout-factor` is
/// not given.
const TIMEOUT_FACTOR_VARIABLE: &str = "SUPERCOV_STRONG_TIMEOUT_FACTOR";
/// A Playwright attempt's time limit: its test's baseline × this + 5 s
/// (StrykerJS's `timeoutFactor` and `timeoutMS`).
const DEFAULT_TIMEOUT_FACTOR: f64 = 1.5;
/// How a verdict that ran past its time limit begins
/// (`playwrightStrong.mjs`).
const TIME_LIMIT_REASON: &str = "ran past its time limit";

/// A time limit's factor as given (`from` names where): a number above 0.
fn timeout_factor(value: &str, from: &str) -> Result<f64, String> {
    value
        .parse::<f64>()
        .ok()
        .filter(|factor| factor.is_finite() && *factor > 0.0)
        .ok_or_else(|| format!("{from} takes a number above 0, not {value}"))
}

/// The factor of the pass: `--timeout-factor`, else the variable, else the
/// default.
fn resolved_timeout_factor(given: Option<f64>, variable: Option<&str>) -> Result<f64, String> {
    match (given, variable.filter(|value| !value.is_empty())) {
        (Some(factor), _) => Ok(factor),
        (None, Some(value)) => timeout_factor(value, TIMEOUT_FACTOR_VARIABLE),
        (None, None) => Ok(DEFAULT_TIMEOUT_FACTOR),
    }
}

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
            "--budget" => {
                index += 1;
                let value = args.get(index).ok_or("--budget needs seconds")?;
                let seconds = value
                    .parse::<u64>()
                    .ok()
                    .filter(|seconds| *seconds > 0)
                    .ok_or_else(|| {
                        format!("--budget takes a whole number of seconds, not {value}")
                    })?;
                options.budget = Some(std::time::Duration::from_secs(seconds));
            }
            "--timeout-factor" => {
                index += 1;
                let value = args.get(index).ok_or("--timeout-factor needs a number")?;
                options.timeout_factor = Some(timeout_factor(value, "--timeout-factor")?);
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
reads that run again. With --shard, its verdicts go into a run of its own,
never one restored from another job: on an unchanged checkout it is made
from the newest run of it whose command passed, without measuring again.
A run of the same files keeps every verdict it holds. After an edit, a
mutant keeps its verdict where neither its file nor any test that could kill
it changed, and could reach nothing that changed. --budget counts from the
command's start: measuring spends it too.

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
    // The budget is the whole command's: measuring and planning spend it
    // too, so a job ends when its budget says.
    let started = std::time::Instant::now();
    let given = options.command.clone();
    let root = match std::env::current_dir() {
        Ok(root) => root,
        Err(error) => {
            eprintln!("[supercov] {error}");
            return ExitCode::from(2);
        }
    };
    let inventory = discover_runs(&root).ok();
    let latest = inventory
        .as_ref()
        .and_then(|inventory| select_run(inventory, Some("latest")).ok().cloned());
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
    // A shard's verdicts go into a run of its own. On an unchanged checkout
    // the latest run can be one restored from an earlier round, and every
    // job of this round would save into its own copy of it: copies of one
    // run share its id, and no merge can add them up. Its own run is made
    // from a run of this checkout whose command passed, with no measuring:
    // measuring again records what that run records.
    let own_run = options.shard.is_some() && !options.plan;
    let continued = match (&latest, &inventory) {
        (Some(latest), Some(inventory)) if own_run && !stale => {
            continued_run(inventory, latest, given.as_deref())
        }
        _ => None,
    };
    let mut before = Before {
        started: Some(started),
        ..Before::default()
    };
    if let Some(source) = continued {
        let (run_id, started_at) = crate::public_run_identity();
        if let Err(error) =
            supercov_engine::lifecycle::derive_run(&root, source, &run_id, &started_at)
        {
            eprintln!(
                "[supercov] could not make this shard's run from {}: {error}",
                source.id
            );
            return ExitCode::from(2);
        }
        eprintln!(
            "[supercov] not measuring: the checkout is unchanged since run {}, so this shard saves its verdicts into run {run_id}, made from it",
            source.id
        );
        before.derived_from = Some(source.id.clone());
    } else if !valued || own_run {
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
            } else if own_run {
                "a shard saves its verdicts into a run of its own, and no run of this checkout that recorded its mutants' values and whose command passed can be made into one"
            } else {
                "the latest run recorded no values for its mutants"
            },
            command.join(" ")
        );
        // Measured as `supercov -- <command>` would be, its report on the
        // standard error: the standard output is this command's answer.
        // Failing tests still leave a run; the pass says what they cost.
        let measuring = std::time::Instant::now();
        let measured = std::env::current_exe().and_then(|program| {
            use supercov_engine::fault_process::Launch;
            std::process::Command::new(program)
                .arg("--")
                .args(&command)
                .env("SUPERCOV_MUTANTS", "1")
                .stdout(std::io::stderr())
                // fault-site: mutants-test-command.mutants-command
                .launch_status()
        });
        if let Err(error) = measured {
            eprintln!("[supercov] could not measure: {error}");
            return ExitCode::from(2);
        }
        before.measuring = Some(measuring.elapsed());
    }
    let mut forwarded = vec!["latest".to_owned(), "mutants".to_owned(), "test".to_owned()];
    forwarded.extend(args);
    run_command(&forwarded, true, before)
}

/// What `supercov mutants` did before its pass: when the command started,
/// which its budget counts from, and how it came by the run the pass tests.
#[derive(Debug, Default, Clone)]
struct Before {
    started: Option<std::time::Instant>,
    /// How long measuring the run took, where one was measured.
    measuring: Option<std::time::Duration>,
    /// The run a shard's own run was made from, where it was not measured.
    derived_from: Option<String>,
}

/// The run a shard continues on a checkout unchanged since `latest`: the
/// newest run with `latest`'s fingerprint, not a merge, that recorded its
/// mutants' values and whose command passed -- the command given, where one
/// is. A run whose command failed planned without what its failing tests
/// would have run; measuring again may give a better one.
fn continued_run<'a>(
    inventory: &'a supercov_engine::run_store::RunInventory,
    latest: &StoredRun,
    given: Option<&[String]>,
) -> Option<&'a StoredRun> {
    let command = given.unwrap_or(&latest.metadata.command);
    let mut runs = inventory
        .runs
        .iter()
        .filter(|run| {
            run.metadata.merged != Some(true)
                && run.metadata.test_exit_code == Some(0)
                && run.metadata.command == command
                && run.metadata.integrity.fingerprint == latest.metadata.integrity.fingerprint
        })
        .collect::<Vec<_>>();
    runs.sort_by(|left, right| right.metadata.started_at.cmp(&left.metadata.started_at));
    runs.into_iter()
        .find(|run| supercov_engine::strong_plan::run_recorded_mutant_values(&run.evidence_path))
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
    let before = Before {
        started: Some(std::time::Instant::now()),
        ..Before::default()
    };
    run_command(args, false, before)
}

/// `command`, keeping earlier verdicts where `carry`.
fn run_command(args: &[String], carry: bool, before: Before) -> ExitCode {
    if args.iter().any(|a| matches!(a.as_str(), "--help" | "-h")) {
        print!("{HELP}");
        return ExitCode::SUCCESS;
    }
    let options = match parse(&args[3..]) {
        Ok(options) => Options {
            carry,
            before,
            ..options
        },
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
    let factor = resolved_timeout_factor(
        options.timeout_factor,
        std::env::var(TIMEOUT_FACTOR_VARIABLE).ok().as_deref(),
    )?;
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
    // How many candidates a first tier holds: the run's own kills say.
    let first_tier = supercov_engine::strong_tiers::size(&plan, &read_saved(run));
    narrow(&mut plan, &root, run, options)?;
    let narrowed = plan.clone();
    let mut saved = read_saved(run);
    // The candidates each mutant with no verdict already passed with it, in
    // passes over this run.
    let mut progress = saved_document(run)
        .map(|document| document.progress)
        .unwrap_or_default();
    let asked = plan
        .mutants
        .iter()
        .map(|mutant| (mutant.id.clone(), mutant.file.clone()))
        .collect::<Vec<_>>();
    // After an edit, the verdicts no change could have moved.
    let carried = if options.carry && !options.plan {
        let carried = carry_over(&root, &inventory.runs, run, &plan, &saved);
        add_progress(&mut progress, carried.progress.clone());
        carried
    } else {
        Carried::default()
    };
    // Saved with the pass's own verdicts.
    for (_, verdicts) in &carried.from {
        saved.extend(verdicts.clone());
    }
    settle_progress(&mut progress, &saved);
    // A second pass over the same run runs only what has no verdict.
    plan.mutants
        .retain(|mutant| !saved.contains_key(&mutant.id));
    // Each mutant resumes where earlier passes left it: only the candidates
    // it has not passed with yet run. One that passed every candidate
    // survived.
    let resumed = supercov_engine::strong_verdicts::resume(&mut plan, &progress);
    saved.extend(passed_every_candidate(&resumed));
    settle_progress(&mut progress, &saved);
    plan.mutants
        .retain(|mutant| !saved.contains_key(&mutant.id));
    if !options.plan
        && let Some(line) = resumed_line(&resumed)
    {
        eprintln!("{line}");
    }
    // A shard: the k-th of n shares of about the same cost; the decided
    // ones with the first shard.
    if let Some((k, n)) = options.shard {
        plan.keep_share(k, n);
    }
    // The cheapest first: a pass cut short has decided as many as it could.
    plan.cheapest_first();
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
    let executed = execute(
        &root,
        run,
        plan.clone(),
        &mut saved,
        &mut progress,
        options,
        factor,
        first_tier,
    )?;
    let verdicts = read_saved(run);
    let mut data = summary(run, &narrowed, &asked, &verdicts, to_run, &executed);
    data["summary"]["durationMs"] = json!(started.elapsed().as_millis() as u64);
    data["scope"] = json!({
        "diff": options.diff,
        "files": options.files,
        "mutants": asked.len(),
        "of": total,
    });
    if let (Some(left), Some(budget)) = (executed.budget_left, options.budget) {
        data["budget"] = json!({
            "seconds": budget.as_secs(),
            "left": left,
            // What the mutants left keep for the next pass.
            "resume": progress.len(),
            "passed": progress.values().map(BTreeSet::len).sum::<usize>(),
        });
    }
    if !resumed.is_empty() {
        data["resumed"] = resumed_json(&resumed);
    }
    // The mutants asked about that some candidates let live and the rest
    // have not run with yet: not survivors, and not in the score.
    let so_far = asked
        .iter()
        .filter(|(id, _)| !verdicts.contains_key(id))
        .filter_map(|(id, _)| progress.get(id))
        .filter(|passed| !passed.is_empty())
        .count();
    if so_far > 0 {
        data["survivedSoFar"] = json!({"mutants": so_far, "firstTier": first_tier});
    }
    let limited = asked
        .iter()
        .filter_map(|(id, _)| verdicts.get(id))
        .filter(|verdict| verdict.status == "timeout")
        .filter(|verdict| {
            verdict
                .reason
                .as_deref()
                .is_some_and(|reason| reason.starts_with(TIME_LIMIT_REASON))
        })
        .count();
    if limited > 0 {
        data["timeLimit"] = json!({"factor": factor, "constantMs": 5000, "timedOut": limited});
    }
    if !carried.from.is_empty() || !carried.not_kept.is_empty() {
        data["carried"] = json!({
            "from": carried.from.iter().map(|(from, _)| from).collect::<Vec<_>>(),
            "mutants": carried.from.iter().map(|(_, verdicts)| verdicts.len()).sum::<usize>(),
            "unchanged": carried.unchanged,
            "notKept": carried.not_kept,
        });
    }
    // How the pass came by its run: made from another without measuring,
    // or measured, and what that spent of the budget.
    if let Some(from) = &options.before.derived_from {
        data["measurement"] = json!({"derivedFrom": from});
    } else if let Some(measuring) = options.before.measuring {
        data["measurement"] = json!({"seconds": measuring.as_secs()});
    }
    if let Some(budget) = options.budget {
        data["budget"]["seconds"] = json!(budget.as_secs());
    }
    data["gate"] = gate(&root, &asked, &verdicts, options)?;
    data["survivors"] = survivors(run, &asked, &verdicts);
    // What keeps this pass from gating, as `runs patch` says of a failed
    // suite: the answer stands, and the command exits 2.
    let mut reasons = Vec::new();
    if let Some(failed) = failed_command(&run.id, run.metadata.test_exit_code, &narrowed) {
        reasons.push(failed);
    }
    // A pass that had mutants to run and ran none of them (where it had the
    // time to: a budget spent first leaves them for the next pass).
    let ran_any = running.iter().any(|id| {
        verdicts
            .get(id)
            .is_some_and(|verdict| verdict.status != "error")
    });
    if to_run > 0 && !ran_any && executed.budget_left.is_none() {
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

/// How a plan's module-level mutants had their candidates narrowed to the
/// tests that read their values: how many were, by how much, and how many
/// kept every test that loaded their module, their value read as a module
/// loaded. `None` where no mutant's value was read through its literal.
fn narrowing<'p>(mutants: impl Iterator<Item = &'p PlannedMutant>) -> Option<Value> {
    let (mut narrowed, mut escaped, mut before, mut after) = (0u64, 0u64, 0u64, 0u64);
    for mutant in mutants {
        let Some(narrowing) = mutant.narrowed else {
            continue;
        };
        if narrowing.escaped {
            escaped += 1;
            continue;
        }
        narrowed += 1;
        before += narrowing.from as u64;
        after += narrowing.to as u64;
    }
    (narrowed + escaped > 0).then(|| {
        json!({
            "mutants": narrowed,
            "candidatesBefore": before,
            "candidatesAfter": after,
            "escaped": escaped,
        })
    })
}

/// The lines that say how candidates were narrowed (`narrowing`).
fn narrowing_text(narrowing: &Value) -> String {
    let mut text = String::new();
    let narrowed = narrowing["mutants"].as_u64().unwrap_or(0);
    if narrowed > 0 {
        text.push_str(&format!(
            "  narrowed      {narrowed} module-level mutant(s) to the tests that read their value: {} candidate(s) where loading their module gave {}\n",
            narrowing["candidatesAfter"], narrowing["candidatesBefore"]
        ));
    }
    let escaped = narrowing["escaped"].as_u64().unwrap_or(0);
    if escaped > 0 {
        text.push_str(&format!(
            "  {escaped} module-level mutant(s) kept every test that loaded their module: their value was read while a module loaded\n"
        ));
    }
    text
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
            "narrowed": narrowing(plan.mutants.iter()),
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
        if let Some(left) = data["budget"]["left"].as_u64() {
            let resume = match data["budget"]["resume"].as_u64() {
                Some(resume) if resume > 0 => format!(
                    "; {resume} of them resume from the {} candidate(s) they passed",
                    data["budget"]["passed"]
                ),
                _ => String::new(),
            };
            extra.push_str(&format!(
                "  budget of {} s spent: {left} mutant(s) left without a verdict, for the next pass{resume}\n",
                data["budget"]["seconds"]
            ));
        }
        if let Some(so_far) = data["survivedSoFar"]["mutants"].as_u64() {
            extra.push_str(&format!(
                "  survived so far {so_far}: lived through some candidates (a first tier holds {}), the rest not run with yet; not survivors, and not in the score: the next pass runs the rest (supercov runs {} mutants --status survived-so-far)\n",
                data["survivedSoFar"]["firstTier"],
                data["run"].as_str().unwrap_or("")
            ));
        }
        if let Some(resumed) = data["resumed"]["mutants"].as_u64() {
            extra.push_str(&format!(
                "  {resumed} mutant(s) resumed where earlier passes left them: {} candidate run(s) they had passed were not run again\n",
                data["resumed"]["skipped"]
            ));
        }
        if let Some(limited) = data["timeLimit"]["timedOut"].as_u64() {
            extra.push_str(&format!(
                "  {limited} timeout(s) ran past their time limit: {}x the test's baseline + {} s\n",
                data["timeLimit"]["factor"],
                data["timeLimit"]["constantMs"].as_u64().unwrap_or(0) / 1000
            ));
        }
        if let Some(carried) = data["carried"]["mutants"].as_u64().filter(|n| *n > 0) {
            let from = data["carried"]["from"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>();
            extra.push_str(&format!(
                "  {carried} verdict(s) kept from run{} {}: {}\n",
                if from.len() == 1 { "" } else { "s" },
                from.join(", "),
                if data["carried"]["unchanged"] != true {
                    "nothing they depend on changed"
                } else if from.len() == 1 {
                    "the checkout is unchanged since it"
                } else {
                    "the checkout is unchanged since them"
                }
            ));
        }
        let mut not_kept = data["carried"]["notKept"]
            .as_object()
            .into_iter()
            .flatten()
            .map(|(reason, count)| (count.as_u64().unwrap_or(0), reason.as_str()))
            .collect::<Vec<_>>();
        if !not_kept.is_empty() {
            not_kept.sort_by(|left, right| right.0.cmp(&left.0).then(left.1.cmp(right.1)));
            extra.push_str(&format!(
                "  {} earlier verdict(s) not kept, so run again: {}\n",
                not_kept.iter().map(|(count, _)| count).sum::<u64>(),
                not_kept
                    .iter()
                    .map(|(count, reason)| format!("{count} {}", not_kept_said(reason)))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        extra.push_str(&narrowing_text(&summary["narrowed"]));
        if let Some(from) = data["measurement"]["derivedFrom"].as_str() {
            extra.push_str(&format!(
                "  not measured: the checkout is unchanged since run {from}, which this run was made from\n"
            ));
        }
        // Measuring that took half the budget or more: the pass had the rest.
        if let (Some(measuring), Some(budget)) = (
            data["measurement"]["seconds"].as_u64(),
            data["budget"]["seconds"].as_u64(),
        ) && measuring * 2 >= budget
        {
            extra.push_str(&format!(
                "  measuring took {measuring} s of the {budget} s budget, which leaves the pass {} s\n",
                budget.saturating_sub(measuring)
            ));
        }
        // The survivors that only change a log line, counted on their own.
        let survived = match data["survivors"]["logOnly"].as_u64() {
            Some(log_only) if log_only > 0 => {
                format!(
                    "{} ({log_only} only change a log line)",
                    summary["survived"]
                )
            }
            _ => summary["survived"].to_string(),
        };
        return format!(
            "Strong mutation for {}{header}: mutation score {score}\n  killed        {}\n  timeout       {}\n  survived      {}\n  no coverage   {}\n  equivalent    {}\n  error         {}\n{}  ran {} mutant(s) in {} ms\n",
            data["run"].as_str().unwrap_or(""),
            summary["killed"],
            summary["timeout"],
            survived,
            summary["noCoverage"],
            summary["equivalent"],
            summary["error"],
            extra,
            summary["ran"],
            summary["durationMs"],
        );
    }
    format!(
        "Strong mutation plan for {}: {} mutants\n  to run        {} (at most {} test runs)\n  survived      {} (no test infected them)\n  no coverage   {}\n  equivalent    {}\n{}",
        data["run"].as_str().unwrap_or(""),
        summary["mutants"],
        summary["toRun"],
        summary["candidateRuns"],
        summary["decided"]["survived"],
        summary["decided"]["noCoverage"],
        summary["decided"]["equivalent"],
        narrowing_text(&summary["narrowed"]),
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

/// What the run's listings show of its strong pass, by mutant id: each
/// verdict, and each mutant some candidates let live with the rest not run
/// with yet, as `survived-so-far` (`strong_tiers::survived_so_far`).
pub(crate) fn read_shown(run: &StoredRun) -> std::collections::BTreeMap<String, Value> {
    let Some(document) = saved_document(run) else {
        return std::collections::BTreeMap::new();
    };
    let mut shown =
        supercov_engine::strong_tiers::survived_so_far(&document.mutants, &document.progress);
    shown.extend(
        document
            .mutants
            .into_iter()
            .map(|(id, verdict)| (id, serde_json::to_value(verdict).expect("plain data"))),
    );
    shown
}

/// The run's saved verdicts as written, where it has any that read.
fn saved_document(run: &StoredRun) -> Option<StrongVerdicts> {
    std::fs::read(results_path(run))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<StrongVerdicts>(&bytes).ok())
}

/// The verdicts of the resumed mutants every candidate of which passed in
/// earlier passes: each survived, against all its candidates.
fn passed_every_candidate(
    resumed: &[supercov_engine::strong_verdicts::Resumed],
) -> Vec<(String, Verdict)> {
    resumed
        .iter()
        .filter(|mutant| mutant.passed == mutant.candidates)
        .map(|mutant| {
            (
                mutant.id.clone(),
                Verdict {
                    status: "survived".into(),
                    killed_by: None,
                    tests: mutant.passed as u64,
                    duration_ms: 0,
                    reason: None,
                },
            )
        })
        .collect()
}

/// Adds `more` to `progress`: the candidates each mutant passed, together.
fn add_progress(progress: &mut Passed, more: Passed) {
    for (id, tests) in more {
        progress.entry(id).or_default().extend(tests);
    }
}

/// Drops the progress of each mutant a verdict now settles: an `error`
/// runs again, and keeps it.
fn settle_progress(progress: &mut Passed, saved: &std::collections::BTreeMap<String, Verdict>) {
    progress.retain(|id, tests| {
        !tests.is_empty()
            && !saved
                .get(id)
                .is_some_and(supercov_engine::strong_verdicts::settles)
    });
}

/// The middle of `values`, which is not empty: the lower of the two in an
/// even count.
fn median(mut values: Vec<usize>) -> usize {
    values.sort_unstable();
    values[(values.len() - 1) / 2]
}

/// What the resumed mutants are, for the answer: how many, how many of
/// them every candidate let live already, how many candidate runs they do
/// not repeat, and the median mutant's candidates left of how many.
fn resumed_json(resumed: &[supercov_engine::strong_verdicts::Resumed]) -> Value {
    let open = resumed
        .iter()
        .filter(|mutant| mutant.passed < mutant.candidates)
        .collect::<Vec<_>>();
    json!({
        "mutants": resumed.len(),
        "survived": resumed.len() - open.len(),
        "skipped": resumed.iter().map(|mutant| mutant.passed).sum::<usize>(),
        "medianLeft": (!open.is_empty()).then(|| median(open.iter().map(|mutant| mutant.candidates - mutant.passed).collect())),
        "medianCandidates": (!open.is_empty()).then(|| median(open.iter().map(|mutant| mutant.candidates).collect())),
    })
}

/// The line a pass that resumes mutants starts with: `37 mutant(s) resume
/// with 4 of 15 candidates left`, the median's, and those that passed every
/// candidate already.
fn resumed_line(resumed: &[supercov_engine::strong_verdicts::Resumed]) -> Option<String> {
    if resumed.is_empty() {
        return None;
    }
    let said = resumed_json(resumed);
    let open = said["mutants"].as_u64().unwrap_or(0) - said["survived"].as_u64().unwrap_or(0);
    let mut parts = Vec::new();
    if open > 0 {
        parts.push(format!(
            "{open} mutant(s) resume where earlier passes left them, with {} of {} candidates left (the median)",
            said["medianLeft"], said["medianCandidates"]
        ));
    }
    let survived = said["survived"].as_u64().unwrap_or(0);
    if survived > 0 {
        parts.push(format!(
            "{survived} passed every candidate in earlier passes, and survived"
        ));
    }
    Some(format!(
        "[supercov] {}: {} candidate run(s) they passed are not run again",
        parts.join("; "),
        said["skipped"]
    ))
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
    let mut listed_survivors = listed
        .iter()
        .filter(|mutant| survived.contains(mutant.id.as_str()))
        .collect::<Vec<_>>();
    // Those that only change a log line are counted on their own, and shown
    // after the rest: equivalent for behaviour, they wait for triage last.
    let log_only = listed_survivors
        .iter()
        .filter(|mutant| mutant.log_only)
        .count();
    listed_survivors.sort_by_key(|mutant| mutant.log_only);
    let shown = listed_survivors
        .into_iter()
        .take(SURVIVORS_SHOWN)
        .map(|mutant| {
            let mut shown = json!({
                "id": mutant.id,
                "place": format!("{}:{}", mutant.file, mutant.span.start[0]),
                "mutator": mutant.mutator,
                "change": crate::human_query::mutant_change(mutant),
            });
            if mutant.log_only {
                shown["logOnly"] = json!(true);
            }
            shown
        })
        .collect::<Vec<_>>();
    json!({"total": survived.len(), "logOnly": log_only, "shown": shown})
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
            "  {}  {}  {}{}\n",
            mutant["place"].as_str().unwrap_or(""),
            mutant["mutator"].as_str().unwrap_or(""),
            mutant["change"].as_str().unwrap_or(""),
            if mutant["logOnly"] == true {
                "  (only changes a log line)"
            } else {
                ""
            }
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
    /// The candidates each open mutant had passed before.
    progress: Passed,
    /// The first tier each mutant runs in this pass, by test id: its
    /// `survived` is progress (`strong_tiers::lived_through`).
    tiers: Tiers,
}

/// Each mutant's first tier in a pass, by mutant id: its candidates' ids.
type Tiers = std::collections::BTreeMap<String, Vec<String>>;

impl Saving {
    /// The driver's verdicts so far, and the candidates the mutants it left
    /// open passed: a first tier's survivor among the latter.
    fn results(&self, results: &Path) -> (std::collections::BTreeMap<String, Verdict>, Passed) {
        let (mut verdicts, _, mut passed) = read_results(results);
        supercov_engine::strong_tiers::lived_through(&mut verdicts, &mut passed, &self.tiers);
        (verdicts, passed)
    }

    /// Saves what was decided before with what the driver wrote so far.
    fn save(&self, results: &Path) -> Result<(), String> {
        let (verdicts, passed) = self.results(results);
        let mut all = self.before.clone();
        all.extend(verdicts);
        let mut progress = self.progress.clone();
        add_progress(&mut progress, passed);
        settle_progress(&mut progress, &all);
        save_to(&self.path, &self.run, &self.mutant_set, &all, &progress)
    }
}

/// While a pass runs: every second, the verdicts its driver wrote so far
/// saved into the run, so a pass that is interrupted or killed keeps them
/// and the next runs the rest; every `PROGRESS_INTERVAL` (or
/// `SUPERCOV_PROGRESS_INTERVAL_MS`), a line on stderr with the verdicts so
/// far and the time left; and once the pass must stop -- the process was
/// sent a signal, or its budget is spent -- the signal sent on to whatever
/// listens for it now, and the process ended should the pass not end within
/// `STOP_PATIENCE`. Whatever the driver.
struct Progress {
    stop: std::sync::mpsc::Sender<()>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Progress {
    /// `costs`: the pass's mutants' estimated costs, by id.
    fn start(
        results: PathBuf,
        costs: std::collections::BTreeMap<String, u64>,
        saving: Saving,
        budget_at: Option<std::time::Instant>,
    ) -> Self {
        let interval = std::env::var("SUPERCOV_PROGRESS_INTERVAL_MS")
            .ok()
            .and_then(|value| value.parse().ok())
            .map_or(PROGRESS_INTERVAL, std::time::Duration::from_millis);
        let (stop, stopped) = std::sync::mpsc::channel::<()>();
        let started = std::time::Instant::now();
        let thread = std::thread::spawn(move || {
            let mut said = std::time::Instant::now();
            let mut seen = 0;
            let mut stopping = None;
            // When the pass's first attempt with a mutant began: its pace is
            // measured from there, not from the command's start.
            let mut first_mutant_at = None;
            while let Err(std::sync::mpsc::RecvTimeoutError::Timeout) =
                stopped.recv_timeout(SAVE_INTERVAL.min(interval))
            {
                // A loop held up here (a test makes one): the budget's own
                // deadline still ends the process (`hold_to_budget`).
                while supercov_engine::fault::injected("strong-progress-stalls") {
                    std::thread::sleep(SAVE_INTERVAL);
                }
                let verdicts = saving.results(&results).0;
                if verdicts.len() > seen {
                    seen = verdicts.len();
                    // A save that fails here is made again when the pass ends.
                    let _ = saving.save(&results);
                }
                if said.elapsed() >= interval {
                    said = std::time::Instant::now();
                    let now = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map_or(0, |since| since.as_millis() as u64);
                    let status = crate::strong_status::read(
                        &results.with_file_name("playwright").join("status"),
                    );
                    first_mutant_at = first_mutant_at
                        .or(status.as_ref().and_then(|status| status.first_mutant_at));
                    let pace = first_mutant_at.map_or(started.elapsed(), |first| {
                        std::time::Duration::from_millis(now.saturating_sub(first))
                    });
                    let left = status.as_ref().and_then(|status| status.left_ms);
                    eprintln!(
                        "{}",
                        progress_line(&verdicts, &costs, started.elapsed(), pace, left)
                    );
                    let last = results
                        .parent()
                        .and_then(crate::strong_status::last_output_line);
                    for line in crate::strong_status::lines(
                        status.as_ref(),
                        now,
                        verdicts.len(),
                        last.as_deref(),
                    ) {
                        eprintln!("{line}");
                    }
                }
                let Some(signal) = stop_signal(budget_at) else {
                    continue;
                };
                if stopping.is_none() {
                    if budget_spent() {
                        eprintln!(
                            "[supercov] budget spent, {} s into the pass: stopping it; what it decided is saved into the run",
                            started.elapsed().as_secs()
                        );
                    }
                    // From here the command's servers go down under the
                    // tests still running: the driver trusts no test that
                    // ends after it (a poll earlier, for a signal the
                    // supervisor heard first).
                    let _ = std::fs::write(
                        results.with_file_name(STOPPED_AT),
                        now_ms()
                            .saturating_sub(SAVE_INTERVAL.as_millis() as u64)
                            .to_string(),
                    );
                }
                let since = *stopping.get_or_insert_with(std::time::Instant::now);
                if since.elapsed() >= STOP_PATIENCE {
                    end_now(&results, &saving, signal);
                }
                // Sent again each time: a supervisor that started its
                // command after the signal came has not heard it.
                supercov_engine::process_supervision::signal_self(signal);
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

/// The file beside a pass's results that says when the pass began to stop,
/// in milliseconds since the epoch (`playwrightStrongReporter.mjs` reads it).
const STOPPED_AT: &str = "stopped-at";

/// Now, in milliseconds since the epoch.
fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_millis() as u64)
}

/// How long a pass told to stop has to end before this process ends it.
/// The supervisor stops the command's processes within its own grace of
/// the signal (5 s); what a pass waits on after that does not hold it.
const STOP_PATIENCE: std::time::Duration = std::time::Duration::from_secs(8);

/// How long past its budget a command may still be running, whatever holds
/// it up -- the progress loop itself, the work after the pass -- before the
/// budget's own deadline ends it (`hold_to_budget`): well past the progress
/// loop's patience, and the summary a large run's pass prints after its
/// stop (its listing reads the run's index, 15 s on a 65,000-mutant run).
const BUDGET_DEADLINE: std::time::Duration = std::time::Duration::from_secs(45);

/// Ends the process once `budget_at` and [`BUDGET_DEADLINE`] have passed,
/// from a thread that does nothing else: the progress loop stops a pass at
/// its budget and ends it [`STOP_PATIENCE`] later, but only while that loop
/// runs, and a job's time limit counts everything. The verdicts were saved
/// into the run as they came.
fn hold_to_budget(budget_at: std::time::Instant) {
    std::thread::spawn(move || {
        let deadline = budget_at + BUDGET_DEADLINE;
        std::thread::sleep(deadline.saturating_duration_since(std::time::Instant::now()));
        eprintln!(
            "[supercov] still running {} s after its budget was spent, so it is ended: the verdicts it reached were saved into the run as they came, and the next pass runs the rest",
            BUDGET_DEADLINE.as_secs()
        );
        std::process::exit(
            supercov_engine::process_supervision::ForwardedSignal::Sigterm.exit_code(),
        )
    });
}

/// Set once the pass's budget is spent: the termination that stops the
/// pass is the budget's, and the pass ends as one that finished.
static BUDGET_SPENT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Why a running pass must stop, as the signal that stops it: its budget
/// spent (a termination), or the signal the process was sent.
fn stop_signal(
    budget_at: Option<std::time::Instant>,
) -> Option<supercov_engine::process_supervision::ForwardedSignal> {
    use std::sync::atomic::Ordering;
    if BUDGET_SPENT.load(Ordering::SeqCst)
        || budget_at.is_some_and(|at| std::time::Instant::now() >= at)
    {
        BUDGET_SPENT.store(true, Ordering::SeqCst);
        return Some(supercov_engine::process_supervision::ForwardedSignal::Sigterm);
    }
    supercov_engine::process_supervision::interruption()
}

/// Ends this process now: a pass told to stop did not end in time. What it
/// decided is saved into the run first.
fn end_now(
    results: &Path,
    saving: &Saving,
    signal: supercov_engine::process_supervision::ForwardedSignal,
) -> ! {
    let _ = saving.save(results);
    let why = if BUDGET_SPENT.load(std::sync::atomic::Ordering::SeqCst) {
        "its budget spent".to_owned()
    } else {
        format!("sent {}", signal.name())
    };
    eprintln!(
        "[supercov] the strong pass did not end within {} s of being told to stop ({why}), so it is ended: the verdicts it reached are saved into the run, and the next pass runs the rest",
        STOP_PATIENCE.as_secs()
    );
    std::process::exit(signal.exit_code())
}

/// A pass's results so far: each mutant's verdict, what its driver said
/// of itself, and the candidates each mutant it left open passed with it
/// (a `progress` record: `{"progress": true, "id": .., "passed": [..]}`).
fn read_results(
    results: &Path,
) -> (
    std::collections::BTreeMap<String, Verdict>,
    Vec<Value>,
    Passed,
) {
    let text = std::fs::read_to_string(results).unwrap_or_default();
    let mut verdicts = std::collections::BTreeMap::new();
    let mut passes = Vec::new();
    let mut progress = Passed::new();
    for line in text.lines() {
        // A line cut short by a driver still writing it says nothing yet.
        let Ok(record) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if record["summary"] == true {
            passes.push(record);
            continue;
        }
        if record["progress"] == true {
            if let Some(id) = record["id"].as_str() {
                progress.entry(id.to_owned()).or_default().extend(
                    record["passed"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(Value::as_str)
                        .map(str::to_owned),
                );
            }
            continue;
        }
        let Some(id) = record["id"].as_str() else {
            continue;
        };
        if let Ok(verdict) = serde_json::from_value::<Verdict>(record.clone()) {
            verdicts.insert(id.to_owned(), verdict);
        }
    }
    (verdicts, passes, progress)
}

/// How far a pass got: its verdicts so far of the mutants it runs, and the
/// time left at the pace so far. The pace is the plan's: the estimated cost
/// of the mutants decided over the time they took (`pace`: since the first
/// mutant began, where the driver says when; the command's start-up and
/// baselines are not the mutants' pace), which a few costly mutants do not
/// swing as a count of mutants did (the cheapest run first). A driver that
/// counts the runs its round has left (`round_left`, Playwright's: each
/// mutant's candidates still to try, at each test's baseline and each
/// invocation's own pace) knows better, and is said instead: the cheap and
/// likely kills come first, so the mutants decided early are no measure of
/// those left.
fn progress_line(
    verdicts: &std::collections::BTreeMap<String, Verdict>,
    costs: &std::collections::BTreeMap<String, u64>,
    elapsed: std::time::Duration,
    pace: std::time::Duration,
    round_left: Option<u64>,
) -> String {
    let count = |status: &str| verdicts.values().filter(|v| v.status == status).count();
    let spent = verdicts.keys().filter_map(|id| costs.get(id)).sum::<u64>();
    let left = if let Some(left) = round_left {
        format!(
            "; about {} left in this round",
            supercov_engine::process_supervision::format_duration(u128::from(left))
        )
    } else if spent > 0 {
        let all = costs.values().sum::<u64>();
        let rest = pace.as_millis() * u128::from(all.saturating_sub(spent)) / u128::from(spent);
        format!(
            "; about {} left",
            supercov_engine::process_supervision::format_duration(rest)
        )
    } else {
        String::new()
    };
    format!(
        "[supercov] strong pass: {} of {} mutants ({} killed, {} timed out, {} survived, {} errors), {}{left}",
        verdicts.len(),
        costs.len(),
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
    progress: &Passed,
) -> Result<(), String> {
    save_to(&results_path(run), &run.id, mutant_set, saved, progress)
}

/// Writes verdicts to `path` whole, then renames them into place: a pass
/// killed while it saves leaves the last whole save.
fn save_to(
    path: &Path,
    run: &str,
    mutant_set: &str,
    saved: &std::collections::BTreeMap<String, Verdict>,
    progress: &Passed,
) -> Result<(), String> {
    supercov_engine::strong_verdicts::save(
        path,
        &StrongVerdicts {
            schema_version: supercov_engine::strong_plan::STRONG_VERDICTS_SCHEMA_VERSION,
            run: run.to_owned(),
            mutant_set: mutant_set.to_owned(),
            mutants: saved.clone(),
            progress: progress.clone(),
        },
    )
}

/// What running a plan's mutants did, beside the verdicts it saved.
struct Executed {
    command_exit: Option<i32>,
    /// What each invocation's driver said of itself.
    passes: Vec<Value>,
    /// Where the budget was spent: how many mutants it left without a
    /// verdict, for the next pass.
    budget_left: Option<usize>,
}

/// Whether the pass's budget is spent.
fn budget_spent() -> bool {
    BUDGET_SPENT.load(std::sync::atomic::Ordering::SeqCst)
}

/// A pass's exit codes that mean a signal ended it: Ctrl-C, a hang-up, a
/// termination.
fn interrupted(exit: i32) -> bool {
    matches!(exit, 129 | 130 | 143)
}

/// Runs the plan's mutants and saves every verdict into the run, as it
/// comes. A mutant with more than `first_tier` candidates runs its first
/// tier (`strong_tiers`) in the first round, every such mutant before any
/// runs the rest.
#[allow(clippy::too_many_arguments)]
fn execute(
    root: &Path,
    run: &StoredRun,
    plan: StrongPlan,
    saved: &mut std::collections::BTreeMap<String, Verdict>,
    progress: &mut Passed,
    options: &Options,
    factor: f64,
    first_tier: usize,
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
        budget_left: None,
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
        // A signal is recorded rather than obeyed at once: the pass sends it
        // on to its command's supervisor, saves what it decided and ends in
        // time (`Progress`), wherever it is when the signal comes.
        let _recorder = supercov_engine::process_supervision::InterruptionRecorder::install();
        // The budget counts from the command's start: what measuring and
        // planning spent of it is not the pass's. One already spent runs
        // nothing.
        let budget_at = options.budget.map(|budget| {
            options
                .before
                .started
                .unwrap_or_else(std::time::Instant::now)
                + budget
        });
        if budget_at.is_some_and(|at| std::time::Instant::now() >= at) {
            BUDGET_SPENT.store(true, std::sync::atomic::Ordering::SeqCst);
        }
        // One measuring spent already runs nothing, and says so.
        if let Some(at) = budget_at.filter(|at| std::time::Instant::now() < *at) {
            hold_to_budget(at);
        }
        // A pass that stops before its end (a test worker that would not
        // stop even when forced) leaves its verdicts; the next pass runs
        // what has none, while each pass gets further.
        let mut exit = 0;
        // The first process said to have run out of heap: what ended the
        // command before the verdicts of the mutants left.
        let mut ran_out = None;
        // A first tier for each mutant with many candidates: every mutant
        // runs its likeliest killers before any runs the rest.
        let tiers = supercov_engine::strong_tiers::first_tiers(&remaining, &plan.tests, first_tier);
        // The first round runs them: each with its first tier, the cheapest
        // first. The rounds after run what is left.
        let mut tiered = (!tiers.is_empty()).then(|| {
            let mut round = StrongPlan {
                mutant_set: plan.mutant_set.clone(),
                tests: plan.tests.clone(),
                mutants: remaining
                    .iter()
                    .map(|mutant| PlannedMutant {
                        candidates: tiers.get(&mutant.id).unwrap_or(&mutant.candidates).clone(),
                        ..mutant.clone()
                    })
                    .collect(),
            };
            round.cheapest_first();
            let by_id = tiers
                .iter()
                .map(|(id, tier)| {
                    let tier = tier
                        .iter()
                        .map(|candidate| plan.tests[candidate.test].id.clone())
                        .collect();
                    (id.clone(), tier)
                })
                .collect::<Tiers>();
            (round.mutants, by_id)
        });
        // How many candidates each resumed mutant passed before this pass: a
        // survivor's verdict counts them with the ones it passes now.
        let mut passed_before = passed_counts(progress);
        let no_tiers = Tiers::new();
        for _ in 0..PASSES {
            if remaining.is_empty() || budget_spent() {
                break;
            }
            let before = remaining.len();
            let (round, round_tiers) = match &tiered {
                Some((round, tiers)) => (round.as_slice(), tiers),
                None => (remaining.as_slice(), &no_tiers),
            };
            // Each language's mutants go to its own driver, one after another
            // (Rust compiled to wasm under JavaScript tests to JavaScript's).
            let driver_of = |mutant: &supercov_engine::strong_plan::PlannedMutant| {
                supercov_engine::strong_driver::driver_of(mutant, &plan.tests).to_owned()
            };
            let languages = round.iter().map(driver_of).collect::<BTreeSet<_>>();
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
                        root,
                        run,
                        &plan,
                        round,
                        &language,
                        &command,
                        &work,
                        saved,
                        progress,
                        budget_at,
                        factor,
                        round_tiers,
                    );
                    let (mut verdicts, passes, mut passed) = read_results(&results);
                    supercov_engine::strong_tiers::lived_through(
                        &mut verdicts,
                        &mut passed,
                        round_tiers,
                    );
                    for (id, verdict) in &mut verdicts {
                        if verdict.status == "survived" {
                            verdict.tests += passed_before.get(id).copied().unwrap_or(0);
                        }
                    }
                    for sentence in supercov_engine::strong_driver::out_of_memory(&work) {
                        eprintln!("[supercov] {sentence}");
                        ran_out.get_or_insert(sentence);
                    }
                    // Success or failure, the pass keeps only its logs.
                    if !supercov_engine::strong_driver::keeps_work_directories(std::env::var_os(
                        supercov_engine::strong_driver::KEEP,
                    )) {
                        supercov_engine::strong_driver::finish_work_directory(&work);
                    }
                    saved.extend(verdicts);
                    add_progress(progress, passed);
                    settle_progress(progress, saved);
                    executed.passes.extend(passes);
                    save(run, &plan.mutant_set, saved, progress)?;
                    exit = ran?;
                    executed.command_exit = Some(exit);
                    // The budget's termination ends the pass as a finished
                    // one; any other signal as an interrupted one.
                    if interrupted(exit) && !budget_spent() {
                        return Err(Stop::Interrupted(exit));
                    }
                    Ok(())
                })?;
            remaining.retain(|mutant| !saved.contains_key(&mutant.id));
            // The rest of each first tier's survivor, from where it is.
            if tiered.take().is_some() {
                let mut rest = StrongPlan {
                    mutant_set: plan.mutant_set.clone(),
                    tests: plan.tests.clone(),
                    mutants: std::mem::take(&mut remaining),
                };
                let resumed = supercov_engine::strong_verdicts::resume(&mut rest, progress);
                saved.extend(passed_every_candidate(&resumed));
                settle_progress(progress, saved);
                remaining = rest.mutants;
                remaining.retain(|mutant| !saved.contains_key(&mutant.id));
                passed_before = passed_counts(progress);
                continue;
            }
            if budget_spent() || remaining.len() == before {
                break;
            }
        }
        // Out of time: what is left has no verdict, for the next pass.
        if budget_spent() {
            executed.budget_left = Some(remaining.len());
            remaining.clear();
        }
        // A mutant no pass gave a verdict did not run to its end.
        for mutant in &remaining {
            saved.entry(mutant.id.clone()).or_insert_with(|| Verdict {
                status: "error".into(),
                killed_by: None,
                tests: 0,
                duration_ms: 0,
                reason: Some(match &ran_out {
                    Some(sentence) => format!(
                        "the command ended (exit {exit}) before this mutant's verdict: {sentence}"
                    ),
                    None => format!("the command ended (exit {exit}) before this mutant's verdict"),
                }),
            });
        }
    }
    save(run, &plan.mutant_set, saved, progress)?;
    Ok(executed)
}

/// How many candidates each mutant passed, by id.
fn passed_counts(progress: &Passed) -> std::collections::BTreeMap<String, u64> {
    progress
        .iter()
        .map(|(id, tests)| (id.clone(), tests.len() as u64))
        .collect()
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
    progress: &Passed,
    budget_at: Option<std::time::Instant>,
    factor: f64,
    tiers: &Tiers,
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
    // The driver's time limit for an attempt goes with the plan it runs.
    let mut written = serde_json::to_value(&pass_plan).expect("plain data serializes");
    written["timeoutFactor"] = json!(factor);
    // fault-site: mutants-test-command.run-language
    supercov_engine::fault_fs::write(
        &plan_path,
        serde_json::to_vec(&written).expect("plain data serializes"),
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
        pass_plan.estimated_ms(),
        Saving {
            path: results_path(run),
            run: run.id.clone(),
            mutant_set: plan.mutant_set.clone(),
            before: saved.clone(),
            progress: progress.clone(),
            tiers: tiers.clone(),
        },
        budget_at,
    );
    let exit = supercov_engine::strong_driver::run_strong_pass(language, &pass, &mut diagnostics);
    // A pass that does not end once told to stop (a test makes one): the
    // progress thread ends the process in time.
    while supercov_engine::fault::injected("strong-pass-lingers") {
        std::thread::sleep(SAVE_INTERVAL);
    }
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
            "narrowed": narrowing(
                plan.mutants
                    .iter()
                    .filter(|mutant| asked_verdicts.contains_key(&mutant.id))
            ),
        },
        "passes": executed.passes,
    })
}

/// What `carry_over` kept of earlier runs' verdicts, and why it kept no
/// more.
#[derive(Debug, Default)]
struct Carried {
    /// The verdicts kept from each earlier run, by mutant id, with the run's
    /// id, newest run first.
    from: Vec<(String, std::collections::BTreeMap<String, Verdict>)>,
    /// Whether every run kept from was measured on the files as they are.
    unchanged: bool,
    /// The mutants an earlier run has a verdict for that none kept, counted
    /// by why, as the newest run that has one says.
    not_kept: std::collections::BTreeMap<&'static str, usize>,
    /// The candidates each mutant still undecided passed in the earlier
    /// runs, every run's together, as `kept_progress` keeps them.
    progress: Passed,
}

/// Why an earlier verdict was not kept, as `Carried::not_kept` counts it.
const ERROR_RUNS_AGAIN: &str = "error";
const FILE_CHANGED: &str = "fileChanged";
const WHOLE_FILE_CHANGED: &str = "wholeFileChanged";
const NEW_TEST: &str = "newTest";
const TEST_REACHED: &str = "testReached";
const NOT_RUN_THEN: &str = "notRunThen";
const UNVIABLE_CHANGED: &str = "unviableChanged";

/// How the pass's summary says each reason a verdict was not kept.
fn not_kept_said(reason: &str) -> &'static str {
    match reason {
        ERROR_RUNS_AGAIN => "were errors, which run again",
        FILE_CHANGED => "their file changed",
        WHOLE_FILE_CHANGED => "a file they run whole changed",
        NEW_TEST => "a test that could kill them is new",
        TEST_REACHED => "a test that could kill them could reach a change",
        UNVIABLE_CHANGED => "did not compile, and the code they compile with changed",
        _ => "no test could kill them in that run",
    }
}

/// Verdicts earlier runs of this checkout's project still give `run`'s
/// mutants (`docs/design/mutants.md`, Keeping verdicts): of the earlier runs
/// of the same command, dependencies, configuration and mutant set, newest
/// first, each gives the mutants still undecided what it keeps of its
/// verdicts. A run measured on the files as they are keeps every verdict
/// (`unchanged_verdicts`): two measurements of the same files differ only by
/// what varies from run to run, which moves no verdict. Any other keeps what
/// no change since could move (`kept_verdicts`). A strong pass split across
/// machines leaves each part's verdicts in its own run.
fn carry_over(
    root: &Path,
    runs: &[StoredRun],
    run: &StoredRun,
    plan: &StrongPlan,
    saved: &std::collections::BTreeMap<String, Verdict>,
) -> Carried {
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
    let mut carried = Carried {
        unchanged: true,
        ..Carried::default()
    };
    // Why each mutant was not kept, as the newest run with a verdict says.
    let mut refused = std::collections::BTreeMap::<String, &'static str>::new();
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
        let same = supercov_engine::source_manifest::same_files(earlier, run);
        let reach = if same {
            None
        } else {
            // A run whose record of what each test ran is gone says nothing
            // of what a change since could reach.
            let Ok(affected) = supercov_engine::source_manifest::affected_tests(root, earlier)
            else {
                continue;
            };
            Some(Reach::of(&affected))
        };
        let judged = match &reach {
            None => unchanged_verdicts(&undecided, &document.mutants),
            Some(reach) => kept_verdicts(&undecided, &plan.tests, &document.mutants, reach),
        };
        let mut kept = std::collections::BTreeMap::new();
        for (id, judgment) in judged {
            match judgment {
                Ok(verdict) => {
                    kept.insert(id, verdict);
                }
                Err(why) => {
                    refused.entry(id).or_insert(why);
                }
            }
        }
        if !kept.is_empty() {
            carried.unchanged &= same;
            undecided.retain(|mutant| !kept.contains_key(&mutant.id));
            carried.from.push((earlier.id.clone(), kept));
        }
        add_progress(
            &mut carried.progress,
            kept_progress(&undecided, &plan.tests, &document.progress, reach.as_ref()),
        );
    }
    for mutant in undecided {
        if let Some(why) = refused.get(&mutant.id) {
            *carried.not_kept.entry(why).or_default() += 1;
        }
    }
    carried
}

/// The candidates each still undecided mutant passed in `earlier` that no
/// change since could move, by mutant id: on the files as they are
/// (`reach` none), every one that is a candidate still; after an edit, those
/// whose test runs as it did (`Reach::runs_as_it_did`) where the mutant's
/// file is byte for byte as it was. A pass is kept candidate by candidate: a
/// change that reaches one test leaves the others' passes standing.
fn kept_progress(
    undecided: &[&supercov_engine::strong_plan::PlannedMutant],
    tests: &[supercov_engine::strong_plan::PlannedTest],
    earlier: &Passed,
    reach: Option<&Reach>,
) -> Passed {
    let mut kept = Passed::new();
    for mutant in undecided {
        let Some(passed) = earlier.get(&mutant.id) else {
            continue;
        };
        if !supercov_engine::strong_verdicts::resumes(mutant, tests)
            || reach.is_some_and(|reach| reach.changed.contains(&mutant.file))
        {
            continue;
        }
        let standing = mutant
            .candidates
            .iter()
            .map(|candidate| &tests[candidate.test])
            .filter(|test| passed.contains(&test.id))
            .filter(|test| reach.is_none_or(|reach| reach.runs_as_it_did(test)))
            .map(|test| test.id.clone())
            .collect::<BTreeSet<_>>();
        if !standing.is_empty() {
            kept.insert(mutant.id.clone(), standing);
        }
    }
    kept
}

/// A verdict a pass reached, which stands while what it rests on does:
/// killed, survived, timeout, or a mutant that did not compile. An error
/// says nothing of the mutant, and runs again.
fn judged(verdict: &Verdict) -> Result<Verdict, &'static str> {
    match verdict.status.as_str() {
        "killed" | "survived" | "timeout" | "unviable" => Ok(verdict.clone()),
        "error" => Err(ERROR_RUNS_AGAIN),
        _ => Err(NOT_RUN_THEN),
    }
}

/// The verdicts of an earlier run measured on the files as they are, for
/// the mutants still undecided: every verdict a pass reached, whatever the
/// two measurements' evidence says.
fn unchanged_verdicts(
    undecided: &[&supercov_engine::strong_plan::PlannedMutant],
    earlier: &std::collections::BTreeMap<String, Verdict>,
) -> Vec<(String, Result<Verdict, &'static str>)> {
    undecided
        .iter()
        .filter_map(|mutant| Some((mutant.id.clone(), judged(earlier.get(&mutant.id)?))))
        .collect()
}

/// What `tests affected` says of an earlier run, as keeping its verdicts and
/// its progress reads it: the test files a change since may have reached,
/// the tests it found unaffected (a test that failed in it counts where the
/// change reaches nothing that run covered), every test it had, and the
/// files changed.
struct Reach {
    reached_files: BTreeSet<String>,
    unaffected: BTreeSet<(String, String)>,
    known: BTreeSet<(String, String)>,
    changed: BTreeSet<String>,
}

impl Reach {
    fn of(affected: &Value) -> Self {
        let entries = |key: &str| {
            affected[key]
                .as_array()
                .into_iter()
                .flatten()
                .cloned()
                .collect::<Vec<_>>()
        };
        let named = |entry: &Value| {
            (
                entry["file"].as_str().unwrap_or_default().to_owned(),
                entry["name"].as_str().unwrap_or_default().to_owned(),
            )
        };
        let beyond_the_run = affected["beyondTheRun"]
            .as_array()
            .is_some_and(|changes| !changes.is_empty());
        // Failed, and nothing else: reached only where the change reaches what
        // the run covered.
        let only_failed = |entry: &Value| {
            !beyond_the_run
                && entry["reasons"] == json!([supercov_engine::source_manifest::DID_NOT_PASS])
        };
        let (failed, reached) = entries("affected")
            .into_iter()
            .chain(entries("undetermined"))
            .partition::<Vec<_>, _>(only_failed);
        // A test file holding any test a change may have reached.
        let reached_files = reached
            .iter()
            .map(|entry| named(entry).0)
            .collect::<BTreeSet<_>>();
        let unaffected = entries("unaffected")
            .iter()
            .chain(&failed)
            .map(named)
            .collect::<BTreeSet<_>>();
        // Every test the run had: one not unaffected was reached, and its file
        // with it.
        let known = unaffected
            .iter()
            .cloned()
            .chain(reached.iter().map(named))
            .collect::<BTreeSet<_>>();
        let changed = affected["changedFiles"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|entry| entry["file"].as_str().unwrap_or_default().to_owned())
            .collect::<BTreeSet<_>>();
        Self {
            reached_files,
            unaffected,
            known,
            changed,
        }
    }

    /// Whether a test runs as it did: the earlier run had it and found it
    /// unaffected, and no test of its file was reached (they share the
    /// file's state).
    fn runs_as_it_did(&self, test: &supercov_engine::strong_plan::PlannedTest) -> bool {
        let file = test.file.clone().unwrap_or_default();
        !self.reached_files.contains(&file) && self.unaffected.contains(&(file, test.name.clone()))
    }
}

/// The verdicts of `earlier` that no change since could move, by mutant id,
/// given what `tests affected` says of that run (`affected`), and why each
/// other was not kept: a mutant keeps its verdict (killed, survived or
/// timeout; an error runs again) where its file is byte for byte as it was,
/// and every candidate test it has now is one the earlier run had and found
/// unaffected, as is every test of their files (they share the file's
/// state) and of the files it runs whole. A test file's edit reaches its
/// tests, a source edit the tests that ran the changed code; a new test that
/// could kill it is a candidate the earlier run did not have. A test that
/// failed in the earlier run left a record cut short: what it could have
/// reached is bounded by what the run covered, so it is unaffected where the
/// change reaches none of that, and its failing, which varies from run to
/// run, moves no verdict.
fn kept_verdicts(
    undecided: &[&supercov_engine::strong_plan::PlannedMutant],
    tests: &[supercov_engine::strong_plan::PlannedTest],
    earlier: &std::collections::BTreeMap<String, Verdict>,
    reach: &Reach,
) -> Vec<(String, Result<Verdict, &'static str>)> {
    let Reach {
        reached_files,
        known,
        changed,
        ..
    } = reach;
    let named_test = |index: usize| {
        let test = &tests[index];
        (test.file.clone().unwrap_or_default(), test.name.clone())
    };
    let mut judgments = Vec::new();
    for mutant in undecided {
        let Some(verdict) = earlier.get(&mutant.id) else {
            continue;
        };
        let candidates = mutant
            .candidates
            .iter()
            .map(|candidate| named_test(candidate.test))
            .collect::<Vec<_>>();
        let judgment = judged(verdict).and_then(|verdict| {
            // Whether a mutant compiles rests on more than its file.
            if verdict.status == "unviable" {
                Err(UNVIABLE_CHANGED)
            } else if changed.contains(mutant.file.as_str()) {
                Err(FILE_CHANGED)
            } else if mutant
                .whole_files
                .iter()
                .any(|file| reached_files.contains(file))
            {
                Err(WHOLE_FILE_CHANGED)
            } else if candidates.iter().any(|test| !known.contains(test)) {
                Err(NEW_TEST)
            } else if candidates
                .iter()
                .any(|test| reached_files.contains(&test.0))
            {
                Err(TEST_REACHED)
            } else {
                Ok(verdict)
            }
        });
        judgments.push((mutant.id.clone(), judgment));
    }
    judgments
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
            server_ran: false,
            passed_before: 0,
            covering: 0,
            statement: None,
            function: None,
            function_hit_by: Default::default(),
            narrowed: None,
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
                    server_ran: false,
                    passed_before: 0,
                    covering: 0,
                    statement: None,
                    function: None,
                    function_hit_by: Default::default(),
                    narrowed: None,
                })
                .collect(),
        }
    }

    /// The plan and the pass say how many module-level mutants had their
    /// candidates narrowed and by how much, and how many kept every test.
    #[test]
    fn the_plan_says_how_far_reads_narrowed_module_level_candidates() {
        use supercov_engine::strong_plan::Narrowed;
        let mut plan = plan_of(&[], &["infected", "infected", "infected", "unknown"]);
        plan.mutants[0].narrowed = Some(Narrowed {
            from: 40,
            to: 3,
            escaped: false,
        });
        plan.mutants[1].narrowed = Some(Narrowed {
            from: 40,
            to: 0,
            escaped: false,
        });
        plan.mutants[2].narrowed = Some(Narrowed {
            from: 40,
            to: 40,
            escaped: true,
        });
        let narrowed = narrowing(plan.mutants.iter()).unwrap();
        assert_eq!(
            narrowed,
            json!({"mutants": 2, "candidatesBefore": 80, "candidatesAfter": 3, "escaped": 1})
        );
        assert_eq!(
            narrowing_text(&narrowed),
            "  narrowed      2 module-level mutant(s) to the tests that read their value: 3 candidate(s) where loading their module gave 80\n  1 module-level mutant(s) kept every test that loaded their module: their value was read while a module loaded\n"
        );
        assert_eq!(narrowing(plan.mutants[3..].iter()), None);
        assert_eq!(narrowing_text(&Value::Null), "");
        let escaped_only = narrowing(plan.mutants[2..].iter()).unwrap();
        assert!(narrowing_text(&escaped_only).starts_with("  1 module-level"));
        let summary = json!({"mutants": 4, "toRun": 4, "candidateRuns": 3, "tests": 0,
            "decided": {"survived": 0, "noCoverage": 0, "equivalent": 0}, "narrowed": narrowed});
        assert!(
            plan_text(&json!({"run": "r1", "summary": summary}))
                .ends_with("their value was read while a module loaded\n")
        );
        let mut ran = summary.clone();
        for (key, value) in [
            ("killed", 1),
            ("timeout", 0),
            ("survived", 0),
            ("noCoverage", 0),
            ("error", 0),
            ("ran", 1),
            ("durationMs", 1),
        ] {
            ran[key] = json!(value);
        }
        assert!(
            plan_text(&json!({"run": "r1", "summary": ran}))
                .contains("  narrowed      2 module-level")
        );
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
        let spent = plan_text(&json!({"run": "r1", "summary": summary,
            "budget": {"seconds": 450, "left": 2423}}));
        assert!(
            spent.contains(
                "  budget of 450 s spent: 2423 mutant(s) left without a verdict, for the next pass\n"
            ),
            "{spent}"
        );
        let limited = plan_text(&json!({"run": "r1", "summary": summary,
            "timeLimit": {"factor": 1.5, "constantMs": 5000, "timedOut": 7}}));
        assert!(
            limited.contains(
                "  7 timeout(s) ran past their time limit: 1.5x the test's baseline + 5 s\n"
            ),
            "{limited}"
        );
        // Kept from runs of the files as they are, and why the rest were not,
        // the reason most said first.
        let unchanged = plan_text(&json!({"run": "r3", "summary": summary,
            "carried": {"from": ["r2"], "mutants": 9, "unchanged": true, "notKept": {
                ERROR_RUNS_AGAIN: 1, FILE_CHANGED: 4, WHOLE_FILE_CHANGED: 1, NEW_TEST: 1,
                TEST_REACHED: 1, UNVIABLE_CHANGED: 1, NOT_RUN_THEN: 1}},
            "measurement": {"derivedFrom": "r2"}}));
        assert!(
            unchanged.contains("  9 verdict(s) kept from run r2: the checkout is unchanged since it\n")
                && unchanged.contains("  10 earlier verdict(s) not kept, so run again: 4 their file changed, 1 were errors, which run again, 1 a test that could kill them is new, 1 no test could kill them in that run, 1 a test that could kill them could reach a change, 1 did not compile, and the code they compile with changed, 1 a file they run whole changed\n")
                && unchanged.contains("  not measured: the checkout is unchanged since run r2, which this run was made from\n"),
            "{unchanged}"
        );
        let both = plan_text(&json!({"run": "r3", "summary": summary,
            "carried": {"from": ["r2", "r1"], "mutants": 9, "unchanged": true}}));
        assert!(
            both.contains(
                "  9 verdict(s) kept from runs r2, r1: the checkout is unchanged since them\n"
            ),
            "{both}"
        );
        // Measuring that took half the budget is said; less is not.
        let measured = |seconds: u64| {
            plan_text(&json!({"run": "r1", "summary": summary,
                "measurement": {"seconds": seconds}, "budget": {"seconds": 300}}))
        };
        assert!(
            measured(150).contains(
                "  measuring took 150 s of the 300 s budget, which leaves the pass 150 s\n"
            ),
            "{}",
            measured(150)
        );
        assert!(!measured(149).contains("measuring took"));
        let whole = plan_text(&json!({"run": "r1", "summary": summary, "carried": {"mutants": 0}}));
        assert!(whole.starts_with("Strong mutation for r1: "), "{whole}");
        assert!(!whole.contains("kept from"), "{whole}");
    }

    /// An earlier verdict is kept where its mutant's file and every test
    /// that could kill it are as they were: not an error's, not a changed
    /// file's, not where a candidate, its file's other tests, or a file run
    /// whole was reached, nor where a candidate is new; each other says why.
    /// A test that failed in the earlier run is reached only where the change
    /// reaches what the run covered.
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
            test("t/d.test.js", "d1"),
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
            mutant("failed-then", "src/a.js", &[5], &[]),
            mutant("unviable", "src/a.js", &[0], &[]),
            mutant("no-coverage", "src/a.js", &[0], &[]),
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
                    "error" | "unviable" | "no-coverage" => mutant.id.as_str(),
                    "timeout-kept" => "timeout",
                    _ => "killed",
                };
                (mutant.id.clone(), verdict(status))
            })
            .collect();
        let mut affected = json!({
            "affected": [
                {"file": "t/b.test.js", "name": "b1", "reasons": ["t/b.test.js changed"]},
                {"file": "t/d.test.js", "name": "d1", "reasons": [supercov_engine::source_manifest::DID_NOT_PASS]}
            ],
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
        let judged = |affected: &Value| {
            kept_verdicts(&undecided, &tests, &earlier, &Reach::of(affected))
                .into_iter()
                .map(|(id, judgment)| (id, judgment.map(|verdict| verdict.status)))
                .collect::<Vec<_>>()
        };
        let said = |id: &str, judgment: Result<&str, &'static str>| {
            (id.to_owned(), judgment.map(str::to_owned))
        };
        assert_eq!(
            judged(&affected),
            [
                said("kept", Ok("killed")),
                said("timeout-kept", Ok("timeout")),
                said("error", Err(ERROR_RUNS_AGAIN)),
                said("file-changed", Err(FILE_CHANGED)),
                said("whole-reached", Err(WHOLE_FILE_CHANGED)),
                said("candidate-reached", Err(TEST_REACHED)),
                said("file-mate-reached", Err(TEST_REACHED)),
                said("new-candidate", Err(NEW_TEST)),
                said("fileless", Ok("killed")),
                said("failed-then", Ok("killed")),
                said("unviable", Err(UNVIABLE_CHANGED)),
                said("no-coverage", Err(NOT_RUN_THEN)),
            ]
        );
        // The change reaches code the run covered: a test that failed in it
        // could have reached it.
        affected["beyondTheRun"] = json!(["src/b.js changed (the run covered code in it)"]);
        assert!(judged(&affected).contains(&said("failed-then", Err(TEST_REACHED))));
        // A run that says nothing of its tests: every candidate is new.
        assert!(
            judged(&json!({}))
                .iter()
                .all(|(_, judgment)| judgment.is_err())
        );
        assert!(kept_verdicts(&[], &tests, &earlier, &Reach::of(&affected)).is_empty());
        // On the files as they are, every verdict a pass reached is kept.
        let unchanged = unchanged_verdicts(&undecided, &earlier)
            .into_iter()
            .filter(|(_, judgment)| judgment.is_err())
            .map(|(id, judgment)| (id, judgment.map(|verdict| verdict.status)))
            .collect::<Vec<_>>();
        assert_eq!(
            unchanged,
            [
                said("error", Err(ERROR_RUNS_AGAIN)),
                said("no-coverage", Err(NOT_RUN_THEN))
            ]
        );
    }

    /// A pass is kept candidate by candidate. After an edit: on the rules
    /// verdicts are kept by, the candidate a change reached loses its pass
    /// and the others keep theirs; a test that is no candidate now lends
    /// nothing, nor a mutant whose file changed. On the files as they are,
    /// every pass of a test that is a candidate still. Never a mutant that
    /// runs a test file whole under a runner that does not run its
    /// candidates one by one.
    #[test]
    fn a_pass_is_kept_candidate_by_candidate() {
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
            test("t/n.test.js", "new"),
        ];
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
        let mutants = [
            mutant("partial", "src/a.js", &[0, 1], &[]),
            mutant("never-run", "src/a.js", &[0], &[]),
            mutant("file-changed", "src/b.js", &[0], &[]),
            mutant("whole", "src/a.js", &[0], &["t/a.test.js"]),
            mutant("new-candidate", "src/a.js", &[3], &[]),
            mutant("untouched", "src/a.js", &[2], &[]),
        ];
        let undecided = mutants.iter().collect::<Vec<_>>();
        let passed = |of: &[(&str, &[&str])]| -> Passed {
            of.iter()
                .map(|(id, tests)| {
                    (
                        (*id).to_owned(),
                        tests.iter().map(|test| (*test).to_owned()).collect(),
                    )
                })
                .collect()
        };
        let earlier = passed(&[
            ("partial", &["a1", "b1"]),
            ("never-run", &["a1", "gone"]),
            ("file-changed", &["a1"]),
            ("whole", &["a1"]),
            ("new-candidate", &["new"]),
        ]);
        let reach = Reach::of(&json!({
            "affected": [{"file": "t/b.test.js", "name": "b1"}],
            "unaffected": [
                {"file": "t/a.test.js", "name": "a1"},
                {"file": "t/c.test.js", "name": "c1"}
            ],
            "changedFiles": [{"file": "src/b.js"}],
        }));
        assert_eq!(
            kept_progress(&undecided, &tests, &earlier, Some(&reach)),
            passed(&[("never-run", &["a1"]), ("partial", &["a1"])])
        );
        assert_eq!(
            kept_progress(&undecided, &tests, &earlier, None),
            passed(&[
                ("file-changed", &["a1"]),
                ("never-run", &["a1"]),
                ("new-candidate", &["new"]),
                ("partial", &["a1", "b1"]),
            ])
        );
    }

    /// A pass that resumes mutants says so as it starts (the median
    /// mutant's candidates left of how many), and in its summary; a pass
    /// its budget stopped says what the mutants it left keep.
    #[test]
    fn resumed_mutants_are_said() {
        use supercov_engine::strong_verdicts::Resumed;
        let resumed = |id: &str, passed: usize, candidates: usize| Resumed {
            id: id.into(),
            passed,
            candidates,
        };
        assert_eq!(resumed_line(&[]), None);
        // One every candidate let live before survived, against all of them;
        // one with candidates left runs them.
        let survived = passed_every_candidate(&[resumed("done", 3, 3), resumed("open", 1, 3)]);
        assert_eq!(survived.len(), 1);
        assert_eq!(survived[0].0, "done");
        assert_eq!(
            (survived[0].1.status.as_str(), survived[0].1.tests),
            ("survived", 3)
        );
        let four = [
            resumed("a", 11, 15),
            resumed("b", 2, 4),
            resumed("c", 1, 15),
            resumed("d", 3, 3),
        ];
        assert_eq!(
            resumed_line(&four).unwrap(),
            "[supercov] 3 mutant(s) resume where earlier passes left them, with 4 of 15 candidates left (the median); 1 passed every candidate in earlier passes, and survived: 17 candidate run(s) they passed are not run again"
        );
        assert_eq!(
            resumed_line(&four[3..]).unwrap(),
            "[supercov] 1 passed every candidate in earlier passes, and survived: 3 candidate run(s) they passed are not run again"
        );
        assert_eq!(
            resumed_line(&four[1..2]).unwrap(),
            "[supercov] 1 mutant(s) resume where earlier passes left them, with 2 of 4 candidates left (the median): 2 candidate run(s) they passed are not run again"
        );
        let summary = json!({"killed": 1, "timeout": 0, "survived": 0, "noCoverage": 0,
            "equivalent": 0, "error": 0, "ran": 1, "durationMs": 5, "mutationScore": null});
        let text = plan_text(&json!({"run": "r1", "summary": summary,
            "resumed": resumed_json(&four),
            "budget": {"seconds": 60, "left": 3, "resume": 2, "passed": 9}}));
        assert!(
            text.contains("  4 mutant(s) resumed where earlier passes left them: 17 candidate run(s) they had passed were not run again\n"),
            "{text}"
        );
        assert!(
            text.contains("  budget of 60 s spent: 3 mutant(s) left without a verdict, for the next pass; 2 of them resume from the 9 candidate(s) they passed\n"),
            "{text}"
        );
        let none_kept = plan_text(&json!({"run": "r1", "summary": summary,
            "budget": {"seconds": 60, "left": 3, "resume": 0, "passed": 0}}));
        assert!(
            none_kept.contains("for the next pass\n") && !none_kept.contains("resume"),
            "{none_kept}"
        );
    }

    /// A pass's results carry the candidates each mutant it left open
    /// passed, together; a record with no mutant says nothing. A verdict
    /// that settles a mutant drops its progress, an error keeps it.
    #[test]
    fn a_pass_s_results_carry_its_open_mutants_progress() {
        let directory =
            std::env::temp_dir().join(format!("supercov-results-progress-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let results = directory.join("results.jsonl");
        std::fs::write(
            &results,
            concat!(
                "{\"progress\":true,\"id\":\"m1\",\"passed\":[\"t1\",\"t2\"]}\n",
                "{\"progress\":true,\"id\":\"m1\",\"passed\":[\"t3\",4]}\n",
                "{\"progress\":true,\"passed\":[\"t1\"]}\n",
                "{\"progress\":true,\"id\":\"m2\"}\n",
                "{\"id\":\"m3\",\"status\":\"killed\"}\n",
            ),
        )
        .unwrap();
        let (verdicts, _, mut progress) = read_results(&results);
        assert_eq!(verdicts.keys().collect::<Vec<_>>(), ["m3"]);
        let set = |tests: &[&str]| {
            tests
                .iter()
                .map(|test| (*test).to_owned())
                .collect::<BTreeSet<_>>()
        };
        assert_eq!(progress["m1"], set(&["t1", "t2", "t3"]));
        assert!(progress["m2"].is_empty());
        progress.insert("m3".into(), set(&["t1"]));
        progress.insert("m4".into(), set(&["t1"]));
        let mut saved = verdicts;
        saved.insert(
            "m4".into(),
            Verdict {
                status: "error".into(),
                killed_by: None,
                tests: 0,
                duration_ms: 0,
                reason: None,
            },
        );
        settle_progress(&mut progress, &saved);
        assert_eq!(progress.keys().collect::<Vec<_>>(), ["m1", "m4"]);
        let _ = std::fs::remove_dir_all(&directory);
    }

    /// A survivor that only changes a log line is marked as such.
    #[test]
    fn a_survivor_that_only_changes_a_log_line_says_so() {
        let shown = json!({"total": 2, "logOnly": 1, "shown": [
            {"place": "a.js:1", "mutator": "M", "change": "x → y"},
            {"place": "a.js:2", "mutator": "StringLiteral", "change": "\"saved\" → \"\"", "logOnly": true},
        ]});
        assert_eq!(
            survivors_text("r1", &shown),
            "Survived:\n  a.js:1  M  x → y\n  a.js:2  StringLiteral  \"saved\" → \"\"  (only changes a log line)\n"
        );
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

    /// The time left is the plan's estimate of the mutants left at the pace
    /// so far, by cost: a few costly mutants do not swing it.
    #[test]
    fn a_pass_s_time_left_is_its_cost_left_at_its_pace() {
        let verdict = |status: &str| Verdict {
            status: status.into(),
            killed_by: None,
            tests: 1,
            duration_ms: 1,
            reason: None,
        };
        let costs = [("cheap", 100), ("also-cheap", 100), ("costly", 9_800)]
            .map(|(id, cost)| (id.to_owned(), cost))
            .into_iter()
            .collect::<std::collections::BTreeMap<_, _>>();
        let elapsed = std::time::Duration::from_secs(20);
        let none = progress_line(&Default::default(), &costs, elapsed, elapsed, None);
        assert_eq!(
            none,
            "[supercov] strong pass: 0 of 3 mutants (0 killed, 0 timed out, 0 survived, 0 errors), 20s"
        );
        let decided = [
            ("cheap", verdict("killed")),
            ("also-cheap", verdict("survived")),
        ]
        .map(|(id, verdict)| (id.to_owned(), verdict))
        .into_iter()
        .collect();
        let line = progress_line(&decided, &costs, elapsed, elapsed, None);
        assert!(
            line.starts_with(
                "[supercov] strong pass: 2 of 3 mutants (1 killed, 0 timed out, 1 survived, 0 errors), 20s; about "
            ),
            "{line}"
        );
        assert!(line.ends_with(&format!(
            "about {} left",
            supercov_engine::process_supervision::format_duration(980_000)
        )));
        // Paced from the first mutant, 10 s in: the start-up is not the
        // mutants' pace.
        let paced = progress_line(&decided, &costs, elapsed, elapsed / 2, None);
        assert!(paced.ends_with(&format!(
            "about {} left",
            supercov_engine::process_supervision::format_duration(490_000)
        )));
        // The driver's count of the runs its round has left, where it says
        // one, before any verdict too.
        let counted = progress_line(&Default::default(), &costs, elapsed, elapsed, Some(95_000));
        assert!(
            counted.ends_with("0 errors), 20s; about 1m35s left in this round"),
            "{counted}"
        );
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
        assert_eq!(
            parsed(&["--budget", "450"]).unwrap().budget,
            Some(std::time::Duration::from_secs(450))
        );
        assert_eq!(
            parsed(&["--timeout-factor", "2.5"]).unwrap().timeout_factor,
            Some(2.5)
        );
        // The option, else the variable (where it says something), else
        // StrykerJS's 1.5.
        assert_eq!(resolved_timeout_factor(Some(3.0), Some("2")), Ok(3.0));
        assert_eq!(resolved_timeout_factor(None, Some("2")), Ok(2.0));
        assert_eq!(resolved_timeout_factor(None, Some("")), Ok(1.5));
        assert_eq!(resolved_timeout_factor(None, None), Ok(1.5));
        assert_eq!(
            resolved_timeout_factor(None, Some("0")),
            Err("SUPERCOV_STRONG_TIMEOUT_FACTOR takes a number above 0, not 0".into())
        );
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
            (&["--budget"], "--budget needs seconds"),
            (
                &["--budget", "0"],
                "--budget takes a whole number of seconds, not 0",
            ),
            (&["--budget", "1.5"], "not 1.5"),
            (&["--timeout-factor"], "--timeout-factor needs a number"),
            (
                &["--timeout-factor", "fast"],
                "--timeout-factor takes a number above 0, not fast",
            ),
            (&["--timeout-factor", "inf"], "not inf"),
            (&["--timeout-factor", "NaN"], "not NaN"),
            (&["--timeout-factor", "-1"], "not -1"),
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
