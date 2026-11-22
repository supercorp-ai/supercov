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
//! mutant's limit, 3 times the baseline and 10 s. A mutant's tests run
//! cheapest first and stop at the first that fails: `killed`. One that runs
//! past its limit is stopped: `timeout`. Where `SUPERCOV_MUTANT_HIT_LIMIT`
//! is set, a test whose mutant was evaluated more often than that stops at
//! once, also a `timeout`. Every test passing is `survived`.
//!
//! Doctests are compiled by rustdoc against the switched library and run in
//! one `cargo test --doc` per mutant, after its libtest candidates.
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
    let started = Instant::now();
    let plan: Value = serde_json::from_slice(
        &crate::fault_fs::read(&request.plan)
            .map_err(|e| format!("{}: {e}", request.plan.display()))?,
    )
    .map_err(|error| format!("{}: {error}", request.plan.display()))?;
    let results = Results::create(&request.results)?;
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
    let mut jobs = Vec::new();
    let mut sources = BTreeMap::<String, Option<(String, Vec<RustMutant>)>>::new();
    for planned in plan["mutants"].as_array().into_iter().flatten() {
        if !planned["decided"].is_null() {
            continue;
        }
        let id = planned["id"].as_str().unwrap_or_default().to_owned();
        let file = planned["file"].as_str().unwrap_or_default().to_owned();
        let index = planned["index"].as_u64().unwrap_or(u64::MAX) as usize;
        let listed = sources.entry(file.clone()).or_insert_with(|| {
            let source = crate::fault_fs::read_to_string(root.join(&file)).ok()?;
            let mutants = crate::rust_mutants::enumerate(&source)?;
            Some((source, mutants))
        });
        let Some(mutant) = listed
            .as_ref()
            .and_then(|(_, mutants)| mutants.get(index))
            .filter(|mutant| mutant_id(&file, mutant) == id)
        else {
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
        });
    }
    let _ = writeln!(
        diagnostics,
        "[supercov] strong pass: {} Rust mutant(s) to run",
        jobs.len()
    );
    if jobs.is_empty() {
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
        for job in &jobs {
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
        }
        results.write(&json!({"summary": true, "mutants": jobs.len(), "browser": true}));
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
    };

    // Baselines: every test a mutant runs against, with no mutant.
    let needed = switchable
        .iter()
        .chain(alone.iter().map(|(at, _)| at))
        .flat_map(|at| jobs[*at].candidates.iter().copied())
        .collect::<BTreeSet<_>>();
    let baselines = baselines(&runner, &tests, &needed, request.workers);
    let usable = |test: usize| {
        tests[test].runnable
            && baselines
                .get(&test)
                .is_some_and(|ran| ran.outcome == Outcome::Passed)
    };
    let limit_of = |test: usize| {
        let baseline = baselines
            .get(&test)
            .map_or(Duration::from_secs(10), |ran| ran.duration);
        baseline * 3 + Duration::from_secs(10)
    };

    // The switched mutants, side by side.
    let next = AtomicUsize::new(0);
    let counts = Mutex::new(BTreeMap::<&'static str, usize>::new());
    pool(request.workers.min(switchable.len()), || {
        while let Some(at) = switchable.get(next.fetch_add(1, Ordering::Relaxed)) {
            let job = &jobs[*at];
            let named = format!("{}#{}", job.file, job.ordinal);
            let status = run_job(
                job,
                &tests,
                &runner,
                &usable,
                &limit_of,
                Some(&named),
                &results,
            );
            *counts.lock().expect("counts").entry(status).or_default() += 1;
        }
    });

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
        ) {
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
                };
                let status = run_job(
                    job,
                    &tests,
                    &alone_runner,
                    &usable,
                    &limit_of,
                    None,
                    &results,
                );
                *counts.lock().expect("counts").entry(status).or_default() += 1;
            }
        }
    }
    // Leave the plain copy as written.
    let _ = sync_project(&root, &places.plain);

    let counts = counts.into_inner().expect("counts");
    results.write(&json!({
        "summary": true,
        "mutants": jobs.len(),
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

/// The name of the package whose manifest is nearest `file`, up from it.
fn package_of(root: &Path, file: &str) -> Option<String> {
    let file = root.join(file);
    for directory in file
        .ancestors()
        .skip(1)
        .take_while(|directory| directory.starts_with(root))
    {
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
) -> Result<Result<Vec<Artifact>, Vec<CompileError>>, String> {
    let mut arguments = invocation.arguments.clone();
    arguments.extend(["--no-run".into(), "--message-format=json".into()]);
    let output = crate::child_signal_guard::output(
        Command::new(&invocation.program)
            .args(&arguments)
            .current_dir(&project.workspace_root)
            .env("CARGO_TARGET_DIR", &project.target_directory)
            .env(
                "RUSTFLAGS",
                crate::rust_test_runner::capped_rustflags(project, invocation),
            ),
    )
    .map_err(|error| format!("cargo: {error}"))?;
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
    if !output.status.success() {
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
        for span in message["spans"].as_array().into_iter().flatten() {
            let (Some(file), Some(start), Some(end)) = (
                span["file_name"].as_str(),
                span["byte_start"].as_u64(),
                span["byte_end"].as_u64(),
            ) else {
                continue;
            };
            let path = Path::new(file);
            let relative = if path.is_absolute() {
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
) -> Result<(crate::rust_project::StrongWorkspace, BuildOutcome), String> {
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
    let built = cargo_build(&prepared.project, invocation)?;
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
) -> Result<Built, String> {
    let mut builds = 0usize;
    loop {
        builds += 1;
        let _ = writeln!(
            diagnostics,
            "[supercov] building {} switched mutant(s) (build {builds})",
            switchable.len()
        );
        let (prepared, built) = switched_build(root, places, jobs, switchable, invocation)?;
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
            let typed = error
                .code
                .as_deref()
                .is_some_and(|code| OWN_ERRORS.contains(&code));
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
        // Errors at no switch: the first mutant whose switch breaks the
        // build, by halving.
        let (mut low, mut high) = (0usize, switchable.len());
        // The first `low` switched mutants build; the first `high` do not
        // (it is the build that just failed).
        let fails = |count: usize| -> Result<bool, String> {
            let (_, built) = switched_build(root, places, jobs, &switchable[..count], invocation)?;
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
) -> Result<Result<Built, String>, String> {
    sync_project(root, &places.plain)?;
    let range = usize::from(job.mutant.range.start())..usize::from(job.mutant.range.end());
    let mutated = format!(
        "{}{}{}",
        &source[..range.start],
        job.mutant.replacement,
        &source[range.end..]
    );
    let path = places.plain.join(&job.file);
    crate::fault_fs::write(&path, mutated)
        .map_err(|error| format!("{}: {error}", path.display()))?;
    let workspace = crate::fault_fs::canonicalize(&places.plain)
        .map_err(|error| format!("{}: {error}", places.plain.display()))?;
    let mut project = crate::rust_project::PreparedRustProject::plain(workspace);
    project.target_directory = places.plain_target.clone();
    match cargo_build(&project, invocation)? {
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
        crate::fault_fs::write(&cargo, &described.stdout)
            .map_err(|e| format!("{}: {e}", cargo.display()))?;
        let parsed: Value = serde_json::from_slice(&listed.stdout)
            .map_err(|error| format!("cargo nextest list: {error}"))?;
        let binary_ids = parsed["rust-binaries"]
            .as_object()
            .into_iter()
            .flatten()
            .filter_map(|(id, binary)| {
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
            let (mut ran, _, stderr) = timed_with_streams(&mut command, limit);
            // nextest's own timeout ends the test as a timeout, and a filter
            // that matched nothing ran nothing.
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
        Some(timed(&mut command, limit))
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
        let (ran, stdout) = timed_with_output(&mut command, limit);
        let failed = stdout.lines().find_map(|line| {
            line.strip_prefix("test ")
                .and_then(|rest| rest.strip_suffix(" ... FAILED"))
                .map(str::to_owned)
        });
        (ran, failed)
    }
}

fn timed(command: &mut Command, limit: Duration) -> Ran {
    timed_with_output(command, limit).0
}

fn timed_with_output(command: &mut Command, limit: Duration) -> (Ran, String) {
    let (ran, stdout, _) = timed_with_streams(command, limit);
    (ran, stdout)
}

/// Runs `command` for at most `limit`, and how it ended, with its standard
/// output and error.
fn timed_with_streams(command: &mut Command, limit: Duration) -> (Ran, String, String) {
    let started = Instant::now();
    let child = command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .launch();
    let Ok(mut child) = child else {
        return (
            Ran {
                outcome: Outcome::Failed,
                duration: started.elapsed(),
            },
            String::new(),
            String::new(),
        );
    };
    let _registered = crate::child_signal_guard::register(&child);
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
    // A child that cannot be waited on runs, as far as this can tell, until
    // its limit.
    let status = loop {
        if let Some(status) = child.try_wait().ok().flatten() {
            break Some(status);
        }
        if started.elapsed() > limit {
            let _ = child.kill();
            let _ = child.wait();
            break None;
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    let duration = started.elapsed();
    let stdout = String::from_utf8_lossy(&out.join().unwrap_or_default()).into_owned();
    let stderr = String::from_utf8_lossy(&err.join().unwrap_or_default()).into_owned();
    let outcome = match status {
        None => Outcome::TimedOut,
        Some(_) if stderr.contains(TIMEOUT_MARKER) || stdout.contains(TIMEOUT_MARKER) => {
            Outcome::HitLimit
        }
        Some(status) if status.success() && !libtest_ran_nothing(&stdout) => Outcome::Passed,
        Some(_) => Outcome::Failed,
    };
    (Ran { outcome, duration }, stdout, stderr)
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
        while let Some(test) = needed.get(next.fetch_add(1, Ordering::Relaxed)) {
            let planned = &tests[*test];
            let ran = if planned.runner == "rustdoc" {
                let package = planned.project.clone().unwrap_or_default();
                runner
                    .run_doctests(&package, &[&planned.name], None, Duration::from_secs(600))
                    .0
            } else {
                match runner.run(planned, None, 0, Duration::from_secs(600)) {
                    Some(ran) => ran,
                    None => continue,
                }
            };
            found.lock().expect("baselines").insert(*test, ran);
        }
    });
    found.into_inner().expect("baselines")
}

/// Runs one mutant against its candidates and writes its verdict: in the
/// switched build with `ordinal` active, or in a build of its own (`None`).
fn run_job(
    job: &Job,
    tests: &[PlannedTest],
    runner: &TestRunner<'_>,
    usable: &dyn Fn(usize) -> bool,
    limit_of: &dyn Fn(usize) -> Duration,
    ordinal: Option<&str>,
    results: &Results,
) -> &'static str {
    let started = Instant::now();
    // Tests of another runner (JavaScript tests over the crate's wasm) are
    // the JavaScript pass's to run (`docs/design/mutation-drivers.md`).
    let foreign = job
        .candidates
        .iter()
        .map(|test| tests[*test].runner.as_str())
        .find(|runner| !matches!(*runner, "rust-libtest" | "nextest" | "rustdoc"));
    if let Some(runner) = foreign
        && job
            .candidates
            .iter()
            .all(|test| tests[*test].runner == runner)
    {
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
        return "error";
    }
    let mut candidates = job
        .candidates
        .iter()
        .copied()
        .filter(|test| usable(*test))
        .collect::<Vec<_>>();
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
            return "error";
        }
        let reason = if job.candidates.is_empty() {
            "no test can kill it"
        } else {
            "every test that reaches it failed in the measured run"
        };
        results.verdict(&job.id, "survived", None, 0, started, Some(reason.into()));
        return "survived";
    }
    candidates.sort_by_key(|test| limit_of(*test));
    let (doctests, libtests): (Vec<_>, Vec<_>) = candidates
        .into_iter()
        .partition(|test| tests[*test].runner == "rustdoc");
    let mut ran = 0usize;
    for test in libtests {
        let planned = &tests[test];
        let limit = limit_of(test);
        let Some(outcome) = runner.run(planned, ordinal, hit_limit(), limit) else {
            continue;
        };
        ran += 1;
        match outcome.outcome {
            Outcome::Passed => {}
            Outcome::Failed => {
                results.verdict(&job.id, "killed", Some(&planned.id), ran, started, None);
                return "killed";
            }
            Outcome::TimedOut | Outcome::HitLimit => {
                results.verdict(
                    &job.id,
                    "timeout",
                    Some(&planned.id),
                    ran,
                    started,
                    Some(format!("ran past {} ms", limit.as_millis())),
                );
                return "timeout";
            }
        }
    }
    let mut by_package = BTreeMap::<String, Vec<usize>>::new();
    for test in doctests {
        by_package
            .entry(tests[test].project.clone().unwrap_or_default())
            .or_default()
            .push(test);
    }
    for (package, members) in by_package {
        let names = members
            .iter()
            .map(|test| tests[*test].name.as_str())
            .collect::<Vec<_>>();
        let limit = members.iter().map(|test| limit_of(*test)).sum::<Duration>();
        let (outcome, failed) = runner.run_doctests(&package, &names, ordinal, limit);
        ran += members.len();
        // As for a libtest case: the doctest that failed kills the mutant,
        // and one that ran past its limit or hit the limit is a timeout.
        let (status, killed_by, reason) = match outcome.outcome {
            Outcome::Passed => continue,
            Outcome::Failed => (
                "killed",
                Some(failed.unwrap_or(tests[members[0]].id.clone())),
                None,
            ),
            Outcome::TimedOut | Outcome::HitLimit => (
                "timeout",
                failed,
                Some(format!("doctests ran past {} ms", limit.as_millis())),
            ),
        };
        results.verdict(&job.id, status, killed_by.as_deref(), ran, started, reason);
        return status;
    }
    results.verdict(&job.id, "survived", None, ran, started, None);
    "survived"
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
        std::fs::write(path, format!("#!/bin/sh\n{body}")).unwrap();
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
        let (root, request) = crate_with(
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
        assert_eq!(doctest["killedBy"], "src/lib.rs - a (line 1)");
        let untested = &found["src/lib.rs:5:7-5:8:BinaryOperator:*"];
        assert_eq!(untested["reason"], "no test can kill it", "{found:?}");
        let _ = std::fs::remove_dir_all(root);
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
        // A test that cannot be found when its mutant runs (after its
        // baseline) is passed over.
        request.command[0] = "cargo".into();
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
        // Found in the code before the tests, by the call alone: a measured
        // build wraps the `?` around it, and the needle is in this test too.
        let text = include_str!("rust_strong.rs");
        let body = &text[..text.find("#[cfg(test)]\nmod tests").unwrap()];
        let place = format!(
            "fs.canonicalize#rust_strong.rs:{}@2",
            body[..body.find("fault_fs::canonicalize(executable)").unwrap()]
                .lines()
                .count()
                + 1
        );
        crate::fault::during(&place, || run_strong_rust(&request, &mut Vec::new())).unwrap();
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

    fn verdicts(request: &StrongRustRequest) -> BTreeMap<String, Value> {
        std::fs::read_to_string(&request.results)
            .unwrap_or_default()
            .lines()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .filter_map(|record| Some((record["id"].as_str()?.to_owned(), record)))
            .collect()
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
        // `sync_project`'s operations fail where it is called from. Every
        // needle is a call as written, which a measured build keeps on its
        // line though it wraps a `?` around it.
        let text = include_str!("rust_strong.rs");
        let body = &text[..text.find("#[cfg(test)]\nmod tests").unwrap()];
        let line_of =
            |text: &str, needle: &str| text[..text.find(needle).unwrap()].lines().count() + 1;
        let sync_start = line_of(body, "fn sync_project(");
        let sync_end = sync_start
            + body[body.find("fn sync_project(").unwrap()..]
                .split("\n}\n")
                .next()
                .unwrap()
                .lines()
                .count();
        let mut sites = Vec::new();
        for (number, line) in body.lines().enumerate() {
            let place = format!("rust_strong.rs:{}", number + 1);
            if line.contains("sync_project(") && !line.contains("fn sync_project") {
                // The copy's first directory, the first entry read, a
                // directory inside, and a file written.
                for operation in [
                    "create_dir_all",
                    "read_dir#@2",
                    "create_dir_all#@2",
                    "symlink_metadata",
                    "write",
                ] {
                    let (operation, nth) = operation.split_once("#").unwrap_or((operation, ""));
                    sites.push(format!("fs.{operation}#{place}{nth}"));
                }
            }
            if (sync_start..=sync_end).contains(&(number + 1)) {
                continue;
            }
            for (pattern, kind) in [("crate::fault_fs::", "fs."), (".launch()", "spawn")] {
                if let Some(at) = line.find(pattern) {
                    let operation = line[at + pattern.len()..]
                        .split('(')
                        .next()
                        .unwrap_or_default();
                    sites.push(if kind == "spawn" {
                        format!("spawn#{place}")
                    } else {
                        format!("fs.{operation}#{place}")
                    });
                }
            }
        }
        // Commands run to their output, by their order in a pass; `rustc
        // --print`; and the switched copy written.
        let guard = include_str!("child_signal_guard.rs");
        let output = line_of(guard, "pub fn output(") + 5;
        let runner = include_str!("rust_test_runner.rs");
        let printed = line_of(runner, "Command::new(\"rustc\")") + 4;
        for nth in 1..=8 {
            sites.push(format!("spawn#child_signal_guard.rs:{output}@{nth}"));
        }
        for nth in 1..=3 {
            sites.push(format!("spawn#rust_test_runner.rs:{printed}@{nth}"));
        }
        let project = include_str!("rust_project.rs");
        sites.push(format!(
            "fs.write#rust_project.rs:{}",
            line_of(project, "fs::write(&path, &file_switched.code)")
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
            .filter(|site| {
                site.split_once("rust_strong.rs:")
                    .and_then(|(_, line)| line.split('@').next()?.parse::<usize>().ok())
                    .is_some_and(|line| (nextest_start..=nextest_end).contains(&line))
            })
            .cloned()
            .collect::<Vec<_>>();
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
}
