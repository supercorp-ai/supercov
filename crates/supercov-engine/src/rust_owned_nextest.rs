//! `cargo nextest run` for the owned Rust frontend.
//!
//! nextest already runs every test in a process of its own and honours
//! Cargo's target runner, so nothing about its scheduling, retries or output
//! has to be reproduced: nextest builds the instrumented workspace, lists it
//! and runs it exactly as the user asked, and this program is configured as
//! the target runner. Every attempt nextest launches passes through here,
//! gets an evidence directory of its own, and is recorded together with the
//! exact identity nextest exposes to runners (`NEXTEST_TEST_NAME`,
//! `NEXTEST_ATTEMPT`, `NEXTEST_BINARY_ID`, ...). The listing nextest produced
//! beforehand names every selected test, so a test that ran unrecorded, or
//! was listed but never ran, fails the run rather than thinning the report.
//!
//! A runner the user already configured for the target keeps running the
//! binaries: this program wraps it rather than replacing it.

use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsString,
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::Command,
};

use nextest_metadata::TestListSummary;
use serde::{Deserialize, Serialize};

use crate::{
    coverage_report::{ExecutionScope, RawTestResult, TestProvenance},
    rust_cargo_configuration::{RustCargoResolvedRunner, resolve_cargo_runner_plan},
    rust_project::PreparedRustProject,
    rust_runner_attempt::{
        RustRunnerInvocationIdentity, classify_rust_runner_environment,
        parse_nextest_version_output, validate_nextest_version,
    },
    rust_test_runner::{
        CargoTestInvocation, RustTestRunnerError, capped_rustflags, instrumented_stack_environment,
        io_error, nextest_list_invocation, nextest_version_arguments, relative_source, snapshot,
    },
};

/// The first argument that makes this program nextest's target runner.
pub const RUNNER_MODE_ARGUMENT: &str = "__nextest-runner";
const PLAN_FILE: &str = "runner-plan.json";
const ATTEMPTS_DIRECTORY: &str = "attempts";

/// What the runner needs to know per target: the runner the user configured,
/// if any, which this program wraps.
#[derive(Debug, Serialize, Deserialize)]
struct RunnerPlan {
    /// The instrumentation the tests belong to, for their evidence.
    token: String,
    targets: Vec<RunnerTarget>,
}

#[derive(Debug, Serialize, Deserialize)]
struct RunnerTarget {
    target: String,
    underlying: Option<RustCargoResolvedRunner>,
}

/// One attempt nextest launched, as the runner recorded it.
#[derive(Debug, Serialize, Deserialize)]
struct AttemptRecord {
    binary_id: String,
    test_name: String,
    attempt: usize,
    total_attempts: usize,
    runner_attempt_id: String,
    exit_code: i32,
    evidence: PathBuf,
}

/// A test binary nextest listed, joined to the Cargo target it was built from.
#[derive(Debug, Clone)]
struct NextestArtifact {
    executable: PathBuf,
    binary_name: String,
    kind: String,
    source: String,
}

pub struct NextestOutcome {
    pub results: Vec<RawTestResult>,
    pub artifact_files: Vec<PathBuf>,
    pub exit_code: i32,
    /// The crates a test's Worker returned a region for: `wrangler dev`
    /// built them, though the run's own build did not.
    pub built_crates: BTreeSet<String>,
    /// A test started a Worker with `wrangler dev --remote`.
    pub remote_workers: bool,
    /// The `wrangler dev` the tests started, by their notes.
    pub started_workers: Vec<String>,
}

// -------------------------------------------------------------- the engine

/// Run the user's `cargo nextest run` against the instrumented workspace with
/// this program as the target runner, and read back every attempt.
pub(crate) fn run_nextest(
    project: &PreparedRustProject,
    invocation: &CargoTestInvocation,
    root: &Path,
    run_id: &str,
    rust_workers: Option<&crate::rust_worker::CargoTestWorkers>,
    diagnostics: &mut dyn Write,
) -> Result<NextestOutcome, RustTestRunnerError> {
    fs::create_dir_all(root.join(ATTEMPTS_DIRECTORY)).map_err(io_error)?;
    let program = std::env::current_exe()
        .and_then(fs::canonicalize)
        .map_err(io_error)?;
    let cargo = |arguments: &[String]| {
        let mut command = Command::new(&invocation.program);
        command
            .args(arguments)
            .current_dir(&project.workspace_root)
            .env("CARGO_TARGET_DIR", &project.target_directory)
            .env("RUSTFLAGS", capped_rustflags(project, invocation))
            .env(crate::rust_runtime::RUN_ID_ENV, run_id)
            // Every test process descends from this one, through this
            // program as nextest's runner: a test that starts `wrangler dev`
            // finds Supercov's first, as under `cargo test`.
            .envs(
                rust_workers
                    .iter()
                    .flat_map(|workers| workers.environment()),
            );
        command
    };

    // The identity contract is pinned to the nextest versions it was verified
    // against; anything else fails before a test runs.
    let version = cargo(&nextest_version_arguments(invocation))
        .output()
        .map_err(|error| RustTestRunnerError::Launch(error.to_string()))?;
    if !version.status.success() {
        return Err(RustTestRunnerError::CargoFailed(combined_output(
            &version.stdout,
            &version.stderr,
        )));
    }
    let version = parse_nextest_version_output(&version.stdout)
        .map_err(|error| RustTestRunnerError::Context(error.to_string()))?;
    validate_nextest_version(&version).map_err(|error| {
        RustTestRunnerError::UnsupportedCommand(format!("cargo-nextest {version}: {error}"))
    })?;
    if crate::rust_runner_attempt::newer_than_checked(&version) {
        writeln!(
            diagnostics,
            "[supercov] cargo-nextest {version} is newer than the releases Supercov is tested with; it runs as they do, and a test it does not name to its runner stops the run"
        )
        .map_err(|error| RustTestRunnerError::Io(error.to_string()))?;
    }

    // The target runner configuration: one entry per target nextest may run
    // for, each wrapping whatever runner the user had configured.
    let plan =
        resolve_cargo_runner_plan(&project.workspace_root, &project.workspace_root, invocation)
            .map_err(|error| RustTestRunnerError::Context(error.to_string()))?;
    let runner_plan = RunnerPlan {
        token: project.probes.token.clone(),
        targets: plan
            .targets
            .iter()
            .map(|target| RunnerTarget {
                target: target.target.clone(),
                underlying: target
                    .underlying_runner
                    .as_ref()
                    .map(|runner| runner.resolve(&project.workspace_root)),
            })
            .collect(),
    };
    fs::write(root.join(PLAN_FILE), serde_json::to_vec(&runner_plan)?).map_err(io_error)?;
    let runner_configuration = runner_configuration_arguments(&program, root, &runner_plan)?;

    // The listing: every test nextest selected, per binary, before any runs.
    let listing = nextest_list_invocation(invocation)?;
    let mut list_arguments = listing.arguments;
    list_arguments.extend(runner_configuration.iter().cloned());
    if !listing.runner_arguments.is_empty() {
        list_arguments.push("--".into());
        list_arguments.extend(listing.runner_arguments);
    }
    let listed = cargo(&list_arguments)
        .output()
        .map_err(|error| RustTestRunnerError::Launch(error.to_string()))?;
    if !listed.status.success() {
        return Err(RustTestRunnerError::CargoFailed(combined_output(
            &listed.stdout,
            &listed.stderr,
        )));
    }
    let catalog =
        TestListSummary::parse_json(String::from_utf8_lossy(&listed.stdout)).map_err(|error| {
            RustTestRunnerError::CargoJson(format!("invalid nextest test listing: {error}"))
        })?;
    let metadata =
        cargo(&cargo_metadata_arguments(invocation).map_err(RustTestRunnerError::Context)?)
            .output()
            .map_err(|error| RustTestRunnerError::Launch(error.to_string()))?;
    if !metadata.status.success() {
        return Err(RustTestRunnerError::CargoFailed(combined_output(
            &metadata.stdout,
            &metadata.stderr,
        )));
    }
    let metadata: CargoMetadataOutput =
        serde_json::from_slice(&metadata.stdout).map_err(|error| {
            RustTestRunnerError::CargoJson(format!("invalid Cargo metadata: {error}"))
        })?;
    let artifacts = nextest_artifacts(
        &project.workspace_root,
        &project.target_directory,
        &catalog,
        &metadata,
    )?;

    // The run itself, streaming nextest's own output.
    let command = invocation.command_position();
    let mut run_arguments = invocation.arguments[..command + 2].to_vec();
    run_arguments.extend(runner_configuration.iter().cloned());
    run_arguments.extend(invocation.arguments[command + 2..].iter().cloned());
    if !invocation.runner_arguments.is_empty() {
        run_arguments.push("--".into());
        run_arguments.extend(invocation.runner_arguments.iter().cloned());
    }
    let status = crate::child_signal_guard::status(&mut cargo(&run_arguments))
        .map_err(|error| RustTestRunnerError::Launch(error.to_string()))?;
    let exit_code = status.code().unwrap_or(1);

    // Every attempt the runner recorded, checked against the listing.
    let mut selected = BTreeSet::new();
    for (binary_id, suite) in &catalog.rust_suites {
        for (name, case) in &suite.test_cases {
            if case.filter_match.is_match() {
                selected.insert((binary_id.to_string(), name.to_string()));
            }
        }
    }
    let mut records = read_attempts(&root.join(ATTEMPTS_DIRECTORY))?;
    records.sort_by(|left, right| {
        (&left.binary_id, &left.test_name, left.attempt).cmp(&(
            &right.binary_id,
            &right.test_name,
            right.attempt,
        ))
    });
    let mut seen = BTreeSet::new();
    let mut ran = BTreeSet::new();
    let mut built_crates = BTreeSet::new();
    let mut remote_workers = false;
    let mut started_workers = Vec::new();
    let mut results = Vec::with_capacity(records.len());
    for record in records {
        let key = (record.binary_id.clone(), record.test_name.clone());
        if !selected.contains(&key) {
            return Err(RustTestRunnerError::Context(format!(
                "nextest ran a test its listing did not select: {} {}",
                record.binary_id, record.test_name
            )));
        }
        if !seen.insert((key.clone(), record.attempt)) {
            return Err(RustTestRunnerError::Context(format!(
                "nextest attempt {} of {} {} was recorded twice",
                record.attempt, record.binary_id, record.test_name
            )));
        }
        ran.insert(key);
        let artifact = artifacts.get(&record.binary_id).ok_or_else(|| {
            RustTestRunnerError::Context(format!(
                "nextest ran a binary its listing lacks: {}",
                record.binary_id
            ))
        })?;
        let test_id = format!("{}::{}", artifact.source, record.test_name);
        let passed = record.exit_code == 0;
        if !passed {
            writeln!(
                diagnostics,
                "[supercov] Rust test failed on attempt {}: {test_id}",
                record.attempt
            )
            .map_err(io_error)?;
        }
        let attempt_id = format!("{run_id}:{}:{}", record.runner_attempt_id, record.attempt);
        if rust_workers.is_some() {
            remote_workers |= crate::rust_worker::started_remote(&record.evidence);
            started_workers.extend(crate::rust_worker::started_worker_configs(&record.evidence));
            built_crates.extend(
                crate::rust_worker::materialize_worker_windows(
                    &record.evidence,
                    &project.probes.token,
                )
                .map_err(RustTestRunnerError::Io)?,
            );
        }
        let evidence = snapshot(
            &project.manifest,
            &project.probes,
            &record.evidence,
            &attempt_id,
        )?;
        results.push(RawTestResult {
            span: None,
            test_id: Some(test_id.clone()),
            scope: Some(ExecutionScope {
                version: 1,
                run_id: run_id.into(),
                worker_id: format!("nextest-{}", record.binary_id),
                test_id: test_id.clone(),
                test_key: test_id.clone(),
                retry: record.attempt - 1,
                attempt_id: attempt_id.clone(),
            }),
            test: test_id,
            test_file: Some(artifact.source.clone()),
            title: Some(record.test_name.clone()),
            retry: Some(record.attempt - 1),
            status: Some(if passed { "passed" } else { "failed" }.into()),
            expected_status: Some("passed".into()),
            flaky: passed && record.attempt > 1,
            provenance: TestProvenance {
                runner: "nextest".into(),
                kind: artifact.kind.clone(),
                project: Some(artifact.binary_name.clone()),
                source: "supercov-owned-process-per-test".into(),
                tags: Vec::new(),
            },
            role: "test".into(),
            attribution: crate::coverage_report::ATTRIBUTION_EXACT.into(),
            phases: evidence.phases,
            runtime: vec![evidence.snapshot],
            browser: Vec::new(),
            server: Vec::new(),
        });
    }
    // nextest stops early on a failure when asked to; then a listed test that
    // never ran is expected, and the exit code already says so.
    if exit_code == 0 {
        let missing = selected
            .difference(&ran)
            .map(|(binary_id, name)| format!("{binary_id} {name}"))
            .collect::<Vec<_>>();
        if !missing.is_empty() {
            return Err(RustTestRunnerError::Context(format!(
                "nextest listed {} test(s) it never ran through the runner: {}",
                missing.len(),
                missing.join(", ")
            )));
        }
    }
    Ok(NextestOutcome {
        results,
        built_crates,
        remote_workers,
        started_workers,
        artifact_files: artifacts
            .into_values()
            .map(|artifact| artifact.executable)
            .collect(),
        exit_code,
    })
}

/// `--config target.<triple>.runner=[...]` for every target of the plan: this
/// program in runner mode, told where to record and which target it serves.
fn runner_configuration_arguments(
    program: &Path,
    root: &Path,
    plan: &RunnerPlan,
) -> Result<Vec<String>, RustTestRunnerError> {
    let json = |value: &str| serde_json::to_string(value).map_err(RustTestRunnerError::Json);
    let program = json(program.to_str().ok_or_else(|| {
        RustTestRunnerError::Context("the Supercov executable path is not UTF-8".into())
    })?)?;
    let root = json(root.to_str().ok_or_else(|| {
        RustTestRunnerError::Context("the nextest evidence root is not UTF-8".into())
    })?)?;
    let marker = json(RUNNER_MODE_ARGUMENT)?;
    let mut arguments = Vec::with_capacity(plan.targets.len() * 2);
    let mut seen = BTreeSet::new();
    for target in &plan.targets {
        if !seen.insert(target.target.as_str()) {
            return Err(RustTestRunnerError::Context(format!(
                "the Cargo runner plan names target {} twice",
                target.target
            )));
        }
        let triple = json(&target.target)?;
        arguments.push("--config".into());
        arguments.push(format!(
            "target.{triple}.runner=[{program},{marker},{root},{triple}]"
        ));
    }
    if arguments.is_empty() {
        return Err(RustTestRunnerError::Context(
            "the Cargo runner plan selected no targets".into(),
        ));
    }
    Ok(arguments)
}

/// Whether a test binary nextest calls `nextest_kind` is built from a Cargo
/// target of `cargo_kind`. nextest names a library's test binary `lib`
/// whatever crate types the library declares: a `cdylib`-only crate (a
/// Worker, a wasm library) is `lib` to nextest and `cdylib` to Cargo.
fn built_as(nextest_kind: &str, cargo_kind: &str) -> bool {
    nextest_kind == cargo_kind
        || nextest_kind == "lib"
            && matches!(
                cargo_kind,
                "lib" | "rlib" | "dylib" | "cdylib" | "staticlib"
            )
}

/// Join nextest's listing to Cargo's metadata: each binary to the target it
/// was built from, and so to the source file its tests are attributed to.
fn nextest_artifacts(
    workspace_root: &Path,
    target_directory: &Path,
    catalog: &TestListSummary,
    metadata: &CargoMetadataOutput,
) -> Result<BTreeMap<String, NextestArtifact>, RustTestRunnerError> {
    let canonical_target = fs::canonicalize(target_directory).map_err(io_error)?;
    let packages = metadata
        .packages
        .iter()
        .map(|package| (package.id.as_str(), package))
        .collect::<BTreeMap<_, _>>();
    let mut artifacts = BTreeMap::new();
    for (binary_id, suite) in &catalog.rust_suites {
        let binary_id = binary_id.to_string();
        if suite.binary.binary_id.to_string() != binary_id {
            return Err(RustTestRunnerError::CargoJson(format!(
                "nextest suite key disagrees with binary identity {binary_id}"
            )));
        }
        let package = packages
            .get(suite.binary.package_id.as_str())
            .ok_or_else(|| {
                RustTestRunnerError::CargoJson(format!(
                    "nextest binary {binary_id} names an unknown Cargo package {}",
                    suite.binary.package_id
                ))
            })?;
        if package.name != suite.package_name {
            return Err(RustTestRunnerError::CargoJson(format!(
                "nextest binary {binary_id} package name disagrees with Cargo metadata"
            )));
        }
        let mut targets = package.targets.iter().filter(|target| {
            target.name == suite.binary.binary_name
                && target
                    .kind
                    .iter()
                    .any(|kind| built_as(suite.binary.kind.as_str(), kind))
        });
        let target = targets.next().ok_or_else(|| {
            RustTestRunnerError::CargoJson(format!(
                "nextest binary {binary_id} has no exact Cargo metadata target"
            ))
        })?;
        if targets.next().is_some() {
            return Err(RustTestRunnerError::CargoJson(format!(
                "nextest binary {binary_id} ambiguously matches Cargo metadata targets"
            )));
        }
        let executable =
            fs::canonicalize(suite.binary.binary_path.as_std_path()).map_err(io_error)?;
        if !executable.starts_with(&canonical_target)
            || !fs::metadata(&executable).is_ok_and(|metadata| metadata.is_file())
        {
            return Err(RustTestRunnerError::UnsafeArtifact(
                executable.display().to_string(),
            ));
        }
        let source = fs::canonicalize(&target.src_path).map_err(io_error)?;
        artifacts.insert(
            binary_id,
            NextestArtifact {
                executable,
                binary_name: suite.binary.binary_name.clone(),
                kind: if suite.binary.kind.as_str() == "test" {
                    "integration".into()
                } else {
                    "unit".into()
                },
                source: relative_source(workspace_root, &source)?,
            },
        );
    }
    if artifacts.is_empty() {
        return Err(RustTestRunnerError::CargoJson(
            "nextest listed no test binaries".into(),
        ));
    }
    Ok(artifacts)
}

fn read_attempts(directory: &Path) -> Result<Vec<AttemptRecord>, RustTestRunnerError> {
    let mut records = Vec::new();
    for entry in fs::read_dir(directory).map_err(io_error)? {
        let path = entry.map_err(io_error)?.path();
        if path
            .extension()
            .is_some_and(|extension| extension == "json")
        {
            records.push(serde_json::from_slice(&fs::read(&path).map_err(io_error)?)?);
        }
    }
    Ok(records)
}

fn combined_output(stdout: &[u8], stderr: &[u8]) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(stderr),
        String::from_utf8_lossy(stdout)
    )
    .trim()
    .to_owned()
}

// -------------------------------------------------------------- the runner

/// This program as nextest's target runner: `__nextest-runner <root> <target>
/// <binary> <arguments...>`. A list pass runs the binary untouched; an attempt
/// runs it under an evidence directory of its own and records the identity
/// nextest supplied.
pub fn nextest_runner(arguments: Vec<OsString>) -> i32 {
    match nextest_runner_inner(arguments) {
        Ok(code) => code,
        Err(error) => {
            eprintln!("[supercov] {error}");
            2
        }
    }
}

fn nextest_runner_inner(arguments: Vec<OsString>) -> Result<i32, RustTestRunnerError> {
    let mut arguments = arguments.into_iter();
    let (Some(root), Some(target), Some(binary)) =
        (arguments.next(), arguments.next(), arguments.next())
    else {
        return Err(RustTestRunnerError::Context(
            "the nextest runner needs an evidence root, a target and a binary".into(),
        ));
    };
    let root = PathBuf::from(root);
    let target = target.into_string().map_err(|_| {
        RustTestRunnerError::Context("the nextest runner target is not UTF-8".into())
    })?;
    let rest = arguments.collect::<Vec<_>>();
    let plan: RunnerPlan =
        serde_json::from_slice(&fs::read(root.join(PLAN_FILE)).map_err(io_error)?)?;
    let planned = plan
        .targets
        .iter()
        .find(|candidate| candidate.target == target)
        .ok_or_else(|| {
            RustTestRunnerError::Context(format!(
                "the nextest runner was invoked for an unplanned target: {target}"
            ))
        })?;
    let mut command = match &planned.underlying {
        Some(underlying) => {
            let mut command = Command::new(&underlying.program);
            command.args(&underlying.arguments).arg(&binary);
            command
        }
        None => Command::new(&binary),
    };
    command.args(&rest);
    let identity = classify_rust_runner_environment()
        .map_err(|error| RustTestRunnerError::Context(error.to_string()))?;
    let attempt = match identity {
        RustRunnerInvocationIdentity::NextestList(_) => {
            let status = command
                .status()
                .map_err(|error| RustTestRunnerError::Launch(error.to_string()))?;
            return Ok(exit_code(&status));
        }
        RustRunnerInvocationIdentity::CargoSingleAttempt => {
            return Err(RustTestRunnerError::Context(
                "the nextest runner was invoked without nextest's identity".into(),
            ));
        }
        RustRunnerInvocationIdentity::NextestAttempt(attempt) => attempt,
    };
    let attempts = root.join(ATTEMPTS_DIRECTORY);
    let sequence = claim_sequence(&attempts)?;
    let evidence = attempts.join(format!("{sequence:08}"));
    fs::create_dir_all(&evidence).map_err(io_error)?;
    let status = command
        .envs(instrumented_stack_environment())
        .envs(crate::rust_runtime::evidence_environment(
            &plan.token,
            &evidence,
        ))
        .status()
        .map_err(|error| RustTestRunnerError::Launch(error.to_string()))?;
    let code = exit_code(&status);
    let record = AttemptRecord {
        binary_id: attempt.invocation.binary_id,
        test_name: attempt.test_name,
        attempt: attempt.retry + 1,
        total_attempts: attempt.total_attempts,
        runner_attempt_id: attempt.runner_attempt_id,
        exit_code: code,
        evidence,
    };
    fs::write(
        attempts.join(format!("{sequence:08}.json")),
        serde_json::to_vec(&record)?,
    )
    .map_err(io_error)?;
    Ok(code)
}

/// Attempts run concurrently; each claims the next free sequence number by
/// creating its marker exclusively.
fn claim_sequence(directory: &Path) -> Result<usize, RustTestRunnerError> {
    fs::create_dir_all(directory).map_err(io_error)?;
    for candidate in 0..1_000_000_usize {
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(directory.join(format!("{candidate:08}.claim")))
        {
            Ok(_) => return Ok(candidate),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(io_error(error)),
        }
    }
    Err(RustTestRunnerError::Io(
        "too many nextest attempt claims".into(),
    ))
}

/// The child's exit code as nextest should see it: the code itself, or the
/// shell convention of 128 plus the signal that ended it.
fn exit_code(status: &std::process::ExitStatus) -> i32 {
    if let Some(code) = status.code() {
        return code;
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(signal) = status.signal() {
            return 128 + signal;
        }
    }
    1
}

/// What `cargo metadata` says of the workspace members: enough to find the
/// source file behind each test binary nextest lists.
#[derive(Debug, Deserialize)]
pub(crate) struct CargoMetadataOutput {
    pub(crate) packages: Vec<CargoMetadataPackage>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct CargoMetadataPackage {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) targets: Vec<CargoMetadataTarget>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct CargoMetadataTarget {
    pub(crate) name: String,
    pub(crate) kind: Vec<String>,
    pub(crate) src_path: PathBuf,
}

/// The `cargo metadata` command for the workspace a test command selects:
/// its toolchain override and the options that choose the manifest, the
/// configuration and how Cargo may resolve.
pub(crate) fn cargo_metadata_arguments(
    invocation: &crate::rust_test_runner::CargoTestInvocation,
) -> Result<Vec<String>, String> {
    let command = invocation.command_position();
    let mut arguments = invocation.arguments[..command]
        .iter()
        .filter(|argument| argument.starts_with('+'))
        .cloned()
        .collect::<Vec<_>>();
    arguments.extend([
        "metadata".into(),
        "--format-version=1".into(),
        "--no-deps".into(),
    ]);
    let command_width = match invocation.kind {
        crate::rust_test_runner::RustCargoCommandKind::CargoTest => 1,
        crate::rust_test_runner::RustCargoCommandKind::NextestRun => 2,
    };
    let mut index = command + command_width;
    while index < invocation.arguments.len() {
        let argument = &invocation.arguments[index];
        let name = argument
            .split_once('=')
            .map_or(argument.as_str(), |(name, _)| name);
        let takes_value = match name {
            "--manifest-path" | "--config" | "-Z" => Some(!argument.contains('=')),
            "--frozen" | "--locked" | "--offline" | "--ignore-rust-version" => Some(false),
            _ => None,
        };
        if let Some(takes_value) = takes_value {
            arguments.push(argument.clone());
            if takes_value {
                index += 1;
                let value = invocation
                    .arguments
                    .get(index)
                    .ok_or_else(|| format!("Cargo option {argument} has no value"))?;
                arguments.push(value.clone());
            }
        }
        index += 1;
    }
    Ok(arguments)
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;

    /// A library's test binary is `lib` to nextest whatever crate type Cargo
    /// builds it as; any other kind must match by name.
    #[test]
    fn nextest_kinds_match_cargo_target_kinds() {
        for (nextest, cargo, built) in [
            ("lib", "lib", true),
            ("lib", "cdylib", true),
            ("lib", "staticlib", true),
            ("lib", "bin", false),
            ("test", "test", true),
            ("test", "lib", false),
            ("bin", "lib", false),
        ] {
            assert_eq!(built_as(nextest, cargo), built, "{nextest} {cargo}");
        }
    }

    /// nextest's listing joins Cargo's metadata only where every binary
    /// names its own key, a known package by its own name, exactly one
    /// target and an executable inside the target directory.
    #[test]
    fn nextest_binaries_join_cargo_targets_or_are_refused_by_name() {
        let root = std::env::temp_dir().join(format!(
            "supercov-nextest-join-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(root.join("target/debug/deps")).unwrap();
        fs::create_dir_all(root.join("src")).unwrap();
        fs::create_dir_all(root.join("tests")).unwrap();
        fs::write(root.join("src/lib.rs"), "").unwrap();
        fs::write(root.join("tests/it.rs"), "").unwrap();
        fs::write(root.join("target/debug/deps/tiny-1"), "").unwrap();
        fs::write(root.join("target/debug/deps/it-2"), "").unwrap();
        fs::write(root.join("outside"), "").unwrap();
        let root = fs::canonicalize(&root).unwrap();
        let suite = |key: &str, name: &str, kind: &str, binary: &str| {
            json!({
                "package-name": "tiny",
                "binary-id": key,
                "binary-name": name,
                "package-id": "tiny-id",
                "kind": kind,
                "binary-path": root.join(binary),
                "build-platform": "target",
                "cwd": root,
                "status": "listed",
                "testcases": {}
            })
        };
        let listing = |suites: Value| {
            let text = json!({
                "rust-build-meta": {
                    "target-directory": root.join("target"),
                    "build-directory": root.join("target"),
                    "base-output-directories": ["debug"],
                    "non-test-binaries": {},
                    "build-script-out-dirs": {},
                    "build-script-info": {},
                    "linked-paths": [],
                    "platforms": {
                        "host": {
                            "platform": {"triple": "aarch64-apple-darwin", "target-features": "unknown"},
                            "libdir": {"status": "unavailable", "reason": "test"}
                        },
                        "targets": []
                    },
                    "target-platforms": [{"triple": "aarch64-apple-darwin", "target-features": "unknown"}],
                    "target-platform": null
                },
                "test-count": 0,
                "rust-suites": suites
            })
            .to_string();
            TestListSummary::parse_json(text).unwrap()
        };
        let metadata = |targets: Value, name: &str| -> CargoMetadataOutput {
            serde_json::from_value(json!({
                "packages": [{"id": "tiny-id", "name": name, "targets": targets}]
            }))
            .unwrap()
        };
        let targets = json!([
            {"name": "tiny", "kind": ["cdylib"], "src_path": root.join("src/lib.rs")},
            {"name": "it", "kind": ["test"], "src_path": root.join("tests/it.rs")}
        ]);
        let join = |catalog: &TestListSummary, metadata: &CargoMetadataOutput| {
            nextest_artifacts(&root, &root.join("target"), catalog, metadata)
        };
        let both = listing(json!({
            "tiny": suite("tiny", "tiny", "lib", "target/debug/deps/tiny-1"),
            "tiny::it": suite("tiny::it", "it", "test", "target/debug/deps/it-2")
        }));
        let joined = join(&both, &metadata(targets.clone(), "tiny")).unwrap();
        assert_eq!(joined["tiny"].kind, "unit");
        assert_eq!(joined["tiny"].source, "src/lib.rs");
        assert_eq!(joined["tiny::it"].kind, "integration");
        let refused = |catalog: TestListSummary, metadata: CargoMetadataOutput| match join(
            &catalog, &metadata,
        ) {
            Err(error) => error.to_string(),
            Ok(_) => "joined".into(),
        };
        let one = |suite: Value| listing(json!({ "tiny": suite }));
        let lib = suite("tiny", "tiny", "lib", "target/debug/deps/tiny-1");
        let mut unknown = lib.clone();
        unknown["package-id"] = "other-id".into();
        for (label, error, expected) in [
            (
                "key",
                refused(
                    one(suite("other", "tiny", "lib", "target/debug/deps/tiny-1")),
                    metadata(targets.clone(), "tiny"),
                ),
                "nextest suite key disagrees with binary identity tiny",
            ),
            (
                "package",
                refused(one(unknown), metadata(targets.clone(), "tiny")),
                "names an unknown Cargo package other-id",
            ),
            (
                "name",
                refused(one(lib.clone()), metadata(targets.clone(), "renamed")),
                "package name disagrees",
            ),
            (
                "target",
                refused(
                    one(suite("tiny", "gone", "lib", "target/debug/deps/tiny-1")),
                    metadata(targets.clone(), "tiny"),
                ),
                "has no exact Cargo metadata target",
            ),
            (
                "ambiguous",
                refused(
                    one(lib.clone()),
                    metadata(
                        json!([
                            {"name": "tiny", "kind": ["lib"], "src_path": root.join("src/lib.rs")},
                            {"name": "tiny", "kind": ["rlib"], "src_path": root.join("src/lib.rs")}
                        ]),
                        "tiny",
                    ),
                ),
                "ambiguously matches",
            ),
            (
                "outside",
                refused(
                    one(suite("tiny", "tiny", "lib", "outside")),
                    metadata(targets.clone(), "tiny"),
                ),
                "outside",
            ),
            (
                "empty",
                refused(listing(json!({})), metadata(targets.clone(), "tiny")),
                "nextest listed no test binaries",
            ),
        ] {
            assert!(error.contains(expected), "{label}: {error}");
        }
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn runner_configuration_names_every_target_once() {
        let plan = RunnerPlan {
            token: "0123456789abcdef".into(),
            targets: vec![
                RunnerTarget {
                    target: "aarch64-apple-darwin".into(),
                    underlying: None,
                },
                RunnerTarget {
                    target: "x86_64-unknown-linux-gnu".into(),
                    underlying: Some(RustCargoResolvedRunner {
                        program: "/opt/qemu".into(),
                        arguments: vec!["--fast".into()],
                    }),
                },
            ],
        };
        let arguments = runner_configuration_arguments(
            Path::new("/opt/super cov/supercov"),
            Path::new("/work/.supercov/rust-evidence/run/nextest"),
            &plan,
        )
        .unwrap();
        assert_eq!(
            arguments,
            [
                "--config",
                "target.\"aarch64-apple-darwin\".runner=[\"/opt/super cov/supercov\",\"__nextest-runner\",\"/work/.supercov/rust-evidence/run/nextest\",\"aarch64-apple-darwin\"]",
                "--config",
                "target.\"x86_64-unknown-linux-gnu\".runner=[\"/opt/super cov/supercov\",\"__nextest-runner\",\"/work/.supercov/rust-evidence/run/nextest\",\"x86_64-unknown-linux-gnu\"]",
            ]
        );
        let duplicated = RunnerPlan {
            token: "0123456789abcdef".into(),
            targets: vec![
                RunnerTarget {
                    target: "aarch64-apple-darwin".into(),
                    underlying: None,
                },
                RunnerTarget {
                    target: "aarch64-apple-darwin".into(),
                    underlying: None,
                },
            ],
        };
        assert!(
            runner_configuration_arguments(Path::new("/s"), Path::new("/r"), &duplicated).is_err()
        );
    }

    #[test]
    fn attempt_records_round_trip() {
        let record = AttemptRecord {
            binary_id: "fixture::suite".into(),
            test_name: "passes_on_retry".into(),
            attempt: 2,
            total_attempts: 2,
            runner_attempt_id: "run:fixture::suite$passes_on_retry".into(),
            exit_code: 0,
            evidence: "/r/attempts/00000003".into(),
        };
        let decoded: AttemptRecord =
            serde_json::from_slice(&serde_json::to_vec(&record).unwrap()).unwrap();
        assert_eq!(decoded.attempt, 2);
        assert_eq!(decoded.test_name, "passes_on_retry");
        assert_eq!(decoded.evidence, PathBuf::from("/r/attempts/00000003"));
    }

    #[test]
    fn a_plan_with_no_targets_configures_no_runner() {
        let error = runner_configuration_arguments(
            Path::new("/s"),
            Path::new("/r"),
            &RunnerPlan {
                targets: Vec::new(),
                token: "0123456789abcdef".into(),
            },
        )
        .unwrap_err();
        assert!(error.to_string().contains("selected no targets"), "{error}");
    }

    #[cfg(unix)]
    #[test]
    fn a_child_ended_by_a_signal_exits_as_a_shell_reports_it() {
        use std::os::unix::process::ExitStatusExt;
        assert_eq!(exit_code(&std::process::ExitStatus::from_raw(3 << 8)), 3);
        // SIGKILL, as a test killed by the kernel's out-of-memory killer.
        assert_eq!(exit_code(&std::process::ExitStatus::from_raw(9)), 137);
        // Stopped by SIGSTOP: neither an exit code nor a terminating signal.
        assert_eq!(exit_code(&std::process::ExitStatus::from_raw(0x137f)), 1);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_claim_that_cannot_be_created_is_an_error_not_a_retry() {
        // Nothing can create a file in procfs, so the first claim fails with
        // an error other than an existing claim.
        let error = claim_sequence(Path::new("/proc/self")).unwrap_err();
        assert!(matches!(error, RustTestRunnerError::Io(_)), "{error}");
    }

    #[test]
    fn metadata_keeps_the_options_that_choose_the_workspace() {
        let invocation = CargoTestInvocation {
            program: "cargo".into(),
            kind: crate::rust_test_runner::RustCargoCommandKind::NextestRun,
            arguments: [
                "+nightly",
                "nextest",
                "run",
                "--manifest-path",
                "member/Cargo.toml",
                "--config=net.offline=true",
                "--locked",
                "--retries",
                "2",
                "-Z",
                "unstable-options",
            ]
            .map(String::from)
            .to_vec(),
            runner_arguments: Vec::new(),
        };
        assert_eq!(
            cargo_metadata_arguments(&invocation).unwrap(),
            [
                "+nightly",
                "metadata",
                "--format-version=1",
                "--no-deps",
                "--manifest-path",
                "member/Cargo.toml",
                "--config=net.offline=true",
                "--locked",
                "-Z",
                "unstable-options",
            ]
        );
        let missing = CargoTestInvocation {
            arguments: ["nextest", "run", "--config"].map(String::from).to_vec(),
            ..invocation
        };
        assert_eq!(
            cargo_metadata_arguments(&missing).unwrap_err(),
            "Cargo option --config has no value"
        );
    }

    /// A one-crate workspace and a script standing in for Cargo that answers
    /// `nextest --version`, `nextest list`, `metadata` and `nextest run` from
    /// files the test writes: `<step>.out` is printed, `<step>.status` is the
    /// exit status, and `run.sh` is what `nextest run` does in place of
    /// running tests through the runner.
    #[cfg(unix)]
    struct FakeNextest {
        root: PathBuf,
        project: PreparedRustProject,
        cargo: PathBuf,
        evidence: PathBuf,
    }

    #[cfg(unix)]
    impl FakeNextest {
        fn new() -> Self {
            use std::os::unix::fs::PermissionsExt;
            let nonce = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let root = std::env::temp_dir().join(format!(
                "supercov-fake-nextest-{}-{nonce}",
                std::process::id()
            ));
            fs::create_dir_all(root.join("src")).unwrap();
            fs::write(
                root.join("Cargo.toml"),
                "[package]\nname='fixture'\nversion='0.0.0'\nedition='2024'\n",
            )
            .unwrap();
            fs::write(
                root.join("src/lib.rs"),
                "pub fn one() -> i32 { 1 }\n#[cfg(test)]\nmod tests {\n    #[test] fn alpha() { assert_eq!(super::one(), 1); }\n    #[test] fn beta() {}\n}\n",
            )
            .unwrap();
            let root = fs::canonicalize(&root).unwrap();
            let project = crate::rust_project::prepare_rust_project(&root, None).unwrap();
            let binary = project.target_directory.join("debug/deps/fixture-0123");
            fs::create_dir_all(binary.parent().unwrap()).unwrap();
            fs::write(&binary, "").unwrap();
            let fake = root.join("fake");
            fs::create_dir_all(&fake).unwrap();
            let cargo = fake.join("cargo");
            fs::write(
                &cargo,
                format!(
                    r#"#!/bin/sh
fake='{fake}'
printf '%s\n' "$*" >> "$fake/calls"
case "$*" in
  *"nextest --version"*) step=version ;;
  *"nextest list"*) step=list ;;
  *"nextest run"*) step=run ;;
  *metadata*) step=metadata ;;
esac
[ -f "$fake/$step.out" ] && cat "$fake/$step.out"
[ -f "$fake/$step.sh" ] && . "$fake/$step.sh"
exit "$(cat "$fake/$step.status" 2>/dev/null || echo 0)"
"#,
                    fake = fake.display()
                ),
            )
            .unwrap();
            fs::set_permissions(&cargo, fs::Permissions::from_mode(0o755)).unwrap();
            let fixture = Self {
                evidence: root.join(".supercov/nextest"),
                root,
                project,
                cargo,
            };
            fixture.answer(
                "version",
                "cargo-nextest 0.9.140\nrelease: 0.9.140\nhost: x86_64-unknown-linux-gnu\n",
            );
            fixture.list(&binary);
            fixture.answer(
                "metadata",
                &serde_json::json!({"packages": [{
                    "id": "path+file:///fixture#0.0.0",
                    "name": "fixture",
                    "targets": [
                        // A binary of the same name is not the library's
                        // test binary.
                        {"name": "fixture", "kind": ["bin"], "src_path": fixture.root.join("src/lib.rs")},
                        {"name": "fixture", "kind": ["lib"], "src_path": fixture.root.join("src/lib.rs")},
                    ],
                }]})
                .to_string(),
            );
            fixture
        }

        fn answer(&self, step: &str, output: &str) {
            fs::write(self.root.join("fake").join(format!("{step}.out")), output).unwrap();
        }

        fn fail(&self, step: &str, status: i32) {
            fs::write(
                self.root.join("fake").join(format!("{step}.status")),
                status.to_string(),
            )
            .unwrap();
        }

        /// The listing: `alpha` is selected and `beta` is filtered out.
        fn list(&self, binary: &Path) {
            self.answer(
                "list",
                &serde_json::json!({
                    "rust-build-meta": {
                        "target-directory": self.project.target_directory,
                        "base-output-directories": [],
                        "non-test-binaries": {},
                        "linked-paths": [],
                    },
                    "test-count": 2,
                    "rust-suites": {"fixture": {
                        "package-name": "fixture",
                        "binary-id": "fixture",
                        "binary-name": "fixture",
                        "package-id": "path+file:///fixture#0.0.0",
                        "kind": "lib",
                        "binary-path": binary,
                        "build-platform": "target",
                        "cwd": self.root,
                        "testcases": {
                            "tests::alpha": {"kind": "test", "ignored": false, "filter-match": {"status": "matches"}},
                            "tests::beta": {"kind": "test", "ignored": false, "filter-match": {"status": "mismatch", "reason": "string"}},
                        },
                    }},
                })
                .to_string(),
            );
        }

        /// `nextest run` records these attempts, as the runner would, and
        /// exits with `status`.
        fn runs(&self, attempts: &[(&str, usize, i32)], status: i32) {
            let mut script = String::new();
            for (index, (test, attempt, code)) in attempts.iter().enumerate() {
                let record = AttemptRecord {
                    binary_id: "fixture".into(),
                    test_name: (*test).into(),
                    attempt: *attempt,
                    total_attempts: 2,
                    runner_attempt_id: format!("fixture${test}"),
                    exit_code: *code,
                    evidence: self.evidence.join(format!("attempts/{index:08}")),
                };
                script.push_str(&format!(
                    "printf '%s' '{}' > '{}'\n",
                    serde_json::to_string(&record).unwrap(),
                    self.evidence
                        .join(format!("attempts/{index:08}.json"))
                        .display()
                ));
            }
            fs::write(self.root.join("fake/run.sh"), script).unwrap();
            self.fail("run", status);
        }

        fn calls(&self) -> String {
            fs::read_to_string(self.root.join("fake/calls")).unwrap_or_default()
        }

        fn run(&self, arguments: &[&str]) -> (Result<NextestOutcome, RustTestRunnerError>, String) {
            let mut command = vec![self.cargo.display().to_string()];
            command.extend(arguments.iter().map(|argument| (*argument).to_owned()));
            let invocation =
                crate::rust_test_runner::cargo_invocation(&self.root, &command).unwrap();
            let mut diagnostics = Vec::new();
            let outcome = run_nextest(
                &self.project,
                &invocation,
                &self.evidence,
                "run",
                None,
                &mut diagnostics,
            );
            (outcome, String::from_utf8(diagnostics).unwrap())
        }
    }

    #[cfg(unix)]
    impl Drop for FakeNextest {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.root).ok();
        }
    }

    #[cfg(unix)]
    #[test]
    fn nextest_attempts_are_read_back_against_its_listing() {
        let fake = FakeNextest::new();
        fake.runs(&[("tests::alpha", 1, 101), ("tests::alpha", 2, 0)], 0);
        let (outcome, diagnostics) = fake.run(&["nextest", "run", "--", "--exact"]);
        let outcome = outcome.unwrap();
        assert_eq!(outcome.exit_code, 0);
        let results = outcome
            .results
            .iter()
            .map(|result| (result.test.as_str(), result.status.as_deref(), result.flaky))
            .collect::<Vec<_>>();
        assert_eq!(
            results,
            [
                ("src/lib.rs::tests::alpha", Some("failed"), false),
                ("src/lib.rs::tests::alpha", Some("passed"), true),
            ]
        );
        assert!(
            diagnostics.contains("Rust test failed on attempt 1: src/lib.rs::tests::alpha"),
            "{diagnostics}"
        );
        // The arguments after `--` reach the binaries in the listing and in
        // the run alike.
        let calls = fake.calls();
        let list = calls
            .lines()
            .find(|line| line.contains("nextest list"))
            .unwrap();
        let run = calls
            .lines()
            .find(|line| line.contains("nextest run"))
            .unwrap();
        assert!(list.ends_with("-- --exact"), "{list}");
        assert!(run.ends_with("-- --exact"), "{run}");
    }

    #[cfg(unix)]
    #[test]
    fn a_selected_test_nextest_never_ran_fails_a_passing_run_only() {
        let fake = FakeNextest::new();
        fake.runs(&[], 0);
        let error = fake.run(&["nextest", "run"]).0.err().unwrap();
        assert!(
            error.to_string().contains(
                "nextest listed 1 test(s) it never ran through the runner: fixture tests::alpha"
            ),
            "{error}"
        );
        // A run that stopped early on a failure leaves listed tests unrun.
        fake.runs(&[], 100);
        let outcome = fake.run(&["nextest", "run"]).0.unwrap();
        assert_eq!(outcome.exit_code, 100);
        assert!(outcome.results.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn an_attempt_the_listing_cannot_account_for_fails_the_run() {
        let fake = FakeNextest::new();
        fake.runs(&[("tests::beta", 1, 0)], 0);
        let error = fake.run(&["nextest", "run"]).0.err().unwrap();
        assert!(
            error
                .to_string()
                .contains("nextest ran a test its listing did not select: fixture tests::beta"),
            "{error}"
        );
        fake.runs(&[("tests::alpha", 1, 0), ("tests::alpha", 1, 0)], 0);
        let error = fake.run(&["nextest", "run"]).0.err().unwrap();
        assert!(
            error
                .to_string()
                .contains("nextest attempt 1 of fixture tests::alpha was recorded twice"),
            "{error}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_failing_nextest_step_stops_the_run_with_its_output() {
        for step in ["version", "list", "metadata"] {
            let fake = FakeNextest::new();
            fake.answer(step, &format!("{step} went wrong"));
            fake.fail(step, 101);
            let error = fake.run(&["nextest", "run"]).0.err().unwrap();
            assert!(
                matches!(&error, RustTestRunnerError::CargoFailed(output) if output == &format!("{step} went wrong")),
                "{step}: {error:?}"
            );
            assert!(!fake.calls().contains("nextest run"), "{step}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_nextest_newer_than_the_checked_releases_is_named() {
        let fake = FakeNextest::new();
        fake.answer(
            "version",
            "cargo-nextest 9.0.0\nrelease: 9.0.0\nhost: x86_64-unknown-linux-gnu\n",
        );
        fake.runs(&[("tests::alpha", 1, 0)], 0);
        let (outcome, diagnostics) = fake.run(&["nextest", "run"]);
        assert_eq!(outcome.unwrap().results.len(), 1);
        assert!(
            diagnostics
                .contains("cargo-nextest 9.0.0 is newer than the releases Supercov is tested with"),
            "{diagnostics}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_listed_binary_outside_a_file_is_refused() {
        let fake = FakeNextest::new();
        // A directory inside the target directory is no test binary.
        let directory = fake.project.target_directory.join("debug/deps/not-a-file");
        fs::create_dir_all(&directory).unwrap();
        fake.list(&directory);
        let error = fake.run(&["nextest", "run"]).0.err().unwrap();
        assert!(
            matches!(&error, RustTestRunnerError::UnsafeArtifact(path) if path.ends_with("not-a-file")),
            "{error:?}"
        );
    }
}
