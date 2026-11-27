//! The strong pass for Rust: each mutant the plan runs, against its
//! candidate tests, by exact name through the test binary.
//!
//! One build holds every mutant of the pass behind switches
//! (`rust_schemata`). A test process runs with one mutant switched on
//! (`SUPERCOV_ACTIVE_MUTANT`), so a mutant costs its tests' processes and no
//! build: cargo-mutants builds once per mutant. A mutant that does not
//! compile breaks that build; the compiler's errors are read by their byte
//! ranges in the switched files, and the mutants behind them leave the
//! build, which is built again:
//!
//! - an error in a mutant's own code, of a kind that says the code itself
//!   does not type-check (a mismatched type, a missing trait or method), is
//!   that mutant's: it is `unviable`, as cargo-mutants reports it;
//! - any other error at a switch (in its original branch, or of another
//!   kind) may be the switch's doing rather than the mutant's: its mutants
//!   are built on their own, below;
//! - an error at no switch is found by halving the switched mutants until
//!   the build passes, and the mutant it comes down to is built on its own.
//!   Such an error is one a mutant causes elsewhere in its own file: deleting
//!   an arm of a `match` whose only `_` arm has a guard leaves the match
//!   without the arm that made it exhaustive, an error at its scrutinee. No
//!   error at no switch is known in another file than its mutant's, so the
//!   halving is over every switched mutant, not those of the errors' files.
//!
//! A mutant built on their own, and one `rust_schemata::placeable` refuses,
//! is written into a plain copy of the project and built there, as
//! cargo-mutants does: unviable where that build fails.
//!
//! Every test a mutant needs runs first with no mutant (its baseline): a
//! test that fails there cannot tell a mutant apart, and its time sets the
//! mutant's limit, cargo-mutants': 5 times the baseline, at least 20 s. A mutant's tests run
//! cheapest first and stop at the first that fails: `killed`. One that runs
//! past its limit beside other tests runs again alone, after a run of its
//! test without the mutant that sets its limit there: past that, `timeout`.
//! Where `SUPERCOV_MUTANT_HIT_LIMIT`
//! is set, a test whose mutant was evaluated more often than that stops at
//! once, also a `timeout`. Every test passing is `survived`.
//!
//! Doctests are compiled by rustdoc against the switched library and run in
//! one `cargo test --doc` per mutant, after its libtest candidates.
//!
//! A pass told to stop (its budget, a signal: `stopped-at` beside its
//! results) starts nothing more, ends the processes it runs, and judges
//! nothing they said; each candidate a mutant passed is written as it
//! passes (`progress`), so the next pass runs it against the rest.
//!
//! Verdicts are written to the results as each mutant ends, one JSON line
//! each (`{ id, status, killedBy, tests, durationMs, reason }`), and a last
//! line with `summary: true` says what the pass did.

use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    io::{Read, Write},
    ops::Range,
    path::{Path, PathBuf},
    process::Command,
    sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use serde_json::{Value, json};

use crate::fault_process::Launch;

use crate::{
    rust_mutants::RustMutant,
    rust_schemata::{ACTIVE_MUTANT_ENV, HIT_LIMIT_ENV, TIMEOUT_MARKER},
    rust_test_runner::{CargoTestInvocation, RustCargoCommandKind},
};

/// What a strong pass over a Rust run needs.
#[derive(Debug, Clone)]
pub struct StrongRustRequest {
    pub root: PathBuf,
    pub command: Vec<String>,
    /// The pass's `plan.json`.
    pub plan: PathBuf,
    /// Where verdicts go, one JSON line each.
    pub results: PathBuf,
    /// How many tests run side by side ([`workers`]).
    pub workers: usize,
}

/// How many tests a pass runs side by side: `SUPERCOV_STRONG_JOBS`, or half
/// the CPUs.
pub fn workers() -> usize {
    std::env::var("SUPERCOV_STRONG_JOBS")
        .ok()
        .and_then(|value| value.parse::<std::num::NonZeroUsize>().ok())
        .map_or_else(
            || {
                std::thread::available_parallelism()
                    .map(usize::from)
                    .unwrap_or(1)
                    .div_ceil(2)
            },
            usize::from,
        )
}

/// Runs `work` on `workers` threads at once, this one among them, so one
/// worker runs on the caller's thread alone.
fn pool(workers: usize, work: impl Fn() + Sync) {
    std::thread::scope(|scope| {
        for _ in 1..workers {
            scope.spawn(&work);
        }
        work();
    });
}

/// The evaluations of the active mutant a test process stops at: none
/// unless `SUPERCOV_MUTANT_HIT_LIMIT` names a number. A test process runs
/// alone and is stopped at its time limit, so the limit only ends a loop the
/// mutant made endless sooner; a stop there is a `timeout`.
fn hit_limit() -> u64 {
    std::env::var(HIT_LIMIT_ENV)
        .ok()
        .and_then(|value| value.trim().parse().ok())
        .unwrap_or(0)
}

/// The compiler errors that say a mutant's own code does not type-check,
/// wherever it is put: the mutant is unviable.
const OWN_ERRORS: &[&str] = &[
    "E0061", "E0067", "E0277", "E0282", "E0283", "E0308", "E0368", "E0369", "E0412", "E0423",
    "E0425", "E0433", "E0599", "E0600", "E0603", "E0604", "E0605", "E0606", "E0614", "E0624",
];

/// One mutant the pass runs.
#[derive(Debug, Clone)]
struct Job {
    id: String,
    file: String,
    /// Its index in its file's list: what its switch is named by.
    ordinal: u64,
    mutant: RustMutant,
    /// Its candidate tests, as indices into the plan's tests.
    candidates: Vec<usize>,
    /// Decided by the plan without running (`decided` in `plan.json`): it
    /// is switched into the build only to find whether it compiles, and
    /// runs nothing.
    check_only: bool,
    /// How many of its candidates passed with it in earlier passes, which
    /// this one does not run again (`passedBefore` in `plan.json`).
    passed_before: usize,
}

/// A test the pass can run.
#[derive(Debug, Clone)]
struct PlannedTest {
    id: String,
    name: String,
    runner: String,
    /// A doctest's package (in the plan, its file).
    project: Option<String>,
    runnable: bool,
}

/// A test binary Cargo built.
#[derive(Debug, Clone)]
struct Artifact {
    executable: PathBuf,
    /// The target's root, relative to the workspace: the first part of its
    /// tests' ids.
    source: String,
    package_directory: PathBuf,
}

/// A compiler error, by the file and the bytes it points at.
#[derive(Debug, Clone)]
struct CompileError {
    code: Option<String>,
    spans: Vec<(String, Range<usize>)>,
    rendered: String,
}

/// How a test ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Outcome {
    Passed,
    Failed,
    /// Past its time limit.
    TimedOut,
    /// Stopped at the hit limit.
    HitLimit,
    /// Stopped because the pass is stopping: it says nothing of its mutant.
    Stopped,
    /// Its process could not be started: it ran nothing.
    Unstarted,
}

struct Ran {
    outcome: Outcome,
    duration: Duration,
}

/// Where everything the pass builds lives, under the project.
struct Places {
    switched: PathBuf,
    switched_target: PathBuf,
    plain: PathBuf,
    plain_target: PathBuf,
}

impl Places {
    fn under(root: &Path) -> Self {
        let base = root.join(".supercov/strong/rust");
        Self {
            switched: base.join("switched"),
            switched_target: base.join("switched-target"),
            plain: base.join("plain"),
            plain_target: base.join("plain-target"),
        }
    }
}

/// The results file, written a line at a time.
struct Results {
    file: Mutex<std::fs::File>,
}

impl Results {
    fn create(path: &Path) -> Result<Self, String> {
        let file =
            // fault-site: rust-strong.create
            crate::fault_fs::open_with(std::fs::OpenOptions::new().create(true).append(true), path)
                .map_err(|error| format!("{}: {error}", path.display()))?;
        Ok(Self {
            file: Mutex::new(file),
        })
    }

    fn write(&self, record: &Value) {
        let mut line = serde_json::to_vec(record).expect("plain data serializes");
        line.push(b'\n');
        let mut file = self.file.lock().expect("results lock");
        let _ = file.write_all(&line);
        let _ = file.flush();
    }

    /// Candidates `id` passed with its mutant on: a pass that stops before
    /// its verdict leaves them, and the next runs it against the rest
    /// (`docs/mutation.md`, partial progress). Each test runs in a process of
    /// its own, so its passing holds whatever runs beside or after it.
    fn passed(&self, id: &str, tests: &[&str]) {
        self.write(&json!({"progress": true, "id": id, "passed": tests}));
    }

    fn verdict(
        &self,
        id: &str,
        status: &str,
        killed_by: Option<&str>,
        tests: usize,
        started: Instant,
        reason: Option<String>,
    ) {
        let mut record = json!({
            "id": id,
            "status": status,
            "tests": tests,
            "durationMs": started.elapsed().as_millis() as u64,
        });
        if let Some(killed_by) = killed_by {
            record["killedBy"] = json!(killed_by);
        }
        if let Some(reason) = reason {
            record["reason"] = json!(reason);
        }
        self.write(&record);
    }
}

/// The id a cargo-mutants mutant of `file` has.
pub(crate) fn mutant_id(file: &str, mutant: &RustMutant) -> String {
    format!(
        "{file}:{}:{}-{}:{}:{}:{}",
        mutant.start.0,
        mutant.start.1,
        mutant.end.0,
        mutant.end.1,
        mutant.genre,
        mutant.descriptor()
    )
}

/// Runs a strong pass's plan over a Rust project and writes the verdicts.
/// The exit code is 0 where the pass ran to its end.
pub fn run_strong_rust(
    request: &StrongRustRequest,
    diagnostics: &mut dyn Write,
) -> Result<i32, String> {
    let stop = Stop::beside(&request.results);
    let outcome = pass(request, &stop, diagnostics);
    if stop.now() {
        // Told to stop, it ends as the signal ends a command, in time: its
        // copies are put aside at once and removed behind it (or by the
        // next pass), and what it decided is saved.
        let _ = crate::lifecycle::remove_scratch_tree_deferred(
            &request.root,
            &request.root.join(".supercov/strong/rust"),
        );
        crate::lifecycle::sweep_trash_in_background(&request.root);
        return Ok(crate::process_supervision::interruption()
            .map_or(STOPPED_EXIT, |signal| signal.exit_code()));
    }
    discard(&request.root, ".supercov/strong/rust");
    outcome
}

/// A test's time limit with a mutant, from its time without one:
/// cargo-mutants' rule, 5 times the baseline and at least 20 s. A mutant that
/// makes a test slower but lets it end within that is no timeout there.
fn limit(baseline: Duration) -> Duration {
    (baseline * 5).max(Duration::from_secs(20))
}

/// How a mutant's run against its candidates ended.
enum Ended {
    /// Its verdict is written: which.
    Decided(&'static str),
    /// The stop cut it short: no verdict, for the next pass.
    Stopped,
    /// A test ran past its limit beside the pass's other tests, where the
    /// limit may be the machine's load: no verdict yet, it is judged again
    /// alone.
    Slow(Slow),
}

/// A mutant to judge again alone: the tests whose process ran past its
/// limit (one libtest case, or a package's doctests), and the candidates it
/// already passed, which it does not run again.
struct Slow {
    tests: Vec<usize>,
    passed: Vec<usize>,
}

/// Why a test's run with a mutant is a `timeout`: the hit limit, or its
/// time limit when judged again alone (a time limit beside other tests
/// decides nothing).
fn timeout_reason(limit: Duration, outcome: Outcome) -> String {
    match outcome {
        Outcome::HitLimit => "evaluated its mutant past the hit limit".to_owned(),
        _ => format!(
            "ran past {} ms, alone, after its test ran again without the mutant",
            limit.as_millis()
        ),
    }
}

/// What a pass its budget stopped ends with: a termination's, as the
/// command of a JavaScript pass does.
const STOPPED_EXIT: i32 = 143;

/// The error a build the stop cut short ends with.
const STOPPED: &str = "the pass was told to stop";

/// Remove a pass's copies and builds, whatever its end: they hold no
/// verdict (the results are the driver's, beside its plan), and a switched
/// build is a gigabyte or more. The next pass builds afresh.
fn discard(root: &Path, relative: &str) {
    let _ = crate::lifecycle::remove_scratch_tree_deferred(root, &root.join(relative));
    let _ = crate::lifecycle::sweep_trash(root);
}

/// [`run_strong_rust`], leaving its copies and builds behind.
fn pass(
    request: &StrongRustRequest,
    stop: &Stop,
    diagnostics: &mut dyn Write,
) -> Result<i32, String> {
    let started = Instant::now();
    let plan: Value = serde_json::from_slice(
        // fault-site: rust-strong.pass.read
        &crate::fault_fs::read(&request.plan)
            .map_err(|e| format!("{}: {e}", request.plan.display()))?,
    )
    .map_err(|error| format!("{}: {error}", request.plan.display()))?;
    let results = Results::create(&request.results)?;
    // fault-site: rust-strong.pass.canonicalize
    let root = crate::fault_fs::canonicalize(&request.root)
        .map_err(|error| format!("{}: {error}", request.root.display()))?;
    let mut tests = planned_tests(&plan);
    // A doctest runs by its package, the one whose manifest is nearest its
    // file.
    for test in &mut tests {
        test.project = match (&test.runner[..], &test.project) {
            ("rustdoc", Some(file)) => package_of(&root, file),
            _ => None,
        };
    }

    // Each mutant to run, found again in its file as the plan names it.
    let (jobs, sources) = planned_jobs(&plan, &root, &tests, &results, started);
    // One whose tests all run under another runner (JavaScript tests over
    // the crate's wasm in a browser page; under Node the JavaScript pass has
    // them, `docs/design/mutation-drivers.md`) is an error here, before the
    // command is read as Cargo's.
    let (jobs, foreign): (Vec<Job>, Vec<Job>) = jobs
        .into_iter()
        .partition(|job| foreign_runner(job, &tests).is_none());
    for job in &foreign {
        let runner = foreign_runner(job, &tests).expect("partitioned");
        results.verdict(
            &job.id,
            "error",
            None,
            0,
            started,
            Some(format!(
                "its tests run under {runner}, which the Rust pass does not drive"
            )),
        );
    }
    // A decided mutant is checked only in a package whose tests the pass
    // runs: the command builds those, and a workspace's other members may
    // hold thousands of mutants it never compiles.
    let mut packages = BTreeMap::<String, Option<String>>::new();
    let mut package = |file: &str| {
        packages
            .entry(file.to_owned())
            .or_insert_with(|| package_of(&root, file))
            .clone()
    };
    let tested = jobs
        .iter()
        .filter(|job| !job.check_only)
        .map(|job| package(&job.file))
        .collect::<BTreeSet<_>>();
    let jobs = jobs
        .into_iter()
        .filter(|job| !job.check_only || tested.contains(&package(&job.file)))
        .collect::<Vec<_>>();
    let to_run = jobs.iter().filter(|job| !job.check_only).count();
    let _ = writeln!(
        diagnostics,
        "[supercov] strong pass: {to_run} Rust mutant(s) to run",
    );
    if to_run == 0 {
        results.write(&json!({"summary": true, "mutants": 0}));
        return Ok(0);
    }

    // `wasm-pack test` runs as the `cargo test` it stands for, through
    // wasm-bindgen's runner, which the Node hook of the launcher switches a
    // wasm module's mutant on in.
    let learned = if crate::rust_wasm_pack::is_wasm_pack_test(&request.command) {
        Some(crate::rust_wasm_pack::learn(
            &root,
            &request.command,
            &root.join(".supercov/strong/rust/wasm-pack"),
        )?)
    } else {
        None
    };
    let command = learned
        .as_ref()
        .map_or(&request.command, |learned| &learned.command);
    let runner_environment = learned
        .as_ref()
        .map(|learned| learned.runner_environment.clone())
        .unwrap_or_default();
    let invocation = crate::rust_test_runner::cargo_invocation(&root, command)
        .map_err(|error| error.to_string())?;
    let build_invocation = build_invocation(&invocation);
    // Tests run in Chrome get their mutant from the driver proxy; another
    // browser's page has no way in yet.
    let names = |wanted: &[&str]| {
        runner_environment
            .iter()
            .any(|(name, _)| wanted.contains(&name.as_str()))
    };
    if names(&["GECKODRIVER", "SAFARIDRIVER", "MSEDGEDRIVER"]) && !names(&["CHROMEDRIVER"]) {
        // There is a job at least.
        jobs.iter().filter(|job| !job.check_only).for_each(|job| {
            results.verdict(
                &job.id,
                "error",
                None,
                0,
                started,
                Some(
                    "a strong pass switches mutants in wasm tests run in Chrome only, not yet in this browser".into(),
                ),
            );
        });
        results.write(&json!({"summary": true, "mutants": to_run, "browser": true}));
        return Ok(0);
    }

    // Which mutants a run-time switch can hold (there is at least one job).
    let mut switchable = Vec::new();
    let mut alone = Vec::<(usize, &'static str)>::new();
    jobs.iter().enumerate().for_each(|(at, job)| {
        let (source, _) = sources[&job.file].as_ref().expect("a job's file was read");
        let (tree, _) =
            crate::rust_instrumenter::parse_source(source).expect("the file was enumerated");
        let tree = ra_ap_syntax::AstNode::syntax(&tree).clone();
        match crate::rust_schemata::placeable(&tree, &job.mutant) {
            Some(reason) => alone.push((at, reason)),
            None => switchable.push(at),
        }
    });

    // The switched build, with every mutant it can hold.
    let places = Places::under(&root);
    let mut unviable = BTreeMap::<usize, String>::new();
    let built = build_switched(
        &root,
        &places,
        &jobs,
        &mut switchable,
        &mut alone,
        &mut unviable,
        &build_invocation,
        &runner_environment,
        diagnostics,
        stop,
    )?;
    for (at, error) in &unviable {
        results.verdict(
            &jobs[*at].id,
            "unviable",
            None,
            0,
            started,
            Some(format!("does not compile: {error}")),
        );
    }
    // A decided mutant was switched in only to find whether it compiles: it
    // runs nothing, and is not built on its own.
    switchable.retain(|at| !jobs[*at].check_only);
    alone.retain(|(at, _)| !jobs[*at].check_only);
    let _ = writeln!(
        diagnostics,
        "[supercov] switched {} mutant(s) into one build; {} unviable; {} to build on their own",
        switchable.len(),
        unviable.len(),
        alone.len()
    );

    // Each test, by its id, in that build.
    let by_id = built
        .artifacts
        .iter()
        .map(|artifact| (artifact.source.clone(), artifact))
        .collect::<BTreeMap<_, _>>();
    let runner = TestRunner {
        results: request.results.clone(),
        launcher: &built.launcher,
        target_libdir: &built.target_libdir,
        by_source: by_id,
        doctest: DoctestCommand::new(&invocation, &built.project, &places.switched_target),
        nextest: match invocation.kind {
            RustCargoCommandKind::NextestRun => Some(Nextest::prepare(
                &invocation,
                &built.project,
                &places.switched_target,
            )?),
            RustCargoCommandKind::CargoTest => None,
        },
        stop: stop.clone(),
        reports_reach: learned.is_none(),
    };

    // Baselines: every test a mutant runs against, with no mutant.
    let needed = switchable
        .iter()
        .chain(alone.iter().map(|(at, _)| at))
        .flat_map(|at| jobs[*at].candidates.iter().copied())
        .collect::<BTreeSet<_>>();
    let baselines = baselines(&runner, &tests, &needed, request.workers);
    // Baselines a stop cut short tell no mutant's candidates.
    if stop.now() {
        return Ok(0);
    }
    let usable = |test: usize| {
        tests[test].runnable
            && baselines
                .get(&test)
                .is_some_and(|ran| ran.outcome == Outcome::Passed)
    };
    let limit_of = |test: usize| {
        limit(
            baselines
                .get(&test)
                .map_or(Duration::from_secs(10), |ran| ran.duration),
        )
    };

    // The switched mutants, side by side.
    let next = AtomicUsize::new(0);
    let counts = Mutex::new(BTreeMap::<&'static str, usize>::new());
    let count = |ended: Ended| match ended {
        Ended::Decided(status) => {
            *counts.lock().expect("counts").entry(status).or_default() += 1;
            None
        }
        Ended::Stopped => None,
        Ended::Slow(slow) => Some(slow),
    };
    let slow = Mutex::new(Vec::<(usize, Slow)>::new());
    pool(request.workers.min(switchable.len()), || {
        // Each worker takes the next mutant until none is left (a worker
        // started after the others took them all takes none).
        // Once the pass stops, each ends at once.
        std::iter::from_fn(|| switchable.get(next.fetch_add(1, Ordering::Relaxed))).for_each(
            |at| {
                let job = &jobs[*at];
                let named = format!("{}#{}", job.file, job.ordinal);
                let ended = run_job(
                    job,
                    &tests,
                    &runner,
                    &usable,
                    &limit_of,
                    Some(&named),
                    &results,
                    None,
                );
                if let Some(again) = count(ended) {
                    slow.lock().expect("slow").push((*at, again));
                }
            },
        );
    });

    // Each mutant whose test ran past its limit beside others, again alone,
    // one at a time.
    let mut slow = slow.into_inner().expect("slow");
    slow.sort_by_key(|(at, _)| *at);
    for (at, again) in &slow {
        let job = &jobs[*at];
        let named = format!("{}#{}", job.file, job.ordinal);
        let ended = judge_alone(
            job,
            again,
            &tests,
            &runner,
            &runner,
            &usable,
            &baselines,
            Some(&named),
            &results,
        );
        count(ended);
    }

    // The mutants built on their own, one at a time.
    for (at, why) in &alone {
        let job = &jobs[*at];
        let mutant_started = Instant::now();
        let (source, _) = sources[&job.file].as_ref().expect("a job's file was read");
        match build_alone(
            &root,
            &places,
            &build_invocation,
            &runner_environment,
            job,
            source,
            stop,
        ) {
            // The stop cut its build short: it is left for the next pass.
            Err(_) if stop.now() => break,
            Err(error) => {
                results.verdict(
                    &job.id,
                    "error",
                    None,
                    0,
                    mutant_started,
                    Some(format!("could not build it on its own ({why}): {error}")),
                );
                *counts.lock().expect("counts").entry("error").or_default() += 1;
            }
            Ok(Err(error)) => {
                results.verdict(
                    &job.id,
                    "unviable",
                    None,
                    0,
                    mutant_started,
                    Some(format!("does not compile: {error}")),
                );
                *counts
                    .lock()
                    .expect("counts")
                    .entry("unviable")
                    .or_default() += 1;
            }
            Ok(Ok(alone_build)) => {
                let alone_runner = TestRunner {
                    results: request.results.clone(),
                    launcher: &alone_build.launcher,
                    target_libdir: &alone_build.target_libdir,
                    by_source: alone_build
                        .artifacts
                        .iter()
                        .map(|artifact| (artifact.source.clone(), artifact))
                        .collect(),
                    doctest: DoctestCommand::new(
                        &invocation,
                        &alone_build.project,
                        &places.plain_target,
                    ),
                    nextest: match invocation.kind {
                        RustCargoCommandKind::NextestRun => Some(Nextest::prepare(
                            &invocation,
                            &alone_build.project,
                            &places.plain_target,
                        )?),
                        RustCargoCommandKind::CargoTest => None,
                    },
                    stop: stop.clone(),
                    reports_reach: false,
                };
                let ended = run_job(
                    job,
                    &tests,
                    &alone_runner,
                    &usable,
                    &limit_of,
                    None,
                    &results,
                    None,
                );
                // Its build holds it with no switch: its tests run again
                // without it in the switched build.
                if let Some(again) = count(ended) {
                    count(judge_alone(
                        job,
                        &again,
                        &tests,
                        &runner,
                        &alone_runner,
                        &usable,
                        &baselines,
                        None,
                        &results,
                    ));
                }
            }
        }
    }
    // Leave the plain copy as written.
    // fault-site: rust-strong.pass.sync-project
    let _ = sync_project(&root, &places.plain);

    let counts = counts.into_inner().expect("counts");
    results.write(&json!({
        "summary": true,
        "mutants": to_run,
        "switched": switchable.len(),
        "builtAlone": alone.len(),
        "unviable": unviable.len() + counts.get("unviable").copied().unwrap_or(0),
        "baselines": baselines.len(),
        "failedBaselines": baselines.values().filter(|ran| ran.outcome != Outcome::Passed).count(),
        "statuses": counts,
        "durationMs": started.elapsed().as_millis() as u64,
    }));
    Ok(0)
}

/// The plan's Rust mutants to run, each found again in its file as the
/// plan names it, and the files read; a mutant whose file changed since the
/// run gets its `error` here.
fn planned_jobs(
    plan: &Value,
    root: &Path,
    tests: &[PlannedTest],
    results: &Results,
    started: Instant,
) -> (Vec<Job>, Sources) {
    let mut jobs = Vec::new();
    let mut sources = BTreeMap::<String, Option<(String, Vec<RustMutant>)>>::new();
    let undecided = plan["mutants"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|planned| planned["decided"].is_null())
        .map(|planned| (planned, false));
    let decided = plan["decided"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|planned| (planned, true));
    for (planned, check_only) in undecided.chain(decided) {
        let id = planned["id"].as_str().unwrap_or_default().to_owned();
        let file = planned["file"].as_str().unwrap_or_default().to_owned();
        let index = planned["index"].as_u64().unwrap_or(u64::MAX) as usize;
        let listed = sources.entry(file.clone()).or_insert_with(|| {
            // fault-site: rust-strong.planned-jobs
            let source = crate::fault_fs::read_to_string(root.join(&file)).ok()?;
            let mutants = crate::rust_mutants::enumerate(&source)?;
            Some((source, mutants))
        });
        let Some(mutant) = listed
            .as_ref()
            .and_then(|(_, mutants)| mutants.get(index))
            .filter(|mutant| mutant_id(&file, mutant) == id)
        else {
            // A decided one keeps the plan's verdict.
            if check_only {
                continue;
            }
            results.verdict(
                &id,
                "error",
                None,
                0,
                started,
                Some(format!("{file} changed since the run: measure it again")),
            );
            continue;
        };
        let candidates = planned["candidates"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|candidate| candidate["test"].as_u64())
            .map(|test| test as usize)
            .filter(|test| *test < tests.len())
            .collect::<Vec<_>>();
        jobs.push(Job {
            id,
            file,
            ordinal: index as u64,
            mutant: mutant.clone(),
            candidates,
            check_only,
            passed_before: planned["passedBefore"].as_u64().unwrap_or(0) as usize,
        });
    }
    (jobs, sources)
}

/// The runner all of `job`'s candidates run under, where it is one and not
/// Rust's.
fn foreign_runner<'t>(job: &Job, tests: &'t [PlannedTest]) -> Option<&'t str> {
    let first = tests[*job.candidates.first()?].runner.as_str();
    let foreign = !matches!(first, "rust-libtest" | "nextest" | "rustdoc");
    (foreign
        && job
            .candidates
            .iter()
            .all(|test| tests[*test].runner == first))
    .then_some(first)
}

/// Each file a plan's mutants are in: its source and its mutants, or `None`
/// where it no longer reads or parses.
type Sources = BTreeMap<String, Option<(String, Vec<RustMutant>)>>;

/// The name of the package whose manifest is nearest `file`, up from it.
fn package_of(root: &Path, file: &str) -> Option<String> {
    let file = root.join(file);
    for directory in file
        .ancestors()
        .skip(1)
        .take_while(|directory| directory.starts_with(root))
    {
        // fault-site: rust-strong.package-of
        let Ok(text) = crate::fault_fs::read_to_string(directory.join("Cargo.toml")) else {
            continue;
        };
        let manifest = text.parse::<toml::Table>().ok()?;
        if let Some(name) = manifest
            .get("package")
            .and_then(|package| package.get("name"))
            .and_then(|name| name.as_str())
        {
            return Some(name.to_owned());
        }
    }
    None
}

fn planned_tests(plan: &Value) -> Vec<PlannedTest> {
    plan["tests"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|test| PlannedTest {
            id: test["id"].as_str().unwrap_or_default().to_owned(),
            name: test["title"]
                .as_str()
                .or(test["name"].as_str())
                .unwrap_or_default()
                .to_owned(),
            runner: test["runner"].as_str().unwrap_or_default().to_owned(),
            project: test["file"].as_str().map(str::to_owned),
            runnable: test["runnable"].as_bool().unwrap_or(true),
        })
        .collect()
}

/// The `cargo test` that builds what the command tests: a nextest run's
/// shared options, given to `cargo test`.
fn build_invocation(invocation: &CargoTestInvocation) -> CargoTestInvocation {
    let position = invocation.command_position();
    let mut arguments = invocation.arguments[..position].to_vec();
    arguments.push("test".into());
    let skip = match invocation.kind {
        RustCargoCommandKind::CargoTest => 1,
        RustCargoCommandKind::NextestRun => 2,
    };
    // What follows `--` is the runner's (`runner_arguments`), never here.
    let mut rest = invocation.arguments[position + skip..].iter();
    while let Some(argument) = rest.next() {
        if !argument.starts_with('-') {
            // A test name filter: the pass runs tests by their exact names.
            continue;
        }
        let name = argument
            .split_once('=')
            .map_or(argument.as_str(), |(n, _)| n);
        // A nextest option, or one this build does not need, is left out.
        if let Some(takes_value) = crate::rust_test_runner::cargo_option_value(name) {
            arguments.push(argument.clone());
            if takes_value
                && !argument.contains('=')
                && let Some(value) = rest.next()
            {
                arguments.push(value.clone());
            }
        }
    }
    CargoTestInvocation {
        program: invocation.program.clone(),
        kind: RustCargoCommandKind::CargoTest,
        arguments,
        runner_arguments: Vec::new(),
    }
}

/// Makes `copy` hold the project's sources as written: each file that
/// differs is written again, and one that does not is left alone, so Cargo
/// rebuilds only what changed. Its file operations fail, for a test, at the
/// place it is called from.
#[track_caller]
fn sync_project(root: &Path, copy: &Path) -> Result<(), String> {
    #[track_caller]
    fn walk(from: &Path, to: &Path, top: bool) -> std::io::Result<()> {
        crate::fault_fs::create_dir_all(to)?;
        for entry in crate::fault_fs::read_dir(from)? {
            let entry = entry?;
            let name = entry.file_name();
            let text = name.to_string_lossy();
            if top
                && matches!(
                    text.as_ref(),
                    ".git" | ".supercov" | "target" | "node_modules"
                )
            {
                continue;
            }
            let kind = crate::fault_fs::symlink_metadata(entry.path())?.file_type();
            let target = to.join(&name);
            if kind.is_dir() {
                walk(&entry.path(), &target, false)?;
            } else if kind.is_file() || kind.is_symlink() {
                let Ok(bytes) = crate::fault_fs::read(entry.path()) else {
                    continue;
                };
                if crate::fault_fs::read(&target).ok().as_deref() != Some(bytes.as_slice()) {
                    crate::fault_fs::write(&target, &bytes)?;
                }
            }
        }
        Ok(())
    }
    walk(root, copy, true).map_err(|error| format!("{}: {error}", copy.display()))
}

/// A build of the project and how to run its tests.
struct Built {
    project: crate::rust_project::PreparedRustProject,
    artifacts: Vec<Artifact>,
    launcher: crate::rust_test_runner::ArtifactLauncher,
    target_libdir: PathBuf,
}

/// `cargo test --no-run` in `project`'s workspace: its test binaries, or the
/// compiler's errors.
fn cargo_build(
    project: &crate::rust_project::PreparedRustProject,
    invocation: &CargoTestInvocation,
    stop: &Stop,
) -> Result<Result<Vec<Artifact>, Vec<CompileError>>, String> {
    let mut arguments = invocation.arguments.clone();
    arguments.extend(["--no-run".into(), "--message-format=json".into()]);
    let output = supervised(
        Command::new(&invocation.program)
            .args(&arguments)
            .current_dir(&project.workspace_root)
            .env("CARGO_TARGET_DIR", &project.target_directory)
            .env(
                "RUSTFLAGS",
                crate::rust_test_runner::capped_rustflags(project, invocation),
            ),
        None,
        stop,
    )
    .map_err(|error| format!("cargo: {error}"))?;
    // A build the stop cut short says nothing of its mutants.
    let Some(status) = output.status.filter(|_| !output.stopped) else {
        return Err(STOPPED.into());
    };
    let workspace = &project.workspace_root;
    let mut artifacts = Vec::new();
    let mut errors = Vec::new();
    for line in output.stdout.split(|byte| *byte == b'\n') {
        let Ok(message) = serde_json::from_slice::<Value>(line) else {
            continue;
        };
        match message["reason"].as_str() {
            Some("compiler-artifact") if message["profile"]["test"] == true => {
                let (Some(executable), Some(source)) = (
                    message["executable"].as_str(),
                    message["target"]["src_path"].as_str(),
                ) else {
                    continue;
                };
                let source =
                    // fault-site: rust-strong.cargo-build
                    crate::fault_fs::canonicalize(source).unwrap_or_else(|_| source.into());
                let Ok(relative) = source.strip_prefix(workspace) else {
                    continue;
                };
                artifacts.push(Artifact {
                    executable: executable.into(),
                    source: relative.to_string_lossy().replace('\\', "/"),
                    package_directory: message["manifest_path"]
                        .as_str()
                        .and_then(|path| Path::new(path).parent())
                        .unwrap_or(workspace)
                        .to_path_buf(),
                });
            }
            Some("compiler-message") if message["message"]["level"] == "error" => {
                errors.push(compile_error(&message["message"], workspace));
            }
            _ => {}
        }
    }
    if !status.success() {
        if errors.is_empty() {
            return Err(String::from_utf8_lossy(&output.stderr).trim().to_owned());
        }
        return Ok(Err(errors));
    }
    artifacts.sort_by(|left, right| left.executable.cmp(&right.executable));
    artifacts.dedup_by(|left, right| left.executable == right.executable);
    Ok(Ok(artifacts))
}

/// An error and every span it and its notes point at, by file relative to
/// the workspace.
fn compile_error(message: &Value, workspace: &Path) -> CompileError {
    let mut spans = Vec::new();
    let mut collect = |message: &Value| {
        // An error inside a macro's expansion (`vec![Default::default()]`)
        // points into the macro too: its call site, up the chain, is where
        // the code is.
        let spans_and_call_sites = message["spans"]
            .as_array()
            .into_iter()
            .flatten()
            .flat_map(|span| {
                std::iter::successors(Some(span), |span| {
                    Some(&span["expansion"]["span"]).filter(|site| site.is_object())
                })
            })
            .collect::<Vec<_>>();
        for span in spans_and_call_sites {
            let (Some(file), Some(start), Some(end)) = (
                span["file_name"].as_str(),
                span["byte_start"].as_u64(),
                span["byte_end"].as_u64(),
            ) else {
                continue;
            };
            let path = Path::new(file);
            // `/rustc/…/library/…` has a root but, on Windows, no drive: it is
            // no less outside the workspace there.
            let relative = if path.has_root() {
                // fault-site: rust-strong.compile-error
                crate::fault_fs::canonicalize(path)
                    .ok()
                    .and_then(|path| path.strip_prefix(workspace).ok().map(Path::to_path_buf))
            } else {
                Some(path.to_path_buf())
            };
            if let Some(relative) = relative {
                spans.push((
                    relative.to_string_lossy().replace('\\', "/"),
                    start as usize..end as usize,
                ));
            }
        }
    };
    collect(message);
    for child in message["children"].as_array().into_iter().flatten() {
        collect(child);
    }
    CompileError {
        code: message["code"]["code"].as_str().map(str::to_owned),
        spans,
        rendered: message["message"].as_str().unwrap_or_default().to_owned(),
    }
}

/// A build's test binaries, or the compiler's errors.
type BuildOutcome = Result<Vec<Artifact>, Vec<CompileError>>;

/// The switched copy with the mutants at `chosen` (positions in `jobs`),
/// built: its preparation, and its test binaries or its errors.
fn switched_build(
    root: &Path,
    places: &Places,
    jobs: &[Job],
    chosen: &[usize],
    invocation: &CargoTestInvocation,
    stop: &Stop,
) -> Result<(crate::rust_project::StrongWorkspace, BuildOutcome), String> {
    // fault-site: rust-strong.switched-build
    sync_project(root, &places.switched)?;
    let mut by_file = BTreeMap::<String, Vec<(u64, RustMutant)>>::new();
    for at in chosen {
        let job = &jobs[*at];
        by_file
            .entry(job.file.clone())
            .or_default()
            .push((job.ordinal, job.mutant.clone()));
    }
    let mut prepared = crate::rust_project::prepare_rust_strong_project(&places.switched, &by_file)
        .map_err(|error| error.to_string())?;
    prepared.project.target_directory = places.switched_target.clone();
    let built = cargo_build(&prepared.project, invocation, stop)?;
    Ok((prepared, built))
}

/// Builds the switched copy with the mutants in `switchable`, taking out
/// those its errors come from until it builds: the unviable ones into
/// `unviable`, the ones to build on their own into `alone`.
#[allow(clippy::too_many_arguments)]
fn build_switched(
    root: &Path,
    places: &Places,
    jobs: &[Job],
    switchable: &mut Vec<usize>,
    alone: &mut Vec<(usize, &'static str)>,
    unviable: &mut BTreeMap<usize, String>,
    invocation: &CargoTestInvocation,
    runner_environment: &[(String, String)],
    diagnostics: &mut dyn Write,
    stop: &Stop,
) -> Result<Built, String> {
    let mut builds = 0usize;
    loop {
        builds += 1;
        let _ = writeln!(
            diagnostics,
            "[supercov] building {} switched mutant(s) (build {builds})",
            switchable.len()
        );
        let (prepared, built) = switched_build(root, places, jobs, switchable, invocation, stop)?;
        if !prepared.declined.is_empty() {
            let declined = prepared
                .declined
                .iter()
                .cloned()
                .collect::<HashMap<(String, u64), &'static str>>();
            switchable.retain(|at| match declined.get(&key(&jobs[*at])) {
                Some(why) => {
                    alone.push((*at, why));
                    false
                }
                None => true,
            });
            continue;
        }
        let errors = match built {
            Ok(artifacts) => {
                let launcher = crate::rust_test_runner::ArtifactLauncher::for_invocation(
                    &prepared.project,
                    invocation,
                    &places.switched_target.join("supercov-hooks"),
                )
                .map_err(|error| error.to_string())?
                .with_environment(runner_environment);
                let target_libdir =
                    crate::rust_test_runner::rustc_target_libdir(&prepared.project, invocation)
                        .map_err(|error| error.to_string())?;
                return Ok(Built {
                    project: prepared.project,
                    artifacts,
                    launcher,
                    target_libdir,
                });
            }
            Err(errors) => errors,
        };
        let ordinal_of = switchable
            .iter()
            .map(|at| (key(&jobs[*at]), *at))
            .collect::<HashMap<_, _>>();
        let mut out = BTreeSet::new();
        // A failed build has at least one error.
        errors.iter().for_each(|error| {
            let (own, at_switch) = attribute(error, &prepared.switched);
            // A macro the mutant's code names that is not in scope (`vec!` in
            // a `no_std` crate) has no error code.
            let typed = error
                .code
                .as_deref()
                .map_or(error.rendered.starts_with("cannot find macro"), |code| {
                    OWN_ERRORS.contains(&code)
                });
            // The switched files hold only the mutants in `switchable`.
            if own.len() == 1 && typed {
                let at = ordinal_of[own.iter().next().expect("one")];
                unviable
                    .entry(at)
                    .or_insert_with(|| first_line(&error.rendered));
                out.insert(at);
            } else {
                for ordinal in own.iter().chain(&at_switch) {
                    let at = ordinal_of[ordinal];
                    if !unviable.contains_key(&at) && out.insert(at) {
                        alone.push((at, "switch-error"));
                    }
                }
            }
        });
        if !out.is_empty() {
            switchable.retain(|at| !out.contains(at));
            continue;
        }
        // Errors at no switch with decided mutants in the build: they were
        // there only to find whether they compile, and they leave it all at
        // once (keeping the plan's verdicts) rather than cost a halving.
        if switchable.iter().any(|at| jobs[*at].check_only) {
            switchable.retain(|at| !jobs[*at].check_only);
            continue;
        }
        // Errors at no switch: the first mutant whose switch breaks the
        // build, by halving.
        let (mut low, mut high) = (0usize, switchable.len());
        // The first `low` switched mutants build; the first `high` do not
        // (it is the build that just failed).
        let fails = |count: usize| -> Result<bool, String> {
            let (_, built) =
                switched_build(root, places, jobs, &switchable[..count], invocation, stop)?;
            Ok(built.is_err())
        };
        builds += 1;
        if fails(0)? {
            return Err(format!(
                "the project does not build without mutants: {}",
                errors
                    .iter()
                    .map(|error| first_line(&error.rendered))
                    .collect::<Vec<_>>()
                    .join("; ")
            ));
        }
        while high - low > 1 {
            let middle = (low + high) / 2;
            builds += 1;
            if fails(middle)? {
                high = middle;
            } else {
                low = middle;
            }
        }
        let culprit = switchable[high - 1];
        switchable.retain(|at| *at != culprit);
        alone.push((culprit, "switch-error"));
    }
}

/// A JavaScript strong pass over Rust compiled to wasm
/// (`docs/design/mutation-drivers.md`): switches the plan's Rust mutants of
/// the Cargo workspace at `prefix` (its directory in the project, `""` at
/// the project's root) into the copy's workspace at `copy`, where the
/// command builds the wasm. A mutant a switch cannot hold, or whose file
/// changed since the run, is an `error` here, and one that does not compile
/// `unviable`; those are taken out of the plan the JavaScript driver runs.
/// Which mutants compile is found as the Rust pass finds it, by building:
/// here `cargo build --lib` for wasm, in a scratch copy under `root`.
/// Returns how many mutants are switched in.
pub(crate) fn switch_wasm_workspace(
    root: &Path,
    prefix: &str,
    copy: &Path,
    plan_path: &Path,
    results_path: &Path,
    cargo: &WasmCargo,
    diagnostics: &mut dyn Write,
) -> Result<usize, String> {
    let outcome = switch_wasm(
        root,
        prefix,
        copy,
        plan_path,
        results_path,
        cargo,
        diagnostics,
    );
    // The scratch copy only told which mutants compile.
    discard(root, ".supercov/strong/rust-wasm");
    outcome
}

/// [`switch_wasm_workspace`], leaving its scratch copy behind.
fn switch_wasm(
    root: &Path,
    prefix: &str,
    copy: &Path,
    plan_path: &Path,
    results_path: &Path,
    cargo: &WasmCargo,
    diagnostics: &mut dyn Write,
) -> Result<usize, String> {
    let started = Instant::now();
    let mut plan: Value = serde_json::from_slice(
        // fault-site: rust-strong.switch-wasm.read
        &crate::fault_fs::read(plan_path).map_err(|e| format!("{}: {e}", plan_path.display()))?,
    )
    .map_err(|error| format!("{}: {error}", plan_path.display()))?;
    let under = |file: &str| -> Option<String> {
        if prefix.is_empty() {
            return Some(file.to_owned());
        }
        file.strip_prefix(prefix)?
            .strip_prefix('/')
            .map(str::to_owned)
    };
    let ours = |mutant: &Value| {
        mutant["decided"].is_null()
            && mutant["language"] == crate::mutant_list::RUST
            && mutant["file"].as_str().and_then(under).is_some()
    };
    let mut own = plan.clone();
    own["mutants"] = Value::Array(
        plan["mutants"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|mutant| ours(mutant))
            .cloned()
            .collect(),
    );
    if own["mutants"].as_array().is_none_or(Vec::is_empty) {
        return Ok(0);
    }
    let results = Results::create(results_path)?;
    let tests = planned_tests(&own);
    let (jobs, sources) = planned_jobs(&own, root, &tests, &results, started);
    let mut switchable = Vec::new();
    for (at, job) in jobs.iter().enumerate() {
        let (source, _) = sources[&job.file].as_ref().expect("a job's file was read");
        let (tree, _) =
            crate::rust_instrumenter::parse_source(source).expect("the file was enumerated");
        let tree = ra_ap_syntax::AstNode::syntax(&tree).clone();
        match crate::rust_schemata::placeable(&tree, &job.mutant) {
            Some(reason) => results.verdict(
                &job.id,
                "error",
                None,
                0,
                started,
                Some(format!(
                    "a switch cannot hold it ({reason}), and a JavaScript strong pass builds the wasm once"
                )),
            ),
            None => switchable.push(at),
        }
    }
    // Each mutant by its file in the workspace and its index there.
    let by_key = jobs
        .iter()
        .enumerate()
        .map(|(at, job)| ((under(&job.file).expect("ours"), job.ordinal), at))
        .collect::<HashMap<_, _>>();
    let by_file = |chosen: &[usize]| {
        let mut by_file = BTreeMap::<String, Vec<(u64, RustMutant)>>::new();
        for at in chosen {
            let job = &jobs[*at];
            by_file
                .entry(under(&job.file).expect("ours"))
                .or_default()
                .push((job.ordinal, job.mutant.clone()));
        }
        by_file
    };
    let packages = jobs
        .iter()
        .filter_map(|job| package_of(root, &job.file))
        .collect::<BTreeSet<_>>();
    let scratch = root
        .join(".supercov/strong/rust-wasm")
        .join(if prefix.is_empty() { "_" } else { prefix });
    let checked = scratch.join("switched");
    let target = scratch.join("target");
    let source_root = root.join(prefix);
    let build = |chosen: &[usize]| -> Result<_, String> {
        // fault-site: rust-strong.switch-wasm.sync-project
        sync_project(&source_root, &checked)?;
        let prepared = crate::rust_project::prepare_rust_strong_project(&checked, &by_file(chosen))
            .map_err(|error| error.to_string())?;
        let errors = wasm_check(cargo, &prepared.project.workspace_root, &target, &packages)?;
        Ok((prepared, errors))
    };
    let mut unviable = BTreeMap::<usize, String>::new();
    let mut builds = 0usize;
    loop {
        builds += 1;
        let _ = writeln!(
            diagnostics,
            "[supercov] building {} switched Rust mutant(s) for wasm (build {builds})",
            switchable.len()
        );
        let (prepared, errors) = build(&switchable)?;
        if errors.is_empty() {
            break;
        }
        let mut out = BTreeSet::new();
        let mut unexplained = Vec::new();
        // A build with errors has one at least.
        errors.iter().for_each(|error| {
            // An error in a mutant's own code: the mutant does not compile.
            // Any other (one at a switch, which `placeable` keeps out, or in
            // code no mutant changed) is no mutant's.
            match attribute(error, &prepared.switched).0.first() {
                Some(key) => {
                    let at = by_key[key];
                    unviable
                        .entry(at)
                        .or_insert_with(|| first_line(&error.rendered));
                    out.insert(at);
                }
                None => unexplained.push(first_line(&error.rendered)),
            }
        });
        // Only errors that are no mutant's: the crate does not build.
        if out.is_empty() {
            return Err(format!(
                "the crate does not build for wasm: {}",
                unexplained.join("; ")
            ));
        }
        switchable.retain(|at| !out.contains(at));
    }
    for (at, error) in &unviable {
        results.verdict(
            &jobs[*at].id,
            "unviable",
            None,
            0,
            started,
            Some(format!("does not compile: {error}")),
        );
    }
    crate::rust_project::prepare_rust_strong_project(copy, &by_file(&switchable))
        .map_err(|error| error.to_string())?;
    let _ = writeln!(
        diagnostics,
        "[supercov] switched {} Rust mutant(s) into the wasm the command builds; {} unviable",
        switchable.len(),
        unviable.len()
    );
    // The JavaScript driver runs only what is switched in.
    let kept = switchable
        .iter()
        .map(|at| jobs[*at].id.as_str())
        .collect::<BTreeSet<_>>();
    plan["mutants"]
        .as_array_mut()
        .expect("a plan with mutants of this workspace lists them")
        .retain(|mutant| !ours(mutant) || kept.contains(mutant["id"].as_str().unwrap_or("")));
    // fault-site: rust-strong.switch-wasm.write
    crate::fault_fs::write(
        plan_path,
        serde_json::to_vec(&plan).expect("plain data serializes"),
    )
    .map_err(|e| format!("{}: {e}", plan_path.display()))?;
    Ok(switchable.len())
}

/// `cargo build --lib` of `packages` for `wasm32-unknown-unknown` in
/// `workspace` with `cargo`: the compiler's errors, none where it built.
/// The Cargo a wasm build runs, and the flags it gives rustc (the user's
/// `RUSTFLAGS`).
pub(crate) struct WasmCargo {
    pub program: String,
    pub rustflags: String,
}

fn wasm_check(
    cargo: &WasmCargo,
    workspace: &Path,
    target: &Path,
    packages: &BTreeSet<String>,
) -> Result<Vec<CompileError>, String> {
    let rustflags = [
        cargo.rustflags.as_str(),
        "--cap-lints=warn -A unused_parens",
    ]
    .into_iter()
    .filter(|flags| !flags.is_empty())
    .collect::<Vec<_>>()
    .join(" ");
    let mut command = Command::new(&cargo.program);
    command.args([
        "build",
        "--lib",
        "--target",
        "wasm32-unknown-unknown",
        "--message-format=json",
    ]);
    command.args(packages.iter().flat_map(|package| ["-p", package.as_str()]));
    let output = crate::child_signal_guard::output(
        command
            .current_dir(workspace)
            .env("CARGO_TARGET_DIR", target)
            .env("RUSTFLAGS", rustflags),
    )
    .map_err(|error| format!("cargo: {error}"))?;
    let errors = output
        .stdout
        .split(|byte| *byte == b'\n')
        .filter_map(|line| serde_json::from_slice::<Value>(line).ok())
        .filter(|message| {
            message["reason"] == "compiler-message" && message["message"]["level"] == "error"
        })
        .map(|message| compile_error(&message["message"], workspace))
        .collect::<Vec<_>>();
    if !output.status.success() && errors.is_empty() {
        return Err(format!(
            "cargo build for wasm: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(errors)
}

/// A mutant by its file and its index there.
type MutantKey = (String, u64);

/// A job's mutant: its file and its index there.
fn key(job: &Job) -> (String, u64) {
    (job.file.clone(), job.ordinal)
}

/// The mutants (by file and index) an error is in the own code of, and
/// those of the innermost switch it is in.
fn attribute(
    error: &CompileError,
    switched: &BTreeMap<String, crate::rust_schemata::SwitchedFile>,
) -> (BTreeSet<MutantKey>, BTreeSet<MutantKey>) {
    let mut own = BTreeSet::new();
    let mut at_switch = BTreeSet::new();
    for (name, range) in &error.spans {
        let Some(file) = switched.get(name) else {
            continue;
        };
        own.extend(
            file.replacements
                .iter()
                .filter(|(_, code)| code.start <= range.start && range.end <= code.end)
                .map(|(ordinal, _)| (name.clone(), *ordinal)),
        );
        if let Some((_, ordinals)) = file
            .switches
            .iter()
            .filter(|(whole, _)| whole.start <= range.start && range.end <= whole.end)
            .min_by_key(|(whole, _)| whole.len())
        {
            at_switch.extend(ordinals.iter().map(|ordinal| (name.clone(), *ordinal)));
        }
    }
    (own, at_switch)
}

fn first_line(text: &str) -> String {
    text.lines().next().unwrap_or_default().trim().to_owned()
}

/// The plain copy with one mutant written into its file, built.
fn build_alone(
    root: &Path,
    places: &Places,
    invocation: &CargoTestInvocation,
    runner_environment: &[(String, String)],
    job: &Job,
    source: &str,
    stop: &Stop,
) -> Result<Result<Built, String>, String> {
    // fault-site: rust-strong.build-alone.sync-project
    sync_project(root, &places.plain)?;
    let range = usize::from(job.mutant.range.start())..usize::from(job.mutant.range.end());
    let mutated = format!(
        "{}{}{}",
        &source[..range.start],
        job.mutant.replacement,
        &source[range.end..]
    );
    let path = places.plain.join(&job.file);
    // fault-site: rust-strong.build-alone.write
    crate::fault_fs::write(&path, mutated)
        .map_err(|error| format!("{}: {error}", path.display()))?;
    // fault-site: rust-strong.build-alone.canonicalize
    let workspace = crate::fault_fs::canonicalize(&places.plain)
        .map_err(|error| format!("{}: {error}", places.plain.display()))?;
    let mut project = crate::rust_project::PreparedRustProject::plain(workspace);
    project.target_directory = places.plain_target.clone();
    match cargo_build(&project, invocation, stop)? {
        Err(errors) => Ok(Err(errors
            .first()
            .map(|error| first_line(&error.rendered))
            .unwrap_or_default())),
        Ok(artifacts) => {
            let launcher = crate::rust_test_runner::ArtifactLauncher::for_invocation(
                &project,
                invocation,
                &places.plain_target.join("supercov-hooks"),
            )
            .map_err(|error| error.to_string())?
            .with_environment(runner_environment);
            let target_libdir = crate::rust_test_runner::rustc_target_libdir(&project, invocation)
                .map_err(|error| error.to_string())?;
            Ok(Ok(Built {
                project,
                artifacts,
                launcher,
                target_libdir,
            }))
        }
    }
}

/// How the pass runs a test of a build.
struct TestRunner<'b> {
    /// The pass's results, beside which a process stopped at the hit limit
    /// says so.
    results: PathBuf,
    launcher: &'b crate::rust_test_runner::ArtifactLauncher,
    target_libdir: &'b Path,
    by_source: BTreeMap<String, &'b Artifact>,
    doctest: DoctestCommand,
    /// For a `cargo nextest run` command, how nextest runs a test of this
    /// build, with its configuration.
    nextest: Option<Nextest>,
    stop: Stop,
    /// Whether a process of this build says when it first evaluates its
    /// active mutant (`<results>.reached`): a native switched build does;
    /// a build of one mutant has no switch, and wasm has no file to say it
    /// in.
    reports_reach: bool,
}

/// `cargo nextest run` of one test of a build already made: nextest's
/// profile and configuration (its setup scripts, per-test environment,
/// timeouts and retries) apply, and nothing is built again
/// (`--binaries-metadata`, `--cargo-metadata`).
struct Nextest {
    program: String,
    /// Everything before the filter.
    arguments: Vec<String>,
    workspace: PathBuf,
    /// Each test binary's nextest id, by its executable.
    binary_ids: BTreeMap<PathBuf, String>,
}

impl Nextest {
    /// Lists the build's binaries with nextest, from `workspace`.
    fn prepare(
        invocation: &CargoTestInvocation,
        project: &crate::rust_project::PreparedRustProject,
        target: &Path,
    ) -> Result<Self, String> {
        let position = invocation.command_position();
        let prefix = invocation.arguments[..position].to_vec();
        let environment = [
            ("CARGO_TARGET_DIR".to_owned(), target.display().to_string()),
            (
                "RUSTFLAGS".to_owned(),
                crate::rust_test_runner::capped_rustflags(project, invocation),
            ),
        ];
        let (kept, build) = Self::options(invocation);
        let metadata = target.join("supercov-nextest");
        // fault-site: rust-strong.prepare.create-dir-all
        crate::fault_fs::create_dir_all(&metadata)
            .map_err(|e| format!("{}: {e}", metadata.display()))?;
        let binaries = metadata.join("binaries.json");
        let cargo = metadata.join("cargo.json");
        let listed = crate::child_signal_guard::output(
            Command::new(&invocation.program)
                .args(&prefix)
                .args([
                    "nextest",
                    "list",
                    "--list-type",
                    "binaries-only",
                    "--message-format",
                    "json",
                ])
                .args(&build)
                .current_dir(&project.workspace_root)
                .envs(environment.clone()),
        )
        .map_err(|error| format!("cargo nextest list: {error}"))?;
        if !listed.status.success() {
            return Err(format!(
                "cargo nextest list failed: {}",
                String::from_utf8_lossy(&listed.stderr).trim()
            ));
        }
        // fault-site: rust-strong.prepare.write-1
        crate::fault_fs::write(&binaries, &listed.stdout)
            .map_err(|e| format!("{}: {e}", binaries.display()))?;
        let described = crate::child_signal_guard::output(
            Command::new(&invocation.program)
                .args(&prefix)
                .args([
                    "metadata",
                    "--format-version=1",
                    "--all-features",
                    "--no-deps",
                ])
                .current_dir(&project.workspace_root),
        )
        .map_err(|error| format!("cargo metadata: {error}"))?;
        // fault-site: rust-strong.prepare.write-2
        crate::fault_fs::write(&cargo, &described.stdout)
            .map_err(|e| format!("{}: {e}", cargo.display()))?;
        let parsed: Value = serde_json::from_slice(&listed.stdout)
            .map_err(|error| format!("cargo nextest list: {error}"))?;
        let binary_ids = parsed["rust-binaries"]
            .as_object()
            .into_iter()
            .flatten()
            .filter_map(|(id, binary)| {
                // fault-site: rust-strong.prepare.canonicalize
                let path = crate::fault_fs::canonicalize(binary["binary-path"].as_str()?).ok()?;
                Some((path, id.clone()))
            })
            .collect();
        let mut arguments = prefix;
        arguments.extend(["nextest".into(), "run".into(), "--no-fail-fast".into()]);
        arguments.extend(kept);
        arguments.push(format!("--binaries-metadata={}", binaries.display()));
        arguments.push(format!("--cargo-metadata={}", cargo.display()));
        arguments.push("--color=never".into());
        Ok(Self {
            program: invocation.program.clone(),
            arguments,
            workspace: project.workspace_root.clone(),
            binary_ids,
        })
    }

    /// The command's own nextest options that say how tests run, and its
    /// Cargo options that say what is built.
    fn options(invocation: &CargoTestInvocation) -> (Vec<String>, Vec<String>) {
        let mut kept = Vec::new();
        let mut build = Vec::new();
        let mut rest = invocation.arguments[invocation.command_position() + 2..].iter();
        while let Some(argument) = rest.next() {
            let (name, joined) = argument
                .split_once('=')
                .map_or((argument.as_str(), false), |(name, _)| (name, true));
            let (into, takes) = match name {
                "--profile" | "-P" | "--config-file" | "--tool-config-file" | "--retries"
                | "--test-threads" | "-j" => (&mut kept, true),
                _ => match crate::rust_test_runner::cargo_option_value(name) {
                    Some(takes) => (&mut build, takes),
                    // A nextest option for something else.
                    None => continue,
                },
            };
            into.push(argument.clone());
            if takes && !joined {
                into.extend(rest.next().cloned());
            }
        }
        (kept, build)
    }

    /// The command that runs exactly `name` of the binary at `executable`.
    fn command(&self, executable: &Path, name: &str) -> Option<Command> {
        let id = self
            .binary_ids
            // fault-site: rust-strong.command
            .get(&crate::fault_fs::canonicalize(executable).ok()?)?;
        let mut command = Command::new(&self.program);
        command
            .args(&self.arguments)
            .arg("-E")
            .arg(format!("binary_id(={id}) & test(={name})"))
            .current_dir(&self.workspace);
        Some(command)
    }
}

/// `cargo test --doc` for named doctests of a package, in a build's copy.
struct DoctestCommand {
    program: String,
    /// Everything before the doctests' names.
    arguments: Vec<String>,
    workspace: PathBuf,
    target: PathBuf,
    rustflags: String,
}

impl DoctestCommand {
    fn new(
        invocation: &CargoTestInvocation,
        project: &crate::rust_project::PreparedRustProject,
        target: &Path,
    ) -> Self {
        let build = build_invocation(invocation);
        let position = build.command_position();
        let mut arguments = build.arguments[..=position].to_vec();
        arguments.push("--doc".into());
        // Package and feature options; no other target.
        let mut rest = build.arguments[position + 1..].iter();
        while let Some(argument) = rest.next() {
            let name = argument
                .split_once('=')
                .map_or(argument.as_str(), |(n, _)| n);
            let keep = !matches!(
                name,
                "--lib"
                    | "--bins"
                    | "--bin"
                    | "--examples"
                    | "--example"
                    | "--tests"
                    | "--test"
                    | "--benches"
                    | "--bench"
                    | "--all-targets"
                    | "--doc"
                    | "-p"
                    | "--package"
                    | "--workspace"
                    | "--all"
                    | "--exclude"
            );
            let takes_value = crate::rust_test_runner::cargo_option_value(name).unwrap_or(false)
                && !argument.contains('=');
            let value = if takes_value { rest.next() } else { None };
            if keep {
                arguments.push(argument.clone());
                arguments.extend(value.cloned());
            }
        }
        Self {
            program: invocation.program.clone(),
            arguments,
            workspace: project.workspace_root.clone(),
            target: target.to_path_buf(),
            rustflags: crate::rust_test_runner::capped_rustflags(project, invocation),
        }
    }

    fn command(&self, package: &str, names: &[&str]) -> Command {
        let mut command = Command::new(&self.program);
        command
            .args(&self.arguments)
            // rustdoc's names do not match `--exact`; each is whole, from
            // its file's path to its line, and matches only itself.
            .args(["-p", package, "--"])
            .args(names)
            .current_dir(&self.workspace)
            .env("CARGO_TARGET_DIR", &self.target)
            .env("RUSTFLAGS", &self.rustflags);
        command
    }
}

impl TestRunner<'_> {
    /// Runs one libtest case, with `mutant` active where it is given.
    fn run(
        &self,
        test: &PlannedTest,
        mutant: Option<&str>,
        hit_limit: u64,
        limit: Duration,
    ) -> Option<Ran> {
        let (source, name) = test.id.split_once("::")?;
        let artifact = self.by_source.get(source)?;
        if let Some(nextest) = &self.nextest {
            let mut command = nextest.command(&artifact.executable, name)?;
            command.env(HIT_LIMIT_ENV, hit_limit.to_string());
            match mutant {
                Some(named) => command.env(ACTIVE_MUTANT_ENV, named),
                None => command.env_remove(ACTIVE_MUTANT_ENV),
            };
            command.env(crate::strong_driver::STRONG_RESULTS, &self.results);
            let (mut ran, _, stderr) = timed(&mut command, limit, &self.stop);
            // nextest's own timeout ends the test as a timeout, and a filter
            // that matched nothing ran nothing (once the pass stops, the
            // mutant's next test is stopped before it starts).
            if stderr.contains("TIMEOUT") {
                ran.outcome = Outcome::TimedOut;
            } else if stderr.contains("Starting 0 tests") {
                return None;
            }
            return Some(ran);
        }
        // The mutant and the limit are the test's own: a WASI module is
        // given them by name.
        let mut variables = vec![(HIT_LIMIT_ENV, hit_limit.to_string())];
        variables.extend(mutant.map(|named| (ACTIVE_MUTANT_ENV, named.to_owned())));
        let mut command = self
            .launcher
            .command_with_guest_environment(&artifact.executable, &variables);
        if mutant.is_none() {
            command.env_remove(ACTIVE_MUTANT_ENV);
        }
        command
            .args(["--exact", name])
            .current_dir(&artifact.package_directory)
            .envs(crate::rust_test_runner::instrumented_stack_environment())
            .envs(crate::rust_test_runner::dynamic_library_environment(
                self.target_libdir,
                &artifact.executable,
            ));
        command.env(crate::strong_driver::STRONG_RESULTS, &self.results);
        Some(timed(&mut command, limit, &self.stop).0)
    }

    /// Runs one attempt of [`attempts`], `members`: a libtest case, or a
    /// package's doctests together. How it ended, and the doctest that
    /// failed where one did; `None` where the build has no such test.
    fn attempt(
        &self,
        tests: &[PlannedTest],
        members: &[usize],
        mutant: Option<&str>,
        hit_limit: u64,
        limit: Duration,
    ) -> Option<(Ran, Option<String>)> {
        let first = &tests[members[0]];
        if first.runner != "rustdoc" {
            return self
                .run(first, mutant, hit_limit, limit)
                .map(|ran| (ran, None));
        }
        let names = members
            .iter()
            .map(|test| tests[*test].name.as_str())
            .collect::<Vec<_>>();
        let package = first.project.clone().unwrap_or_default();
        Some(self.run_doctests(&package, &names, mutant, limit))
    }

    /// Runs doctests of one package together, with `mutant` active.
    fn run_doctests(
        &self,
        package: &str,
        names: &[&str],
        mutant: Option<&str>,
        limit: Duration,
    ) -> (Ran, Option<String>) {
        let mut command = self.doctest.command(package, names);
        command.env(HIT_LIMIT_ENV, hit_limit().to_string());
        match mutant {
            Some(named) => command.env(ACTIVE_MUTANT_ENV, named),
            None => command.env_remove(ACTIVE_MUTANT_ENV),
        };
        command.env(crate::strong_driver::STRONG_RESULTS, &self.results);
        let (ran, stdout, _) = timed(&mut command, limit, &self.stop);
        // rustdoc writes the path with the platform's separator; the plan's
        // doctest ids use `/` everywhere, and a kill names its test by its id.
        let failed = stdout.lines().find_map(|line| {
            line.strip_prefix("test ")
                .and_then(|rest| rest.strip_suffix(" ... FAILED"))
                .map(|name| name.replace('\\', "/"))
        });
        (ran, failed)
    }
}

/// Whether the pass has been told to stop. The CLI writes `stopped-at`
/// beside the results before it signals the pass, for its budget or for a
/// signal alike (`docs/design/strong-mutation.md`, the budget). From then on
/// the pass starts nothing, stops what it runs, and judges nothing a stop
/// cut short: such a test says nothing of its mutant, which is left for the
/// next pass.
#[derive(Debug, Clone)]
struct Stop {
    marker: PathBuf,
}

impl Stop {
    /// The stop the CLI says beside `results`.
    fn beside(results: &Path) -> Self {
        Self {
            marker: results.with_file_name(crate::strong_driver::STOPPED_AT),
        }
    }

    fn now(&self) -> bool {
        self.marker.exists()
    }
}

/// How a process the pass started ended.
struct Supervised {
    /// Its exit, or `None` where the limit or the stop ended it.
    status: Option<std::process::ExitStatus>,
    /// The stop ended it, or it ended by a stopping signal once the pass
    /// was stopping.
    stopped: bool,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    duration: Duration,
}

/// Runs `command` to its end, its limit (where it has one) or the pass's
/// stop, whichever comes first, in a process group of its own: a limit or a
/// stop ends the group, so what a test started (nextest's test process
/// under `cargo nextest`, rustdoc's under `cargo test --doc`) ends with it,
/// and a terminal's Ctrl-C, sent to Supercov's group, reaches the pass alone,
/// which stops it as its budget does.
fn supervised(
    command: &mut Command,
    limit: Option<Duration>,
    stop: &Stop,
) -> std::io::Result<Supervised> {
    let started = Instant::now();
    // A pass that is stopping starts nothing.
    if stop.now() {
        return Ok(Supervised {
            status: None,
            stopped: true,
            stdout: Vec::new(),
            stderr: Vec::new(),
            duration: Duration::ZERO,
        });
    }
    process_group(command);
    let mut child = command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        // fault-site: rust-strong.supervised
        .launch()?;
    let _registered = crate::child_signal_guard::register_group(&child);
    let mut stdout = child.stdout.take().expect("piped");
    let mut stderr = child.stderr.take().expect("piped");
    let out = std::thread::spawn(move || {
        let mut text = Vec::new();
        let _ = stdout.read_to_end(&mut text);
        text
    });
    let err = std::thread::spawn(move || {
        let mut text = Vec::new();
        let _ = stderr.read_to_end(&mut text);
        text
    });
    // The stop is a file: looked for every 50 ms, not at every poll.
    let mut looked = Instant::now();
    let mut stopped = false;
    // A child that cannot be waited on runs, as far as this can tell, until
    // its limit or the stop.
    let status = loop {
        if let Some(status) = child.try_wait().ok().flatten() {
            break Some(status);
        }
        if looked.elapsed() >= STOP_POLL {
            looked = Instant::now();
            stopped = stop.now();
        }
        if stopped || limit.is_some_and(|limit| started.elapsed() > limit) {
            kill_group(&mut child);
            let _ = child.wait();
            break None;
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    let duration = started.elapsed();
    // A process ended by a signal that stops a pass, which reached it before
    // the pass heard of it, is the stop's: given a moment, the stop shows.
    if status.is_some_and(stopping_signal) {
        let since = Instant::now();
        loop {
            if stop.now() {
                stopped = true;
                break;
            }
            if since.elapsed() >= STOP_SETTLE {
                break;
            }
            std::thread::sleep(STOP_POLL);
        }
    }
    Ok(Supervised {
        status,
        stopped,
        stdout: out.join().unwrap_or_default(),
        stderr: err.join().unwrap_or_default(),
        duration,
    })
}

/// How often a running process's pass looks for its stop.
const STOP_POLL: Duration = Duration::from_millis(50);

/// How long a process ended by a stopping signal waits for the pass's stop
/// to show (the CLI writes it within a second of the signal).
const STOP_SETTLE: Duration = Duration::from_secs(2);

/// Whether `status` is an end by SIGINT, SIGTERM or SIGHUP, which stop a
/// pass, or the exit code a process gives for one (`cargo` and nextest pass
/// their test's on).
fn stopping_signal(status: std::process::ExitStatus) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if status
            .signal()
            .is_some_and(|signal| [libc::SIGINT, libc::SIGTERM, libc::SIGHUP].contains(&signal))
        {
            return true;
        }
    }
    status
        .code()
        .is_some_and(|code| matches!(code, 129 | 130 | 143))
}

/// A process the pass starts leads a process group of its own.
#[cfg(unix)]
fn process_group(command: &mut Command) {
    use std::os::unix::process::CommandExt;
    command.process_group(0);
}

#[cfg(not(unix))]
fn process_group(_command: &mut Command) {}

/// Ends `child` and every process of its group.
#[cfg(unix)]
fn kill_group(child: &mut std::process::Child) {
    // SAFETY: kill(2) with a negative pid signals a process group: the
    // child's own, made for it at its start.
    unsafe {
        libc::kill(-(child.id() as i32), libc::SIGKILL);
    }
}

#[cfg(not(unix))]
fn kill_group(child: &mut std::process::Child) {
    let _ = child.kill();
}

/// Runs a test's `command` for at most `limit`, or until the pass stops,
/// and how it ended, with its standard output and error.
fn timed(command: &mut Command, limit: Duration, stop: &Stop) -> (Ran, String, String) {
    let Ok(ended) = supervised(command, Some(limit), stop) else {
        return (
            Ran {
                outcome: Outcome::Unstarted,
                duration: Duration::ZERO,
            },
            String::new(),
            String::new(),
        );
    };
    let stdout = String::from_utf8_lossy(&ended.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&ended.stderr).into_owned();
    let outcome = match ended.status {
        _ if ended.stopped => Outcome::Stopped,
        None => Outcome::TimedOut,
        Some(_) if stderr.contains(TIMEOUT_MARKER) || stdout.contains(TIMEOUT_MARKER) => {
            Outcome::HitLimit
        }
        Some(status) if status.success() && !libtest_ran_nothing(&stdout) => Outcome::Passed,
        Some(_) => Outcome::Failed,
    };
    (
        Ran {
            outcome,
            duration: ended.duration,
        },
        stdout,
        stderr,
    )
}

/// A libtest run that matched no test: `running 0 tests`.
fn libtest_ran_nothing(stdout: &str) -> bool {
    stdout.contains("running 0 tests") && !stdout.contains("running 1 test")
}

/// Each needed test run once with no mutant.
fn baselines(
    runner: &TestRunner<'_>,
    tests: &[PlannedTest],
    needed: &BTreeSet<usize>,
    workers: usize,
) -> BTreeMap<usize, Ran> {
    let needed = needed.iter().copied().collect::<Vec<_>>();
    let next = AtomicUsize::new(0);
    let found = Mutex::new(BTreeMap::new());
    pool(workers.min(needed.len()), || {
        // As the mutants: each worker takes the next test until none is
        // left (once the pass stops, each ends at once).
        std::iter::from_fn(|| needed.get(next.fetch_add(1, Ordering::Relaxed))).for_each(|test| {
            let Some((ran, _)) = runner.attempt(tests, &[*test], None, 0, Duration::from_secs(600))
            else {
                return;
            };
            found.lock().expect("baselines").insert(*test, ran);
        });
    });
    found.into_inner().expect("baselines")
}

/// Judges again, alone, a mutant whose test ran past its limit beside the
/// pass's other tests: that test (or package's doctests) runs again without
/// the mutant first, in `plain`, and its limit is then 5 times the slower of
/// that run and its baseline, at least 20 s; then the mutant runs, in `mutated`,
/// against the candidates it has not passed. Only a test past that limit is
/// a `timeout`: one baseline measured while the machine was quieter does not
/// make a slow test a timeout once it is busier. A test that does not pass
/// without the mutant now says nothing of it: an `error`.
#[allow(clippy::too_many_arguments)]
fn judge_alone(
    job: &Job,
    again: &Slow,
    tests: &[PlannedTest],
    plain: &TestRunner<'_>,
    mutated: &TestRunner<'_>,
    usable: &dyn Fn(usize) -> bool,
    baselines: &BTreeMap<usize, Ran>,
    ordinal: Option<&str>,
    results: &Results,
) -> Ended {
    let started = Instant::now();
    let now = plain
        .attempt(tests, &again.tests, None, 0, Duration::from_secs(600))
        .map_or(Outcome::Unstarted, |(ran, _)| ran.outcome);
    let duration = started.elapsed();
    match now {
        // Stopped: the mutant's run ends at once too, with no verdict.
        Outcome::Passed | Outcome::Stopped => {}
        _ => {
            results.verdict(
                &job.id,
                "error",
                None,
                again.passed.len(),
                started,
                Some(format!(
                    "{} ran past its limit with the mutant, and did not pass without it when run again",
                    tests[again.tests[0]].id
                )),
            );
            return Ended::Decided("error");
        }
    }
    // The run without the mutant, shared among the tests that ran in it.
    let share = duration / again.tests.len() as u32;
    let limit_of = |test: usize| {
        let baseline = baselines
            .get(&test)
            .map_or(Duration::from_secs(10), |ran| ran.duration);
        let base = if again.tests.contains(&test) {
            baseline.max(share)
        } else {
            baseline
        };
        limit(base)
    };
    run_job(
        job,
        tests,
        mutated,
        usable,
        &limit_of,
        ordinal,
        results,
        Some(again),
    )
}

/// Runs one mutant against its candidates and writes its verdict: in the
/// switched build with `ordinal` active, or in a build of its own (`None`).
/// Judged `again` (`judge_alone`), it skips the candidates it passed and a
/// test past its limit is a `timeout`; otherwise such a test leaves it
/// [`Ended::Slow`].
#[allow(clippy::too_many_arguments)]
fn run_job(
    job: &Job,
    tests: &[PlannedTest],
    runner: &TestRunner<'_>,
    usable: &dyn Fn(usize) -> bool,
    limit_of: &dyn Fn(usize) -> Duration,
    ordinal: Option<&str>,
    results: &Results,
    again: Option<&Slow>,
) -> Ended {
    let started = Instant::now();
    let passed_before = again.map(|slow| slow.passed.as_slice()).unwrap_or_default();
    let mut candidates = job
        .candidates
        .iter()
        .copied()
        .filter(|test| usable(*test) && !passed_before.contains(test))
        .collect::<Vec<_>>();
    // Judged again, a mutant has the test that ran past its limit left.
    if candidates.is_empty() {
        // Tests the measured run saw fail cannot tell a mutant apart: it
        // lives, as StrykerJS's rule has it. Tests that passed there and
        // cannot run here say nothing of it: an error.
        if job.candidates.iter().any(|test| tests[*test].runnable) {
            results.verdict(
                &job.id,
                "error",
                None,
                0,
                started,
                Some("its tests did not pass without a mutant in the pass's build".into()),
            );
            return Ended::Decided("error");
        }
        let reason = if job.candidates.is_empty() {
            "no test can kill it"
        } else {
            "every test that reaches it failed in the measured run"
        };
        results.verdict(&job.id, "survived", None, 0, started, Some(reason.into()));
        return Ended::Decided("survived");
    }
    candidates.sort_by_key(|test| limit_of(*test));
    let mut ran = passed_before.len();
    let mut passed = passed_before.to_vec();
    // The first attempt that decides the mutant, its candidates cheapest
    // first; past them all, it survived.
    let decided = attempts(candidates, tests).into_iter().find_map(|members| {
        let limit = members.iter().map(|test| limit_of(*test)).sum::<Duration>();
        let (outcome, failed) = runner.attempt(tests, &members, ordinal, hit_limit(), limit)?;
        ran += members.len();
        // The test that failed, where the run named it (a package's
        // doctests run together), or the attempt's first test.
        let culprit = failed.unwrap_or_else(|| tests[members[0]].id.clone());
        let (status, reason) = match outcome.outcome {
            Outcome::Passed => {
                let ids = members
                    .iter()
                    .map(|test| tests[*test].id.as_str())
                    .collect::<Vec<_>>();
                results.passed(&job.id, &ids);
                passed.extend(&members);
                return None;
            }
            // A test the stop cut short says nothing: the mutant is left for
            // the next pass, which runs it against the candidates it has not
            // passed.
            Outcome::Stopped => return Some(Ended::Stopped),
            // Past its limit, beside other tests: the limit may be the
            // machine's load. It is judged again alone.
            Outcome::TimedOut if again.is_none() => {
                return Some(Ended::Slow(Slow {
                    tests: members,
                    passed: passed.clone(),
                }));
            }
            Outcome::Unstarted => ("error", Some(format!("could not start {culprit}"))),
            Outcome::Failed => ("killed", None),
            Outcome::TimedOut | Outcome::HitLimit => {
                ("timeout", Some(timeout_reason(limit, outcome.outcome)))
            }
        };
        let killed_by = (status != "error").then_some(culprit.as_str());
        results.verdict(&job.id, status, killed_by, ran, started, reason);
        Some(Ended::Decided(status))
    });
    decided.unwrap_or_else(|| {
        // Every candidate passed. Where no process ever ran the mutated
        // code, the tests never ran it in its mutated form -- they ran a
        // separately built artifact, such as a Worker's wasm -- and could
        // not have failed: not a survivor.
        if let Some(named) = ordinal
            && runner.reports_reach
            && job.passed_before == 0
            && !reached(&runner.results, named)
        {
            results.verdict(
                &job.id,
                "no-coverage",
                None,
                ran,
                started,
                Some(not_reached_reason(ran)),
            );
            return Ended::Decided("no-coverage");
        }
        results.verdict(&job.id, "survived", None, ran, started, None);
        Ended::Decided("survived")
    })
}

/// The reason code a mutant whose tests passed without running its mutated
/// code is given, before its plain words.
pub(crate) const NOT_REACHED_MUTATED: &str = "not-reached-mutated";

fn not_reached_reason(ran: usize) -> String {
    format!(
        "{NOT_REACHED_MUTATED}: its {ran} test(s) passed but never ran the mutated code; they ran a separately built artifact (such as a Worker's wasm built outside the pass) or a copy of the code the pass did not switch, so they could not fail"
    )
}

/// Whether a process of the pass whose results are at `results` said it
/// evaluated mutant `named` (`<file>#<index>`) switched on.
fn reached(results: &Path, named: &str) -> bool {
    let mut path = results.as_os_str().to_owned();
    path.push(".reached");
    // fault-site: rust-strong.reached
    crate::fault_fs::read_to_string(Path::new(&path))
        .is_ok_and(|text| text.lines().any(|line| line == named))
}

/// The processes a mutant's candidates run in, in order: each libtest or
/// nextest case alone, then each package's doctests together, after them.
fn attempts(candidates: Vec<usize>, tests: &[PlannedTest]) -> Vec<Vec<usize>> {
    let (doctests, libtests): (Vec<_>, Vec<_>) = candidates
        .into_iter()
        .partition(|test| tests[*test].runner == "rustdoc");
    let mut by_package = BTreeMap::<String, Vec<usize>>::new();
    for test in doctests {
        by_package
            .entry(tests[test].project.clone().unwrap_or_default())
            .or_default()
            .push(test);
    }
    libtests
        .into_iter()
        .map(|test| vec![test])
        .chain(by_package.into_values())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn invocation(command: &[&str]) -> CargoTestInvocation {
        crate::rust_test_runner::cargo_invocation(
            Path::new("."),
            &command
                .iter()
                .map(|word| (*word).to_owned())
                .collect::<Vec<_>>(),
        )
        .unwrap()
    }

    #[test]
    fn a_pass_builds_with_the_commands_cargo_options_and_no_filter() {
        let built = build_invocation(&invocation(&[
            "cargo",
            "test",
            "-p",
            "a",
            "--features=x",
            "name",
            "--",
            "--nocapture",
        ]));
        assert_eq!(built.arguments, ["test", "-p", "a", "--features=x"]);
        let built = build_invocation(&invocation(&[
            "cargo",
            "nextest",
            "run",
            "--workspace",
            "--release",
            "filter",
        ]));
        assert_eq!(built.kind, RustCargoCommandKind::CargoTest);
        assert_eq!(built.arguments, ["test", "--workspace", "--release"]);
        // A nextest option is left out (its value, read alone, is a filter
        // to the build), and a Cargo option last keeps no value it lacks.
        let built = build_invocation(&invocation(&[
            "cargo",
            "nextest",
            "run",
            "--retries",
            "2",
            "-p",
        ]));
        assert_eq!(built.arguments, ["test", "-p"]);
    }

    #[test]
    fn a_doctest_runs_by_the_package_nearest_its_file() {
        let root =
            std::env::temp_dir().join(format!("supercov-rust-strong-{}", std::process::id()));
        crate::fault_fs::create_dir_all(root.join("inner/src")).unwrap();
        crate::fault_fs::write(
            root.join("Cargo.toml"),
            "[workspace]\nmembers = [\"inner\"]\n",
        )
        .unwrap();
        crate::fault_fs::write(
            root.join("inner/Cargo.toml"),
            "[package]\nname = \"inner\"\n",
        )
        .unwrap();
        assert_eq!(
            package_of(&root, "inner/src/lib.rs").as_deref(),
            Some("inner")
        );
        assert_eq!(package_of(&root, "src/lib.rs"), None);
        assert_eq!(package_of(&root, ""), None);
        crate::fault_fs::create_dir_all(root.join("bad/src")).unwrap();
        crate::fault_fs::write(root.join("bad/Cargo.toml"), "[package\n").unwrap();
        assert_eq!(package_of(&root, "bad/src/lib.rs"), None);
        // The copy holds what the project holds, and keeps what is the same;
        // a link is copied as what it points at, and neither a dangling link
        // nor a socket is copied.
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink("Cargo.toml", root.join("inner/linked.toml")).unwrap();
            std::os::unix::fs::symlink("gone", root.join("inner/dangling")).unwrap();
            std::os::unix::net::UnixListener::bind(root.join("inner/socket")).unwrap();
        }
        let copy = root.join("copy");
        sync_project(&root.join("inner"), &copy).unwrap();
        assert!(copy.join("Cargo.toml").exists());
        #[cfg(unix)]
        {
            assert!(copy.join("linked.toml").is_file());
            assert!(!copy.join("socket").exists() && !copy.join("dangling").exists());
        }
        let written = crate::fault_fs::metadata(copy.join("Cargo.toml"))
            .unwrap()
            .modified()
            .unwrap();
        sync_project(&root.join("inner"), &copy).unwrap();
        assert_eq!(
            crate::fault_fs::metadata(copy.join("Cargo.toml"))
                .unwrap()
                .modified()
                .unwrap(),
            written
        );
        crate::fault_fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn an_error_belongs_to_the_mutant_whose_code_it_is_in() {
        let mut switched = BTreeMap::new();
        switched.insert(
            "src/a.rs".to_owned(),
            crate::rust_schemata::SwitchedFile {
                code: String::new(),
                replacements: vec![(3, 10..20), (4, 30..40)],
                switches: vec![(0..50, vec![3, 4]), (5..25, vec![3])],
            },
        );
        let error = |range: Range<usize>| CompileError {
            code: Some("E0308".into()),
            spans: vec![("src/a.rs".into(), range)],
            rendered: "mismatched types".into(),
        };
        let (own, around) = attribute(&error(12..15), &switched);
        assert_eq!(own, BTreeSet::from([("src/a.rs".to_owned(), 3)]));
        assert_eq!(around, BTreeSet::from([("src/a.rs".to_owned(), 3)]));
        let (own, around) = attribute(&error(44..46), &switched);
        assert!(own.is_empty());
        assert_eq!(around.len(), 2);
        // An error in a macro's expansion is also at the macro's call site.
        let expanded = compile_error(
            &json!({
                "code": {"code": "E0277"},
                "message": "the trait bound `T: Default` is not satisfied",
                "spans": [{
                    "file_name": "/rustc/library/alloc/src/macros.rs",
                    "byte_start": 1, "byte_end": 2,
                    "expansion": {"span": {"file_name": "src/a.rs", "byte_start": 12, "byte_end": 15}}
                }],
                "children": []
            }),
            Path::new("/workspace"),
        );
        assert_eq!(expanded.spans, [("src/a.rs".to_owned(), 12..15)]);
        assert!(libtest_ran_nothing("running 0 tests\n"));
        assert!(!libtest_ran_nothing("running 1 test\n"));
        assert!(!libtest_ran_nothing("running 0 tests\nrunning 1 test\n"));
        assert_eq!(first_line("a\nb"), "a");
    }

    /// A small crate and a plan over every mutant of its library.
    fn fixture(label: &str) -> (PathBuf, StrongRustRequest) {
        let root = std::env::temp_dir().join(format!(
            "supercov-rust-strong-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::create_dir_all(root.join("tests")).unwrap();
        std::fs::write(
            root.join("Cargo.toml"),
            "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[workspace]\n",
        )
        .unwrap();
        let source = concat!(
            "/// ```\n/// assert_eq!(fixture::double(3), 6);\n/// ```\n",
            "pub fn double(v: u32) -> u32 {\n    v * 2\n}\n",
            "pub fn greet(name: &str) -> String {\n    let mut out = String::from(\"hi \");\n    out += name;\n    out\n}\n",
            "pub const fn c() -> u8 {\n    1 + 1\n}\n",
            "pub fn spin(n: u32) -> u32 {\n    let mut i = 0;\n    while i < n {\n        i += 1;\n    }\n    i\n}\n",
        );
        std::fs::write(root.join("src/lib.rs"), source).unwrap();
        std::fs::write(
            root.join("tests/t.rs"),
            "#[test]\nfn d() { assert_eq!(fixture::double(2), 4); }\n#[test]\nfn g() { assert_eq!(fixture::greet(\"a\"), \"hi a\"); }\n#[test]\nfn c() { assert_eq!(fixture::c(), 2); }\n#[test]\nfn s() { assert!(fixture::spin(3) >= 3); }\n#[test]\nfn broken() { panic!(\"always\"); }\n",
        )
        .unwrap();
        let mutants = crate::rust_mutants::enumerate(source).unwrap();
        let tests = [
            "tests/t.rs::d",
            "tests/t.rs::g",
            "tests/t.rs::c",
            "tests/t.rs::s",
            "tests/t.rs::broken",
            "src/lib.rs - double (line 1)",
            "tests/t.rs::gone",
            "web/a.test.js > a",
            "tests/none.rs::x",
        ];
        let plan = json!({
            "tests": tests.iter().map(|id| {
                let (runner, name) = if id.contains(" - ") { ("rustdoc", *id) } else if id.ends_with(" > a") { ("vitest", *id) } else { ("rust-libtest", *id) };
                json!({"id": id, "file": id.split("::").next().unwrap().split(" - ").next().unwrap(), "name": name, "title": name, "runner": runner, "runnable": !id.ends_with("broken")})
            }).collect::<Vec<_>>(),
            "mutants": mutants.iter().enumerate().map(|(index, mutant)| {
                let candidates = match (mutant.start.0, mutant.descriptor()) {
                    // A candidate that failed in the measured run, and one
                    // the build does not have.
                    (5, "1") if mutant.genre == "FnValue" => vec![4],
                    (5, "0") if mutant.genre == "FnValue" => vec![6, 8],
                    // Only a JavaScript test over the crate's wasm.
                    (16, "1") if mutant.genre == "FnValue" => vec![7],
                    (4..=6, _) => vec![0, 5],
                    (7..=11, _) => vec![1],
                    (12..=14, _) => vec![2],
                    // A JavaScript test beside a Rust one: the Rust one runs.
                    _ => vec![7, 3],
                };
                json!({"id": mutant_id("src/lib.rs", mutant), "file": "src/lib.rs", "index": index, "decided": null,
                    "candidates": candidates.iter().map(|test| json!({"test": test})).collect::<Vec<_>>()})
            })
            .chain([
                json!({"id": "src/lib.rs:1:1-1:2:FnValue:gone", "file": "src/lib.rs", "index": 99, "decided": null, "candidates": []}),
                json!({"id": "decided", "file": "src/lib.rs", "index": 0, "decided": "survived"}),
            ])
            .collect::<Vec<_>>(),
        });
        std::fs::write(root.join("plan.json"), serde_json::to_vec(&plan).unwrap()).unwrap();
        let request = StrongRustRequest {
            root: root.clone(),
            command: vec!["cargo".into(), "test".into()],
            plan: root.join("plan.json"),
            results: root.join("results.jsonl"),
            workers: 1,
        };
        (root, request)
    }

    /// A crate of `files`, its tests in `tests/t.rs`, and a plan that runs
    /// every mutant of `mutated` against every test.
    fn crate_with(
        label: &str,
        files: &[(&str, &str)],
        mutated: &[&str],
    ) -> (PathBuf, StrongRustRequest) {
        let root = std::env::temp_dir().join(format!(
            "supercov-rust-strong-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        for (path, text) in files {
            let path = root.join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        }
        let tests = files
            .iter()
            .find(|(path, _)| *path == "tests/t.rs")
            .map(|(_, text)| {
                text.lines()
                    .filter_map(|line| line.strip_prefix("#[test] fn "))
                    .map(|rest| format!("tests/t.rs::{}", rest.split('(').next().unwrap()))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let mut mutants = Vec::new();
        for file in mutated {
            let source = std::fs::read_to_string(root.join(file)).unwrap();
            for (index, mutant) in crate::rust_mutants::enumerate(&source)
                .unwrap()
                .iter()
                .enumerate()
            {
                mutants.push(json!({"id": mutant_id(file, mutant), "file": file, "index": index,
                    "candidates": (0..tests.len()).map(|test| json!({"test": test})).collect::<Vec<_>>()}));
            }
        }
        let plan = json!({
            "tests": tests.iter().map(|id| json!({"id": id, "file": "tests/t.rs", "name": id, "runner": "rust-libtest"})).collect::<Vec<_>>(),
            "mutants": mutants,
        });
        std::fs::write(root.join("plan.json"), serde_json::to_vec(&plan).unwrap()).unwrap();
        let request = StrongRustRequest {
            root: root.clone(),
            command: vec!["cargo".into(), "test".into()],
            plan: root.join("plan.json"),
            results: root.join("results.jsonl"),
            workers: 1,
        };
        (root, request)
    }

    /// An executable shell script at `path`.
    #[cfg(unix)]
    fn script(path: &Path, body: &str) -> String {
        use std::os::unix::fs::PermissionsExt;
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        // A nextest profile the outer run names (CI's `affected`) is no
        // profile of the fixture's: the commands a test runs go without it.
        std::fs::write(path, format!("#!/bin/sh\nunset NEXTEST_PROFILE\n{body}")).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path.display().to_string()
    }

    /// `cargo`, except that a `--no-run` build whose copy holds `marker`
    /// prints `errors` (lines of shell that print compiler messages, given
    /// `at FILE TEXT N`: the byte N past where TEXT is in FILE) and fails.
    #[cfg(unix)]
    fn failing_cargo(root: &Path, rules: &[(&str, &str)]) -> String {
        let mut body = String::from(concat!(
            "case \" $* \" in *\" --no-run \"*) ;; *) exec cargo \"$@\" ;; esac\n",
            "at() { o=$(grep -boF \"$2\" \"$1\" | head -1 | cut -d: -f1); echo $((o + $3)); }\n",
            "error() { printf '{\"reason\":\"compiler-message\",\"message\":{\"level\":\"error\",\"code\":%s,\"message\":\"%s\",\"spans\":[%s],\"children\":[%s]}}\\n' \"$1\" \"$2\" \"$3\" \"$4\"; }\n",
            "span() { printf '{\"file_name\":\"%s\",\"byte_start\":%s,\"byte_end\":%s}' \"$1\" \"$2\" \"$(($2 + 1))\"; }\n",
            "failed=\n",
        ));
        for (marker, errors) in rules {
            body.push_str(&format!(
                "if grep -rqF '{marker}' src; then failed=1\n{errors}\nfi\n"
            ));
        }
        body.push_str("[ -z \"$failed\" ] || exit 101\nexec cargo \"$@\"\n");
        script(&root.join("bin/cargo"), &body)
    }

    /// Nextest's options: those that say how tests run are kept, Cargo's
    /// build options are given to the build, and the rest left out.
    #[test]
    fn nextest_keeps_how_tests_run_and_builds_what_the_command_builds() {
        let (kept, build) = Nextest::options(&invocation(&[
            "cargo",
            "nextest",
            "run",
            "--profile",
            "ci",
            "-j=4",
            "--no-capture",
            "--features",
            "x",
            "--release",
            "--target=t",
            "filter",
            "--",
            "--retries",
            "3",
        ]));
        assert_eq!(kept, ["--profile", "ci", "-j=4"]);
        assert_eq!(build, ["--features", "x", "--release", "--target=t"]);
    }

    /// A plan with nothing to run, and a command that is no Cargo test.
    #[test]
    fn nothing_to_run_and_no_cargo_command() {
        let (root, mut request) = crate_with("empty", &[], &[]);
        std::fs::write(&request.plan, "{}").unwrap();
        assert_eq!(run_strong_rust(&request, &mut Vec::new()), Ok(0));
        assert_eq!(
            std::fs::read_to_string(&request.results).unwrap(),
            "{\"summary\":true,\"mutants\":0}\n"
        );
        // A plan that is not JSON, and a mutant in a file that does not parse.
        std::fs::write(&request.plan, "not json").unwrap();
        assert!(
            run_strong_rust(&request, &mut Vec::new())
                .unwrap_err()
                .contains("plan.json")
        );
        std::fs::write(root.join("broken.rs"), "fn (").unwrap();
        std::fs::write(
            &request.plan,
            r#"{"mutants": [{"id": "broken.rs:1:1-1:2:FnValue:()", "file": "broken.rs", "index": 0}]}"#,
        )
        .unwrap();
        request.results = root.join("broken.jsonl");
        assert_eq!(run_strong_rust(&request, &mut Vec::new()), Ok(0));
        assert_eq!(
            verdicts(&request)["broken.rs:1:1-1:2:FnValue:()"]["status"],
            "error"
        );
        // Doctests run with the command's package and feature options, and
        // no other target.
        let doctest = DoctestCommand::new(
            &invocation(&[
                "cargo",
                "test",
                "-p",
                "a",
                "--features",
                "x",
                "--lib",
                "--test=t",
                "--target",
                "w",
                "--release",
            ]),
            &crate::rust_project::PreparedRustProject::plain(root.clone()),
            &root,
        );
        assert_eq!(
            doctest.arguments,
            [
                "test",
                "--doc",
                "--features",
                "x",
                "--target",
                "w",
                "--release"
            ]
        );
        let (_, plain) = fixture("not-cargo");
        request = plain;
        request.command = vec!["true".into()];
        let error = run_strong_rust(&request, &mut Vec::new()).unwrap_err();
        assert!(error.contains("Cargo"), "{error}");
        let _ = std::fs::remove_dir_all(root);
        let _ = std::fs::remove_dir_all(request.root);
    }

    /// The compiler's errors in a switched build are read for the mutants
    /// they come from: a type error in one mutant's own code makes it
    /// unviable; any other error in a mutant's code or its switch, and an
    /// error at no switch (found by halving), builds the mutants behind it on
    /// their own. A file no crate compiles, and code the compiler evaluates,
    /// are built on their own too.
    #[cfg(unix)]
    #[test]
    fn a_build_s_errors_are_read_for_the_mutants_behind_them() {
        let lib = "pub mod b;\npub fn one(v: u32) -> u32 {\n    v + 1\n}\npub fn two(v: u32) -> u32 {\n    v * 2\n}\npub const N: u8 = 0 + 1;\n";
        let (root, mut request) = crate_with(
            "errors",
            &[
                (
                    "Cargo.toml",
                    "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[workspace]\n",
                ),
                ("src/lib.rs", lib),
                ("src/b.rs", "pub fn three(v: u32) -> u32 {\n    v - 3\n}\n"),
                (
                    "src/orphan.rs",
                    "pub fn lost(v: u32) -> u32 {\n    v + 1\n}\n",
                ),
                (
                    "tests/t.rs",
                    "#[test] fn one() { assert_eq!(fixture::one(1), 2); }\n#[test] fn two() { assert_eq!(fixture::two(3), 6); }\n#[test] fn three() { assert_eq!(fixture::b::three(5), 2); }\n#[test] fn n() { assert_eq!(fixture::N, 1); }\n",
                ),
            ],
            &["src/lib.rs", "src/b.rs", "src/orphan.rs"],
        );
        let own = |index: u64, value: &str| {
            let prefix = format!("on(\"src/lib.rs\", {index}) {{ return ");
            format!(
                "$(span src/lib.rs $(at src/lib.rs '{prefix}{value}' {}))",
                prefix.len()
            )
        };
        let switch_of = |index: u64| {
            format!("$(span src/lib.rs $(at src/lib.rs 'on(\"src/lib.rs\", {index})' 0))")
        };
        let first = [
            // A type error in mutant 0's own code, also named by a note
            // through its absolute path, with spans nothing is read from.
            format!(
                "error '{{\"code\":\"E0308\"}}' mismatched \"{}\" \"{{\\\"spans\\\":[$(span \"$PWD/src/lib.rs\" $(at src/lib.rs 'on(\"src/lib.rs\", 0) {{ return 0' 29))]}},{{\\\"spans\\\":[$(span tests/t.rs 1),$(span \"$PWD/gone.rs\" 1),{{\\\"file_name\\\":\\\"src/lib.rs\\\"}}]}}\"",
                own(0, "0")
            ),
            // Another kind of error in mutant 1's code.
            format!("error '{{\"code\":\"E0999\"}}' other \"{}\" ''", own(1, "1")),
            // A type error at the switch of mutants 2 and 3, outside their code.
            format!("error '{{\"code\":\"E0308\"}}' mismatched \"{}\" ''", switch_of(2)),
            // A type error in two mutants' code at once.
            format!("error '{{\"code\":\"E0308\"}}' mismatched \"{},{}\" ''", own(4, "0"), own(5, "1")),
            // The same places again: nothing more.
            format!("error null again \"{}\" ''", own(0, "0")),
            // An error with no place at all, as a linker's: no mutant's.
            "error null unplaced \"\" ''".to_owned(),
            format!("error null again \"{}\" ''", switch_of(2)),
        ]
        .join("\n");
        // A mutant of `b.rs` breaks the build at no switch, in `lib.rs`.
        let halving = "error null elsewhere \"$(span src/lib.rs 0)\" ''";
        request.command = vec![
            failing_cargo(
                &root,
                &[
                    ("on(\"src/lib.rs\", 0)", &first),
                    ("on(\"src/b.rs\", 2)", halving),
                ],
            ),
            "test".into(),
        ];
        let mut said = Vec::new();
        assert_eq!(run_strong_rust(&request, &mut said), Ok(0));
        let said = String::from_utf8(said).unwrap();
        let found = verdicts(&request);
        let status = |id: &str| {
            found
                .get(id)
                .map(|record| record["status"].as_str().unwrap().to_owned())
                .unwrap_or_else(|| panic!("no {id}: {found:?}\n{said}"))
        };
        assert_eq!(status("src/lib.rs:3:5-3:10:FnValue:0"), "unviable");
        for id in [
            "src/lib.rs:3:5-3:10:FnValue:1",
            "src/lib.rs:3:7-3:8:BinaryOperator:-",
            "src/lib.rs:6:5-6:10:FnValue:0",
            "src/lib.rs:6:7-6:8:BinaryOperator:/",
            "src/b.rs:2:7-2:8:BinaryOperator:+",
            "src/lib.rs:8:21-8:22:BinaryOperator:*",
        ] {
            assert_eq!(status(id), "killed", "{id}\n{said}");
        }
        // `0 - 1` overflows where the compiler evaluates it.
        assert_eq!(status("src/lib.rs:8:21-8:22:BinaryOperator:-"), "unviable");
        assert_eq!(status("src/orphan.rs:2:7-2:8:BinaryOperator:-"), "survived");
        let summary = std::fs::read_to_string(&request.results)
            .unwrap()
            .lines()
            .last()
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .unwrap();
        // Halving over every switched mutant takes out only `b.rs`' culprit:
        // five are left in the switched build, those no error is traced to.
        assert_eq!(summary["switched"], 5, "{summary}\n{said}");
        // A build the halving starts that cannot be made ends the pass: the
        // one with no suspect (the fourth), then the first of the halves.
        // Each build syncs its copy from one call, a directory at a time:
        // the project's own four (itself, `src`, `tests`, `bin`).
        for build in [4, 5] {
            let place = format!(
                "fs.create_dir_all#rust-strong.switched-build@{}",
                4 * (build - 1) + 1
            );
            request.results = root.join(format!("halving-{build}.jsonl"));
            let failed =
                crate::fault::during(&place, || run_strong_rust(&request, &mut Vec::new()));
            assert!(failed.is_err(), "{place}: {failed:?}");
        }
        let _ = std::fs::remove_dir_all(root);
    }

    /// With the compiler itself: deleting an arm of a `match` whose only `_`
    /// arm has a guard leaves it without the arm that made it exhaustive.
    /// The error is at the match's scrutinee, at no switch, so halving finds
    /// each such mutant, which then fails its own build: unviable, as
    /// cargo-mutants reports it. The rest stay in the switched build.
    #[cfg(unix)]
    #[test]
    fn an_error_at_no_switch_is_found_by_halving() {
        let (root, mut request) = crate_with(
            "halving",
            &[
                (
                    "Cargo.toml",
                    "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[workspace]\n",
                ),
                (
                    "src/lib.rs",
                    "pub fn pick(b: bool, c: bool) -> u8 {\n    match b {\n        true => 1,\n        false => 2,\n        _ if c => 3,\n    }\n}\n",
                ),
                (
                    "tests/t.rs",
                    "#[test] fn pick() { assert_eq!(fixture::pick(true, false), 1); assert_eq!(fixture::pick(false, false), 2); }\n",
                ),
            ],
            &["src/lib.rs"],
        );
        let mut said = Vec::new();
        assert_eq!(run_strong_rust(&request, &mut said), Ok(0));
        let said = String::from_utf8(said).unwrap();
        let found = verdicts(&request);
        let arms = found
            .iter()
            .filter(|(id, _)| id.contains(":MatchArm:"))
            // An arm's id: its start (the line and column), then its end.
            .map(|(id, record)| {
                (
                    id.split('-').next().unwrap(),
                    record["status"].as_str().unwrap(),
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(
            arms,
            [
                ("src/lib.rs:3:9", "unviable"),
                ("src/lib.rs:4:9", "unviable")
            ],
            "{found:?}\n{said}"
        );
        // `pick` returning 0 or 1 is caught by the switched build's test.
        for (id, record) in &found {
            if id.contains(":FnValue:") {
                assert_eq!(record["status"], "killed", "{id}\n{said}");
            }
        }
        // One suspect alone is the culprit, with no halving.
        let plan: Value = serde_json::from_slice(&std::fs::read(&request.plan).unwrap()).unwrap();
        let mut one = plan.clone();
        one["mutants"] = Value::Array(
            plan["mutants"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|mutant| {
                    mutant["id"]
                        .as_str()
                        .unwrap()
                        .starts_with("src/lib.rs:3:9-")
                })
                .cloned()
                .collect(),
        );
        std::fs::write(&request.plan, serde_json::to_vec(&one).unwrap()).unwrap();
        request.results = root.join("one.jsonl");
        let mut said = Vec::new();
        assert_eq!(run_strong_rust(&request, &mut said), Ok(0));
        let found = verdicts(&request);
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(
            found.values().all(|record| record["status"] == "unviable"),
            "{found:?}\n{}",
            String::from_utf8_lossy(&said)
        );
        // The pass's copies and builds are gone with it: a switched build
        // alone is far more than a megabyte.
        assert!(!root.join(".supercov/strong/rust").exists());
        assert!(left_behind(&root) < 1 << 20, "{}", left_behind(&root));
        // A test artifact Cargo reports without an executable, or built from
        // a file outside the workspace, is none of the pass's.
        request.command = vec![
            script(
                &root.join("bin/noisy/cargo"),
                concat!(
                    "case \" $* \" in *\" --no-run \"*)\n",
                    "echo '{\"reason\":\"compiler-artifact\",\"profile\":{\"test\":true},\"executable\":null,\"target\":{\"src_path\":\"/nowhere/a.rs\"}}'\n",
                    "echo '{\"reason\":\"compiler-artifact\",\"profile\":{\"test\":true},\"executable\":\"/bin/true\",\"target\":{\"src_path\":\"/nowhere/b.rs\"}}'\n",
                    ";; esac\nexec cargo \"$@\"\n",
                ),
            ),
            "test".into(),
        ];
        request.results = root.join("noisy.jsonl");
        assert_eq!(run_strong_rust(&request, &mut Vec::new()), Ok(0));
        assert_eq!(verdicts(&request).len(), 1);
        let _ = std::fs::remove_dir_all(root);
    }

    /// A build that fails with no compiler error, or fails without any
    /// mutant, or a nextest that cannot list the build, ends the pass with
    /// what went wrong.
    #[cfg(unix)]
    #[test]
    fn a_project_that_does_not_build_ends_the_pass() {
        let (root, mut request) = fixture("broken");
        request.command = vec![
            script(
                &root.join("bin/silent/cargo"),
                "case \" $* \" in *\" --no-run \"*) echo boom >&2; exit 101 ;; esac\nexec cargo \"$@\"\n",
            ),
            "test".into(),
        ];
        assert_eq!(
            run_strong_rust(&request, &mut Vec::new()),
            Err("boom".into())
        );
        request.command[0] = failing_cargo(
            &root,
            &[("", "error null broken \"$(span src/lib.rs 0)\" ''")],
        );
        let error = run_strong_rust(&request, &mut Vec::new()).unwrap_err();
        assert!(
            error.starts_with("the project does not build without mutants: broken"),
            "{error}"
        );
        // A pass that ends in an error leaves its copies no more than one
        // that ends well.
        assert!(!root.join(".supercov/strong/rust").exists());
        assert!(left_behind(&root) < 1 << 20, "{}", left_behind(&root));
        request.command = vec![
            script(
                &root.join("bin/unlisted/cargo"),
                "[ \"$1 $2\" = \"nextest list\" ] && { echo unlisted >&2; exit 1; }\nexec cargo \"$@\"\n",
            ),
            "nextest".into(),
            "run".into(),
        ];
        let error = run_strong_rust(&request, &mut Vec::new()).unwrap_err();
        assert_eq!(error, "cargo nextest list failed: unlisted");
        let _ = std::fs::remove_dir_all(root);
    }

    /// `wasm-pack test` is run once to learn the `cargo test` it runs; a pass
    /// whose tests run in a browser other than Chrome records each mutant an
    /// error, and one that cannot be learned ends with why.
    #[cfg(unix)]
    #[test]
    fn wasm_pack_tests_are_learned_and_another_browser_s_mutants_are_errors() {
        let (root, mut request) = crate_with(
            "wasm-pack",
            &[
                (
                    "Cargo.toml",
                    "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[workspace]\n",
                ),
                ("src/lib.rs", "pub fn one(v: u32) -> u32 {\n    v + 1\n}\n"),
                (
                    "tests/t.rs",
                    "#[test] fn one() { assert_eq!(fixture::one(1), 2); }\n",
                ),
            ],
            &["src/lib.rs"],
        );
        request.command = vec![
            script(
                &root.join("bin/firefox/wasm-pack"),
                "GECKODRIVER=/bin/false exec cargo test\n",
            ),
            "test".into(),
        ];
        assert_eq!(run_strong_rust(&request, &mut Vec::new()), Ok(0));
        let found = verdicts(&request);
        assert_eq!(found.len(), 4, "{found:?}");
        assert!(found.values().all(|record| record["status"] == "error"
            && record["reason"].as_str().unwrap().contains("Chrome only")));
        // Chrome beside it: the pass runs, here natively.
        request.results = root.join("chrome.jsonl");
        request.command[0] = script(
            &root.join("bin/chrome/wasm-pack"),
            "CHROMEDRIVER=/bin/false GECKODRIVER=/bin/false exec cargo test\n",
        );
        assert_eq!(run_strong_rust(&request, &mut Vec::new()), Ok(0));
        assert_eq!(
            verdicts(&request)["src/lib.rs:2:7-2:8:BinaryOperator:-"]["status"],
            "killed"
        );
        request.command[0] = script(
            &root.join("bin/stops/wasm-pack"),
            "echo stopped >&2; exit 1\n",
        );
        let error = run_strong_rust(&request, &mut Vec::new()).unwrap_err();
        assert!(error.contains("stopped"), "{error}");
        let _ = std::fs::remove_dir_all(root);
    }

    /// A test that says it stopped at the hit limit, on standard error or
    /// in the output its failure shows, is a timeout, and so is a doctest's;
    /// a mutant no test reaches is never run.
    #[test]
    fn a_test_stopped_at_the_hit_limit_is_a_timeout() {
        let marker = TIMEOUT_MARKER;
        let lib = format!(
            "/// ```\n/// if fixture::a(2) != 3 {{ panic!(\"{marker}\"); }}\n/// ```\npub fn a(v: u32) -> u32 {{\n    v + 1\n}}\n"
        );
        let tests = format!(
            "#[test] fn err() {{ if fixture::a(1) != 2 {{ std::io::Write::write_all(&mut std::io::stderr(), b\"{marker}\\n\").unwrap(); panic!(); }} }}\n#[test] fn out() {{ if fixture::a(1) != 2 {{ panic!(\"{marker}\"); }} }}\n"
        );
        let (root, request) = crate_with(
            "hit-limit",
            &[
                (
                    "Cargo.toml",
                    "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[workspace]\n",
                ),
                ("src/lib.rs", &lib),
                ("tests/t.rs", &tests),
            ],
            &["src/lib.rs"],
        );
        let mut plan: Value =
            serde_json::from_slice(&std::fs::read(&request.plan).unwrap()).unwrap();
        plan["tests"].as_array_mut().unwrap().push(json!({
            "id": "src/lib.rs - a (line 1)", "file": "src/lib.rs", "name": "src/lib.rs - a (line 1)", "runner": "rustdoc"
        }));
        // Body to 0: `err`; to 1: `out`; `+` to `-`: the doctest; `*`: none.
        for (mutant, candidates) in plan["mutants"].as_array_mut().unwrap().iter_mut().zip([
            vec![0],
            vec![1],
            vec![2],
            vec![],
        ]) {
            mutant["candidates"] = json!(
                candidates
                    .iter()
                    .map(|test| json!({"test": test}))
                    .collect::<Vec<_>>()
            );
        }
        std::fs::write(&request.plan, serde_json::to_vec(&plan).unwrap()).unwrap();
        assert_eq!(run_strong_rust(&request, &mut Vec::new()), Ok(0));
        let found = verdicts(&request);
        let status = |id: &str| found[id]["status"].as_str().unwrap().to_owned();
        assert_eq!(
            status("src/lib.rs:5:5-5:10:FnValue:0"),
            "timeout",
            "{found:?}"
        );
        assert_eq!(
            status("src/lib.rs:5:5-5:10:FnValue:1"),
            "timeout",
            "{found:?}"
        );
        let doctest = &found["src/lib.rs:5:7-5:8:BinaryOperator:-"];
        assert_eq!(doctest["status"], "timeout", "{found:?}");
        // rustdoc names a doctest by its file's path as the platform writes
        // it; the kill names it by its id, with `/` on every platform.
        assert_eq!(doctest["killedBy"], "src/lib.rs - a (line 1)");
        let untested = &found["src/lib.rs:5:7-5:8:BinaryOperator:*"];
        assert_eq!(untested["reason"], "no test can kill it", "{found:?}");
        let _ = std::fs::remove_dir_all(root);
    }

    /// A pass told to stop (the CLI's `stopped-at` beside its results)
    /// starts nothing more, ends the test it runs with every process of its
    /// group, and judges nothing that test said: its mutant is left for the
    /// next pass, with the candidate it already passed, and the pass ends at
    /// once as a terminated command does.
    #[test]
    fn a_pass_told_to_stop_ends_its_tests_and_judges_nothing_they_said() {
        // The tests run in the pass's copy: `b` says it started where this
        // test looks, by an absolute path.
        let said = std::env::temp_dir().join(format!(
            "supercov-rust-strong-stop-said-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&said).unwrap();
        let tests = format!(
            concat!(
                "#[test] fn a() {{ assert!(fixture::small(3)); }}\n",
                // Slower than `a` without a mutant (2 s), so it runs after `a`;
                // with one, it says so and waits: what ends it is the stop.
                "#[test] fn b() {{ std::fs::write(std::path::Path::new({0:?}).join(format!(\"baseline-{{}}\", std::process::id())), \"\").unwrap(); std::thread::sleep(std::time::Duration::from_secs(2)); if std::env::var_os(\"SUPERCOV_ACTIVE_MUTANT\").is_some() {{ std::fs::write(std::path::Path::new({0:?}).join(format!(\"started-{{}}\", std::process::id())), \"\").unwrap(); std::thread::sleep(std::time::Duration::from_secs(300)); }} assert!(!fixture::small(10)); }}\n",
            ),
            said.display().to_string()
        );
        let (root, request) = crate_with(
            "stop",
            &[
                (
                    "Cargo.toml",
                    "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[workspace]\n",
                ),
                (
                    "src/lib.rs",
                    "pub fn small(v: u32) -> bool {\n    v < 10\n}\n",
                ),
                ("tests/t.rs", &tests),
            ],
            &["src/lib.rs"],
        );
        let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stopper = {
            let said = said.clone();
            let done = done.clone();
            let marker = request
                .results
                .with_file_name(crate::strong_driver::STOPPED_AT);
            std::thread::spawn(move || {
                while !done.load(Ordering::SeqCst) {
                    let started = std::fs::read_dir(&said)
                        .unwrap()
                        .filter_map(Result::ok)
                        .find_map(|entry| {
                            let name = entry.file_name().to_string_lossy().into_owned();
                            name.strip_prefix("started-")?.parse::<u32>().ok()
                        });
                    if let Some(pid) = started {
                        std::fs::write(&marker, "0").unwrap();
                        return Some((pid, Instant::now()));
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
                None
            })
        };
        let outcome = run_strong_rust(&request, &mut Vec::new());
        let ended = Instant::now();
        done.store(true, Ordering::SeqCst);
        let records = std::fs::read_to_string(&request.results)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .collect::<Vec<_>>();
        let (pid, stopped) = stopper
            .join()
            .unwrap()
            .unwrap_or_else(|| panic!("no test started with a mutant: {records:?}"));
        assert_eq!(outcome, Ok(STOPPED_EXIT));
        assert!(
            ended.duration_since(stopped) < Duration::from_secs(10),
            "{:?} after the stop",
            ended.duration_since(stopped)
        );
        // No verdict: the one test that said anything was cut short.
        assert!(
            records.iter().all(|record| record["status"].is_null()),
            "{records:?}"
        );
        // The first mutant, `true`, passed `a` before `b` held it.
        assert_eq!(
            records
                .iter()
                .filter(|record| record["progress"] == true)
                .collect::<Vec<_>>(),
            [
                &json!({"progress": true, "id": "src/lib.rs:2:5-2:11:FnValue:true", "passed": ["tests/t.rs::a"]})
            ]
        );
        // The held test is gone, and so are the pass's copies.
        #[cfg(unix)]
        {
            // SAFETY: signal 0 only asks whether the process exists.
            let alive = unsafe { libc::kill(pid as i32, 0) } == 0;
            assert!(!alive, "test process {pid} outlived the stop");
        }
        #[cfg(not(unix))]
        let _ = pid;
        assert!(!root.join(".supercov/strong/rust").exists());

        // Stopped while the baselines run: no mutant runs, none is judged
        // from baselines a stop cut short.
        let mut again = request.clone();
        again.results = root.join("again.jsonl");
        let marker = again
            .results
            .with_file_name(crate::strong_driver::STOPPED_AT);
        let _ = std::fs::remove_file(&marker);
        let watcher = {
            let said = said.clone();
            let marker = marker.clone();
            std::thread::spawn(move || {
                let begun = Instant::now();
                while begun.elapsed() < Duration::from_secs(300) {
                    let baselines = std::fs::read_dir(&said)
                        .unwrap()
                        .filter(|entry| {
                            entry
                                .as_ref()
                                .unwrap()
                                .file_name()
                                .to_string_lossy()
                                .starts_with("baseline-")
                        })
                        .count();
                    // Two from the first pass (`b` without and with its
                    // mutant); the third is this one's baseline.
                    if baselines >= 3 {
                        std::fs::write(&marker, "0").unwrap();
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
            })
        };
        assert_eq!(run_strong_rust(&again, &mut Vec::new()), Ok(STOPPED_EXIT));
        watcher.join().unwrap();
        let said_again = std::fs::read_to_string(&again.results).unwrap_or_default();
        assert!(!said_again.contains("\"status\""), "{said_again}");

        // Stopped before it began: it builds nothing and ends at once.
        let mut stopped = request.clone();
        stopped.results = root.join("stopped.jsonl");
        std::fs::write(
            stopped
                .results
                .with_file_name(crate::strong_driver::STOPPED_AT),
            "0",
        )
        .unwrap();
        assert_eq!(run_strong_rust(&stopped, &mut Vec::new()), Ok(STOPPED_EXIT));

        // Stopped while a mutant is built on its own: it is left for the
        // next pass, not called an error.
        let constant = crate_with(
            "stop-alone",
            &[
                (
                    "Cargo.toml",
                    "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[workspace]\n",
                ),
                ("src/lib.rs", "pub const fn k() -> u32 {\n    2 + 3\n}\n"),
                (
                    "tests/t.rs",
                    "#[test] fn k() { assert_eq!(fixture::k(), 5); }\n",
                ),
            ],
            &["src/lib.rs"],
        );
        let plain = constant.0.join(".supercov/strong/rust/plain");
        let marker = constant
            .1
            .results
            .with_file_name(crate::strong_driver::STOPPED_AT);
        let watcher = std::thread::spawn(move || {
            let begun = Instant::now();
            while !plain.exists() && begun.elapsed() < Duration::from_secs(300) {
                std::thread::sleep(Duration::from_millis(5));
            }
            std::fs::write(marker, "0").unwrap();
        });
        assert_eq!(
            run_strong_rust(&constant.1, &mut Vec::new()),
            Ok(STOPPED_EXIT)
        );
        watcher.join().unwrap();
        assert!(
            verdicts(&constant.1).is_empty(),
            "{:?}",
            verdicts(&constant.1)
        );
        let _ = std::fs::remove_dir_all(constant.0);
        let _ = std::fs::remove_dir_all(root);
        let _ = std::fs::remove_dir_all(said);
    }

    /// A test process that ends by a signal that stops a pass is the stop's
    /// once the stop shows; any other end is the test's.
    #[cfg(unix)]
    #[test]
    fn a_process_ended_by_a_stopping_signal_waits_for_the_stop() {
        use std::os::unix::process::ExitStatusExt;
        assert!(stopping_signal(std::process::ExitStatus::from_raw(
            libc::SIGTERM
        )));
        assert!(stopping_signal(std::process::ExitStatus::from_raw(
            143 << 8
        )));
        assert!(!stopping_signal(std::process::ExitStatus::from_raw(
            libc::SIGABRT
        )));
        assert!(!stopping_signal(std::process::ExitStatus::from_raw(
            101 << 8
        )));
        let root = std::env::temp_dir().join(format!(
            "supercov-rust-strong-signal-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let stop = Stop::beside(&root.join("results.jsonl"));
        let terminated = |stop: &Stop| {
            supervised(
                Command::new("sh").args(["-c", "kill -TERM $$"]),
                Some(Duration::from_secs(60)),
                stop,
            )
            .unwrap()
        };
        // No stop shows: the test ended so, and says so.
        let alone = terminated(&stop);
        assert!(!alone.stopped && alone.status.is_some());
        // The stop shows a moment after: the end was the stop's.
        let marker = root.join(crate::strong_driver::STOPPED_AT);
        let writer = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(200));
            std::fs::write(marker, "0").unwrap();
        });
        let cut = terminated(&stop);
        writer.join().unwrap();
        assert!(cut.stopped);
        let _ = std::fs::remove_dir_all(root);
    }

    /// A mutant the plan decided without running (no test reaches it) is
    /// switched into the build to find whether it compiles: one that does
    /// not is `unviable`, as cargo-mutants reports it; one that does keeps
    /// the plan's verdict, and nothing runs it.
    #[test]
    fn a_decided_mutant_that_does_not_compile_is_unviable() {
        let (root, request) = crate_with(
            "decided",
            &[
                (
                    "Cargo.toml",
                    "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[workspace]\n",
                ),
                (
                    "src/lib.rs",
                    "pub struct NoDefault(u8);\npub fn make(v: u8) -> NoDefault {\n    NoDefault(v + 1)\n}\npub fn used(v: u8) -> u8 {\n    v * 2\n}\npub fn h(b: bool, c: bool) -> u8 {\n    match b {\n        true => 1,\n        false => 2,\n        _ if c => 3,\n    }\n}\npub mod np {\n    #![no_implicit_prelude]\n    pub fn v() -> ::std::vec::Vec<u8> {\n        ::std::vec::Vec::new()\n    }\n}\n",
                ),
                (
                    "tests/t.rs",
                    "#[test] fn uses() { assert_eq!(fixture::used(2), 4); }\n",
                ),
            ],
            &["src/lib.rs"],
        );
        let mut plan: Value =
            serde_json::from_slice(&std::fs::read(&request.plan).unwrap()).unwrap();
        let (decided, undecided): (Vec<Value>, Vec<Value>) = plan["mutants"]
            .as_array()
            .unwrap()
            .iter()
            .cloned()
            .partition(|mutant| !mutant["id"].as_str().unwrap().starts_with("src/lib.rs:6:"));
        plan["mutants"] = Value::Array(undecided);
        let mut decided = decided
            .into_iter()
            .map(|mut mutant| {
                mutant["decided"] = json!("no-coverage");
                mutant["candidates"] = json!([]);
                mutant
            })
            .collect::<Vec<_>>();
        // `h`'s arms deleted leave its `match` without an arm that makes it
        // exhaustive, an error at no switch: rather than halve over them,
        // the decided mutants leave the build, keeping the plan's verdicts.
        // One in a package whose tests the pass does not run is not built:
        // it keeps the plan's verdict, compiling or not.
        let other = "pub struct NoDefault(u8);\npub fn make(v: u8) -> NoDefault {\n    NoDefault(v + 1)\n}\n";
        std::fs::create_dir_all(root.join("other/src")).unwrap();
        std::fs::write(
            root.join("other/Cargo.toml"),
            "[package]\nname = \"other\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .unwrap();
        std::fs::write(root.join("other/src/lib.rs"), other).unwrap();
        // The crate depends on it, so the build compiles it.
        std::fs::write(
            root.join("Cargo.toml"),
            "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\nother = { path = \"other\" }\n\n[workspace]\n",
        )
        .unwrap();
        let elsewhere = crate::rust_mutants::enumerate(other).unwrap();
        decided.push(json!({
            "id": mutant_id("other/src/lib.rs", &elsewhere[0]), "file": "other/src/lib.rs",
            "index": 0, "decided": "no-coverage", "candidates": []
        }));
        // One whose file changed since the run keeps the plan's verdict.
        let mut moved = decided[0].clone();
        moved["id"] = json!("src/lib.rs:9:5-9:21:FnValue:Default::default()");
        decided.push(moved);
        plan["decided"] = Value::Array(decided);
        std::fs::write(&request.plan, serde_json::to_vec(&plan).unwrap()).unwrap();
        assert_eq!(run_strong_rust(&request, &mut Vec::new()), Ok(0));
        let found = verdicts(&request);
        let unviable = &found["src/lib.rs:3:5-3:21:FnValue:Default::default()"];
        assert_eq!(unviable["status"], "unviable", "{found:?}");
        // A macro the replacement names that is not in scope there.
        let no_macro = &found["src/lib.rs:18:9-18:31:FnValue:vec![]"];
        assert_eq!(no_macro["status"], "unviable", "{found:?}");
        // The ones that compile keep the plan's verdict: none is written.
        assert!(
            !found.contains_key("src/lib.rs:3:17-3:18:BinaryOperator:-"),
            "{found:?}"
        );
        assert_eq!(
            found["src/lib.rs:6:7-6:8:BinaryOperator:/"]["status"],
            "killed"
        );
        assert!(
            !found.contains_key("src/lib.rs:9:5-9:21:FnValue:Default::default()"),
            "{found:?}"
        );
        assert!(
            !found.contains_key("src/lib.rs:10:9-10:18:MatchArm:removed"),
            "{found:?}"
        );
        assert!(
            !found.contains_key("other/src/lib.rs:3:5-3:21:FnValue:Default::default()"),
            "{found:?}"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    /// A test whose process cannot be started makes its mutant an error,
    /// not a kill.
    #[test]
    fn a_test_that_cannot_start_is_an_error() {
        let (root, request) = crate_with(
            "unstarted",
            &[
                (
                    "Cargo.toml",
                    "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[workspace]\n",
                ),
                ("src/lib.rs", "pub fn one() -> u32 {\n    1\n}\n"),
                (
                    "tests/t.rs",
                    "#[test] fn a() { assert_eq!(fixture::one(), 1); }\n",
                ),
            ],
            &["src/lib.rs"],
        );
        // The build (1), the baseline (2), then the first mutant's test.
        let outcome = crate::fault::during("spawn#rust-strong.supervised@3", || {
            run_strong_rust(&request, &mut Vec::new())
        });
        assert_eq!(outcome, Ok(0));
        let found = verdicts(&request);
        let first = found
            .values()
            .find(|record| record["status"] == "error")
            .unwrap_or_else(|| panic!("{found:?}"));
        assert_eq!(
            first["reason"], "could not start tests/t.rs::a",
            "{found:?}"
        );
        assert!(first["killedBy"].is_null(), "{first}");
        let _ = std::fs::remove_dir_all(root);
    }

    /// A test past its limit beside others runs again alone, after a run
    /// without the mutant, and its limit is then that run's: a machine that
    /// slows down once the baselines have run (a test that never ends beside
    /// the others, and takes a few seconds more alone, against a baseline of
    /// a moment) makes no timeout of a kill or a survivor, and the candidate
    /// it already passed is not run again. A test that fails without the
    /// mutant when run again is an `error`; a mutant built alone is judged
    /// again alone in its own build; one that never ends is a `timeout`.
    #[test]
    fn a_timeout_is_judged_again_alone_at_the_machine_s_speed() {
        let said = std::env::temp_dir().join(format!(
            "supercov-rust-strong-slow-said-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&said).unwrap();
        // Each test: its baseline passes at once; with a mutant beside the
        // others it waits until its limit ends it; run again without the
        // mutant it takes 2 s more (the machine is busier) and says so,
        // after which its mutant's run ends as the test does.
        let tests = format!(
            concat!(
                "#[test] fn b() {{ let said = std::path::Path::new({0:?}); std::thread::sleep(std::time::Duration::from_secs(1)); let mutated = std::env::var_os(\"SUPERCOV_ACTIVE_MUTANT\").is_some(); if mutated && !said.join(\"round2\").exists() {{ std::thread::sleep(std::time::Duration::from_secs(300)); }} if !mutated && said.join(\"seen\").exists() {{ std::fs::write(said.join(\"round2\"), \"\").unwrap(); std::thread::sleep(std::time::Duration::from_secs(2)); }} if !mutated {{ std::fs::write(said.join(\"seen\"), \"\").unwrap(); }} assert!(fixture::small(3)); }}\n",
                "#[test] fn h() {{ assert_eq!(fixture::spin(3), 3); }}\n",
                // Passes, then outlasts its limit with a mutant, then fails
                // without one.
                "#[test] fn c() {{ let said = std::path::Path::new({0:?}); let mutated = std::env::var_os(\"SUPERCOV_ACTIVE_MUTANT\").is_some(); if mutated {{ std::thread::sleep(std::time::Duration::from_secs(300)); }} assert!(!said.join(\"c-seen\").exists(), \"slower now, and failing\"); std::fs::write(said.join(\"c-seen\"), \"\").unwrap(); }}\n",
                // As `b`, for a constant the mutant is built alone in, which
                // it tells by the constant's value.
                "#[test] fn k() {{ let said = std::path::Path::new({0:?}); let mutated = fixture::k() != 5; if mutated && !said.join(\"round2-k\").exists() {{ std::thread::sleep(std::time::Duration::from_secs(300)); }} if !mutated && said.join(\"k-seen\").exists() {{ std::fs::write(said.join(\"round2-k\"), \"\").unwrap(); std::thread::sleep(std::time::Duration::from_secs(2)); }} if !mutated {{ std::fs::write(said.join(\"k-seen\"), \"\").unwrap(); }} assert_eq!(fixture::k(), 5); }}\n",
            ),
            said.display().to_string()
        );
        let (root, mut request) = crate_with(
            "slow",
            &[
                (
                    "Cargo.toml",
                    "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[workspace]\n",
                ),
                (
                    "src/lib.rs",
                    "pub fn small(v: u32) -> bool {\n    v < 10\n}\npub fn spin(n: u32) -> u32 {\n    let mut i = 0;\n    while i != n {\n        i += 1;\n    }\n    i\n}\npub const fn k() -> u32 {\n    2 + 3\n}\n",
                ),
                ("tests/t.rs", &tests),
            ],
            &["src/lib.rs"],
        );
        // `true` lives through `b`, `false` dies of it, `*=` never ends.
        // `true` passes `h` first (its baseline is the shorter), then `b`.
        let kept = [
            ("src/lib.rs:2:5-2:11:FnValue:true", &[1, 0][..]),
            ("src/lib.rs:2:5-2:11:FnValue:false", &[0]),
            ("src/lib.rs:7:11-7:13:BinaryOperator:*=", &[1]),
            ("src/lib.rs:2:7-2:8:BinaryOperator:==", &[2]),
            ("src/lib.rs:12:7-12:8:BinaryOperator:*", &[3]),
        ];
        let mut plan: Value =
            serde_json::from_slice(&std::fs::read(&request.plan).unwrap()).unwrap();
        plan["mutants"] = Value::Array(
            plan["mutants"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(|mutant| {
                    let (_, tests) = kept.iter().find(|(id, _)| mutant["id"] == *id)?;
                    let mut mutant = mutant.clone();
                    mutant["candidates"] = json!(
                        tests
                            .iter()
                            .map(|test| json!({"test": test}))
                            .collect::<Vec<_>>()
                    );
                    Some(mutant)
                })
                .collect(),
        );
        assert_eq!(plan["mutants"].as_array().unwrap().len(), 5, "{plan}");
        std::fs::write(&request.plan, serde_json::to_vec(&plan).unwrap()).unwrap();
        request.workers = 4;
        assert_eq!(run_strong_rust(&request, &mut Vec::new()), Ok(0));
        let found = verdicts(&request);
        let status = |id: &str| found[id]["status"].as_str().unwrap().to_owned();
        assert_eq!(status(kept[0].0), "survived", "{found:?}");
        // `h`, which it passed beside the others, ran once with it.
        assert_eq!(found[kept[0].0]["tests"], 2, "{found:?}");
        assert_eq!(status(kept[1].0), "killed", "{found:?}");
        let endless = &found[kept[2].0];
        assert_eq!(endless["status"], "timeout", "{found:?}");
        assert!(
            endless["reason"]
                .as_str()
                .unwrap()
                .ends_with("alone, after its test ran again without the mutant"),
            "{endless}"
        );
        // A test that fails without the mutant when run again says nothing.
        let failing = &found[kept[3].0];
        assert_eq!(failing["status"], "error", "{found:?}");
        assert!(
            failing["reason"]
                .as_str()
                .unwrap()
                .ends_with("did not pass without it when run again"),
            "{failing}"
        );
        // A mutant built alone is judged again alone in its own build.
        assert_eq!(status(kept[4].0), "killed", "{found:?}");
        let _ = std::fs::remove_dir_all(root);
        let _ = std::fs::remove_dir_all(said);
    }

    /// Under nextest: a test nextest stops at its own timeout is a timeout, a
    /// test it cannot find runs nothing, and a listing that cannot be read,
    /// or a `cargo` that cannot be started, ends the pass.
    #[cfg(unix)]
    #[test]
    fn nextest_timeouts_missing_tests_and_listings_that_fail() {
        let (root, mut request) = crate_with(
            "nextest",
            &[
                (
                    "Cargo.toml",
                    "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[workspace]\n",
                ),
                (
                    "src/lib.rs",
                    "pub fn one(v: u32) -> u32 {\n    v + 1\n}\npub const K: u32 = 1 + 2;\n",
                ),
                (
                    "tests/t.rs",
                    "#[test] fn one() { assert_eq!(fixture::one(1), 2); }\n#[test] fn k() { assert_eq!(fixture::K, 3); }\n",
                ),
            ],
            &["src/lib.rs"],
        );
        let mut plan: Value =
            serde_json::from_slice(&std::fs::read(&request.plan).unwrap()).unwrap();
        plan["tests"].as_array_mut().unwrap().push(json!({"id": "tests/t.rs::gone", "file": "tests/t.rs", "name": "gone", "runner": "nextest"}));
        std::fs::write(&request.plan, serde_json::to_vec(&plan).unwrap()).unwrap();
        // A run with a mutant on stops as nextest's own timeout stops it.
        request.command = vec![
            script(
                &root.join("bin/slow/cargo"),
                "if [ \"$1 $2\" = \"nextest run\" ] && [ -n \"$SUPERCOV_ACTIVE_MUTANT\" ]; then echo '   TIMEOUT [ 1.000s] t' >&2; exit 100; fi\nexec cargo \"$@\"\n",
            ),
            "nextest".into(),
            "run".into(),
        ];
        assert_eq!(run_strong_rust(&request, &mut Vec::new()), Ok(0));
        let found = verdicts(&request);
        assert_eq!(
            found["src/lib.rs:2:7-2:8:BinaryOperator:-"]["status"], "timeout",
            "{found:?}"
        );
        // The constant's mutants are built on their own, and run there.
        assert_eq!(
            found["src/lib.rs:4:22-4:23:BinaryOperator:*"]["status"], "killed",
            "{found:?}"
        );
        // The test nextest cannot find was no candidate: its baseline ran
        // nothing.
        let summary = std::fs::read_to_string(&request.results).unwrap();
        assert!(
            summary.lines().last().unwrap().contains("\"baselines\":2"),
            "{summary}"
        );
        // Where nextest's listing of the build cannot be kept, the pass ends.
        request.results = root.join("unkept.jsonl");
        let place = "fs.create_dir_all#rust-strong.prepare.create-dir-all";
        let failed = crate::fault::during(place, || run_strong_rust(&request, &mut Vec::new()));
        assert!(failed.is_err(), "{place}: {failed:?}");
        // The build of a mutant on its own (the constant's) that nextest
        // cannot list ends the pass too.
        let command = request.command.clone();
        request.command[0] = script(
            &root.join("bin/plain-unlisted/cargo"),
            "if [ \"$1 $2\" = \"nextest list\" ]; then case \"$CARGO_TARGET_DIR\" in *plain-target*) echo plainly >&2; exit 1 ;; esac; fi\nexec cargo \"$@\"\n",
        );
        request.results = root.join("plain-unlisted.jsonl");
        assert_eq!(
            run_strong_rust(&request, &mut Vec::new()),
            Err("cargo nextest list failed: plainly".into())
        );
        request.command = command;
        // A test that cannot be found when its mutant runs (after its
        // baseline) is passed over.
        request.command[0] = script(&root.join("bin/plain/cargo"), "exec cargo \"$@\"\n");
        request.results = root.join("passed-over.jsonl");
        let mut one = plan.clone();
        one["mutants"] = json!([plan["mutants"]
            .as_array()
            .unwrap()
            .iter()
            .find(|mutant| mutant["id"] == "src/lib.rs:2:7-2:8:BinaryOperator:-")
            .unwrap()
            .clone()]);
        one["mutants"][0]["candidates"] = json!([{"test": 0}]);
        std::fs::write(&request.plan, serde_json::to_vec(&one).unwrap()).unwrap();
        let place = "fs.canonicalize#rust-strong.command@2";
        crate::fault::during(place, || run_strong_rust(&request, &mut Vec::new())).unwrap();
        std::fs::write(&request.plan, serde_json::to_vec(&plan).unwrap()).unwrap();
        assert!(
            verdicts(&request)
                .values()
                .any(|record| record["status"] == "survived" && record["tests"] == 0)
        );
        // A listing that is not JSON, a binary nextest names without a path
        // or with one that is gone, and a `cargo` that is gone.
        for (case, body, expected) in [
            (
                "garbage",
                "[ \"$1 $2\" = \"nextest list\" ] && { echo garbage; exit 0; }\nexec cargo \"$@\"\n",
                "cargo nextest list: ",
            ),
            (
                "pathless",
                "[ \"$1 $2\" = \"nextest list\" ] && { echo '{\"rust-binaries\":{\"a\":{},\"b\":{\"binary-path\":\"/gone\"}}}'; exit 0; }\nexec cargo \"$@\"\n",
                "",
            ),
            (
                "listed-gone",
                "[ \"$1 $2\" = \"nextest list\" ] && rm \"$0\"\nexec cargo \"$@\"\n",
                "cargo metadata: ",
            ),
            (
                "built-gone",
                "cargo \"$@\"; status=$?; case \" $* \" in *\" --no-run \"*) rm \"$0\" ;; esac; exit $status\n",
                "cargo nextest list: ",
            ),
        ] {
            request.command[0] = script(&root.join(format!("bin/{case}/cargo")), body);
            request.results = root.join(format!("{case}.jsonl"));
            match run_strong_rust(&request, &mut Vec::new()) {
                Ok(_) => {
                    assert_eq!(expected, "", "{case}");
                    assert!(
                        verdicts(&request)
                            .values()
                            .all(|record| record["status"] != "killed"),
                        "{case}"
                    );
                }
                Err(error) => assert!(
                    !expected.is_empty() && error.starts_with(expected),
                    "{case}: {error}"
                ),
            }
        }
        let _ = std::fs::remove_dir_all(root);
    }

    /// Where the runner's configuration cannot be read (a `-Z` before the
    /// subcommand, or a configuration file Cargo is given in the plain copy
    /// after its build), a build's tests cannot be run: the switched build
    /// ends the pass, and a mutant built on its own is an error.
    #[cfg(unix)]
    #[test]
    fn a_build_whose_runner_cannot_be_read() {
        let (root, mut request) = fixture("runner");
        request.command = vec![
            script(
                &root.join("bin/unstable/cargo"),
                "for argument; do shift; case \"$argument\" in -Z*) ;; *) set -- \"$@\" \"$argument\" ;; esac; done\nexec cargo \"$@\"\n",
            ),
            "-Zunstable-options".into(),
            "test".into(),
        ];
        assert!(run_strong_rust(&request, &mut Vec::new()).is_err());
        request.command = vec![
            script(
                &root.join("bin/configured/cargo"),
                "cargo \"$@\"; status=$?\ncase \"$PWD\" in */plain) mkdir -p .cargo; echo 'build = 5 =' > .cargo/config.toml ;; esac\nexit $status\n",
            ),
            "test".into(),
        ];
        request.results = root.join("plain.jsonl");
        assert_eq!(run_strong_rust(&request, &mut Vec::new()), Ok(0));
        let reasons = verdicts(&request)
            .values()
            .filter_map(|record| record["reason"].as_str().map(str::to_owned))
            .filter(|reason| reason.starts_with("could not build it on its own"))
            .count();
        assert!(reasons >= 2, "{:?}", verdicts(&request));
        let _ = std::fs::remove_dir_all(root);
    }

    /// The bytes a pass left in the project's `.supercov`.
    #[cfg(unix)]
    fn left_behind(root: &Path) -> u64 {
        fn size(path: &Path) -> u64 {
            std::fs::read_dir(path).map_or(0, |entries| {
                entries
                    .filter_map(Result::ok)
                    .map(|entry| match entry.file_type() {
                        Ok(kind) if kind.is_dir() => size(&entry.path()),
                        _ => entry.metadata().map_or(0, |metadata| metadata.len()),
                    })
                    .sum()
            })
        }
        size(&root.join(".supercov"))
    }

    fn verdicts(request: &StrongRustRequest) -> BTreeMap<String, Value> {
        std::fs::read_to_string(&request.results)
            .unwrap_or_default()
            .lines()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .filter_map(|record| Some((record["id"].as_str()?.to_owned(), record)))
            .collect()
    }

    /// Whether this machine builds for `wasm32-unknown-unknown`; a job that
    /// installed the target sets `SUPERCOV_REQUIRE_RUST_WASM=1`, which makes
    /// its absence a failure.
    fn builds_wasm() -> bool {
        let installed = Command::new("rustup")
            .args(["target", "list", "--installed"])
            .output()
            .is_ok_and(|output| {
                String::from_utf8_lossy(&output.stdout).contains("wasm32-unknown-unknown")
            });
        assert!(
            installed || std::env::var("SUPERCOV_REQUIRE_RUST_WASM").as_deref() != Ok("1"),
            "the wasm32 target is missing where the job installed it"
        );
        installed
    }

    /// A JavaScript strong pass over Rust compiled to wasm switches the
    /// plan's Rust mutants into the copy's crate where they build for wasm:
    /// one that does not compile is unviable, one a switch cannot hold or
    /// whose file changed an error, and the plan the JavaScript driver runs
    /// keeps only what is switched in, beside its other mutants. A crate
    /// that does not build as written, a plan it cannot read or write, and
    /// a Cargo that cannot start end the pass with an error.
    #[test]
    fn a_javascript_pass_switches_a_wasm_crate_s_mutants_where_they_build() {
        if !builds_wasm() {
            return;
        }
        let cargo = WasmCargo {
            program: "cargo".into(),
            rustflags: String::new(),
        };
        let (fixture_root, _) = fixture("wasm");
        let source = std::fs::read_to_string(fixture_root.join("src/lib.rs")).unwrap();
        let root = fixture_root.join("project");
        let copy = fixture_root.join("copy");
        for base in [&root, &copy] {
            std::fs::create_dir_all(base.join("web/rust/src")).unwrap();
            std::fs::write(
                base.join("web/rust/Cargo.toml"),
                "[package]\nname = \"grades\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[lib]\ncrate-type = [\"cdylib\"]\n\n[workspace]\n",
            )
            .unwrap();
            std::fs::write(base.join("web/rust/src/lib.rs"), &source).unwrap();
        }
        let file = "web/rust/src/lib.rs";
        let mutants = crate::rust_mutants::enumerate(&source).unwrap();
        let rust = |index: usize, mutant: &RustMutant| {
            json!({"id": mutant_id(file, mutant), "file": file, "language": "rust", "index": index,
                "decided": null, "candidates": [{"test": 0}]})
        };
        let plan = json!({
            "tests": [{"id": "web/a.test.js > a", "file": "web/a.test.js", "name": "a", "runner": "node:test", "runnable": true}],
            "mutants": mutants.iter().enumerate().map(|(index, mutant)| rust(index, mutant))
                .chain([
                    json!({"id": format!("{file}:1:1-1:2:FnValue:gone"), "file": file, "language": "rust", "index": 99, "decided": null, "candidates": [{"test": 0}]}),
                    json!({"id": "web/a.js:1:1-1:2:BooleanLiteral:false", "file": "web/a.js", "language": "javascript", "index": 0, "decided": null, "candidates": [{"test": 0}]}),
                    json!({"id": "decided", "file": file, "language": "rust", "index": 0, "decided": "survived"}),
                ])
                .collect::<Vec<_>>(),
        });
        let plan_path = fixture_root.join("wasm-plan.json");
        let results = fixture_root.join("wasm-results.jsonl");
        std::fs::write(&plan_path, serde_json::to_vec(&plan).unwrap()).unwrap();
        let mut said = Vec::new();
        let switched = switch_wasm_workspace(
            &root,
            "web/rust",
            &copy.join("web/rust"),
            &plan_path,
            &results,
            &cargo,
            &mut said,
        )
        .unwrap();
        let request = StrongRustRequest {
            root: root.clone(),
            command: Vec::new(),
            plan: plan_path.clone(),
            results: results.clone(),
            workers: 1,
        };
        let found = verdicts(&request);
        let status = |descriptor: &str| found[&format!("{file}:{descriptor}")]["status"].clone();
        assert_eq!(status("9:9-9:11:BinaryOperator:-="), "unviable");
        assert_eq!(status("1:1-1:2:FnValue:gone"), "error");
        assert!(
            found.values().any(|verdict| verdict["reason"]
                .as_str()
                .is_some_and(|reason| reason.starts_with("a switch cannot hold it"))),
            "{found:?}"
        );
        // The plan keeps what is switched in, the JavaScript mutant and the
        // decided one.
        let kept: Value = serde_json::from_slice(&std::fs::read(&plan_path).unwrap()).unwrap();
        let kept = kept["mutants"].as_array().unwrap();
        assert_eq!(kept.len(), switched + 2);
        assert_eq!(found.len() + switched, mutants.len() + 1);
        assert!(kept.iter().any(|mutant| mutant["language"] == "javascript"));
        // The copy's crate has the switches.
        assert_ne!(
            std::fs::read_to_string(copy.join("web/rust/src/lib.rs")).unwrap(),
            source
        );
        let said = String::from_utf8(said).unwrap();
        assert!(
            said.contains(&format!("switched {switched} Rust mutant(s)")),
            "{said}"
        );

        // A plan with no Rust mutant of the workspace switches nothing.
        assert_eq!(
            switch_wasm_workspace(
                &root,
                "elsewhere",
                &copy,
                &plan_path,
                &results,
                &cargo,
                &mut Vec::new()
            )
            .unwrap(),
            0
        );
        // A crate at the project's root that does not build as written.
        let broken = fixture_root.join("broken");
        std::fs::create_dir_all(broken.join("src")).unwrap();
        std::fs::write(
            broken.join("Cargo.toml"),
            "[package]\nname = \"broken\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[workspace]\n",
        )
        .unwrap();
        std::fs::write(
            broken.join("src/lib.rs"),
            "mod other;\npub fn one() -> u32 {\n    1 + other::two()\n}\n",
        )
        .unwrap();
        std::fs::write(
            broken.join("src/other.rs"),
            "pub fn two() -> u32 {\n    \"two\"\n}\n",
        )
        .unwrap();
        let lib = std::fs::read_to_string(broken.join("src/lib.rs")).unwrap();
        let broken_plan = json!({
            "tests": plan["tests"],
            "mutants": crate::rust_mutants::enumerate(&lib).unwrap().iter().enumerate()
                .map(|(index, mutant)| json!({"id": mutant_id("src/lib.rs", mutant), "file": "src/lib.rs", "language": "rust", "index": index, "decided": null, "candidates": [{"test": 0}]}))
                .collect::<Vec<_>>(),
        });
        let broken_plan_path = fixture_root.join("broken-plan.json");
        std::fs::write(&broken_plan_path, serde_json::to_vec(&broken_plan).unwrap()).unwrap();
        let error = switch_wasm_workspace(
            &broken,
            "",
            &broken,
            &broken_plan_path,
            &results,
            &cargo,
            &mut Vec::new(),
        )
        .unwrap_err();
        assert!(
            error.starts_with("the crate does not build for wasm: "),
            "{error}"
        );

        // Its plan unread and unwritten, and a Cargo that does not start or
        // fails before compiling.
        for site in [
            "fs.read#rust-strong.switch-wasm.read",
            "fs.write#rust-strong.switch-wasm.write",
        ] {
            std::fs::write(&plan_path, serde_json::to_vec(&plan).unwrap()).unwrap();
            std::fs::write(copy.join("web/rust/src/lib.rs"), &source).unwrap();
            let failed = crate::fault::during(site, || {
                switch_wasm_workspace(
                    &root,
                    "web/rust",
                    &copy.join("web/rust"),
                    &plan_path,
                    &results,
                    &cargo,
                    &mut Vec::new(),
                )
            });
            assert!(failed.unwrap_err().contains("wasm-plan.json"), "{site}");
        }
        // A scratch copy that cannot be made ends the switch.
        std::fs::write(&plan_path, serde_json::to_vec(&plan).unwrap()).unwrap();
        std::fs::write(copy.join("web/rust/src/lib.rs"), &source).unwrap();
        let site = "fs.create_dir_all#rust-strong.switch-wasm.sync-project";
        let failed = crate::fault::during(site, || {
            switch_wasm_workspace(
                &root,
                "web/rust",
                &copy.join("web/rust"),
                &plan_path,
                &results,
                &cargo,
                &mut Vec::new(),
            )
        });
        assert!(failed.is_err(), "{site}");
        let packages = BTreeSet::from(["grades".to_owned()]);
        let target = fixture_root.join("wasm-target");
        assert!(
            wasm_check(
                &WasmCargo {
                    program: "/nonexistent/cargo".into(),
                    rustflags: String::new()
                },
                &root,
                &target,
                &packages
            )
            .unwrap_err()
            .starts_with("cargo: ")
        );
        let unknown = BTreeSet::from(["nothing-of-that-name".to_owned()]);
        assert!(
            wasm_check(
                &WasmCargo {
                    program: "cargo".into(),
                    rustflags: "-C debuginfo=0".into()
                },
                &root.join("web/rust"),
                &target,
                &unknown
            )
            .unwrap_err()
            .starts_with("cargo build for wasm: ")
        );

        // A plan that is no JSON, results that cannot be written, and a
        // Cargo that does not start, each end the pass.
        let call = |plan: &Path, results: &Path, cargo: &WasmCargo| {
            switch_wasm_workspace(
                &root,
                "web/rust",
                &copy.join("web/rust"),
                plan,
                results,
                cargo,
                &mut Vec::new(),
            )
        };
        let garbled = fixture_root.join("garbled.json");
        std::fs::write(&garbled, "{").unwrap();
        assert!(
            call(&garbled, &results, &cargo)
                .unwrap_err()
                .contains("garbled.json")
        );
        std::fs::write(&plan_path, serde_json::to_vec(&plan).unwrap()).unwrap();
        let nowhere = fixture_root.join("no/such/results.jsonl");
        assert!(
            call(&plan_path, &nowhere, &cargo)
                .unwrap_err()
                .contains("results.jsonl")
        );
        let absent = WasmCargo {
            program: "/nonexistent/cargo".into(),
            rustflags: String::new(),
        };
        std::fs::write(&plan_path, serde_json::to_vec(&plan).unwrap()).unwrap();
        assert!(
            call(&plan_path, &results, &absent)
                .unwrap_err()
                .starts_with("cargo: ")
        );
        // A plan whose only mutant of the workspace changed switches none,
        // and a copy that is no Cargo workspace cannot be switched.
        let changed = json!({"tests": plan["tests"], "mutants": [plan["mutants"].as_array().unwrap().iter().find(|m| m["index"] == 99).unwrap()]});
        std::fs::write(&plan_path, serde_json::to_vec(&changed).unwrap()).unwrap();
        assert_eq!(call(&plan_path, &results, &cargo).unwrap(), 0);
        std::fs::write(&plan_path, serde_json::to_vec(&plan).unwrap()).unwrap();
        assert!(
            switch_wasm_workspace(
                &root,
                "web/rust",
                &fixture_root.join("nothing"),
                &plan_path,
                &results,
                &cargo,
                &mut Vec::new()
            )
            .is_err()
        );
        // A project whose manifest Cargo cannot read.
        let unread = fixture_root.join("unread");
        std::fs::create_dir_all(unread.join("web/rust/src")).unwrap();
        std::fs::write(unread.join("web/rust/Cargo.toml"), "[package\n").unwrap();
        std::fs::write(unread.join("web/rust/src/lib.rs"), &source).unwrap();
        std::fs::write(&plan_path, serde_json::to_vec(&plan).unwrap()).unwrap();
        assert!(
            switch_wasm_workspace(
                &unread,
                "web/rust",
                &copy.join("web/rust"),
                &plan_path,
                &results,
                &cargo,
                &mut Vec::new()
            )
            .is_err()
        );
        // A function returning `impl Iterator`: a body that returns another
        // type is no switch's, so those mutants are errors, and its others
        // are switched.
        let iter = fixture_root.join("iter");
        std::fs::create_dir_all(iter.join("src")).unwrap();
        std::fs::write(
            iter.join("Cargo.toml"),
            "[package]\nname = \"iter\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[workspace]\n",
        )
        .unwrap();
        let evens = "pub fn evens(n: u32) -> impl Iterator<Item = u32> {\n    (0..n).filter(|x| x % 2 == 0)\n}\n";
        std::fs::write(iter.join("src/lib.rs"), evens).unwrap();
        let iter_plan = json!({
            "tests": plan["tests"],
            "mutants": crate::rust_mutants::enumerate(evens).unwrap().iter().enumerate()
                .map(|(index, mutant)| json!({"id": mutant_id("src/lib.rs", mutant), "file": "src/lib.rs", "language": "rust", "index": index, "decided": null, "candidates": [{"test": 0}]}))
                .collect::<Vec<_>>(),
        });
        let iter_plan_path = fixture_root.join("iter-plan.json");
        let iter_results = fixture_root.join("iter-results.jsonl");
        std::fs::write(&iter_plan_path, serde_json::to_vec(&iter_plan).unwrap()).unwrap();
        let iter_switched = switch_wasm_workspace(
            &iter,
            "",
            &iter,
            &iter_plan_path,
            &iter_results,
            &cargo,
            &mut Vec::new(),
        )
        .unwrap();
        let iter_found = verdicts(&StrongRustRequest {
            root: iter.clone(),
            command: Vec::new(),
            plan: iter_plan_path.clone(),
            results: iter_results.clone(),
            workers: 1,
        });
        assert!(iter_switched > 0);
        assert!(!iter_found.is_empty());
        assert!(iter_found.values().all(|verdict| {
            verdict["reason"]
                .as_str()
                .is_some_and(|reason| reason.contains("(opaque-return)"))
        }));
        let _ = std::fs::remove_dir_all(fixture_root);
    }

    /// The driver kills, keeps, times out and finds unviable and
    /// unswitchable mutants, from one switched build; and wherever its I/O
    /// fails it ends with an error or the affected mutants' errors, never a
    /// panic or a verdict it did not earn.
    #[test]
    fn a_pass_over_a_crate_and_every_place_it_can_fail() {
        let (root, request) = fixture("pass");
        let mut said = Vec::new();
        assert_eq!(run_strong_rust(&request, &mut said).unwrap(), 0);
        let found = verdicts(&request);
        let status = |id: &str| found.get(id).map(|record| record["status"].clone());
        for (id, expected) in [
            ("src/lib.rs:5:7-5:8:BinaryOperator:/", "killed"),
            ("src/lib.rs:5:7-5:8:BinaryOperator:+", "killed"),
            ("src/lib.rs:9:9-9:11:BinaryOperator:-=", "unviable"),
            ("src/lib.rs:13:7-13:8:BinaryOperator:-", "killed"),
            ("src/lib.rs:17:13-17:14:BinaryOperator:<=", "survived"),
            ("src/lib.rs:18:11-18:13:BinaryOperator:*=", "timeout"),
            ("src/lib.rs:1:1-1:2:FnValue:gone", "error"),
            ("src/lib.rs:5:5-5:10:FnValue:1", "survived"),
            ("src/lib.rs:5:5-5:10:FnValue:0", "error"),
            ("src/lib.rs:16:5-20:6:FnValue:1", "error"),
        ] {
            assert_eq!(status(id).unwrap(), expected, "{id}: {found:?}");
        }
        // The doctest kills `*` to `+`: 3 + 2 is not 6.
        assert_eq!(
            found["src/lib.rs:5:7-5:8:BinaryOperator:+"]["killedBy"],
            "src/lib.rs - double (line 1)"
        );
        let said = String::from_utf8(said).unwrap();
        assert!(said.contains("to build on their own"), "{said}");

        // Each place the pass reads, writes or starts something, failing:
        // `sync_project`'s operations fail where it is called from. Each such
        // call is named by the comment above it, which every new one needs
        // too, to be failed here.
        let text = include_str!("rust_strong.rs");
        let body = &text[..text.find("#[cfg(test)]\nmod tests").unwrap()];
        // The line a needle is on, wherever on it the needle starts.
        let line_of =
            |text: &str, needle: &str| text[..text.find(needle).unwrap()].matches('\n').count() + 1;
        let sync_start = line_of(body, "fn sync_project(");
        let sync_end = sync_start
            + body[body.find("fn sync_project(").unwrap()..]
                .split("\n}\n")
                .next()
                .unwrap()
                .lines()
                .count();
        // Each site with the line it is on: under nextest, only the places
        // nextest reaches are failed.
        let mut sites = Vec::new();
        let lines = body.lines().collect::<Vec<_>>();
        for (number, line) in lines.iter().enumerate() {
            let synced = line.contains("sync_project(") && !line.contains("fn sync_project");
            let called = !(sync_start..=sync_end).contains(&(number + 1))
                && ["crate::fault_fs::", ".launch()"]
                    .iter()
                    .any(|pattern| line.contains(pattern));
            if !synced && !called {
                continue;
            }
            let name = lines[number - 1]
                .trim()
                .strip_prefix("// fault-site: ")
                .unwrap_or_else(|| panic!("line {}, {line:?}, has no fault-site name", number + 1));
            if synced {
                // The copy's first directory, the first entry read, a
                // directory inside, and a file written.
                for operation in [
                    "create_dir_all",
                    "read_dir entry",
                    "read_dir#@2",
                    "create_dir_all#@2",
                    "symlink_metadata",
                    "write",
                ] {
                    let (operation, nth) = operation.split_once("#").unwrap_or((operation, ""));
                    sites.push((number + 1, format!("fs.{operation}#{name}{nth}")));
                }
            }
            if called {
                let site = match line.find("crate::fault_fs::") {
                    Some(at) => {
                        let operation = line[at + "crate::fault_fs::".len()..]
                            .split('(')
                            .next()
                            .unwrap_or_default();
                        format!("fs.{operation}#{name}")
                    }
                    None => format!("spawn#{name}"),
                };
                sites.push((number + 1, site));
            }
        }
        // Commands run to their output, by their order in a pass; `rustc
        // --print`; and the switched copy written.
        for nth in 1..=8 {
            sites.push((0, format!("spawn#child-signal-guard.output@{nth}")));
        }
        for nth in 1..=3 {
            sites.push((0, format!("spawn#rust-test-runner.rustc-print-path@{nth}")));
        }
        sites.push((
            0,
            "fs.write#rust-project.prepare-rust-strong-project.write-1".to_owned(),
        ));
        assert!(sites.len() > 10, "{sites:?}");
        // One copy for every fault, its plan without the mutant that runs to
        // its time limit, each fault with results of its own: under `cargo
        // test` and under nextest.
        let (fault_root, mut request) = fixture("fault");
        let plan: Value = serde_json::from_slice(&std::fs::read(&request.plan).unwrap()).unwrap();
        let mut small = plan.clone();
        small["mutants"] = Value::Array(
            plan["mutants"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|mutant| !mutant["id"].as_str().unwrap().contains(":18:"))
                .cloned()
                .collect(),
        );
        std::fs::write(&request.plan, serde_json::to_vec(&small).unwrap()).unwrap();
        // Under nextest, the places only nextest reaches.
        let nextest_start = line_of(body, "impl Nextest {");
        let nextest_end = nextest_start
            + body[body.find("impl Nextest {").unwrap()..]
                .split("\n}\n")
                .next()
                .unwrap()
                .lines()
                .count();
        let nextest_sites = sites
            .iter()
            .filter(|(line, _)| (nextest_start..=nextest_end).contains(line))
            .map(|(_, site)| site.clone())
            .collect::<Vec<_>>();
        let sites = sites.into_iter().map(|(_, site)| site).collect::<Vec<_>>();
        assert!(nextest_sites.len() >= 5, "{nextest_sites:?}");
        for (command, sites) in [
            (&["cargo", "test"][..], &sites),
            (&["cargo", "nextest", "run"], &nextest_sites),
        ] {
            request.command = command.iter().map(|word| (*word).to_owned()).collect();
            for (number, site) in sites.iter().enumerate() {
                request.results =
                    fault_root.join(format!("results-{}-{number}.jsonl", command.len()));
                let outcome = crate::contained::contained(|| {
                    crate::fault::during(site, || run_strong_rust(&request, &mut Vec::new()))
                });
                let outcome = outcome.unwrap_or_else(|panic| panic!("{site} panicked: {panic}"));
                if outcome.is_ok() {
                    assert!(
                        !verdicts(&request).is_empty(),
                        "{site}: no verdict and no error"
                    );
                }
            }
        }
        let _ = std::fs::remove_dir_all(fault_root);
        let _ = std::fs::remove_dir_all(root);
    }

    /// A mutant whose candidates all pass is a survivor only where one of
    /// them ran its mutated code. One no process reached -- its tests ran
    /// something built apart from the pass, as a Worker's wasm built by
    /// `wrangler dev` -- is `no-coverage`, saying why; one a test ran
    /// without checking what it gave survived.
    #[test]
    fn a_mutant_no_test_ran_mutated_is_no_survivor() {
        let (_root, request) = crate_with(
            "reach",
            &[
                (
                    "Cargo.toml",
                    "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[workspace]\n",
                ),
                (
                    "src/lib.rs",
                    "pub fn ran(v: u32) -> u32 {\n    v + 1\n}\npub fn apart(v: u32) -> u32 {\n    v * 2\n}\n",
                ),
                (
                    "tests/t.rs",
                    "#[test] fn unchecked() { let _ = fixture::ran(1); }\n",
                ),
            ],
            &["src/lib.rs"],
        );
        let mut said = Vec::new();
        assert_eq!(run_strong_rust(&request, &mut said), Ok(0));
        let said = String::from_utf8(said).unwrap();
        let found = verdicts(&request);
        let ran = found
            .iter()
            .filter(|(id, _)| id.starts_with("src/lib.rs:2:"))
            .collect::<Vec<_>>();
        let apart = found
            .iter()
            .filter(|(id, _)| id.starts_with("src/lib.rs:5:"))
            .collect::<Vec<_>>();
        assert!(!ran.is_empty() && !apart.is_empty(), "{found:?}\n{said}");
        for (id, verdict) in ran {
            assert_eq!(verdict["status"], "survived", "{id}: {verdict}\n{said}");
        }
        for (id, verdict) in apart {
            assert_eq!(verdict["status"], "no-coverage", "{id}: {verdict}\n{said}");
            let reason = verdict["reason"].as_str().unwrap();
            assert!(
                reason.starts_with(
                    "not-reached-mutated: its 1 test(s) passed but never ran the mutated code"
                ),
                "{reason}"
            );
        }
    }
}
