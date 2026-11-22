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
//! - an error at no switch is found by halving the switched mutants of its
//!   file until the build passes, and the mutants it comes down to are built
//!   on their own.
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
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
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
        &std::fs::read(&request.plan).map_err(|e| format!("{}: {e}", request.plan.display()))?,
    )
    .map_err(|error| format!("{}: {error}", request.plan.display()))?;
    let results = Results::create(&request.results)?;
    let root = std::fs::canonicalize(&request.root)
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
            let source = std::fs::read_to_string(root.join(&file)).ok()?;
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

    let invocation = crate::rust_test_runner::cargo_invocation(&root, &request.command)
        .map_err(|error| error.to_string())?;
    let build_invocation = build_invocation(&invocation);

    // Which mutants a run-time switch can hold.
    let mut switchable = Vec::new();
    let mut alone = Vec::<(usize, &'static str)>::new();
    for (at, job) in jobs.iter().enumerate() {
        let (source, _) = sources[&job.file].as_ref().expect("a job's file was read");
        let (tree, _) =
            crate::rust_instrumenter::parse_source(source).expect("the file was enumerated");
        let tree = ra_ap_syntax::AstNode::syntax(&tree).clone();
        match crate::rust_schemata::placeable(&tree, &job.mutant) {
            Some(reason) => alone.push((at, reason)),
            None => switchable.push(at),
        }
    }

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
    };

    // Baselines: every test a mutant runs against, with no mutant.
    let mut needed = BTreeSet::new();
    for at in switchable.iter().chain(alone.iter().map(|(at, _)| at)) {
        needed.extend(jobs[*at].candidates.iter().copied());
    }
    let baselines = baselines(&runner, &tests, &needed);
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
    let workers = std::env::var("SUPERCOV_STRONG_JOBS")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|jobs| *jobs > 0)
        .unwrap_or_else(|| {
            std::thread::available_parallelism()
                .map(usize::from)
                .unwrap_or(1)
                .div_ceil(2)
        });
    let next = AtomicUsize::new(0);
    let counts = Mutex::new(BTreeMap::<&'static str, usize>::new());
    std::thread::scope(|scope| {
        for _ in 0..workers.min(switchable.len()).max(1) {
            scope.spawn(|| {
                loop {
                    let index = next.fetch_add(1, Ordering::Relaxed);
                    let Some(at) = switchable.get(index) else {
                        break;
                    };
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
        }
    });

    // The mutants built on their own, one at a time.
    for (at, why) in &alone {
        let job = &jobs[*at];
        let mutant_started = Instant::now();
        let (source, _) = sources[&job.file].as_ref().expect("a job's file was read");
        match build_alone(&root, &places, &build_invocation, job, source) {
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
    let mut directory = root.join(file);
    while directory.pop() && directory.starts_with(root) {
        let Ok(text) = std::fs::read_to_string(directory.join("Cargo.toml")) else {
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
    let mut rest = invocation.arguments[position + skip..].iter();
    while let Some(argument) = rest.next() {
        if argument == "--" {
            break;
        }
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
/// rebuilds only what changed.
fn sync_project(root: &Path, copy: &Path) -> Result<(), String> {
    fn walk(from: &Path, to: &Path, top: bool) -> std::io::Result<()> {
        std::fs::create_dir_all(to)?;
        for entry in std::fs::read_dir(from)? {
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
            let kind = entry.file_type()?;
            let target = to.join(&name);
            if kind.is_dir() {
                walk(&entry.path(), &target, false)?;
            } else if kind.is_file() || kind.is_symlink() {
                let Ok(bytes) = std::fs::read(entry.path()) else {
                    continue;
                };
                if std::fs::read(&target).ok().as_deref() != Some(bytes.as_slice()) {
                    std::fs::write(&target, &bytes)?;
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
                let source = std::fs::canonicalize(source).unwrap_or_else(|_| source.into());
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
                std::fs::canonicalize(path)
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
                .map_err(|error| error.to_string())?;
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
        let mut unexplained = BTreeSet::new();
        for error in &errors {
            let (own, at_switch) = attribute(error, &prepared.switched);
            let typed = error
                .code
                .as_deref()
                .is_some_and(|code| OWN_ERRORS.contains(&code));
            if own.len() == 1 && typed {
                let ordinal = own.iter().next().expect("one");
                if let Some(at) = ordinal_of.get(ordinal) {
                    unviable
                        .entry(*at)
                        .or_insert_with(|| first_line(&error.rendered));
                    out.insert(*at);
                }
            } else if !own.is_empty() || !at_switch.is_empty() {
                for ordinal in own.iter().chain(&at_switch) {
                    if let Some(at) = ordinal_of.get(ordinal)
                        && !unviable.contains_key(at)
                        && out.insert(*at)
                    {
                        alone.push((*at, "switch-error"));
                    }
                }
            } else {
                unexplained.extend(error.spans.iter().map(|(file, _)| file.clone()));
            }
        }
        if !out.is_empty() {
            switchable.retain(|at| !out.contains(at));
            continue;
        }
        // Errors at no switch: the first mutant whose switch breaks the
        // build, by halving, of those in the errors' files (or of all).
        let in_files = switchable
            .iter()
            .copied()
            .filter(|at| unexplained.contains(&jobs[*at].file))
            .collect::<Vec<_>>();
        let suspects = if in_files.is_empty() {
            switchable.clone()
        } else {
            in_files
        };
        let base = switchable
            .iter()
            .copied()
            .filter(|at| !suspects.contains(at))
            .collect::<Vec<_>>();
        let (mut low, mut high) = (0usize, suspects.len());
        // `base` with the first `low` suspects builds; with the first `high`
        // it does not (it is the build that just failed).
        let fails = |count: usize| -> Result<bool, String> {
            let mut chosen = base.clone();
            chosen.extend_from_slice(&suspects[..count]);
            let (_, built) = switched_build(root, places, jobs, &chosen, invocation)?;
            Ok(built.is_err())
        };
        builds += 1;
        if fails(0)? {
            if base.is_empty() {
                return Err(format!(
                    "the project does not build without mutants: {}",
                    errors
                        .iter()
                        .map(|error| first_line(&error.rendered))
                        .collect::<Vec<_>>()
                        .join("; ")
                ));
            }
            // The errors are not only these mutants': halve all of them.
            switchable.retain(|at| base.contains(at));
            alone.extend(suspects.iter().map(|at| (*at, "switch-error")));
            continue;
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
        let culprit = suspects[high - 1];
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
        for (ordinal, code) in &file.replacements {
            if code.start <= range.start && range.end <= code.end {
                own.insert((name.clone(), *ordinal));
            }
        }
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
    std::fs::write(&path, mutated).map_err(|error| format!("{}: {error}", path.display()))?;
    let workspace = std::fs::canonicalize(&places.plain)
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
            .map_err(|error| error.to_string())?;
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
        let mut command = self.launcher.command(&artifact.executable, None);
        command
            .args(["--exact", name, "--test-threads=1"])
            .current_dir(&artifact.package_directory)
            .envs(crate::rust_test_runner::instrumented_stack_environment())
            .envs(crate::rust_test_runner::dynamic_library_environment(
                self.target_libdir,
                &artifact.executable,
            ))
            .env(HIT_LIMIT_ENV, hit_limit.to_string());
        match mutant {
            Some(named) => command.env(ACTIVE_MUTANT_ENV, named),
            None => command.env_remove(ACTIVE_MUTANT_ENV),
        };
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

/// Runs `command` for at most `limit`, and how it ended, with its standard
/// output.
fn timed_with_output(command: &mut Command, limit: Duration) -> (Ran, String) {
    let started = Instant::now();
    let child = command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn();
    let Ok(mut child) = child else {
        return (
            Ran {
                outcome: Outcome::Failed,
                duration: started.elapsed(),
            },
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
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if started.elapsed() > limit => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(5)),
            Err(_) => break None,
        }
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
    (Ran { outcome, duration }, stdout)
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
) -> BTreeMap<usize, Ran> {
    let needed = needed.iter().copied().collect::<Vec<_>>();
    let next = AtomicUsize::new(0);
    let found = Mutex::new(BTreeMap::new());
    let workers = std::thread::available_parallelism()
        .map(usize::from)
        .unwrap_or(1)
        .div_ceil(2);
    std::thread::scope(|scope| {
        for _ in 0..workers.min(needed.len()).max(1) {
            scope.spawn(|| {
                loop {
                    let index = next.fetch_add(1, Ordering::Relaxed);
                    let Some(test) = needed.get(index) else {
                        break;
                    };
                    let planned = &tests[*test];
                    let ran = if planned.runner == "rustdoc" {
                        let package = planned.project.clone().unwrap_or_default();
                        runner
                            .run_doctests(
                                &package,
                                &[&planned.name],
                                None,
                                Duration::from_secs(600),
                            )
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
    let mut candidates = job
        .candidates
        .iter()
        .copied()
        .filter(|test| usable(*test))
        .collect::<Vec<_>>();
    if candidates.is_empty() {
        let reason = if job.candidates.is_empty() {
            "no test can kill it"
        } else {
            "every test that reaches it failed or could not run without a mutant"
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
        match outcome.outcome {
            Outcome::Passed => {}
            Outcome::Failed | Outcome::HitLimit
                if failed.is_some() || outcome.outcome == Outcome::Failed =>
            {
                let killer = failed.unwrap_or_else(|| tests[members[0]].id.clone());
                results.verdict(&job.id, "killed", Some(&killer), ran, started, None);
                return "killed";
            }
            _ => {
                results.verdict(
                    &job.id,
                    "timeout",
                    None,
                    ran,
                    started,
                    Some(format!("doctests ran past {} ms", limit.as_millis())),
                );
                return "timeout";
            }
        }
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
        std::fs::create_dir_all(root.join("inner/src")).unwrap();
        std::fs::write(
            root.join("Cargo.toml"),
            "[workspace]\nmembers = [\"inner\"]\n",
        )
        .unwrap();
        std::fs::write(
            root.join("inner/Cargo.toml"),
            "[package]\nname = \"inner\"\n",
        )
        .unwrap();
        assert_eq!(
            package_of(&root, "inner/src/lib.rs").as_deref(),
            Some("inner")
        );
        assert_eq!(package_of(&root, "src/lib.rs"), None);
        // The copy holds what the project holds, and keeps what is the same.
        let copy = root.join("copy");
        sync_project(&root.join("inner"), &copy).unwrap();
        assert!(copy.join("Cargo.toml").exists());
        let written = std::fs::metadata(copy.join("Cargo.toml"))
            .unwrap()
            .modified()
            .unwrap();
        sync_project(&root.join("inner"), &copy).unwrap();
        assert_eq!(
            std::fs::metadata(copy.join("Cargo.toml"))
                .unwrap()
                .modified()
                .unwrap(),
            written
        );
        std::fs::remove_dir_all(&root).unwrap();
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
        assert_eq!(first_line("a\nb"), "a");
    }
}
