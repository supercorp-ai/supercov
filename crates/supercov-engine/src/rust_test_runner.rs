//! Stable Cargo/libtest execution for the owned Rust frontend.
//!
//! Source preparation happens in an isolated workspace. Cargo builds each test
//! artifact once; Supercov then executes one exact libtest case per process so
//! run, worker, test, retry and phase attribution do not depend on thread-local
//! state inside the program under test.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Write,
    path::{Component, Path, PathBuf},
    process::{Command, Output},
    sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Instant,
};

use serde::Deserialize;
use supercov_contracts::{
    AttributionPrecision, ExecutionModel, FrontendAttribution, FrontendLimitation,
    FrontendLimitationScope, FrontendRunDeclaration, FrontendRunnerDeclaration,
    LANGUAGE_FRONTEND_PROTOCOL_VERSION, StructuralSource,
};

use crate::{
    coverage_analysis::McdcVector,
    coverage_report::{
        CoverageManifest, CoverageModelDeclaration, CoverageReportRequest, DecisionMeta,
        DecisionSnapshot, ExecutionScope, ExitCodeInput, PersistedCoverageModel, RawTestResult,
        RuntimeSnapshot, TestProvenance,
    },
    evidence_archive::EvidenceArchiveEntry,
    rust_project::PreparedRustProject,
    rust_runtime::{RustProbeObservation, read_rust_probe_directory},
    rust_test_context::preflight_rust_test_contexts,
};

#[derive(Debug, Clone, PartialEq)]
pub struct RustFrontendRun {
    pub declaration: FrontendRunDeclaration,
    pub request: CoverageReportRequest,
    pub exit_code: i32,
    pub artifacts: usize,
    pub artifact_files: Vec<PathBuf>,
    pub build_ms: f64,
    pub execution_ms: f64,
}

impl RustFrontendRun {
    pub fn archive_entries(&self) -> Result<Vec<EvidenceArchiveEntry>, serde_json::Error> {
        let model = PersistedCoverageModel::from_declaration(
            self.request
                .coverage_model
                .as_ref()
                .expect("Rust frontend always declares a coverage model"),
        )
        .expect("Rust coverage model is contract-valid");
        let mut entries = vec![
            EvidenceArchiveEntry {
                path: "coverage-model.json".into(),
                contents: serde_json::to_vec(&model)?,
            },
            EvidenceArchiveEntry {
                path: "frontend.json".into(),
                contents: serde_json::to_vec(&self.declaration)?,
            },
            EvidenceArchiveEntry {
                path: "manifest.json".into(),
                contents: serde_json::to_vec(&self.request.manifest)?,
            },
        ];
        for (index, result) in self.request.raw_results.iter().enumerate() {
            entries.push(EvidenceArchiveEntry {
                path: format!("results/{index:08}/mcdc.json"),
                contents: serde_json::to_vec(result)?,
            });
        }
        Ok(entries)
    }
}

#[derive(Debug)]
pub enum RustTestRunnerError {
    UnsupportedCommand(String),
    Launch(String),
    CargoFailed(String),
    CargoJson(String),
    UnsafeArtifact(String),
    ListFailed(String),
    Probe(String),
    Context(String),
    UnknownProbe(String),
    InvalidVector {
        id: String,
        expected: usize,
        actual: usize,
    },
    Json(serde_json::Error),
    Io(String),
}

impl std::fmt::Display for RustTestRunnerError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnsupportedCommand(reason) => formatter.write_str(reason),
            Self::Launch(reason) => {
                write!(formatter, "could not launch Rust test process: {reason}")
            }
            Self::CargoFailed(reason) => write!(formatter, "Cargo test build failed: {reason}"),
            Self::CargoJson(reason) => write!(formatter, "invalid Cargo JSON output: {reason}"),
            Self::UnsafeArtifact(path) => {
                write!(formatter, "Cargo emitted an unsafe test artifact: {path}")
            }
            Self::ListFailed(reason) => {
                write!(formatter, "could not enumerate Rust tests: {reason}")
            }
            Self::Probe(reason) => write!(formatter, "invalid Rust probe evidence: {reason}"),
            Self::Context(reason) => write!(formatter, "invalid Rust test context: {reason}"),
            Self::UnknownProbe(id) => write!(
                formatter,
                "Rust runtime emitted an unknown obligation: {id}"
            ),
            Self::InvalidVector {
                id,
                expected,
                actual,
            } => write!(
                formatter,
                "Rust decision {id} emitted vector width {actual}; expected {expected}"
            ),
            Self::Json(error) => write!(formatter, "could not encode Rust evidence: {error}"),
            Self::Io(reason) => formatter.write_str(reason),
        }
    }
}

impl std::error::Error for RustTestRunnerError {}

impl From<serde_json::Error> for RustTestRunnerError {
    fn from(value: serde_json::Error) -> Self {
        Self::Json(value)
    }
}

#[derive(Debug, Deserialize)]
struct CargoMessage {
    reason: String,
    #[serde(default)]
    target: Option<CargoArtifactTarget>,
    #[serde(default)]
    profile: Option<CargoArtifactProfile>,
    executable: Option<PathBuf>,
}

#[derive(Debug, Deserialize)]
struct CargoArtifactTarget {
    name: String,
    kind: Vec<String>,
    src_path: PathBuf,
}

#[derive(Debug, Deserialize)]
struct CargoArtifactProfile {
    test: bool,
}

#[derive(Debug, Clone)]
struct TestArtifact {
    executable: PathBuf,
    name: String,
    kind: String,
    source: String,
}

#[derive(Debug)]
struct ProcessTask {
    ordinal: usize,
    artifact_index: usize,
    test_index: usize,
    artifact: TestArtifact,
    test: String,
    context_id: u64,
    directory: PathBuf,
}

#[derive(Debug)]
struct ProcessOutcome {
    task: ProcessTask,
    output: Output,
}

fn shell_words(value: &str) -> Result<Vec<String>, RustTestRunnerError> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut quote = None;
    let mut escaped = false;
    for character in value.chars() {
        if escaped {
            current.push(character);
            escaped = false;
        } else if character == '\\' && quote != Some('\'') {
            escaped = true;
        } else if matches!(character, '\'' | '"') {
            if quote == Some(character) {
                quote = None;
            } else if quote.is_none() {
                quote = Some(character);
            } else {
                current.push(character);
            }
        } else if character.is_whitespace() && quote.is_none() {
            if !current.is_empty() {
                words.push(std::mem::take(&mut current));
            }
        } else {
            current.push(character);
        }
    }
    if escaped || quote.is_some() {
        return Err(RustTestRunnerError::UnsupportedCommand(
            "the expanded Cargo command contains an incomplete quote or escape".into(),
        ));
    }
    if !current.is_empty() {
        words.push(current);
    }
    Ok(words)
}

fn executable_name(value: &str) -> &str {
    Path::new(value)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(value)
        .trim_end_matches(".exe")
        .trim_end_matches(".cmd")
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CargoTestInvocation {
    pub program: String,
    pub arguments: Vec<String>,
    pub runner_arguments: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RustLibtestSelection {
    pub list_arguments: Vec<String>,
    pub run_arguments: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RustCargoExecutionSelection {
    pub run_libtests: bool,
    pub run_doctests: bool,
    pub doctest_arguments: Vec<String>,
}

pub(crate) fn cargo_invocation(
    root: &Path,
    command: &[String],
) -> Result<CargoTestInvocation, RustTestRunnerError> {
    // A process argv is already tokenized. Joining and shell-parsing a direct
    // Cargo command destroys quotes that are payload (not shell syntax), most
    // notably TOML strings passed to Cargo's --config. Only opaque wrapper or
    // package-script commands need textual expansion and shell tokenization.
    let words = if command.iter().any(|word| executable_name(word) == "cargo") {
        command.to_vec()
    } else {
        let expanded = crate::project_discovery::expanded_command(root, command);
        shell_words(&expanded)?
    };
    let cargo = words
        .iter()
        .position(|word| executable_name(word) == "cargo")
        .ok_or_else(|| RustTestRunnerError::UnsupportedCommand(
            "Rust was detected, but the expanded command does not expose a stable Cargo invocation".into(),
        ))?;
    let test = words[cargo + 1..]
        .iter()
        .position(|word| word == "test")
        .map(|position| cargo + 1 + position)
        .ok_or_else(|| {
            RustTestRunnerError::UnsupportedCommand(
                "the first owned Rust runner currently requires `cargo test`; nextest and cross remain detected but explicitly unsupported".into(),
            )
        })?;
    if words[cargo + 1..test]
        .iter()
        .any(|word| matches!(word.as_str(), "&&" | "||" | ";" | "|"))
    {
        return Err(RustTestRunnerError::UnsupportedCommand(
            "the Cargo invocation contains a shell boundary before `test`".into(),
        ));
    }
    let mut arguments = words[cargo + 1..=test].to_vec();
    let mut runner_arguments = Vec::new();
    let mut after_separator = false;
    for argument in &words[test + 1..] {
        if argument == "--" && !after_separator {
            after_separator = true;
            continue;
        }
        if matches!(argument.as_str(), "&&" | "||" | ";" | "|") {
            return Err(RustTestRunnerError::UnsupportedCommand(
                "the Cargo test command contains an unsupported shell boundary".into(),
            ));
        }
        if after_separator {
            runner_arguments.push(argument.clone());
        } else {
            arguments.push(argument.clone());
        }
    }
    Ok(CargoTestInvocation {
        program: words[cargo].clone(),
        arguments,
        runner_arguments,
    })
}

fn cargo_option_takes_value(argument: &str) -> Option<bool> {
    let name = argument.split_once('=').map_or(argument, |(name, _)| name);
    match name {
        "-p" | "--package" | "--exclude" | "--bin" | "--example" | "--test" | "--bench" | "-F"
        | "--features" | "-j" | "--jobs" | "--profile" | "--target" | "--target-dir"
        | "--message-format" | "--color" | "--config" | "-Z" | "--manifest-path" => {
            Some(!argument.contains('='))
        }
        "--no-run"
        | "--no-fail-fast"
        | "--future-incompat-report"
        | "-q"
        | "--quiet"
        | "-v"
        | "--verbose"
        | "--workspace"
        | "--all"
        | "--lib"
        | "--bins"
        | "--examples"
        | "--tests"
        | "--benches"
        | "--all-targets"
        | "--doc"
        | "--all-features"
        | "--no-default-features"
        | "-r"
        | "--release"
        | "--timings"
        | "--ignore-rust-version"
        | "--locked"
        | "--offline"
        | "--frozen" => Some(false),
        _ if argument.starts_with("-vv") => Some(false),
        _ if argument.starts_with("-p") && argument.len() > 2 => Some(false),
        _ if argument.starts_with("-F") && argument.len() > 2 => Some(false),
        _ if argument.starts_with("-j") && argument.len() > 2 => Some(false),
        _ => None,
    }
}

pub(crate) fn rust_libtest_selection(
    invocation: &CargoTestInvocation,
) -> Result<RustLibtestSelection, RustTestRunnerError> {
    let test = invocation
        .arguments
        .iter()
        .position(|argument| argument == "test")
        .ok_or_else(|| {
            RustTestRunnerError::UnsupportedCommand(
                "the expanded Cargo invocation lost its test subcommand".into(),
            )
        })?;
    let mut cargo_filter = None;
    let mut index = test + 1;
    while index < invocation.arguments.len() {
        let argument = &invocation.arguments[index];
        if argument.starts_with('-') {
            let takes_value = cargo_option_takes_value(argument).ok_or_else(|| {
                RustTestRunnerError::UnsupportedCommand(format!(
                    "the pinned Cargo test contract does not recognize option {argument}"
                ))
            })?;
            if takes_value {
                index += 1;
                if index == invocation.arguments.len() {
                    return Err(RustTestRunnerError::UnsupportedCommand(format!(
                        "Cargo option {argument} has no value"
                    )));
                }
            }
        } else if cargo_filter.replace(argument.clone()).is_some() {
            return Err(RustTestRunnerError::UnsupportedCommand(
                "Cargo test has more than one pre-separator TESTNAME".into(),
            ));
        }
        index += 1;
    }

    let mut list_arguments = cargo_filter.into_iter().collect::<Vec<_>>();
    let mut run_arguments = Vec::new();
    let mut index = 0;
    while index < invocation.runner_arguments.len() {
        let argument = &invocation.runner_arguments[index];
        match argument.as_str() {
            "--ignored"
            | "--include-ignored"
            | "--exclude-should-panic"
            | "--test"
            | "--bench"
            | "--force-run-in-process" => {
                list_arguments.push(argument.clone());
                run_arguments.push(argument.clone());
            }
            "--exact" => list_arguments.push(argument.clone()),
            "--skip" => {
                let value = invocation.runner_arguments.get(index + 1).ok_or_else(|| {
                    RustTestRunnerError::UnsupportedCommand(
                        "libtest --skip has no filter value".into(),
                    )
                })?;
                list_arguments.extend([argument.clone(), value.clone()]);
                index += 1;
            }
            _ if argument.starts_with("--skip=") && argument.len() > "--skip=".len() => {
                list_arguments.push(argument.clone());
            }
            _ if !argument.starts_with('-') => list_arguments.push(argument.clone()),
            _ => {
                return Err(RustTestRunnerError::UnsupportedCommand(format!(
                    "libtest option {argument} cannot yet be reproduced exactly by process-per-test execution"
                )));
            }
        }
        index += 1;
    }
    Ok(RustLibtestSelection {
        list_arguments,
        run_arguments,
    })
}

pub(crate) fn rust_cargo_execution_selection(
    invocation: &CargoTestInvocation,
) -> Result<RustCargoExecutionSelection, RustTestRunnerError> {
    let test = invocation
        .arguments
        .iter()
        .position(|argument| argument == "test")
        .ok_or_else(|| {
            RustTestRunnerError::UnsupportedCommand(
                "the expanded Cargo invocation lost its test subcommand".into(),
            )
        })?;
    let mut doc = false;
    let mut other_target = false;
    let mut index = test + 1;
    while index < invocation.arguments.len() {
        let argument = &invocation.arguments[index];
        let name = argument
            .split_once('=')
            .map_or(argument.as_str(), |(name, _)| name);
        match name {
            "--doc" => doc = true,
            "--lib" | "--bins" | "--bin" | "--examples" | "--example" | "--tests" | "--test"
            | "--benches" | "--bench" | "--all-targets" => other_target = true,
            _ => {}
        }
        if argument.starts_with('-') {
            let takes_value = cargo_option_takes_value(argument).ok_or_else(|| {
                RustTestRunnerError::UnsupportedCommand(format!(
                    "the pinned Cargo test contract does not recognize option {argument}"
                ))
            })?;
            if takes_value {
                index += 1;
                if index == invocation.arguments.len() {
                    return Err(RustTestRunnerError::UnsupportedCommand(format!(
                        "Cargo option {argument} has no value"
                    )));
                }
            }
        }
        index += 1;
    }
    if doc && other_target {
        return Err(RustTestRunnerError::UnsupportedCommand(
            "Cargo --doc cannot be combined with another explicit target selection".into(),
        ));
    }
    let run_doctests = doc || !other_target;
    let run_libtests = !doc;
    let mut doctest_arguments = invocation.arguments.clone();
    if run_doctests && !doc {
        doctest_arguments.insert(test + 1, "--doc".into());
    }
    if !invocation.runner_arguments.is_empty() {
        doctest_arguments.push("--".into());
        doctest_arguments.extend(invocation.runner_arguments.iter().cloned());
    }
    Ok(RustCargoExecutionSelection {
        run_libtests,
        run_doctests,
        doctest_arguments,
    })
}

fn relative_source(root: &Path, path: &Path) -> Result<String, RustTestRunnerError> {
    let relative = path
        .strip_prefix(root)
        .map_err(|_| RustTestRunnerError::UnsafeArtifact(path.display().to_string()))?;
    if relative.as_os_str().is_empty()
        || relative
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
    {
        return Err(RustTestRunnerError::UnsafeArtifact(
            path.display().to_string(),
        ));
    }
    Ok(relative.to_string_lossy().replace('\\', "/"))
}

fn build_test_artifacts(
    project: &PreparedRustProject,
    command: &[String],
) -> Result<Vec<TestArtifact>, RustTestRunnerError> {
    let mut invocation = cargo_invocation(&project.workspace_root, command)?;
    invocation
        .arguments
        .extend(["--no-run".into(), "--message-format=json".into()]);
    let output = Command::new(&invocation.program)
        .args(invocation.arguments)
        .current_dir(&project.workspace_root)
        .env("CARGO_TARGET_DIR", &project.target_directory)
        .output()
        .map_err(|error| RustTestRunnerError::Launch(error.to_string()))?;
    if !output.status.success() {
        return Err(RustTestRunnerError::CargoFailed(
            String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        ));
    }
    let canonical_target = fs::canonicalize(&project.target_directory)
        .map_err(|error| RustTestRunnerError::Io(error.to_string()))?;
    let mut artifacts = Vec::new();
    for line in output
        .stdout
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
    {
        let message: CargoMessage = serde_json::from_slice(line)
            .map_err(|error| RustTestRunnerError::CargoJson(error.to_string()))?;
        if message.reason != "compiler-artifact"
            || !message.profile.as_ref().is_some_and(|profile| profile.test)
        {
            continue;
        }
        let (Some(executable), Some(target)) = (message.executable, message.target) else {
            continue;
        };
        let executable = fs::canonicalize(&executable)
            .map_err(|error| RustTestRunnerError::Io(error.to_string()))?;
        if !executable.starts_with(&canonical_target)
            || !fs::metadata(&executable).is_ok_and(|metadata| metadata.is_file())
        {
            return Err(RustTestRunnerError::UnsafeArtifact(
                executable.display().to_string(),
            ));
        }
        let source = fs::canonicalize(target.src_path)
            .map_err(|error| RustTestRunnerError::Io(error.to_string()))?;
        artifacts.push(TestArtifact {
            executable,
            name: target.name,
            kind: if target.kind.iter().any(|kind| kind == "test") {
                "integration".into()
            } else {
                "unit".into()
            },
            source: relative_source(&project.workspace_root, &source)?,
        });
    }
    artifacts.sort_by(|left, right| left.executable.cmp(&right.executable));
    artifacts.dedup_by(|left, right| left.executable == right.executable);
    if artifacts.is_empty() {
        return Err(RustTestRunnerError::CargoJson(
            "Cargo emitted no libtest artifacts".into(),
        ));
    }
    Ok(artifacts)
}

fn list_tests(artifact: &TestArtifact) -> Result<Vec<String>, RustTestRunnerError> {
    let output = Command::new(&artifact.executable)
        .args(["--list", "--format", "terse"])
        .output()
        .map_err(|error| RustTestRunnerError::Launch(error.to_string()))?;
    if !output.status.success() {
        return Err(RustTestRunnerError::ListFailed(
            String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        ));
    }
    let mut tests = String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| line.strip_suffix(": test"))
        .map(str::to_owned)
        .collect::<Vec<_>>();
    tests.sort();
    tests.dedup();
    Ok(tests)
}

fn snapshot(
    manifest: &CoverageManifest,
    directory: &Path,
) -> Result<RuntimeSnapshot, RustTestRunnerError> {
    let points = manifest
        .points
        .iter()
        .map(|point| point.id.as_str())
        .collect::<BTreeSet<_>>();
    let alternatives = manifest
        .branches
        .iter()
        .flat_map(|branch| {
            branch
                .alternatives
                .iter()
                .map(|alternative| alternative.id.as_str())
        })
        .collect::<BTreeSet<_>>();
    let decisions = manifest
        .decisions
        .iter()
        .map(|decision| (decision.id.as_str(), decision))
        .collect::<BTreeMap<_, _>>();
    let mut hits = BTreeSet::new();
    let mut vectors = BTreeMap::<String, BTreeSet<(Vec<Option<bool>>, bool)>>::new();
    for observations in read_rust_probe_directory(directory)
        .map_err(|error| RustTestRunnerError::Probe(error.to_string()))?
        .into_values()
    {
        for observation in observations {
            match observation {
                RustProbeObservation::Hit { id } => {
                    if !points.contains(id.as_str()) && !alternatives.contains(id.as_str()) {
                        return Err(RustTestRunnerError::UnknownProbe(id));
                    }
                    hits.insert(id);
                }
                RustProbeObservation::Decision {
                    id,
                    values,
                    outcome,
                } => {
                    let Some(meta) = decisions.get(id.as_str()) else {
                        return Err(RustTestRunnerError::UnknownProbe(id));
                    };
                    if values.len() != meta.conditions.len() {
                        return Err(RustTestRunnerError::InvalidVector {
                            id,
                            expected: meta.conditions.len(),
                            actual: values.len(),
                        });
                    }
                    hits.insert(format!(
                        "{}:outcome:{}",
                        meta.id,
                        if outcome { "true" } else { "false" }
                    ));
                    vectors
                        .entry(meta.id.clone())
                        .or_default()
                        .insert((values, outcome));
                }
            }
        }
    }
    let mut decision_snapshots = Vec::new();
    for (id, observed) in vectors {
        let meta: DecisionMeta = (*decisions[id.as_str()]).clone();
        decision_snapshots.push(DecisionSnapshot {
            meta,
            vectors: observed
                .into_iter()
                .map(|(values, outcome)| McdcVector { values, outcome })
                .collect(),
        });
    }
    Ok(RuntimeSnapshot {
        decisions: decision_snapshots,
        hits: hits.into_iter().collect(),
        events: Vec::new(),
    })
}

fn rust_coverage_model() -> CoverageModelDeclaration {
    CoverageModelDeclaration {
        language: "rust".into(),
        variant: "rust-owned-probes-v1".into(),
        name: "supercov-rust-owned-v1".into(),
        completeness_meaning: "Every semantics-proven Rust obligation in the owned source denominator was observed; explicit manifest limitations identify unmeasured Rust surfaces.".into(),
        measured: vec![
            "owned Rust statements and function entries".into(),
            "owned atomic condition vectors and decision outcomes".into(),
            "exact process-per-libtest attribution".into(),
        ],
        not_measured: vec![
            "macro-expanded and generated Rust code".into(),
            "const-evaluated code and unsupported structural branch probes".into(),
            "causal linkage to individual actions or passing assertions".into(),
            "all input values, semantic partitions, paths, or concurrency interleavings".into(),
            "mutation score or assertion fault-detection strength".into(),
        ],
    }
}

pub fn run_prepared_rust_tests(
    project: &PreparedRustProject,
    command: &[String],
    run_id: &str,
    generated_at: &str,
    diagnostics: &mut dyn Write,
) -> Result<RustFrontendRun, RustTestRunnerError> {
    let build_started = Instant::now();
    let artifacts = build_test_artifacts(project, command)?;
    let build_ms = build_started.elapsed().as_secs_f64() * 1000.0;
    let evidence_root = project
        .workspace_root
        .join(".supercov/rust-evidence")
        .join(run_id);
    fs::create_dir_all(&evidence_root)
        .map_err(|error| RustTestRunnerError::Io(error.to_string()))?;
    let mut results = Vec::new();
    let mut overall_exit = 0;
    let execution_started = Instant::now();
    let mut tasks = Vec::new();
    for (artifact_index, artifact) in artifacts.iter().enumerate() {
        let tests = list_tests(artifact)?;
        let contexts = preflight_rust_test_contexts(tests.clone())
            .map_err(|error| RustTestRunnerError::Context(error.to_string()))?;
        for (test_index, test) in tests.into_iter().enumerate() {
            let directory = evidence_root.join(format!("{artifact_index:04}-{test_index:08}"));
            fs::create_dir(&directory)
                .map_err(|error| RustTestRunnerError::Io(error.to_string()))?;
            tasks.push(ProcessTask {
                ordinal: tasks.len(),
                artifact_index,
                test_index,
                artifact: artifact.clone(),
                context_id: contexts[&test],
                test,
                directory,
            });
        }
    }
    let workers = std::thread::available_parallelism()
        .map(usize::from)
        .unwrap_or(1)
        .min(tasks.len().max(1));
    let next = AtomicUsize::new(0);
    let outcomes = Mutex::new(Vec::<Result<ProcessOutcome, String>>::with_capacity(
        tasks.len(),
    ));
    std::thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| {
                loop {
                    let index = next.fetch_add(1, Ordering::Relaxed);
                    let Some(task) = tasks.get(index) else { break };
                    let result = Command::new(&task.artifact.executable)
                        .args(["--exact", &task.test, "--nocapture"])
                        .current_dir(&project.workspace_root)
                        .env("SUPERCOV_RUST_EVIDENCE_DIR", &task.directory)
                        .env(
                            crate::rust_probe_transport::RUST_CONTEXT_ENV,
                            format!("{:016x}", task.context_id),
                        )
                        .output()
                        .map(|output| ProcessOutcome {
                            task: ProcessTask {
                                ordinal: task.ordinal,
                                artifact_index: task.artifact_index,
                                test_index: task.test_index,
                                artifact: task.artifact.clone(),
                                test: task.test.clone(),
                                context_id: task.context_id,
                                directory: task.directory.clone(),
                            },
                            output,
                        })
                        .map_err(|error| error.to_string());
                    outcomes
                        .lock()
                        .expect("Rust test result lock poisoned")
                        .push(result);
                }
            });
        }
    });
    let mut outcomes = outcomes
        .into_inner()
        .map_err(|_| RustTestRunnerError::Io("Rust test result lock poisoned".into()))?
        .into_iter()
        .map(|result| result.map_err(RustTestRunnerError::Launch))
        .collect::<Result<Vec<_>, _>>()?;
    outcomes.sort_by_key(|outcome| outcome.task.ordinal);
    for outcome in outcomes {
        let ProcessTask {
            artifact_index,
            test_index,
            artifact,
            test,
            directory,
            ..
        } = outcome.task;
        // Target names are not workspace-unique: two packages may both
        // expose `lib` or the same integration-test target. Source path +
        // libtest name is stable and unique within the frozen workspace.
        let test_id = format!("{}::{test}", artifact.source);
        let worker_id = format!("artifact-{artifact_index:04}");
        let attempt_id = format!("{run_id}:{artifact_index:04}:{test_index:08}");
        let output = outcome.output;
        let exit = output.status.code().unwrap_or(1);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let skipped =
            exit == 0 && (stdout.contains("running 0 tests") || stdout.contains("; 1 ignored;"));
        if exit != 0 {
            writeln!(diagnostics, "[supercov] Rust test failed: {test_id}")
                .map_err(|error| RustTestRunnerError::Io(error.to_string()))?;
            diagnostics
                .write_all(&output.stdout)
                .and_then(|_| diagnostics.write_all(&output.stderr))
                .map_err(|error| RustTestRunnerError::Io(error.to_string()))?;
        }
        if exit != 0 {
            overall_exit = exit;
        }
        results.push(RawTestResult {
            test_id: Some(test_id.clone()),
            scope: Some(ExecutionScope {
                version: 1,
                run_id: run_id.into(),
                worker_id,
                test_id: test_id.clone(),
                test_key: format!("{}::{test}", artifact.source),
                retry: 0,
                attempt_id,
            }),
            test: test_id,
            test_file: Some(artifact.source.clone()),
            title: Some(test),
            retry: Some(0),
            status: Some(
                if exit != 0 {
                    "failed"
                } else if skipped {
                    "skipped"
                } else {
                    "passed"
                }
                .into(),
            ),
            expected_status: Some("passed".into()),
            flaky: false,
            provenance: TestProvenance {
                runner: "rust-libtest".into(),
                kind: artifact.kind,
                project: Some(artifact.name),
                source: "supercov-owned-process-per-test".into(),
            },
            role: "test".into(),
            phases: Vec::new(),
            runtime: vec![snapshot(&project.manifest, &directory)?],
            browser: Vec::new(),
            server: Vec::new(),
        });
    }
    let structural_limitations = project
        .manifest
        .limitations
        .iter()
        .filter_map(|item| {
            item.get("id")
                .and_then(|value| value.as_str())
                .map(str::to_owned)
        })
        .collect();
    Ok(RustFrontendRun {
        declaration: FrontendRunDeclaration {
            protocol_version: LANGUAGE_FRONTEND_PROTOCOL_VERSION,
            frontend_id: "rust".into(),
            frontend_version: "rust-owned-v1".into(),
            language: "rust".into(),
            structural_source: StructuralSource::OwnedProbes,
            runners: vec![FrontendRunnerDeclaration {
                runner: "rust-libtest".into(),
                execution_model: ExecutionModel::ProcessPerTest,
                attribution: FrontendAttribution {
                    run: AttributionPrecision::Exact,
                    worker: AttributionPrecision::Exact,
                    test: AttributionPrecision::Exact,
                    retry: AttributionPrecision::Exact,
                    phase: AttributionPrecision::Exact,
                    action: AttributionPrecision::Unavailable,
                    assertion: AttributionPrecision::Unavailable,
                },
                limitations: vec![
                    FrontendLimitation {
                        id: "rust-action-linkage-unavailable".into(),
                        scopes: vec![FrontendLimitationScope::Action],
                        reason: "Rust test frameworks expose no general action lifecycle".into(),
                    },
                    FrontendLimitation {
                        id: "rust-assertion-linkage-unavailable".into(),
                        scopes: vec![FrontendLimitationScope::Assertion],
                        reason: "assertion macros do not expose a stable per-assertion success lifecycle".into(),
                    },
                ],
            }],
            structural_limitations,
        },
        request: CoverageReportRequest {
            run_id: run_id.into(),
            manifest: project.manifest.clone(),
            raw_results: results,
            generated_at: generated_at.into(),
            coverage_model: Some(rust_coverage_model()),
            integrity: None,
            test_exit_code: ExitCodeInput::Present(Some(overall_exit)),
        },
        exit_code: overall_exit,
        artifacts: artifacts.len(),
        artifact_files: artifacts
            .iter()
            .map(|artifact| artifact.executable.clone())
            .collect(),
        build_ms,
        execution_ms: execution_started.elapsed().as_secs_f64() * 1000.0,
    })
}

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;
    use crate::{
        coverage_report::{ArchiveReportRequest, analyze_coverage_archive},
        evidence_archive::write_archive,
        frontend_protocol::validate_frontend_report_request,
        rust_project::prepare_rust_project,
    };

    #[test]
    fn cargo_and_libtest_selection_is_preserved_without_presentation_guessing() {
        let root = Path::new(".");
        let invocation = cargo_invocation(
            root,
            &[
                "cargo".into(),
                "test".into(),
                "-p".into(),
                "fixture".into(),
                "authored".into(),
                "--".into(),
                "generated".into(),
                "--skip".into(),
                "slow".into(),
                "--include-ignored".into(),
            ],
        )
        .unwrap();
        assert_eq!(invocation.arguments, ["test", "-p", "fixture", "authored"]);
        assert_eq!(
            invocation.runner_arguments,
            ["generated", "--skip", "slow", "--include-ignored"]
        );
        let selection = rust_libtest_selection(&invocation).unwrap();
        assert_eq!(
            selection.list_arguments,
            [
                "authored",
                "generated",
                "--skip",
                "slow",
                "--include-ignored"
            ]
        );
        assert_eq!(selection.run_arguments, ["--include-ignored"]);
    }

    #[test]
    fn direct_cargo_argv_preserves_toml_quotes_inside_config_values() {
        let config = "target.host.runner=[\"runner with spaces\",\"--fixed\"]";
        let invocation = cargo_invocation(
            Path::new("."),
            &[
                "cargo".into(),
                "test".into(),
                "--config".into(),
                config.into(),
            ],
        )
        .unwrap();
        assert_eq!(invocation.arguments, ["test", "--config", config]);
    }

    #[test]
    fn libtest_modes_that_process_per_test_cannot_reproduce_fail_closed() {
        for arguments in [
            vec!["--test-threads", "4"],
            vec!["--shuffle"],
            vec!["--format=json"],
            vec!["--nocapture"],
            vec!["--fail-fast"],
        ] {
            let invocation = CargoTestInvocation {
                program: "cargo".into(),
                arguments: vec!["test".into()],
                runner_arguments: arguments.into_iter().map(str::to_owned).collect(),
            };
            assert!(rust_libtest_selection(&invocation).is_err());
        }
    }

    #[test]
    fn cargo_test_options_are_not_mistaken_for_the_test_name_filter() {
        let invocation = CargoTestInvocation {
            program: "cargo".into(),
            arguments: vec![
                "test".into(),
                "--manifest-path".into(),
                "nested/Cargo.toml".into(),
                "--features=one,two".into(),
                "needle".into(),
            ],
            runner_arguments: vec!["--ignored".into(), "other".into()],
        };
        let selection = rust_libtest_selection(&invocation).unwrap();
        assert_eq!(selection.list_arguments, ["needle", "--ignored", "other"]);
        assert_eq!(selection.run_arguments, ["--ignored"]);
    }

    #[test]
    fn cargo_target_selection_reproduces_when_cargo_runs_doctests() {
        let invocation = CargoTestInvocation {
            program: "cargo".into(),
            arguments: vec![
                "test".into(),
                "-p".into(),
                "fixture".into(),
                "needle".into(),
            ],
            runner_arguments: vec!["--include-ignored".into()],
        };
        let selection = rust_cargo_execution_selection(&invocation).unwrap();
        assert!(selection.run_libtests);
        assert!(selection.run_doctests);
        assert_eq!(
            selection.doctest_arguments,
            [
                "test",
                "--doc",
                "-p",
                "fixture",
                "needle",
                "--",
                "--include-ignored"
            ]
        );

        let mut explicit_doc = invocation.clone();
        explicit_doc.arguments.insert(1, "--doc".into());
        let selection = rust_cargo_execution_selection(&explicit_doc).unwrap();
        assert!(!selection.run_libtests);
        assert!(selection.run_doctests);

        for target in ["--lib", "--tests", "--all-targets", "--example=demo"] {
            let mut selected = invocation.clone();
            selected.arguments.insert(1, target.into());
            let selection = rust_cargo_execution_selection(&selected).unwrap();
            assert!(selection.run_libtests);
            assert!(!selection.run_doctests);
        }
    }

    #[test]
    fn cargo_libtest_runs_produce_queryable_owned_evidence() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "supercov-rust-runner-{}-{nonce}",
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
            r#"
pub fn choose(left: bool, right: bool) -> i32 {
    if left && right { 1 } else { 0 }
}
#[cfg(test)]
mod tests {
    #[test] fn false_path() { assert_eq!(super::choose(false, true), 0); }
    #[test] fn true_path() { assert_eq!(super::choose(true, true), 1); }
    #[test] #[ignore] fn ignored_path() { unreachable!(); }
}
"#,
        )
        .unwrap();
        let project = prepare_rust_project(&root).unwrap();
        let run = run_prepared_rust_tests(
            &project,
            &["cargo".into(), "test".into()],
            "rust-fixture",
            "2026-08-26T00:00:00.000Z",
            &mut Vec::new(),
        )
        .unwrap();
        assert_eq!(run.exit_code, 0);
        assert_eq!(run.request.raw_results.len(), 3);
        assert_eq!(
            run.request
                .raw_results
                .iter()
                .filter_map(|result| result.status.as_deref())
                .collect::<Vec<_>>(),
            ["passed", "skipped", "passed"]
        );
        validate_frontend_report_request(&run.declaration, &run.request).unwrap();
        let archive = root.join("evidence.raw.gz");
        write_archive(run.archive_entries().unwrap(), &archive).unwrap();
        let report = analyze_coverage_archive(&ArchiveReportRequest {
            archive_path: archive,
            run_id: "rust-fixture".into(),
            generated_at: "2026-08-26T00:00:00.000Z".into(),
            integrity: None,
            test_exit_code: ExitCodeInput::Present(Some(0)),
        })
        .unwrap();
        assert_eq!(report.view.tests.len(), 3);
        assert!(report.view.summary.lines.covered > 0);
        assert!(report.view.summary.decisions > 0);
        fs::remove_dir_all(root).unwrap();
    }
}
