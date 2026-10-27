//! Stable Cargo/libtest execution for the owned Rust frontend.
//!
//! Source preparation happens in an isolated workspace. For `cargo test`,
//! Cargo builds each test artifact once and every libtest case then runs in a
//! process of its own with an evidence directory of its own, so attribution
//! is exact by construction; doctests run through `rust_owned_doctests`, with
//! this program standing in for rustdoc. For `cargo nextest run`, nextest
//! keeps its own scheduling and retries and this program serves as its target
//! runner (`rust_owned_nextest`), recording each attempt it launches.

use crate::fault_fs as fs;
use crate::fault_process::Launch;
use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsString,
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
        CoverageManifest, CoverageModelDeclaration, CoveragePhase, CoverageReportRequest,
        DecisionMeta, DecisionSnapshot, ExecutionScope, ExitCodeInput, PersistedCoverageModel,
        RawTestResult, RuntimeEvent, RuntimeSnapshot, TestProvenance,
    },
    evidence_archive::EvidenceArchiveEntry,
    rust_project::PreparedRustProject,
    rust_runtime::{RustProbeObservation, read_rust_region_directory},
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
                contents: serde_json::to_vec(&model).expect("plain data serializes"),
            },
            EvidenceArchiveEntry {
                path: "frontend.json".into(),
                contents: serde_json::to_vec(&self.declaration).expect("plain data serializes"),
            },
            EvidenceArchiveEntry {
                path: "manifest.json".into(),
                contents: serde_json::to_vec(&self.request.manifest)
                    .expect("plain data serializes"),
            },
        ];
        for (index, result) in self.request.raw_results.iter().enumerate() {
            entries.push(EvidenceArchiveEntry {
                path: format!("results/{index:08}/mcdc.json"),
                contents: serde_json::to_vec(result).expect("plain data serializes"),
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
    manifest_path: Option<PathBuf>,
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
    /// Where Cargo runs the binary: the directory of its package's manifest.
    /// tokio's `basic_fs` reads `Cargo.toml` from there.
    package_directory: PathBuf,
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

/// A line of a test process's standard output shown while it runs without
/// capture: libtest's frame of each one-test process (`running 1 test`, its
/// `test result:` and the blank lines around them) is the run's to report,
/// once, and not each process's.
fn shown_without_capture(line: &[u8]) -> bool {
    let text = String::from_utf8_lossy(line);
    let text = text.trim();
    !(text.is_empty()
        || text
            .strip_prefix("running ")
            .is_some_and(|count| count.ends_with(" test") || count.ends_with(" tests"))
        || text.starts_with("test result: "))
}

/// A command line split the way POSIX `sh` splits a script's: quotes group,
/// `''` and `""` are an empty word, and inside double quotes a backslash
/// escapes only `$`, `` ` ``, `"`, `\` and a newline.
fn shell_words(value: &str) -> Result<Vec<String>, RustTestRunnerError> {
    let mut words = Vec::new();
    let mut current = String::new();
    // A word can be empty (`''`), so whether one has begun is its own fact.
    let mut started = false;
    let mut quote = None;
    let mut characters = value.chars().peekable();
    while let Some(character) = characters.next() {
        match (quote, character) {
            (Some('\''), '\'') | (Some('"'), '"') => quote = None,
            (Some('\''), _) => current.push(character),
            (Some(_), '\\') => match characters.peek() {
                // A backslash before a line break continues the line.
                Some('\n') => {
                    characters.next();
                }
                Some('$' | '`' | '"' | '\\') => {
                    current.extend(characters.next());
                }
                Some(_) => current.push('\\'),
                None => quote = Some('\\'),
            },
            (Some(_), _) => current.push(character),
            (None, '\\') => match characters.next() {
                Some('\n') => {}
                Some(escaped) => {
                    current.push(escaped);
                    started = true;
                }
                None => quote = Some('\\'),
            },
            (None, '\'' | '"') => {
                quote = Some(character);
                started = true;
            }
            (None, _) if character.is_whitespace() => {
                if std::mem::take(&mut started) {
                    words.push(std::mem::take(&mut current));
                }
            }
            (None, _) => {
                current.push(character);
                started = true;
            }
        }
    }
    if quote.is_some() {
        return Err(RustTestRunnerError::UnsupportedCommand(
            "the expanded Cargo command contains an incomplete quote or escape".into(),
        ));
    }
    if started {
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
    pub kind: RustCargoCommandKind,
    pub arguments: Vec<String>,
    pub runner_arguments: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RustCargoCommandKind {
    CargoTest,
    NextestRun,
}

impl CargoTestInvocation {
    /// Where `test` (or `nextest run`) sits in the arguments. Only
    /// `cargo_invocation` builds an invocation, and it refuses a command
    /// without one.
    pub(crate) fn command_position(&self) -> usize {
        match self.kind {
            RustCargoCommandKind::CargoTest => self
                .arguments
                .iter()
                .position(|argument| argument == "test"),
            RustCargoCommandKind::NextestRun => self
                .arguments
                .windows(2)
                .position(|pair| pair == ["nextest", "run"]),
        }
        .expect("cargo_invocation keeps the test subcommand")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RustLibtestSelection {
    pub list_arguments: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RustCargoExecutionSelection {
    pub run_libtests: bool,
    pub run_doctests: bool,
    /// The command asked for every test, not `--doc` by name: Cargo then
    /// runs doctests only for the selected packages that can have them.
    pub implicit_doctests: bool,
    pub doctest_arguments: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NextestListInvocation {
    pub arguments: Vec<String>,
    pub runner_arguments: Vec<String>,
}

pub(crate) fn nextest_version_arguments(invocation: &CargoTestInvocation) -> Vec<String> {
    let command = invocation.command_position();
    let mut arguments = invocation.arguments[..command].to_vec();
    arguments.extend(["nextest".into(), "--version".into()]);
    arguments
}

fn nextest_run_only_option(argument: &str) -> Option<bool> {
    let name = argument.split_once('=').map_or(argument, |(name, _)| name);
    match name {
        "-j"
        | "--jobs"
        | "--test-threads"
        | "--retries"
        | "--flaky-result"
        | "--max-fail"
        | "--no-tests"
        | "--failure-output"
        | "--success-output"
        | "--status-level"
        | "--final-status-level"
        | "--show-progress"
        | "--max-progress-running"
        | "--message-format"
        | "--message-format-version" => Some(!argument.contains('=')),
        "--fail-fast"
        | "--ff"
        | "--no-fail-fast"
        | "--nff"
        | "--no-capture"
        | "--nocapture"
        | "--no-output-indent"
        | "--hide-progress-bar"
        | "--no-input-handler" => Some(false),
        // `-j` alone, or with `=`, is matched above: what reaches here is `-j4`.
        _ if argument.starts_with("-j") => Some(false),
        _ => None,
    }
}

fn nextest_unsupported_run_option(argument: &str) -> Option<bool> {
    let name = argument.split_once('=').map_or(argument, |(name, _)| name);
    match name {
        "-R"
        | "--rerun"
        | "--debugger"
        | "--tracer"
        | "--stress-count"
        | "--stress-duration"
        | "--archive-file"
        | "--archive-format"
        | "--extract-to"
        | "--cargo-metadata"
        | "--workspace-remap"
        | "--binaries-metadata"
        | "--target-dir-remap"
        | "--build-dir-remap" => Some(!argument.contains('=')),
        "--no-run" | "--extract-overwrite" | "--persist-extract-tempdir" => Some(false),
        _ => None,
    }
}

fn nextest_shared_option(argument: &str) -> Option<bool> {
    let name = argument.split_once('=').map_or(argument, |(name, _)| name);
    match name {
        "--color"
        | "-p"
        | "--package"
        | "--exclude"
        | "--bin"
        | "--example"
        | "--test"
        | "--bench"
        | "-F"
        | "--features"
        | "--build-jobs"
        | "--cargo-profile"
        | "--target"
        | "--target-dir"
        | "--cargo-message-format"
        | "--config"
        | "--timings"
        | "-Z"
        | "--run-ignored"
        | "--partition"
        | "--platform-filter"
        | "-E"
        | "--filterset"
        | "--filter-expr"
        | "--manifest-path"
        | "--config-file"
        | "--user-config-file"
        | "--tool-config-file"
        | "-P"
        | "--profile" => Some(!argument.contains('=')),
        "--no-pager"
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
        | "--all-features"
        | "--no-default-features"
        | "-r"
        | "--release"
        | "--unit-graph"
        | "--frozen"
        | "--locked"
        | "--offline"
        | "--cargo-quiet"
        | "--cargo-verbose"
        | "--ignore-rust-version"
        | "--future-incompat-report"
        | "--ignore-default-filter"
        | "--override-version-check" => Some(false),
        // The short options alone, or with `=`, are matched above: what
        // reaches here carries its value attached (`-pfoo`).
        _ if ["-p", "-F", "-E", "-P", "-Z"]
            .iter()
            .any(|short| argument.starts_with(short)) =>
        {
            Some(false)
        }
        // `-vv`, `-vvv`: a lone `-` is no verbosity.
        _ if argument.len() > 2
            && argument.starts_with('-')
            && argument[1..].bytes().all(|byte| byte == b'v') =>
        {
            Some(false)
        }
        _ => None,
    }
}

/// Reprojects a pinned `nextest run` invocation into the stable machine-readable
/// `nextest list` contract. Selection/build/configuration arguments are kept
/// byte-for-byte. Runner and presentation arguments are removed because they
/// do not affect the selected-test catalog. Options whose selection semantics
/// require an external recording or a different execution mode fail closed.
pub(crate) fn nextest_list_invocation(
    invocation: &CargoTestInvocation,
) -> Result<NextestListInvocation, RustTestRunnerError> {
    let command = invocation.command_position();
    let mut arguments = invocation.arguments[..command].to_vec();
    arguments.extend(["nextest".into(), "list".into()]);
    let mut index = command + 2;
    while index < invocation.arguments.len() {
        let argument = &invocation.arguments[index];
        if argument == "--" {
            arguments.extend(invocation.arguments[index..].iter().cloned());
            break;
        }
        if let Some(takes_value) = nextest_unsupported_run_option(argument) {
            if takes_value && invocation.arguments.get(index + 1).is_none() {
                return Err(RustTestRunnerError::UnsupportedCommand(format!(
                    "nextest option {argument} has no value"
                )));
            }
            return Err(RustTestRunnerError::UnsupportedCommand(format!(
                "nextest option {argument} cannot yet be assigned exact selected-test identity"
            )));
        }
        if let Some(takes_value) = nextest_run_only_option(argument) {
            if takes_value {
                index += 1;
                if index == invocation.arguments.len() {
                    return Err(RustTestRunnerError::UnsupportedCommand(format!(
                        "nextest option {argument} has no value"
                    )));
                }
            }
        } else if let Some(takes_value) = nextest_shared_option(argument) {
            arguments.push(argument.clone());
            if takes_value {
                index += 1;
                let value = invocation.arguments.get(index).ok_or_else(|| {
                    RustTestRunnerError::UnsupportedCommand(format!(
                        "nextest option {argument} has no value"
                    ))
                })?;
                arguments.push(value.clone());
            }
        } else if argument.starts_with('-') {
            return Err(RustTestRunnerError::UnsupportedCommand(format!(
                "the pinned nextest run contract does not recognize option {argument}"
            )));
        } else {
            arguments.push(argument.clone());
        }
        index += 1;
    }
    arguments.extend(["--message-format".into(), "json".into()]);
    Ok(NextestListInvocation {
        arguments,
        runner_arguments: invocation.runner_arguments.clone(),
    })
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
        // A package script runs with the arguments after its name appended,
        // so a filter given to `npm test -- adds` reaches Cargo.
        let expanded = crate::project_discovery::script_command(root, command)
            .unwrap_or_else(|| crate::project_discovery::expanded_command(root, command));
        shell_words(&expanded)?
    };
    let cargo = words
        .iter()
        .position(|word| executable_name(word) == "cargo")
        .ok_or_else(|| RustTestRunnerError::UnsupportedCommand(
            "Rust was detected, but the expanded command does not expose a stable Cargo invocation".into(),
        ))?;
    let cargo_test = words[cargo + 1..]
        .iter()
        .position(|word| word == "test")
        .map(|position| cargo + 1 + position);
    let nextest = words[cargo + 1..]
        .windows(2)
        .position(|pair| pair == ["nextest", "run"])
        .map(|position| cargo + 1 + position);
    let (kind, command) = match (cargo_test, nextest) {
        (Some(test), None) => (RustCargoCommandKind::CargoTest, test),
        (None, Some(nextest)) => (RustCargoCommandKind::NextestRun, nextest),
        (Some(_), Some(_)) => {
            return Err(RustTestRunnerError::UnsupportedCommand(
                "the Cargo invocation ambiguously contains both test and nextest run".into(),
            ));
        }
        (None, None) => {
            return Err(RustTestRunnerError::UnsupportedCommand(
                "the owned Rust runner currently requires `cargo test` or `cargo nextest run`; cross remains explicitly unsupported"
                    .into(),
            ));
        }
    };
    if words[cargo + 1..command]
        .iter()
        .any(|word| matches!(word.as_str(), "&&" | "||" | ";" | "|"))
    {
        return Err(RustTestRunnerError::UnsupportedCommand(
            "the Cargo invocation contains a shell boundary before `test`".into(),
        ));
    }
    let command_end = command
        + if kind == RustCargoCommandKind::NextestRun {
            1
        } else {
            0
        };
    let mut arguments = words[cargo + 1..=command_end].to_vec();
    let mut runner_arguments = Vec::new();
    let mut after_separator = false;
    for argument in &words[command_end + 1..] {
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
        kind,
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
        // `-p`, `-F` and `-j` alone, or with `=`, are matched above.
        _ if ["-p", "-F", "-j"]
            .iter()
            .any(|short| argument.starts_with(short)) =>
        {
            Some(false)
        }
        _ => None,
    }
}

pub(crate) fn rust_libtest_selection(
    invocation: &CargoTestInvocation,
) -> Result<RustLibtestSelection, RustTestRunnerError> {
    // `rust_cargo_execution_selection` has already refused an option Cargo's
    // contract does not know and an option missing its value.
    let mut cargo_filter = None;
    let mut index = invocation.command_position() + 1;
    while index < invocation.arguments.len() {
        let argument = &invocation.arguments[index];
        if argument.starts_with('-') {
            if cargo_option_takes_value(argument) == Some(true) {
                index += 1;
            }
        } else {
            // The build has run by now, and Cargo refuses a second TESTNAME.
            cargo_filter = Some(argument.clone());
        }
        index += 1;
    }

    let mut list_arguments = cargo_filter.into_iter().collect::<Vec<_>>();
    let mut test_threads = None;
    let mut index = 0;
    while index < invocation.runner_arguments.len() {
        let argument = &invocation.runner_arguments[index];
        match argument.as_str() {
            "--ignored" | "--include-ignored" | "--exclude-should-panic" | "--test" | "--bench" => {
                list_arguments.push(argument.clone());
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
            "--test-threads" => {
                let value = invocation.runner_arguments.get(index + 1).ok_or_else(|| {
                    RustTestRunnerError::UnsupportedCommand(
                        "libtest --test-threads has no value".into(),
                    )
                })?;
                let parsed = parse_libtest_threads(value)?;
                if test_threads.replace(parsed).is_some() {
                    return Err(RustTestRunnerError::UnsupportedCommand(
                        "libtest --test-threads was provided more than once".into(),
                    ));
                }
                index += 1;
            }
            _ if argument.starts_with("--test-threads=") => {
                let value = &argument["--test-threads=".len()..];
                let parsed = parse_libtest_threads(value)?;
                if test_threads.replace(parsed).is_some() {
                    return Err(RustTestRunnerError::UnsupportedCommand(
                        "libtest --test-threads was provided more than once".into(),
                    ));
                }
            }
            "-Z" => {
                let value = invocation.runner_arguments.get(index + 1).ok_or_else(|| {
                    RustTestRunnerError::UnsupportedCommand(
                        "libtest -Z has no feature value".into(),
                    )
                })?;
                // `exclude-should-panic` and the other unstable selection
                // flags must be parsed under the same libtest feature gate
                // during discovery. The user's exact pair is also preserved
                // unchanged for the real artifact execution.
                list_arguments.extend([argument.clone(), value.clone()]);
                index += 1;
            }
            // `-Z` alone is matched above.
            _ if argument.starts_with("-Z") => {
                list_arguments.push(argument.clone());
            }
            "--logfile" | "--color" | "--format" | "--shuffle-seed" => {
                if invocation.runner_arguments.get(index + 1).is_none() {
                    return Err(RustTestRunnerError::UnsupportedCommand(format!(
                        "libtest {argument} has no value"
                    )));
                }
                // Presentation-only values must not leak into the synthetic
                // terse listing. The original argument and value are passed
                // byte-for-byte to the one stock artifact execution.
                index += 1;
            }
            _ if ["--logfile=", "--color=", "--format=", "--shuffle-seed="]
                .iter()
                .any(|prefix| argument.starts_with(prefix) && argument.len() > prefix.len()) => {}
            "--force-run-in-process"
            | "--fail-fast"
            | "--no-capture"
            | "--nocapture"
            | "-q"
            | "--quiet"
            | "--show-output"
            | "--report-time"
            | "--ensure-time"
            | "--shuffle" => {
                // These affect only scheduling, execution or presentation.
                // They are intentionally absent from discovery and retained
                // unchanged in the real artifact argv.
            }
            "--list" | "-h" | "--help" => {
                return Err(RustTestRunnerError::UnsupportedCommand(format!(
                    "libtest {argument} does not execute a test suite; exact non-execution mode support is not implemented"
                )));
            }
            _ if !argument.starts_with('-') => list_arguments.push(argument.clone()),
            _ => {
                return Err(RustTestRunnerError::UnsupportedCommand(format!(
                    "the pinned Rust 1.95 libtest discovery contract does not recognize option {argument}"
                )));
            }
        }
        index += 1;
    }
    Ok(RustLibtestSelection { list_arguments })
}

fn parse_libtest_threads(value: &str) -> Result<usize, RustTestRunnerError> {
    match value.parse::<usize>() {
        Ok(0) => Err(RustTestRunnerError::UnsupportedCommand(
            "argument for --test-threads must not be 0".into(),
        )),
        Ok(value) => Ok(value),
        Err(error) => Err(RustTestRunnerError::UnsupportedCommand(format!(
            "argument for --test-threads must be a number > 0 (error: {error})"
        ))),
    }
}

pub(crate) fn rust_cargo_execution_selection(
    invocation: &CargoTestInvocation,
) -> Result<RustCargoExecutionSelection, RustTestRunnerError> {
    if invocation.kind == RustCargoCommandKind::NextestRun {
        return Ok(RustCargoExecutionSelection {
            run_libtests: true,
            run_doctests: false,
            implicit_doctests: false,
            doctest_arguments: Vec::new(),
        });
    }
    let test = invocation.command_position();
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
        implicit_doctests: run_doctests && !doc,
        doctest_arguments,
    })
}

pub(crate) fn relative_source(root: &Path, path: &Path) -> Result<String, RustTestRunnerError> {
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

/// The build step decides three things for itself: it builds without running,
/// reads Cargo's JSON, and builds into Supercov's own target directory. The
/// command's own `--message-format` and `--target-dir` would collide with
/// those, so they are left out of the build; `--no-run` asks for no test at
/// all, which leaves nothing to measure.
fn build_owned_arguments(arguments: Vec<String>) -> Result<Vec<String>, RustTestRunnerError> {
    let mut kept = Vec::with_capacity(arguments.len());
    let mut arguments = arguments.into_iter();
    while let Some(argument) = arguments.next() {
        let name = argument
            .split_once('=')
            .map_or(argument.as_str(), |(name, _)| name);
        match name {
            "--no-run" => {
                return Err(RustTestRunnerError::UnsupportedCommand(
                    "`--no-run` builds the tests without running any, so there is nothing to measure; run the command without it".into(),
                ));
            }
            "--message-format" | "--target-dir" => {
                if !argument.contains('=') {
                    arguments.next();
                }
            }
            _ => kept.push(argument),
        }
    }
    Ok(kept)
}

fn build_test_artifacts(
    project: &PreparedRustProject,
    command: &[String],
) -> Result<Vec<TestArtifact>, RustTestRunnerError> {
    let mut invocation = cargo_invocation(&project.workspace_root, command)?;
    let lint_invocation = invocation.clone();
    invocation.arguments = build_owned_arguments(invocation.arguments)?;
    invocation
        .arguments
        .extend(["--no-run".into(), "--message-format=json".into()]);
    // The instrumented workspace is ephemeral and its sources are generated,
    // so the HOST crate's lint policy must not reject them: serde builds with
    // `#![deny(warnings)]`, and http's `if ({ frame ... })` decision wrapping
    // trips `unused_parens` into a hard error under it. Capping lints to warn
    // changes nothing about the user's own `cargo test` runs. The user's
    // RUSTFLAGS are preserved ahead of the cap.
    let output = Command::new(&invocation.program)
        .args(invocation.arguments)
        .current_dir(&project.workspace_root)
        .env("CARGO_TARGET_DIR", &project.target_directory)
        .env("RUSTFLAGS", capped_rustflags(project, &lint_invocation))
        .launch_output()
        .map_err(|error| RustTestRunnerError::Launch(error.to_string()))?;
    if !output.status.success() {
        // With --message-format=json the compiler's diagnostics travel on
        // stdout as JSON and Cargo's own summary on stderr; show both, or a
        // failed build says only which crate failed.
        let rendered = output
            .stdout
            .split(|byte| *byte == b'\n')
            .filter_map(|line| serde_json::from_slice::<serde_json::Value>(line).ok())
            .filter(|message| {
                message["reason"] == "compiler-message" && message["message"]["level"] == "error"
            })
            .filter_map(|message| message["message"]["rendered"].as_str().map(str::to_owned))
            .collect::<String>();
        return Err(RustTestRunnerError::CargoFailed(format!(
            "{rendered}{}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
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
        let package_directory = message
            .manifest_path
            .as_deref()
            .and_then(Path::parent)
            .unwrap_or(&project.workspace_root)
            .to_path_buf();
        artifacts.push(TestArtifact {
            executable,
            package_directory,
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

/// How a test artifact runs: as itself, or through the runner Cargo would
/// run it with (`target.<triple>.runner`) -- `wasm-bindgen-test-runner` for a
/// wasm32 module, an emulator for another architecture. Running it any other
/// way runs something Cargo would not.
#[derive(Debug, Clone, Default)]
pub(crate) struct ArtifactLauncher {
    runner: Option<crate::rust_cargo_configuration::RustCargoResolvedRunner>,
    /// Variables the runner reads, from what started the tests: the browser
    /// driver `wasm-pack test` chose, headless or not.
    environment: Vec<(String, String)>,
    /// The proxy each browser test's runner drives Chrome through, which
    /// takes the page's regions before the runner closes its session.
    browser: Option<std::sync::Arc<crate::rust_wasm_browser::BrowserReader>>,
    /// A browser whose tests run unmeasured, to say so.
    unmeasured_browser: Option<String>,
    /// The instrumentation's token, which a page's regions must carry.
    token: String,
    /// The runner is `wasm-bindgen-test-runner`.
    wasm_bindgen_runner: bool,
    /// `NODE_OPTIONS` for a wasm module the runner runs under Node: a hook
    /// that writes each crate's region, which lives in the module's memory,
    /// to the test's evidence directory as the process ends.
    node_options: Option<OsString>,
}

impl ArtifactLauncher {
    /// The launcher for the one target this invocation builds for.
    pub(crate) fn for_invocation(
        project: &PreparedRustProject,
        invocation: &CargoTestInvocation,
        hook_directory: &Path,
    ) -> Result<Self, RustTestRunnerError> {
        Self::for_invocation_with(
            project,
            invocation,
            hook_directory,
            std::env::var_os("NODE_OPTIONS"),
        )
    }

    /// `for_invocation`, with the user's own `NODE_OPTIONS`.
    fn for_invocation_with(
        project: &PreparedRustProject,
        invocation: &CargoTestInvocation,
        hook_directory: &Path,
        user_node_options: Option<OsString>,
    ) -> Result<Self, RustTestRunnerError> {
        let Ok(plan) = crate::rust_cargo_configuration::resolve_cargo_runner_plan(
            &project.workspace_root,
            &project.workspace_root,
            invocation,
        ) else {
            return Ok(Self::default());
        };
        let [target] = plan.targets.as_slice() else {
            if plan
                .targets
                .iter()
                .any(|target| target.underlying_runner.is_some())
            {
                return Err(RustTestRunnerError::UnsupportedCommand(
                    "a command that builds for several targets, one of them with a target runner, cannot be run test by test".into(),
                ));
            }
            return Ok(Self::default());
        };
        let Some(runner) = &target.underlying_runner else {
            return Ok(Self::default());
        };
        let node_options = if target.target.starts_with("wasm32-") {
            fs::create_dir_all(hook_directory).map_err(io_error)?;
            let hook = hook_directory.join("rust-wasm-node.mjs");
            fs::write(&hook, RUST_WASM_NODE_HOOK).map_err(io_error)?;
            let mut options = user_node_options.unwrap_or_default();
            if !options.is_empty() {
                options.push(" ");
            }
            options.push(format!("--import={}", file_url(&hook)));
            Some(options)
        } else {
            None
        };
        let runner = runner.resolve(&project.workspace_root);
        let wasm_bindgen_runner = Path::new(&runner.program)
            .file_stem()
            .is_some_and(|name| name == "wasm-bindgen-test-runner");
        Ok(Self {
            runner: Some(runner),
            environment: Vec::new(),
            browser: None,
            unmeasured_browser: None,
            token: project.probes.token.clone(),
            wasm_bindgen_runner,
            node_options,
        })
    }

    /// The runner's variables; and, for tests `wasm-bindgen-test-runner`
    /// runs in Chrome, the reader of their pages.
    pub(crate) fn with_environment(self, environment: &[(String, String)]) -> Self {
        self.with_environment_from(environment, &|name| std::env::var_os(name))
    }

    /// `with_environment`, with `ambient` as the process's own variables.
    fn with_environment_from(
        mut self,
        environment: &[(String, String)],
        ambient: &dyn Fn(&str) -> Option<OsString>,
    ) -> Self {
        self.environment = environment.to_vec();
        let in_node = environment
            .iter()
            .any(|(name, _)| name == "WASM_BINDGEN_TEST_ONLY_NODE")
            || ambient("WASM_BINDGEN_TEST_ONLY_NODE").is_some();
        if !self.wasm_bindgen_runner || in_node {
            return self;
        }
        match crate::rust_wasm_browser::browser_from(environment, ambient) {
            Some(crate::rust_wasm_browser::Browser::Chrome(driver, arguments)) => {
                match crate::rust_wasm_browser::BrowserReader::start(
                    &driver,
                    &arguments,
                    &self.token,
                ) {
                    Ok(reader) => self.browser = Some(std::sync::Arc::new(reader)),
                    Err(error) => self.unmeasured_browser = Some(format!("Chrome ({error})")),
                }
            }
            Some(crate::rust_wasm_browser::Browser::Other(name)) => {
                // Said only where the driver was chosen by name: a driver
                // found on PATH may never be used (the tests may run in Node).
                // A remote driver, Chrome's included, is always named.
                let named = [
                    "GECKODRIVER",
                    "SAFARIDRIVER",
                    "MSEDGEDRIVER",
                    "GECKODRIVER_REMOTE",
                    "SAFARIDRIVER_REMOTE",
                    "CHROMEDRIVER_REMOTE",
                    "MSEDGEDRIVER_REMOTE",
                ]
                .iter()
                .any(|variable| {
                    environment.iter().any(|(key, _)| key == variable)
                        || ambient(variable).is_some()
                });
                if named {
                    self.unmeasured_browser = Some(name);
                }
            }
            None => {}
        }
        self
    }

    /// The browser whose tests this run cannot read, if one was named.
    pub(crate) fn unmeasured_browser(&self) -> Option<&str> {
        self.unmeasured_browser.as_deref()
    }

    /// The command that runs `executable`, writing its evidence to
    /// `evidence` when it is a test run.
    pub(crate) fn command(&self, executable: &Path, evidence: Option<&Path>) -> Command {
        let mut command = match &self.runner {
            Some(runner) => {
                let mut command = Command::new(&runner.program);
                command.args(&runner.arguments);
                // A WASI runtime gives the module neither the host's
                // directories nor its environment: the evidence directory,
                // and the variable naming it, are handed over by name.
                if let Some(evidence) = evidence
                    && is_wasi_runtime(&runner.program)
                {
                    command
                        .arg(format!("--dir={}", evidence.display()))
                        .arg(format!(
                            "--env=SUPERCOV_RUST_EVIDENCE_DIR={}",
                            evidence.display()
                        ));
                }
                command.arg(executable);
                command
            }
            None => Command::new(executable),
        };
        if let Some(options) = &self.node_options {
            command.env("NODE_OPTIONS", options);
        }
        command.envs(self.environment.iter().map(|(name, value)| (name, value)));
        if let (Some(browser), Some(evidence)) = (&self.browser, evidence) {
            command.env("CHROMEDRIVER_REMOTE", browser.remote_url(evidence));
        }
        command
    }
}

/// Whether a runner program is a WASI runtime, by its name: wasmtime and
/// wasmer both take `--dir` and `--env` before the module.
fn is_wasi_runtime(program: &Path) -> bool {
    program
        .file_stem()
        .and_then(|name| name.to_str())
        .is_some_and(|name| matches!(name, "wasmtime" | "wasmer"))
}

const RUST_WASM_NODE_HOOK: &str = include_str!("../runtime-assets/rust-wasm-node.mjs");

/// A `file:` URL for a path, as `node --import` takes it: bytes outside the
/// unreserved set are percent-encoded, so a space or a `#` is the path's.
fn file_url(path: &Path) -> String {
    let mut url = String::from("file://");
    let text = path.to_string_lossy().replace('\\', "/");
    if !text.starts_with('/') {
        url.push('/');
    }
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~/:".contains(&byte) {
            url.push(byte as char);
        } else {
            url.push_str(&format!("%{byte:02X}"));
        }
    }
    url
}

/// The tests an artifact would run for this invocation. `filters` are the
/// user's libtest arguments that select tests -- a name, `--skip`, `--exact`,
/// `--ignored` -- and libtest applies them to `--list` exactly as it applies
/// them to a run, so the enumeration is the set plain Cargo would execute.
fn list_tests(
    launcher: &ArtifactLauncher,
    executable: &Path,
    filters: &[String],
    environment: &[(&'static str, OsString)],
) -> Result<Vec<String>, RustTestRunnerError> {
    let output = launcher
        .command(executable, None)
        .args(["--list", "--format", "terse"])
        .args(filters)
        .envs(environment.iter().map(|(key, value)| (key, value)))
        .launch_output()
        .map_err(|error| RustTestRunnerError::Launch(error.to_string()))?;
    if !output.status.success() {
        // A libtest binary that dies on a signal writes nothing to stderr, so
        // reporting stderr alone produces an empty, undiagnosable message.
        // Name the artifact and how it ended, and fall back to stdout.
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        let detail = if !stderr.is_empty() {
            stderr
        } else if !stdout.is_empty() {
            format!("no stderr; stdout was {stdout}")
        } else {
            "no output on either stream".to_owned()
        };
        return Err(RustTestRunnerError::ListFailed(format!(
            "{} exited with {} when asked to --list: {detail}",
            executable.display(),
            output.status
        )));
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

/// What one test process recorded: the coverage snapshot and the assertion
/// phases the evidence witnesses.
pub(crate) struct RustEvidence {
    pub(crate) snapshot: RuntimeSnapshot,
    pub(crate) phases: Vec<CoveragePhase>,
}

/// One record a thread wrote, waiting to learn whether an assertion follows.
struct PendingRecord {
    event_type: &'static str,
    id: String,
    vector: Option<McdcVector>,
    sequence: i64,
}

/// `attempt` names what this evidence belongs to, so its phase IDs are
/// unique across the run: every test runs in a process of its own and numbers
/// its records from one.
pub(crate) fn snapshot(
    manifest: &CoverageManifest,
    probes: &crate::rust_instrumenter::RustProbeLayout,
    directory: &Path,
    attempt: &str,
) -> Result<RustEvidence, RustTestRunnerError> {
    // Evidence files are named by the instrumentation that wrote them. A
    // test may build and run a program instrumented on its own -- a fixture
    // prepared inside the instrumented workspace -- and that program
    // inherits the evidence directory; its obligations are not this run's.
    let regions = read_rust_region_directory(directory, &probes.token, probes)
        .map_err(|error| RustTestRunnerError::Probe(error.to_string()))?
        .into_iter()
        .map(|(_, observations)| observations);
    snapshot_regions(manifest, regions, attempt)
}

/// `snapshot` of regions already read: each one's records in the order its
/// process wrote them. A server's windows arrive this way.
pub(crate) fn snapshot_regions(
    manifest: &CoverageManifest,
    regions: impl IntoIterator<Item = Vec<crate::rust_runtime::RustProbeEntry>>,
    attempt: &str,
) -> Result<RustEvidence, RustTestRunnerError> {
    let points = manifest
        .points
        .iter()
        .map(|point| (point.id.as_str(), point))
        .collect::<BTreeMap<_, _>>();
    // An assertion in test code, which is not measured, names a site instead.
    let asserting = manifest
        .points
        .iter()
        .chain(&manifest.assertion_sites)
        .map(|point| (point.id.as_str(), point))
        .collect::<BTreeMap<_, _>>();
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
    // Records a thread wrote since its last assertion. An assertion witnesses
    // its own thread's records and no other's: a test that spawns threads
    // interleaves them in one file.
    let mut pending = BTreeMap::<u64, Vec<PendingRecord>>::new();
    let mut events = Vec::new();
    let mut phases = Vec::new();
    // Records carry no clock. The order they were written in is what matters,
    // and a sequence number preserves exactly that.
    let mut sequence = 0_i64;
    for observations in regions {
        for entry in observations {
            sequence += 1;
            let thread = entry.thread;
            match entry.observation {
                RustProbeObservation::Hit { id } => {
                    if !points.contains_key(id.as_str()) && !alternatives.contains(id.as_str()) {
                        return Err(RustTestRunnerError::UnknownProbe(id));
                    }
                    hits.insert(id.clone());
                    pending.entry(thread).or_default().push(PendingRecord {
                        event_type: "hit",
                        id,
                        vector: None,
                        sequence,
                    });
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
                        .insert((values.clone(), outcome));
                    pending.entry(thread).or_default().push(PendingRecord {
                        event_type: "decision",
                        id,
                        vector: Some(McdcVector { values, outcome }),
                        sequence,
                    });
                }
                RustProbeObservation::Assertion { id } => {
                    // The marker names the statement that asserts: a point
                    // of this manifest, or an assertion site in test code.
                    let Some(point) = asserting.get(id.as_str()) else {
                        return Err(RustTestRunnerError::UnknownProbe(id));
                    };
                    let witnessed = pending.remove(&thread).unwrap_or_default();
                    let phase_id = format!("{attempt}:assertion:{sequence}");
                    phases.push(CoveragePhase {
                        id: phase_id.clone(),
                        kind: "assertion".into(),
                        operation: format!(
                            "Rust assertion at {}:{}:{}",
                            point.file, point.line, point.column
                        ),
                        source: Some(point.source.clone()),
                        caused_by_phase_id: None,
                        started_at_ms: witnessed.first().map_or(sequence, |record| record.sequence),
                        ended_at_ms: Some(sequence),
                        // Reaching the marker is the proof: a failing
                        // assertion panics before it.
                        status: Some("passed".into()),
                        error: None,
                    });
                    for record in witnessed {
                        events.push(RuntimeEvent {
                            event_type: record.event_type.into(),
                            id: record.id,
                            vector: record.vector,
                            timestamp_ms: record.sequence,
                            phase_id: Some(phase_id.clone()),
                            environment: "server".into(),
                        });
                    }
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
    Ok(RustEvidence {
        snapshot: RuntimeSnapshot {
            decisions: decision_snapshots,
            hits: hits.into_iter().collect(),
            events,
            logicals: Vec::new(),
            phase_id: None,
        },
        phases,
    })
}

pub(crate) fn rust_coverage_model() -> CoverageModelDeclaration {
    CoverageModelDeclaration {
        language: "rust".into(),
        variant: "rust-owned-probes-v1".into(),
        name: "supercov-rust-owned-v1".into(),
        completeness_meaning: "Every semantics-proven Rust obligation in the owned source denominator was observed; explicit manifest limitations identify unmeasured Rust surfaces.".into(),
        measured: vec![
            "owned Rust statements and function entries".into(),
            "owned atomic condition vectors and decision outcomes".into(),
            "exact process-per-libtest attribution".into(),
            "exact process-per-doctest attribution".into(),
            "evidence a passing assertion of the same thread witnessed".into(),
        ],
        not_measured: vec![
            "macro-expanded and generated Rust code".into(),
            "const-evaluated code and unsupported structural branch probes".into(),
            "causal linkage to individual actions".into(),
            "all input values, semantic partitions, paths, or concurrency interleavings".into(),
            "mutation score or assertion fault-detection strength".into(),
        ],
    }
}

/// The user's RUSTFLAGS, then a cap so the HOST crate's lint policy cannot
/// reject generated sources: serde builds with `#![deny(warnings)]`, and
/// http's `if ({ frame ... })` decision wrapping trips `unused_parens` into a
/// hard error under it. The user's own `cargo test` runs are unaffected.
/// The stack an instrumented test thread gets. Probes add stack to every
/// frame -- a decision frame, a `?` operand copied through its probe, a
/// wrapper's temporaries -- and a test that recurses to a depth chosen against
/// the default 2 MiB (serde_json's recursion-limit test) overflows under
/// instrumentation. libtest sizes test threads from `RUST_MIN_STACK`, so an
/// instrumented process gets 16 MiB unless the user chose a size. The
/// reservation is virtual; only touched pages cost memory.
pub(crate) fn instrumented_stack_environment() -> Vec<(&'static str, &'static str)> {
    stack_environment(std::env::var_os("RUST_MIN_STACK").is_some())
}

/// `instrumented_stack_environment`, knowing whether the user chose a size.
fn stack_environment(chosen: bool) -> Vec<(&'static str, &'static str)> {
    if chosen {
        Vec::new()
    } else {
        vec![("RUST_MIN_STACK", "16777216")]
    }
}

/// One path the selected toolchain reports through `rustc --print`: the
/// toolchain the command names with `+toolchain`, else the one rustup picks
/// in the workspace, as it does for Cargo there.
fn rustc_print_path(
    project: &PreparedRustProject,
    invocation: &CargoTestInvocation,
    request: &str,
) -> Result<PathBuf, RustTestRunnerError> {
    let selector = invocation.arguments[..invocation.command_position()]
        .iter()
        .find(|argument| argument.starts_with('+'));
    let output = Command::new("rustc")
        .args(selector)
        .args(["--print", request])
        .current_dir(&project.workspace_root)
        .launch_output()
        .map_err(|error| {
            RustTestRunnerError::Launch(format!("rustc --print {request}: {error}"))
        })?;
    if !output.status.success() {
        return Err(RustTestRunnerError::Launch(format!(
            "rustc --print {request} exited with {}",
            output.status
        )));
    }
    Ok(PathBuf::from(
        String::from_utf8_lossy(&output.stdout).trim(),
    ))
}

/// The selected toolchain's sysroot, as `rustc --print sysroot` reports it.
pub(crate) fn rustc_sysroot(
    project: &PreparedRustProject,
    invocation: &CargoTestInvocation,
) -> Result<PathBuf, RustTestRunnerError> {
    rustc_print_path(project, invocation, "sysroot")
}

/// The directory holding the toolchain's own dynamic libraries
/// (`<sysroot>/lib/rustlib/<host>/lib`), where libstd's dylib lives; the
/// plain `<sysroot>/lib` no longer holds it.
pub(crate) fn rustc_target_libdir(
    project: &PreparedRustProject,
    invocation: &CargoTestInvocation,
) -> Result<PathBuf, RustTestRunnerError> {
    rustc_print_path(project, invocation, "target-libdir")
}

/// The dynamic library search path Cargo gives a test binary it runs: the
/// artifact's own directories and the toolchain's target library directory.
/// A proc-macro crate's test harness links libstd dynamically -- async-trait's
/// and serde_derive's cannot even list their tests without this -- and any
/// dylib dependency is found the same way. The user's own path follows.
pub(crate) fn dynamic_library_environment(
    target_libdir: &Path,
    executable: &Path,
) -> Vec<(&'static str, OsString)> {
    library_path_environment(
        target_libdir,
        executable,
        std::env::var_os(LIBRARY_PATH_VARIABLE),
    )
}

/// The variable a platform's loader searches for libraries.
#[cfg(target_os = "macos")]
const LIBRARY_PATH_VARIABLE: &str = "DYLD_FALLBACK_LIBRARY_PATH";
#[cfg(windows)]
const LIBRARY_PATH_VARIABLE: &str = "PATH";
#[cfg(not(any(target_os = "macos", windows)))]
const LIBRARY_PATH_VARIABLE: &str = "LD_LIBRARY_PATH";

/// `dynamic_library_environment`, with the user's own path, `existing`.
fn library_path_environment(
    target_libdir: &Path,
    executable: &Path,
    existing: Option<OsString>,
) -> Vec<(&'static str, OsString)> {
    let variable = LIBRARY_PATH_VARIABLE;
    let mut entries = Vec::new();
    if let Some(deps) = executable.parent() {
        entries.push(deps.to_path_buf());
        if let Some(profile) = deps.parent() {
            entries.push(profile.to_path_buf());
        }
    }
    entries.push(target_libdir.to_path_buf());
    if let Some(existing) = existing {
        entries.extend(std::env::split_paths(&existing));
    }
    match std::env::join_paths(entries) {
        Ok(value) => vec![(variable, value)],
        Err(_) => Vec::new(),
    }
}

/// The compiler's flags for what Supercov builds: the project's own, with no
/// lint reported. What is compiled is the rewritten source, and a lint on it
/// is about Supercov's text: `cargo nextest run`, which shows its build,
/// printed five warnings of unnecessary parentheses for a ten-line file, each
/// quoting a line of probes. A project that denies warnings still builds.
///
/// Building for wasm, lints are capped at `warn` instead: Cargo learns which
/// crate types a target supports from rustc's "dropping unsupported crate
/// type" warnings, and wasm32 drops `dylib` and `proc-macro`, so with every
/// lint allowed it cannot read the answer and stops. The probes'
/// parentheses, the one lint they raise, are allowed by name.
pub(crate) fn capped_rustflags(
    project: &PreparedRustProject,
    invocation: &CargoTestInvocation,
) -> String {
    let plan = crate::rust_cargo_configuration::resolve_cargo_runner_plan(
        &project.workspace_root,
        &project.workspace_root,
        invocation,
    )
    .ok();
    let wasm = plan.as_ref().is_some_and(|plan| {
        plan.targets
            .iter()
            .any(|target| target.target.starts_with("wasm"))
    });
    // `RUSTFLAGS` replaces the flags Cargo's configuration gives rustc
    // (`build.rustflags`, `target.<triple>.rustflags`), so where the user set
    // none, the configuration's come first.
    let mut rustflags = std::env::var("RUSTFLAGS").unwrap_or_else(|_| {
        plan.and_then(|plan| plan.rustflags)
            .unwrap_or_default()
            .join(" ")
    });
    if !rustflags.is_empty() {
        rustflags.push(' ');
    }
    rustflags.push_str(if wasm {
        "--cap-lints=warn -A unused_parens"
    } else {
        "--cap-lints=allow"
    });
    rustflags
}

pub(crate) fn io_error(error: impl std::fmt::Display) -> RustTestRunnerError {
    RustTestRunnerError::Io(error.to_string())
}

/// A libtest case that exited cleanly but ran nothing, or ran its one test as
/// ignored, was skipped rather than passed.
/// Cargo's line for a test binary: `Running unittests src/lib.rs (target/…)`.
/// The source is named from its package and the binary from the workspace, as
/// Cargo names them.
fn running_line(
    workspace: &Path,
    target: &Path,
    artifact: &TestArtifact,
    coloured: bool,
) -> String {
    // Both as text with one separator and no `\\?\` prefix, which Windows
    // puts on one of them and not the other: the source then came out
    // whole and with `/`, `tests/outside.rs` where Cargo writes
    // `tests\outside.rs`.
    let text = |path: &Path| {
        let text = path.to_string_lossy().replace('\\', "/");
        text.strip_prefix("//?/").unwrap_or(&text).to_owned()
    };
    let package = text(&artifact.package_directory);
    let package = package
        .strip_prefix(&text(workspace))
        .map_or("", |inside| inside.trim_matches('/'));
    let source = artifact
        .source
        .strip_prefix(package)
        .map_or(artifact.source.as_str(), |local| {
            local.trim_start_matches('/')
        })
        .split('/')
        .collect::<PathBuf>();
    // Supercov builds into a target directory of its own; the binary is named
    // where Cargo would have put it.
    let executable = artifact.executable.strip_prefix(target).map_or_else(
        |_| artifact.executable.clone(),
        |built| Path::new("target").join(built),
    );
    format!(
        "{} {}{} ({})",
        if coloured {
            "\u{1b}[1m\u{1b}[92m     Running\u{1b}[0m"
        } else {
            "     Running"
        },
        if artifact.kind == "integration" {
            ""
        } else {
            "unittests "
        },
        source.display(),
        executable.display()
    )
}

/// Whether Cargo colours its own lines: as `--color` or `CARGO_TERM_COLOR`
/// says, and otherwise when they go to a terminal that has not asked for
/// none.
fn cargo_colours(invocation: &CargoTestInvocation) -> bool {
    use std::io::IsTerminal;
    cargo_colour_choice(
        colour_flag(&invocation.arguments).as_deref(),
        &|name| std::env::var(name).ok(),
        std::io::stderr().is_terminal(),
    )
}

/// What Cargo decides from what was asked (`--color`, or else
/// `CARGO_TERM_COLOR`), its environment, and whether its lines go to a
/// terminal.
fn cargo_colour_choice(
    asked: Option<&str>,
    variable: &dyn Fn(&str) -> Option<String>,
    terminal: bool,
) -> bool {
    let set = |name: &str| variable(name).filter(|value| !value.is_empty());
    let asked = asked.map(str::to_owned).or_else(|| set("CARGO_TERM_COLOR"));
    match asked.as_deref() {
        Some("always") => true,
        Some("never") => false,
        // As Cargo decides it when left to: forced, refused, and otherwise
        // for a terminal that can show colours.
        _ if set("CLICOLOR_FORCE").is_some_and(|value| value != "0") => true,
        _ if set("NO_COLOR").is_some() || set("CLICOLOR").is_some_and(|value| value == "0") => {
            false
        }
        _ => shows_colours(terminal, set("TERM").as_deref()),
    }
}

/// Whether libtest colours its results: as its own `--color` says, and
/// otherwise when they go to a terminal that can show colours. A test's
/// process writes to a pipe, so it is told.
fn libtest_colours(invocation: &CargoTestInvocation) -> bool {
    use std::io::IsTerminal;
    match colour_flag(&invocation.runner_arguments).as_deref() {
        Some("always") => true,
        Some("never") => false,
        _ => shows_colours(
            std::io::stdout().is_terminal(),
            std::env::var("TERM").ok().as_deref(),
        ),
    }
}

/// `--color=<when>` or `--color <when>`, as Cargo and libtest both take it.
fn colour_flag(arguments: &[String]) -> Option<String> {
    arguments.iter().enumerate().find_map(|(index, argument)| {
        argument
            .strip_prefix("--color=")
            .map(str::to_owned)
            .or_else(|| (argument == "--color").then(|| arguments.get(index + 1).cloned())?)
    })
}

/// Whether output to this stream can be coloured: a terminal whose `TERM`
/// is not empty or `dumb` (Windows consoles need no `TERM`).
fn shows_colours(terminal: bool, term: Option<&str>) -> bool {
    #[cfg(windows)]
    {
        let _ = term;
        terminal
    }
    #[cfg(not(windows))]
    {
        terminal && term.is_some_and(|term| !term.is_empty() && term != "dumb")
    }
}

/// `cargo test -q`.
pub(crate) fn cargo_quiet(invocation: &CargoTestInvocation) -> bool {
    invocation.arguments[invocation.command_position() + 1..]
        .iter()
        .any(|argument| matches!(argument.as_str(), "-q" | "--quiet"))
}

/// libtest's own `-q` or `--format terse`: a character for each test.
pub(crate) fn libtest_quiet(invocation: &CargoTestInvocation) -> bool {
    let runner = &invocation.runner_arguments;
    runner
        .iter()
        .any(|argument| matches!(argument.as_str(), "-q" | "--quiet" | "--format=terse"))
        || runner
            .windows(2)
            .any(|pair| pair[0] == "--format" && pair[1] == "terse")
}

pub(crate) fn libtest_skipped(exit: i32, stdout: &str) -> bool {
    exit == 0 && (stdout.contains("running 0 tests") || stdout.contains("; 1 ignored;"))
}

/// Limitation IDs are unique across a declaration, so each runner names its
/// own: the libtest runner keeps the original IDs, rustdoc's carry its name.
fn rust_runner_limitations(runner: &str) -> Vec<FrontendLimitation> {
    let prefix = if runner == "rust-libtest" {
        "rust".to_owned()
    } else {
        runner.to_owned()
    };
    vec![
        FrontendLimitation {
            id: format!("{prefix}-action-linkage-unavailable"),
            scopes: vec![FrontendLimitationScope::Action],
            reason: "Rust test frameworks expose no general action lifecycle".into(),
        },
        FrontendLimitation {
            id: format!("{prefix}-assertion-linkage-unavailable"),
            scopes: vec![FrontendLimitationScope::Assertion],
            reason: "assertion macros do not expose a stable per-assertion success lifecycle"
                .into(),
        },
    ]
}

pub(crate) fn rust_runner_declaration(runner: &str) -> FrontendRunnerDeclaration {
    FrontendRunnerDeclaration {
        runner: runner.into(),
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
        limitations: rust_runner_limitations(runner),
    }
}

/// Obligations in files the build never read, declined rather than counted.
/// Returns the manifest to report against and the files it declined.
///
/// The obligations stay in the manifest: the evidence files are named by a
/// token derived from its obligation IDs, so removing them would orphan every
/// record the run wrote. `unmeasured` is what the report reads to take an
/// obligation out of the covered/uncovered denominator without pretending it
/// was never there.
fn decline_uncompiled_sources(
    project: &PreparedRustProject,
    also_compiled: &BTreeSet<String>,
) -> (CoverageManifest, BTreeSet<String>) {
    let mut manifest = project.manifest.clone();
    let Some(compiled) = crate::rust_project::compiled_source_files(
        &project.workspace_root,
        &project.target_directory,
    ) else {
        return (manifest, BTreeSet::new());
    };
    let declined = project
        .source_files
        .iter()
        .filter(|file| !compiled.contains(*file) && !also_compiled.contains(*file))
        .cloned()
        .collect::<BTreeSet<_>>();
    // Depinfo that accounts for no crate root at all is depinfo this run did
    // not produce; declining on it would empty the denominator.
    if declined.is_empty()
        || project
            .crate_roots
            .iter()
            .all(|root| !compiled.contains(root))
    {
        return (manifest, BTreeSet::new());
    }
    let mut unmeasured = manifest.unmeasured.iter().cloned().collect::<BTreeSet<_>>();
    unmeasured.extend(
        manifest
            .points
            .iter()
            .filter(|point| declined.contains(&point.file))
            .map(|point| point.id.clone()),
    );
    unmeasured.extend(
        manifest
            .decisions
            .iter()
            .filter(|decision| declined.contains(&decision.file))
            .map(|decision| decision.id.clone()),
    );
    unmeasured.extend(
        manifest
            .branches
            .iter()
            .filter(|branch| declined.contains(&branch.file))
            .map(|branch| branch.id.clone()),
    );
    manifest.unmeasured = unmeasured.into_iter().collect();
    // A boundary declared inside a file that was never compiled says nothing.
    manifest.limitations.retain(|limitation| {
        limitation
            .get("file")
            .and_then(|file| file.as_str())
            .is_none_or(|file| !declined.contains(file))
    });
    // And the scope says so, file by file.
    if let Some(entries) = manifest
        .scope
        .as_mut()
        .and_then(|scope| scope.get_mut("entries"))
        .and_then(serde_json::Value::as_array_mut)
    {
        for entry in entries
            .iter_mut()
            .filter_map(serde_json::Value::as_object_mut)
        {
            let named = entry
                .get("file")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|file| declined.contains(file));
            if named {
                entry.insert("status".into(), "excluded".into());
                entry.insert(
                    "reason".into(),
                    crate::rust_project::RUST_NOT_COMPILED.into(),
                );
            }
        }
    }
    (manifest, declined)
}

/// Take out the code a `#[cfg]` kept from every build that ran. Each crate's
/// runtime wrote what `cfg!` said of the predicates in its files; code no
/// build compiled could never have run, as the code of a file no build read
/// (`decline_uncompiled_sources`). A gate no process of its crate answered
/// for stays counted, and is said to.
fn settle_cfg_gates(
    project: &PreparedRustProject,
    evidence_root: &Path,
    diagnostics: &mut dyn Write,
) -> Result<CoverageManifest, RustTestRunnerError> {
    let probes = &project.probes;
    if probes.cfgs.is_empty() {
        return Ok(project.manifest.clone());
    }
    let settled =
        crate::rust_runtime::read_rust_cfg_settlement(evidence_root, &probes.token, probes)
            .map_err(|error| RustTestRunnerError::Probe(error.to_string()))?;
    settle_cfg_gates_with(&project.manifest, probes, &settled, diagnostics)
}

/// `settle_cfg_gates` with what the builds said already gathered: a byte per
/// gate, bit 0 left out somewhere, bit 1 built somewhere.
pub(crate) fn settle_cfg_gates_with(
    manifest: &CoverageManifest,
    probes: &crate::rust_instrumenter::RustProbeLayout,
    settled: &[u8],
    diagnostics: &mut dyn Write,
) -> Result<CoverageManifest, RustTestRunnerError> {
    let mut manifest = manifest.clone();
    let left_out = probes
        .cfgs
        .iter()
        .zip(settled)
        .filter(|(_, bits)| **bits == 1)
        .map(|(gate, _)| gate)
        .collect::<Vec<_>>();
    let removed = left_out
        .iter()
        .flat_map(|gate| gate.obligations.iter().map(String::as_str))
        .collect::<BTreeSet<_>>();
    let unanswered = probes
        .cfgs
        .iter()
        .zip(settled)
        .filter(|(gate, bits)| {
            **bits == 0
                && gate
                    .obligations
                    .iter()
                    .any(|id| !removed.contains(id.as_str()))
        })
        .map(|(gate, _)| gate)
        .collect::<Vec<_>>();
    let place = |gate: &&crate::rust_instrumenter::RustCfgGate| {
        format!("{}:{} cfg({})", gate.file, gate.line, gate.predicate)
    };
    let listed = |gates: &[&crate::rust_instrumenter::RustCfgGate]| {
        let mut text = gates
            .iter()
            .take(3)
            .map(place)
            .collect::<Vec<_>>()
            .join(", ");
        if gates.len() > 3 {
            text.push_str(", ...");
        }
        text
    };
    if !left_out.is_empty() {
        writeln!(
            diagnostics,
            "[supercov] {} item(s) behind a #[cfg] no build compiled are outside the denominator: {}",
            left_out.len(),
            listed(&left_out)
        )
        .map_err(|error| RustTestRunnerError::Io(error.to_string()))?;
    }
    if !unanswered.is_empty() {
        writeln!(
            diagnostics,
            "[supercov] {} item(s) behind a #[cfg] are counted though no test ran code of their crate to show whether it was built: {}",
            unanswered.len(),
            listed(&unanswered)
        )
        .map_err(|error| RustTestRunnerError::Io(error.to_string()))?;
    }
    if removed.is_empty() {
        return Ok(manifest);
    }
    manifest
        .points
        .retain(|point| !removed.contains(point.id.as_str()));
    manifest
        .decisions
        .retain(|decision| !removed.contains(decision.id.as_str()));
    manifest
        .branches
        .retain(|branch| !removed.contains(branch.id.as_str()));
    manifest
        .unmeasured
        .retain(|id| !removed.contains(id.as_str()));
    // A boundary declared inside code no build compiled says nothing.
    manifest.limitations.retain(|limitation| {
        let file = limitation.get("file").and_then(serde_json::Value::as_str);
        let line = limitation
            .get("line")
            .and_then(serde_json::Value::as_u64)
            .map(|line| line as usize);
        !left_out.iter().any(|gate| {
            file == Some(gate.file.as_str())
                && line.is_some_and(|line| (gate.line..=gate.end_line).contains(&line))
        })
    });
    Ok(manifest)
}

/// The manifest's structural limitation IDs, as the declaration references them.
pub(crate) fn structural_limitations(manifest: &CoverageManifest) -> Vec<String> {
    manifest
        .limitations
        .iter()
        .filter_map(|item| {
            item.get("id")
                .and_then(|value| value.as_str())
                .map(str::to_owned)
        })
        .collect()
}

/// The files of the crates `keys` names.
fn worker_files(project: &PreparedRustProject, keys: &BTreeSet<String>) -> BTreeSet<String> {
    keys.iter()
        .filter_map(|key| project.probes.crates.get(key))
        .flatten()
        .cloned()
        .collect()
}

/// A Rust Worker's code runs in Cloudflare's runtime, and only when a test
/// starts it with `wrangler dev`; plain `cargo test` compiles the crate and
/// never runs it. Where no test of the run did, its files are not measured,
/// and say why, rather than reading as code the tests missed.
///
/// Where tests did start `wrangler dev` and none loaded this crate's measured
/// build, the files name what they started, and why a configuration was left
/// unmeasured where it was. Returns each file left unmeasured, with why.
fn note_unrun_workers(
    project: &mut PreparedRustProject,
    built_crates: &BTreeSet<String>,
    remote: bool,
    started: &[String],
) -> Vec<(String, String)> {
    let mut started = started.to_vec();
    started.sort();
    started.dedup();
    let reason = if remote {
        "A Rust Worker started with `wrangler dev --remote`: it ran on Cloudflare's edge, where wrangler opens no inspector, so what it ran cannot be read; a test that starts it with `wrangler dev` measures it".to_owned()
    } else if started.is_empty() {
        "A Rust Worker: its code runs in Cloudflare's runtime, when a test starts it with `wrangler dev`, and no test of this run did".to_owned()
    } else {
        format!(
            "A Rust Worker no test's `wrangler dev` ran: the tests started {}, and none loaded a build of this crate's measured source; a configuration whose `[build]` builds the crate with worker-build, or whose `main` imports such a build, is measured. A build made before the run, such as a `build/` already in the project, is of the project's source: build the Worker in the measured command or its tests",
            started.join("; ")
        )
    };
    let ran = worker_files(project, built_crates);
    let unrun = worker_files(project, &project.probes.workers)
        .into_iter()
        .filter(|file| !ran.contains(file) && project.source_files.contains(file))
        .collect::<BTreeSet<_>>();
    // A reused build's manifest carries the notes of the run that built it;
    // which Workers ran, and why one did not, are this run's to say.
    project.manifest.limitations.retain(|limitation| {
        !limitation
            .get("id")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|id| id.starts_with("rust-worker-not-run#"))
    });
    for file in &unrun {
        let id = format!("rust-worker-not-run#{file}");
        // The whole file: the report takes it out of the denominator and
        // says why, rather than counting its lines as gaps.
        project.manifest.limitations.push(serde_json::json!({
            "id": id,
            "kind": "source-scope",
            "file": file,
            "line": 1,
            "column": 0,
            "source": "",
            "reason": reason,
            "blocking": false,
            "wholeFile": true,
        }));
    }
    unrun
        .into_iter()
        .map(|file| (file, reason.clone()))
        .collect()
}

/// The first three of `files`, and `, ...` after them when there are more.
fn first_files<'a>(files: impl ExactSizeIterator<Item = &'a str>) -> String {
    let more = if files.len() > 3 { ", ..." } else { "" };
    format!("{}{more}", files.take(3).collect::<Vec<_>>().join(", "))
}

/// Says which files the build left out of the denominator.
fn report_declined(
    declined: &BTreeSet<String>,
    diagnostics: &mut dyn Write,
) -> Result<(), RustTestRunnerError> {
    if declined.is_empty() {
        return Ok(());
    }
    writeln!(
        diagnostics,
        "[supercov] {} source file(s) this build did not compile are outside the denominator: {}",
        declined.len(),
        first_files(declined.iter().map(String::as_str)),
    )
    .map_err(|error| RustTestRunnerError::Io(error.to_string()))
}

/// Says, after the tests' own output, which Worker files the run could not
/// measure and why: they are out of the totals, and `runs check` refuses to
/// judge a run that left them unmeasured.
fn report_unmeasured_workers(
    unmeasured: &[(String, String)],
    diagnostics: &mut dyn Write,
) -> Result<(), RustTestRunnerError> {
    let Some((_, reason)) = unmeasured.first() else {
        return Ok(());
    };
    writeln!(
        diagnostics,
        "[supercov] not measured: {} Rust Worker file(s), {}. {reason}.",
        unmeasured.len(),
        first_files(unmeasured.iter().map(|(file, _)| file.as_str())),
    )
    .map_err(|error| RustTestRunnerError::Io(error.to_string()))
}

/// `rust_workers`: the stand-in for `wrangler` the test processes find
/// first, when the workspace has a Rust Worker a test may start.
pub fn run_prepared_rust_tests(
    project: &PreparedRustProject,
    command: &[String],
    run_id: &str,
    generated_at: &str,
    rust_workers: Option<&crate::rust_worker::CargoTestWorkers>,
    runner_environment: &[(String, String)],
    diagnostics: &mut dyn Write,
) -> Result<RustFrontendRun, RustTestRunnerError> {
    let invocation = cargo_invocation(&project.workspace_root, command)?;
    let selection = rust_cargo_execution_selection(&invocation)?;
    let build_started = Instant::now();
    // `cargo test --doc` alone builds nothing here: Cargo refuses `--no-run`
    // with `--doc`, and the doctest phase below builds what it runs.
    // nextest builds for itself; Cargo builds the libtest artifacts here.
    let artifacts = if selection.run_libtests && invocation.kind == RustCargoCommandKind::CargoTest
    {
        build_test_artifacts(project, command)?
    } else {
        Vec::new()
    };
    let build_ms = build_started.elapsed().as_secs_f64() * 1000.0;
    // The denominator is what the compiler built, not what the module tree
    // resolves: a module behind a `#[cfg]` that is off is resolved, never
    // compiled, and could never be covered. A Rust Worker's crate counts
    // though `cargo test` may never compile it: worker-build does, when a
    // test starts the Worker, and runs that started different Workers have
    // one denominator.
    let (manifest, declined) =
        decline_uncompiled_sources(project, &worker_files(project, &project.probes.workers));
    report_declined(&declined, diagnostics)?;
    let project = &PreparedRustProject {
        manifest,
        ..project.clone()
    };
    // A signal that reaches the runner alone must not leave the test
    // processes running: on 2026-09-07 an orphaned tokio test spun on two
    // cores for an hour. The guard covers every child spawned below.
    let _signal_guard = crate::child_signal_guard::ChildSignalGuard::install()
        .map_err(|error| RustTestRunnerError::Launch(error.to_string()))?;
    let evidence_root = project
        .workspace_root
        .join(".supercov/rust-evidence")
        .join(run_id);
    fs::create_dir_all(&evidence_root)
        .map_err(|error| RustTestRunnerError::Io(error.to_string()))?;
    let mut results = Vec::new();
    let mut overall_exit = 0;
    let execution_started = Instant::now();
    if invocation.kind == RustCargoCommandKind::NextestRun {
        // nextest builds, schedules and retries on its own; this program is
        // its target runner and records every attempt it launches.
        let outcome = crate::rust_owned_nextest::run_nextest(
            project,
            &invocation,
            &evidence_root.join("nextest"),
            run_id,
            rust_workers,
            diagnostics,
        )?;
        let mut project = project.clone();
        let unmeasured_workers = note_unrun_workers(
            &mut project,
            &outcome.built_crates,
            outcome.remote_workers,
            &outcome.started_workers,
        );
        let project = &project;
        let artifact_count = outcome.artifact_files.len();
        let project = &PreparedRustProject {
            manifest: settle_cfg_gates(project, &evidence_root, diagnostics)?,
            ..project.clone()
        };
        report_unmeasured_workers(&unmeasured_workers, diagnostics)?;
        return Ok(RustFrontendRun {
            declaration: FrontendRunDeclaration {
                protocol_version: LANGUAGE_FRONTEND_PROTOCOL_VERSION,
                frontend_id: "rust".into(),
                frontend_version: "rust-owned-v1".into(),
                language: "rust".into(),
                structural_source: StructuralSource::OwnedProbes,
                runners: vec![rust_runner_declaration("nextest")],
                structural_limitations: structural_limitations(&project.manifest),
            },
            request: CoverageReportRequest {
                run_id: run_id.into(),
                manifest: project.manifest.clone(),
                raw_results: outcome.results,
                generated_at: generated_at.into(),
                coverage_model: Some(rust_coverage_model()),
                integrity: None,
                test_exit_code: ExitCodeInput::Present(Some(outcome.exit_code)),
            },
            exit_code: outcome.exit_code,
            artifacts: artifact_count,
            artifact_files: outcome.artifact_files,
            build_ms,
            execution_ms: execution_started.elapsed().as_secs_f64() * 1000.0,
        });
    }
    // The user's libtest filters, validated against the discovery contract.
    // This was computed and never used: every artifact was listed bare, and
    // Supercov ran tests plain Cargo had been told to skip. It is read here,
    // after the nextest branch: a nextest command carries no libtest
    // selection to reconstruct, and nextest applies the user's filters itself.
    let libtest_selection = rust_libtest_selection(&invocation)?;
    // An ignored test the user asked for with `--ignored` is listed, and must
    // then run: `--exact name` alone would report it ignored again.
    let coloured_results = libtest_colours(&invocation);
    let ignored_mode = libtest_selection
        .list_arguments
        .iter()
        .filter(|argument| {
            matches!(
                argument.as_str(),
                "--ignored" | "--include-ignored" | "--exclude-should-panic"
            )
        })
        .cloned()
        .collect::<Vec<_>>();
    let mut tasks = Vec::new();
    // Through the target runner the user configured, when there is one.
    let launcher =
        ArtifactLauncher::for_invocation(project, &invocation, &evidence_root.join("hooks"))?
            .with_environment(runner_environment);
    if let Some(browser) = launcher.unmeasured_browser() {
        writeln!(
            diagnostics,
            "[supercov] wasm tests run in {browser} are not measured: Supercov reads a browser's wasm through Chrome only (`wasm-pack test --chrome`)"
        )
        .map_err(|error| RustTestRunnerError::Io(error.to_string()))?;
    }
    let target_libdir = if artifacts.is_empty() {
        PathBuf::new()
    } else {
        rustc_target_libdir(project, &invocation)?
    };
    // A binary none of whose tests were asked for, and how many it has.
    let mut unasked = Vec::new();
    for (artifact_index, artifact) in artifacts.iter().enumerate() {
        let tests = list_tests(
            &launcher,
            &artifact.executable,
            &libtest_selection.list_arguments,
            &dynamic_library_environment(&target_libdir, &artifact.executable),
        )?;
        if tests.is_empty() {
            let all = list_tests(
                &launcher,
                &artifact.executable,
                &["--include-ignored".to_owned()],
                &dynamic_library_environment(&target_libdir, &artifact.executable),
            )?;
            unasked.push((artifact, all.len()));
        }
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
    // Asked to run without capture, each test does, and what it writes is
    // shown as it writes it: a test that starts a server and prints while it
    // waits is seen waiting.
    let uncaptured = invocation
        .runner_arguments
        .iter()
        .any(|argument| matches!(argument.as_str(), "--nocapture" | "--no-capture"));
    std::thread::scope(|scope| {
        (0..workers).for_each(|_| {
            scope.spawn(|| {
                loop {
                    let index = next.fetch_add(1, Ordering::Relaxed);
                    let Some(task) = tasks.get(index) else { break };
                    let run = |command: &mut Command| {
                        if uncaptured {
                            crate::child_signal_guard::output_streamed(
                                command.arg("--nocapture"),
                                shown_without_capture,
                            )
                        } else {
                            crate::child_signal_guard::output(command)
                        }
                    };
                    let result = run(launcher
                        .command(&task.artifact.executable, Some(&task.directory))
                        // No --nocapture unless asked: libtest's in-memory capture is what
                        // plain `cargo test` gives users, and it re-emits a
                        // failing test's output, which `.output()` still
                        // receives. Streaming instead turns every print in a
                        // hot loop into an unbuffered stderr syscall: bytes'
                        // advance_bytes_mut_remaining_capacity prints per
                        // iteration, and --nocapture alone cost 6.9s of its
                        // 14.4s (baseline 8.0s streamed vs 1.0s captured).
                        .args(["--exact", &task.test])
                        .args(&ignored_mode)
                        .args(coloured_results.then_some("--color=always"))
                        .current_dir(&task.artifact.package_directory)
                        .envs(instrumented_stack_environment())
                        .env(crate::rust_runtime::RUN_ID_ENV, run_id)
                        .envs(dynamic_library_environment(
                            &target_libdir,
                            &task.artifact.executable,
                        ))
                        .envs(crate::rust_runtime::evidence_environment(
                            &project.probes.token,
                            &task.directory,
                        ))
                        .envs(
                            rust_workers
                                .iter()
                                .flat_map(|workers| workers.environment()),
                        ))
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
        });
    });
    let mut outcomes = outcomes
        .into_inner()
        .map_err(|_| RustTestRunnerError::Io("Rust test result lock poisoned".into()))?
        .into_iter()
        .map(|result| result.map_err(RustTestRunnerError::Launch))
        .collect::<Result<Vec<_>, _>>()?;
    outcomes.sort_by_key(|outcome| outcome.task.ordinal);
    // What each test's Worker ran, as region files of its own evidence; and
    // the crates `wrangler dev` built for them, whose files count though the
    // run's own build never compiled them.
    let mut built_crates = BTreeSet::new();
    let mut remote = false;
    let mut started = Vec::new();
    if rust_workers.is_some() {
        for outcome in &outcomes {
            remote |= crate::rust_worker::started_remote(&outcome.task.directory);
            started.extend(crate::rust_worker::started_worker_configs(
                &outcome.task.directory,
            ));
            built_crates.extend(
                crate::rust_worker::materialize_worker_windows(
                    &outcome.task.directory,
                    &project.probes.token,
                )
                .map_err(RustTestRunnerError::Io)?,
            );
        }
    }
    let mut project = project.clone();
    let unmeasured_workers = note_unrun_workers(&mut project, &built_crates, remote, &started);
    let project = &project;
    // What `cargo test` prints for each binary: every test, the failures
    // together, one result.
    // Cargo's `-q` drops its own line for the binary and asks libtest for
    // its terse form; libtest's own `-q` is the terse form alone.
    let unannounced = cargo_quiet(&invocation);
    let terse = unannounced || libtest_quiet(&invocation);
    let coloured = cargo_colours(&invocation);
    let announced = |artifact: &TestArtifact, report: String| {
        (
            (artifact.kind == "integration", artifact.name.clone()),
            crate::rust_libtest_report::Announced {
                heading: (!unannounced).then(|| {
                    running_line(
                        &project.workspace_root,
                        &project.target_directory,
                        artifact,
                        coloured,
                    )
                }),
                report,
            },
        )
    };
    // In Cargo's order: unit tests, then each integration test by name.
    let mut reports = Vec::new();
    for binary in
        outcomes.chunk_by(|left, right| left.task.artifact_index == right.task.artifact_index)
    {
        let texts = binary
            .iter()
            .map(|outcome| {
                (
                    String::from_utf8_lossy(&outcome.output.stdout),
                    String::from_utf8_lossy(&outcome.output.stderr),
                )
            })
            .collect::<Vec<_>>();
        let cases = binary
            .iter()
            .zip(&texts)
            .map(
                |(outcome, (stdout, stderr))| crate::rust_libtest_report::CaseOutput {
                    name: &outcome.task.test,
                    exit: outcome.output.status.code().unwrap_or(1),
                    stdout,
                    stderr,
                },
            )
            .collect::<Vec<_>>();
        let artifact = &binary[0].task.artifact;
        reports.push(announced(
            artifact,
            crate::rust_libtest_report::libtest_report(&cases, terse),
        ));
    }
    for (artifact, all) in unasked {
        reports.push(announced(
            artifact,
            crate::rust_libtest_report::unasked_report(all),
        ));
    }
    reports.sort_by(|(left, _), (right, _)| left.cmp(right));
    let reports = reports
        .into_iter()
        .map(|(_, report)| report)
        .collect::<Vec<_>>();
    crate::rust_libtest_report::write_reports(diagnostics, &reports)
        .map_err(|error| RustTestRunnerError::Io(error.to_string()))?;
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
        let skipped = libtest_skipped(exit, &stdout);
        if exit != 0 {
            overall_exit = exit;
        }
        let evidence = snapshot(&project.manifest, &project.probes, &directory, &attempt_id)?;
        results.push(RawTestResult {
            span: None,
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
    // A nextest run has returned above: this is `cargo test`.
    let doctest_results = if selection.run_doctests {
        crate::rust_owned_doctests::run_doctests(
            project,
            &invocation,
            &selection,
            &evidence_root.join("doctests"),
            run_id,
            diagnostics,
            &mut overall_exit,
        )?
    } else {
        Vec::new()
    };
    // Only observed runners may be declared. A run with no tests at all keeps
    // the libtest declaration, as it always has.
    let project = &PreparedRustProject {
        manifest: settle_cfg_gates(project, &evidence_root, diagnostics)?,
        ..project.clone()
    };
    let ran_libtests = !results.is_empty();
    let ran_doctests = !doctest_results.is_empty();
    results.extend(doctest_results);
    let mut runners = Vec::new();
    if ran_libtests || !ran_doctests {
        runners.push(rust_runner_declaration("rust-libtest"));
    }
    if ran_doctests {
        runners.push(rust_runner_declaration("rustdoc"));
    }
    let structural_limitations = structural_limitations(&project.manifest);
    report_unmeasured_workers(&unmeasured_workers, diagnostics)?;
    Ok(RustFrontendRun {
        declaration: FrontendRunDeclaration {
            protocol_version: LANGUAGE_FRONTEND_PROTOCOL_VERSION,
            frontend_id: "rust".into(),
            frontend_version: "rust-owned-v1".into(),
            language: "rust".into(),
            structural_source: StructuralSource::OwnedProbes,
            runners,
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

    #[test]
    fn each_test_runner_error_says_what_failed() {
        use super::RustTestRunnerError as E;
        let messages = [
            E::UnsupportedCommand("cargo bench is not a test command".into()),
            E::Launch("no cargo".into()),
            E::CargoFailed("exit 101".into()),
            E::CargoJson("eof".into()),
            E::UnsafeArtifact("../t".into()),
            E::ListFailed("exit 1".into()),
            E::Probe("header".into()),
            E::Context("collision".into()),
            E::UnknownProbe("o".into()),
            E::InvalidVector {
                id: "d".into(),
                expected: 2,
                actual: 3,
            },
            E::from(serde_json::from_str::<u8>("x").unwrap_err()),
            E::Io("disk full".into()),
        ]
        .map(|error| error.to_string());
        assert_eq!(
            messages,
            [
                "cargo bench is not a test command",
                "could not launch Rust test process: no cargo",
                "Cargo test build failed: exit 101",
                "invalid Cargo JSON output: eof",
                "Cargo emitted an unsafe test artifact: ../t",
                "could not enumerate Rust tests: exit 1",
                "invalid Rust probe evidence: header",
                "invalid Rust test context: collision",
                "Rust runtime emitted an unknown obligation: o",
                "Rust decision d emitted vector width 3; expected 2",
                "could not encode Rust evidence: expected value at line 1 column 1",
                "disk full",
            ]
        );
    }

    /// `--color` decides when given, in either spelling; otherwise Cargo
    /// follows CLICOLOR_FORCE, NO_COLOR and CLICOLOR, and both colour a
    /// terminal whose TERM is set and not `dumb`.
    #[test]
    fn colours_follow_the_flag_the_environment_and_the_terminal() {
        use super::{cargo_colour_choice, colour_flag, shows_colours};
        let words = |text: &str| text.split(' ').map(str::to_owned).collect::<Vec<_>>();
        assert_eq!(
            colour_flag(&words("test --color=never")).as_deref(),
            Some("never")
        );
        assert_eq!(
            colour_flag(&words("test --color always")).as_deref(),
            Some("always")
        );
        assert_eq!(colour_flag(&words("test --color")), None);
        assert_eq!(colour_flag(&words("test")), None);
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |name: &str| {
                pairs
                    .iter()
                    .find(|(key, _)| *key == name)
                    .map(|(_, value)| (*value).to_owned())
            }
        };
        for (asked, pairs, terminal, coloured) in [
            (Some("always"), &[][..], false, true),
            (Some("never"), &[("CLICOLOR_FORCE", "1")][..], true, false),
            (None, &[("CLICOLOR_FORCE", "1")][..], false, true),
            (
                None,
                &[("CLICOLOR_FORCE", "0"), ("TERM", "xterm")][..],
                true,
                true,
            ),
            (
                None,
                &[("NO_COLOR", "1"), ("TERM", "xterm")][..],
                true,
                false,
            ),
            (None, &[("NO_COLOR", ""), ("TERM", "xterm")][..], true, true),
            (
                None,
                &[("CLICOLOR", "0"), ("TERM", "xterm")][..],
                true,
                false,
            ),
            (
                None,
                &[("CLICOLOR", "1"), ("TERM", "xterm")][..],
                true,
                true,
            ),
            (None, &[("TERM", "xterm")][..], false, false),
            (Some("auto"), &[("TERM", "dumb")][..], true, cfg!(windows)),
            (None, &[][..], true, cfg!(windows)),
            (None, &[("CARGO_TERM_COLOR", "always")][..], false, true),
            (
                Some("never"),
                &[("CARGO_TERM_COLOR", "always")][..],
                false,
                false,
            ),
            (
                None,
                &[("CARGO_TERM_COLOR", ""), ("TERM", "xterm")][..],
                true,
                true,
            ),
        ] {
            assert_eq!(
                cargo_colour_choice(asked, &env(pairs), terminal),
                coloured,
                "{asked:?} {pairs:?} {terminal}"
            );
        }
        assert!(!shows_colours(false, Some("xterm")));
        assert_eq!(shows_colours(true, Some("")), cfg!(windows));
        assert!(shows_colours(true, Some("xterm-256color")));
        let libtest = |choice: &str| {
            super::libtest_colours(
                &super::cargo_invocation(
                    &std::env::temp_dir(),
                    &words(&format!("cargo test -- --color {choice}")),
                )
                .unwrap(),
            )
        };
        assert!(libtest("always"));
        assert!(!libtest("never"));
    }

    #[test]
    fn node_takes_a_hook_by_a_file_url_with_its_bytes_encoded() {
        assert_eq!(
            file_url(Path::new("/tmp/a b/#hook.mjs")),
            "file:///tmp/a%20b/%23hook.mjs"
        );
        assert_eq!(file_url(Path::new("/srv/hook.mjs")), "file:///srv/hook.mjs");
        assert_eq!(file_url(Path::new("")), "file:///");
    }

    #[test]
    fn a_wasi_runtime_is_known_by_its_name() {
        assert!(is_wasi_runtime(Path::new("wasmtime")));
        assert!(is_wasi_runtime(Path::new("/opt/homebrew/bin/wasmtime")));
        assert!(is_wasi_runtime(Path::new("wasmer")));
        assert!(!is_wasi_runtime(Path::new("wasm-bindgen-test-runner")));
        assert!(!is_wasi_runtime(Path::new("qemu-aarch64")));
    }

    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;
    use crate::{
        coverage_report::{ArchiveReportRequest, analyze_coverage_archive},
        evidence_archive::write_archive,
        frontend_protocol::validate_frontend_report_request,
        rust_project::prepare_rust_project,
    };

    /// `cargo +toolchain test` builds with that toolchain, so the rustdoc
    /// that compiles its doctests and the dynamic libraries its tests load
    /// must come from it too.
    #[cfg(unix)]
    #[test]
    fn the_toolchain_is_the_one_the_command_selects() {
        use std::os::unix::fs::PermissionsExt;
        let root = std::env::temp_dir().join(format!(
            "supercov-rust-toolchain-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        // A custom toolchain whose rustc reports a sysroot of its own.
        fs::create_dir_all(root.join("toolchain/bin")).unwrap();
        fs::write(
            root.join("toolchain/bin/rustc.draft"),
            "#!/bin/sh\necho /custom-toolchain-sysroot\n",
        )
        .unwrap();
        let copied = Command::new("cp")
            .arg(root.join("toolchain/bin/rustc.draft"))
            .arg(root.join("toolchain/bin/rustc"))
            .status()
            .unwrap();
        assert!(copied.success());
        fs::set_permissions(
            root.join("toolchain/bin/rustc"),
            fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        fs::create_dir_all(root.join("workspace")).unwrap();
        let project = PreparedRustProject::empty(root.join("workspace"));
        let invocation = |command: &[&str]| {
            cargo_invocation(
                &project.workspace_root,
                &command
                    .iter()
                    .map(|word| (*word).to_owned())
                    .collect::<Vec<_>>(),
            )
            .unwrap()
        };
        // rustup takes a toolchain's directory after `+` as well as its name.
        let selector = format!("+{}", root.join("toolchain").display());
        assert_eq!(
            rustc_sysroot(
                &project,
                &invocation(&["cargo", &selector, "test", "--doc"])
            )
            .unwrap(),
            PathBuf::from("/custom-toolchain-sysroot")
        );
        let default = rustc_sysroot(&project, &invocation(&["cargo", "test", "--doc"])).unwrap();
        assert!(
            default.join("bin/rustdoc").is_file(),
            "{}",
            default.display()
        );
        // A toolchain whose rustc cannot answer stops the run.
        script(&root.join("broken/bin/rustc"), "#!/bin/sh\nexit 3\n");
        let selector = format!("+{}", root.join("broken").display());
        let error = rustc_target_libdir(&project, &invocation(&["cargo", &selector, "test"]))
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("rustc --print target-libdir exited with exit status: 3"),
            "{error}"
        );
        fs::remove_dir_all(root).unwrap();
    }

    /// A directory of its own under the system temporary directory.
    fn scratch(label: &str) -> PathBuf {
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let root = std::env::temp_dir().join(format!(
            "supercov-{label}-{}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();
        root
    }

    /// An executable script, copied into place by another process so that
    /// no descriptor to it is open here while a test thread forks.
    #[cfg(unix)]
    fn script(path: &Path, text: &str) {
        use std::os::unix::fs::PermissionsExt;
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let draft = path.with_extension("draft");
        fs::write(&draft, text).unwrap();
        assert!(
            Command::new("cp")
                .arg(&draft)
                .arg(path)
                .status()
                .unwrap()
                .success()
        );
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn a_listing_that_fails_says_how_with_whatever_the_binary_wrote() {
        let root = scratch("failed-listing");
        let launcher = ArtifactLauncher::default();
        let listed = |name: &str, body: &str| {
            let binary = root.join(name);
            script(&binary, &format!("#!/bin/sh\n{body}\n"));
            list_tests(&launcher, &binary, &[], &[])
                .unwrap_err()
                .to_string()
        };
        assert!(
            listed("stderr", "echo 'cannot start' >&2; echo ignored; exit 2")
                .ends_with("when asked to --list: cannot start"),
        );
        assert!(
            listed("stdout", "echo 'half a listing'; exit 2")
                .ends_with("when asked to --list: no stderr; stdout was half a listing"),
        );
        assert!(
            listed("silent", "kill -9 $$")
                .ends_with("when asked to --list: no output on either stream")
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_wasm_runner_names_the_browser_it_cannot_read() {
        let launcher = || ArtifactLauncher {
            wasm_bindgen_runner: true,
            ..ArtifactLauncher::default()
        };
        let unmeasured = |environment: &[(&str, &str)]| {
            launcher()
                .with_environment(
                    &environment
                        .iter()
                        .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
                        .collect::<Vec<_>>(),
                )
                .unmeasured_browser()
                .map(str::to_owned)
        };
        assert_eq!(
            unmeasured(&[("GECKODRIVER", "/opt/geckodriver")]),
            Some("geckodriver".into())
        );
        // A remote driver runs its tests where Supercov cannot read them,
        // Chrome's as much as any other's.
        assert_eq!(
            unmeasured(&[("CHROMEDRIVER_REMOTE", "http://grid:4444")]),
            Some("a remote chromedriver".into())
        );
        // Tests that run in Node use no browser.
        assert_eq!(
            unmeasured(&[
                ("GECKODRIVER", "/opt/geckodriver"),
                ("WASM_BINDGEN_TEST_ONLY_NODE", "1")
            ]),
            None
        );
        // The same, said in Supercov's own environment rather than the
        // runner's.
        let ambient = |variables: &[(&str, &str)]| {
            let variables = variables
                .iter()
                .map(|(key, value)| ((*key).to_owned(), OsString::from(value)))
                .collect::<BTreeMap<_, _>>();
            launcher()
                .with_environment_from(&[], &|name| variables.get(name).cloned())
                .unmeasured_browser()
                .map(str::to_owned)
        };
        assert_eq!(
            ambient(&[("GECKODRIVER", "/opt/geckodriver")]),
            Some("geckodriver".into())
        );
        // No driver anywhere is no browser; a Chrome driver that cannot
        // start is named with why.
        assert_eq!(ambient(&[]), None);
        assert!(
            ambient(&[("CHROMEDRIVER", "/nonexistent/chromedriver")])
                .is_some_and(|browser| browser.starts_with("Chrome (")),
        );
        assert_eq!(
            ambient(&[
                ("GECKODRIVER", "/opt/geckodriver"),
                ("WASM_BINDGEN_TEST_ONLY_NODE", "1")
            ]),
            None
        );
    }

    /// The user's own stack size and library path are kept: a size they
    /// chose is not overridden, and their path follows Supercov's.
    #[test]
    fn the_user_s_stack_size_and_library_path_are_kept() {
        assert_eq!(stack_environment(true), []);
        assert_eq!(stack_environment(false), [("RUST_MIN_STACK", "16777216")]);
        let user = std::env::join_paths(["/user/a", "/user/b"]).unwrap();
        let environment =
            library_path_environment(Path::new("/lib"), Path::new("/fixture"), Some(user));
        assert_eq!(
            std::env::split_paths(&environment[0].1).collect::<Vec<_>>(),
            ["/", "/lib", "/user/a", "/user/b"].map(PathBuf::from)
        );
        let environment = library_path_environment(Path::new("/lib"), Path::new("/fixture"), None);
        assert_eq!(
            std::env::split_paths(&environment[0].1).collect::<Vec<_>>(),
            ["/", "/lib"].map(PathBuf::from)
        );
        // A directory the variable cannot hold leaves the loader as it was.
        #[cfg(unix)]
        assert_eq!(
            library_path_environment(Path::new("/a:b"), Path::new("/fixture"), None),
            []
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_configured_runner_launches_native_tests_but_not_one_of_several_targets() {
        let root = scratch("artifact-launcher");
        let host = String::from_utf8(Command::new("rustc").arg("-vV").output().unwrap().stdout)
            .unwrap()
            .lines()
            .find_map(|line| line.strip_prefix("host: ").map(str::to_owned))
            .unwrap();
        fs::create_dir_all(root.join(".cargo")).unwrap();
        fs::write(
            root.join(".cargo/config.toml"),
            format!("[target.{host}]\nrunner = ['emulator', '--fast']\n"),
        )
        .unwrap();
        let project = PreparedRustProject::empty(root.clone());
        let invocation = |command: &[&str]| {
            cargo_invocation(
                &root,
                &command
                    .iter()
                    .map(|word| (*word).to_owned())
                    .collect::<Vec<_>>(),
            )
            .unwrap()
        };
        let launcher =
            ArtifactLauncher::for_invocation(&project, &invocation(&["cargo", "test"]), &root)
                .unwrap();
        let command = launcher.command(Path::new("/target/debug/deps/fixture"), None);
        assert_eq!(command.get_program(), "emulator");
        assert_eq!(
            command.get_args().collect::<Vec<_>>(),
            ["--fast", "/target/debug/deps/fixture"]
        );
        // A native target's tests load no wasm hook.
        assert!(!command.get_envs().any(|(name, _)| name == "NODE_OPTIONS"));
        let error = ArtifactLauncher::for_invocation(
            &project,
            &invocation(&[
                "cargo",
                "test",
                &format!("--target={host}"),
                "--target=wasm32-unknown-unknown",
            ]),
            &root,
        )
        .unwrap_err()
        .to_string();
        assert!(
            error.contains("several targets, one of them with a target runner"),
            "{error}"
        );
        // Several targets, none with a runner: each test binary runs itself.
        fs::remove_file(root.join(".cargo/config.toml")).unwrap();
        let launcher = ArtifactLauncher::for_invocation(
            &project,
            &invocation(&[
                "cargo",
                "test",
                &format!("--target={host}"),
                "--target=wasm32-unknown-unknown",
            ]),
            &root,
        )
        .unwrap();
        let command = launcher.command(Path::new("/target/debug/deps/fixture"), None);
        assert_eq!(command.get_program(), "/target/debug/deps/fixture");
        fs::remove_dir_all(root).unwrap();
    }

    /// A run whose wasm tests a browser other than Chrome would run says
    /// they go unmeasured, before it runs anything.
    #[cfg(unix)]
    #[test]
    fn a_run_names_the_browser_whose_wasm_it_cannot_read() {
        let root = scratch("wasm-unmeasured-browser");
        fs::create_dir_all(root.join("src")).unwrap();
        fs::create_dir_all(root.join(".cargo")).unwrap();
        fs::write(
            root.join("Cargo.toml"),
            "[package]\nname='fixture'\nversion='0.0.0'\nedition='2024'\n",
        )
        .unwrap();
        fs::write(root.join("src/lib.rs"), "pub fn one() -> u8 {\n    1\n}\n").unwrap();
        fs::write(
            root.join(".cargo/config.toml"),
            "[target.wasm32-unknown-unknown]\nrunner = 'wasm-bindgen-test-runner'\n",
        )
        .unwrap();
        let project = prepare_rust_project(&root, None).unwrap();
        let mut diagnostics = Vec::new();
        // Whatever becomes of the doctests, which need wasm's standard library.
        let _ = run_prepared_rust_tests(
            &project,
            &["cargo", "test", "--doc", "--target=wasm32-unknown-unknown"].map(String::from),
            "rust-fixture-browser",
            "2026-08-26T00:00:00.000Z",
            None,
            &[("GECKODRIVER".into(), "/opt/geckodriver".into())],
            &mut diagnostics,
        );
        let diagnostics = String::from_utf8(diagnostics).unwrap();
        assert!(
            diagnostics.contains("[supercov] wasm tests run in geckodriver are not measured"),
            "{diagnostics}"
        );
        fs::remove_dir_all(root).unwrap();
    }

    /// A wasm test's runner loads Supercov's hook after the user's own
    /// `NODE_OPTIONS`.
    #[cfg(unix)]
    #[test]
    fn a_wasm_runner_loads_the_hook_after_the_user_s_node_options() {
        let root = scratch("wasm-node-options");
        fs::create_dir_all(root.join(".cargo")).unwrap();
        fs::write(
            root.join(".cargo/config.toml"),
            "[target.wasm32-unknown-unknown]\nrunner = 'wasm-bindgen-test-runner'\n",
        )
        .unwrap();
        let project = PreparedRustProject::empty(root.clone());
        let invocation = cargo_invocation(
            &root,
            &["cargo", "test", "--target=wasm32-unknown-unknown"].map(String::from),
        )
        .unwrap();
        let hooks = root.join("hooks");
        let launcher = ArtifactLauncher::for_invocation_with(
            &project,
            &invocation,
            &hooks,
            Some("--max-old-space-size=64".into()),
        )
        .unwrap();
        let command = launcher.command(Path::new("/target/fixture.wasm"), None);
        assert_eq!(command.get_program(), "wasm-bindgen-test-runner");
        let options = command
            .get_envs()
            .find(|(name, _)| *name == "NODE_OPTIONS")
            .and_then(|(_, value)| value)
            .unwrap()
            .to_string_lossy()
            .into_owned();
        assert_eq!(
            options,
            format!(
                "--max-old-space-size=64 --import={}",
                file_url(&hooks.join("rust-wasm-node.mjs"))
            )
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn code_no_build_compiled_leaves_the_denominator_and_unanswered_code_is_named() {
        let mut probes = crate::rust_instrumenter::RustProbeLayout::default();
        let source = (0..6)
            .map(|index| {
                format!("#[cfg(feature = \"f{index}\")]\npub fn f{index}() -> i32 {{ {index} }}\n")
            })
            .collect::<String>();
        let mut manifest = crate::rust_instrumenter::instrument_rust_file_numbered(
            "src/lib.rs",
            &source,
            "crate::__supercov_runtime_v1",
            false,
            None,
            &mut probes,
        )
        .unwrap()
        .manifest;
        assert_eq!(probes.cfgs.len(), 6, "{:?}", probes.cfgs);
        let first = probes.cfgs[0].line;
        let past_last = probes.cfgs[5].end_line + 1;
        // A boundary inside a gate left out goes with it; one beside it, or
        // in another file, stays.
        let limitation = |file: &str, line: usize| serde_json::json!({"id": format!("{file}:{line}"), "file": file, "line": line});
        manifest.limitations = vec![
            limitation("src/lib.rs", first),
            limitation("src/lib.rs", past_last),
            limitation("src/other.rs", first),
        ];
        // The fifth gate's code is the first's again, as a gate nested in
        // another would be: nobody answered for it, but it is gone anyway.
        probes.cfgs[4].obligations = probes.cfgs[0].obligations.clone();
        let mut diagnostics = Vec::new();
        let settled =
            settle_cfg_gates_with(&manifest, &probes, &[1, 1, 1, 1, 0, 0], &mut diagnostics)
                .unwrap();
        let diagnostics = String::from_utf8(diagnostics).unwrap();
        assert!(
            diagnostics.contains("4 item(s) behind a #[cfg] no build compiled are outside the denominator: src/lib.rs:1 cfg(feature = \"f0\"), src/lib.rs:3 cfg(feature = \"f1\"), src/lib.rs:5 cfg(feature = \"f2\"), ..."),
            "{diagnostics}"
        );
        assert!(
            diagnostics.contains("1 item(s) behind a #[cfg] are counted though no test ran code of their crate to show whether it was built: src/lib.rs:11 cfg(feature = \"f5\")"),
            "{diagnostics}"
        );
        assert_eq!(
            settled
                .limitations
                .iter()
                .map(|limitation| limitation["id"].as_str().unwrap())
                .collect::<Vec<_>>(),
            [
                format!("src/lib.rs:{past_last}"),
                format!("src/other.rs:{first}")
            ]
        );
        assert!(
            settled
                .points
                .iter()
                .all(|point| !probes.cfgs[0].obligations.contains(&point.id))
        );
    }

    /// A writer that takes nothing, as a closed standard error.
    struct Closed;

    impl Write for Closed {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::other("closed"))
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn a_gate_left_out_takes_its_decisions_and_a_closed_writer_is_an_error() {
        let mut probes = crate::rust_instrumenter::RustProbeLayout::default();
        let source = concat!(
            "#[cfg(feature = \"f\")]\n",
            "pub fn f(a: bool, b: bool) -> i32 { if a && b { 1 } else { 2 } }\n",
            "#[cfg(feature = \"g\")]\n",
            "pub fn g(a: bool, b: bool) -> i32 { if a || b { 1 } else { 2 } }\n",
        );
        let mut manifest = crate::rust_instrumenter::instrument_rust_file_numbered(
            "src/lib.rs",
            source,
            "crate::__supercov_runtime_v1",
            false,
            None,
            &mut probes,
        )
        .unwrap()
        .manifest;
        assert_eq!(probes.cfgs.len(), 2, "{:?}", probes.cfgs);
        manifest.unmeasured = manifest
            .points
            .iter()
            .map(|point| point.id.clone())
            .collect();
        let (decisions, branches) = (manifest.decisions.len(), manifest.branches.len());
        assert!(decisions >= 2 && branches >= 4, "{decisions} {branches}");
        // The first gate was left out of every build, the second built.
        let settled = settle_cfg_gates_with(&manifest, &probes, &[1, 2], &mut Vec::new()).unwrap();
        let kept = |file_line: &dyn Fn(usize) -> bool| {
            settled
                .decisions
                .iter()
                .all(|decision| file_line(decision.line))
                && settled.branches.iter().all(|branch| file_line(branch.line))
        };
        assert!(settled.decisions.len() < decisions);
        assert!(settled.branches.len() < branches);
        assert!(settled.unmeasured.len() < manifest.unmeasured.len());
        assert!(kept(&|line| line >= probes.cfgs[1].line));
        for gates in [[1, 2], [0, 2]] {
            let error = settle_cfg_gates_with(&manifest, &probes, &gates, &mut Closed).unwrap_err();
            assert!(matches!(error, RustTestRunnerError::Io(_)), "{error}");
        }
        let declined = BTreeSet::from(["a.rs".to_owned()]);
        assert!(matches!(
            report_declined(&declined, &mut Closed),
            Err(RustTestRunnerError::Io(_))
        ));
        let workers = [("a.rs".to_owned(), "no test started it".to_owned())];
        assert!(matches!(
            report_unmeasured_workers(&workers, &mut Closed),
            Err(RustTestRunnerError::Io(_))
        ));
    }

    #[test]
    fn files_the_build_did_not_read_are_declined_only_on_this_builds_depinfo() {
        let root = scratch("declined-sources");
        fs::create_dir_all(root.join("target/debug/deps")).unwrap();
        fs::write(
            root.join("target/debug/deps/fixture.d"),
            "target/debug/deps/fixture: src/lib.rs src/kept.rs\n",
        )
        .unwrap();
        let mut project = PreparedRustProject::empty(root.clone());
        project.source_files = ["src/gone.rs", "src/kept.rs", "src/lib.rs", "src/worker.rs"]
            .map(String::from)
            .to_vec();
        project.crate_roots = vec!["src/lib.rs".into()];
        project.manifest.scope = Some(serde_json::json!({"entries": [
            {"file": "src/gone.rs", "status": "included"},
            {"file": "src/lib.rs", "status": "included"},
            "not an entry",
        ]}));
        // A boundary in a file left out goes with it; one with no file stays.
        project.manifest.limitations = vec![
            serde_json::json!({"id": "gone", "file": "src/gone.rs"}),
            serde_json::json!({"id": "kept", "file": "src/lib.rs"}),
            serde_json::json!({"id": "anywhere"}),
        ];
        // A Worker's crate that `wrangler dev` built counts.
        let also = BTreeSet::from(["src/worker.rs".to_owned()]);
        let (manifest, declined) = decline_uncompiled_sources(&project, &also);
        assert_eq!(declined, BTreeSet::from(["src/gone.rs".to_owned()]));
        assert_eq!(
            manifest
                .limitations
                .iter()
                .map(|limitation| limitation["id"].as_str().unwrap())
                .collect::<Vec<_>>(),
            ["kept", "anywhere"]
        );
        let entries = manifest.scope.unwrap()["entries"].clone();
        assert_eq!(entries[0]["status"], "excluded");
        assert_eq!(entries[1]["status"], "included");
        // Without a scope there is nothing to mark.
        project.manifest.scope = None;
        assert_eq!(decline_uncompiled_sources(&project, &also).1, declined);
        // Depinfo that names no crate root of this project is another
        // build's.
        project.crate_roots = vec!["src/main.rs".into()];
        assert!(decline_uncompiled_sources(&project, &also).1.is_empty());
        let mut diagnostics = Vec::new();
        report_declined(
            &["a.rs", "b.rs", "c.rs", "d.rs"].map(String::from).into(),
            &mut diagnostics,
        )
        .unwrap();
        assert_eq!(
            String::from_utf8(diagnostics).unwrap(),
            "[supercov] 4 source file(s) this build did not compile are outside the denominator: a.rs, b.rs, c.rs, ...\n"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn the_files_of_a_worker_no_test_started_are_not_measured_and_say_why() {
        let mut project = PreparedRustProject::empty(std::env::temp_dir());
        project.source_files = vec!["plane/src/lib.rs".into(), "runners/src/lib.rs".into()];
        project.probes.crates = BTreeMap::from([
            ("plane".to_owned(), vec!["plane/src/lib.rs".to_owned()]),
            // A file the source roots left out is no file of the run.
            (
                "runners".to_owned(),
                vec![
                    "runners/src/lib.rs".to_owned(),
                    "runners/src/outside.rs".to_owned(),
                ],
            ),
            ("host".to_owned(), vec!["host/src/lib.rs".to_owned()]),
        ]);
        project.probes.workers = BTreeSet::from(["plane".to_owned(), "runners".to_owned()]);
        let empty = project.clone();
        let mut unmeasured = Vec::new();
        for _ in 0..2 {
            unmeasured = note_unrun_workers(
                &mut project,
                &BTreeSet::from(["plane".to_owned()]),
                false,
                &[],
            );
        }
        // One note on the Worker no test ran, for the whole file, however
        // often it is noted; the one that ran has none.
        let notes = project
            .manifest
            .limitations
            .iter()
            .filter(|limitation| limitation["wholeFile"] == true)
            .collect::<Vec<_>>();
        assert_eq!(notes.len(), 1);
        assert_eq!(notes[0]["id"], "rust-worker-not-run#runners/src/lib.rs");
        let reason = notes[0]["reason"].as_str().unwrap();
        assert!(reason.contains("no test of this run did"), "{reason}");
        assert_eq!(
            unmeasured,
            [("runners/src/lib.rs".to_owned(), reason.to_owned())]
        );
        let mut said = Vec::new();
        report_unmeasured_workers(&unmeasured, &mut said).unwrap();
        assert_eq!(
            String::from_utf8(said).unwrap(),
            format!(
                "[supercov] not measured: 1 Rust Worker file(s), runners/src/lib.rs. {reason}.\n"
            )
        );
        let mut said = Vec::new();
        report_unmeasured_workers(&[], &mut said).unwrap();
        assert!(said.is_empty());
        // Every Worker ran: nothing changes.
        let mut ran = empty.clone();
        assert!(
            note_unrun_workers(
                &mut ran,
                &BTreeSet::from(["plane".to_owned(), "runners".to_owned()]),
                false,
                &[],
            )
            .is_empty()
        );
        assert_eq!(ran, empty);
        // A later run on the same build says its own: the first run's note
        // on a Worker gives way to this one's, or to none where it ran.
        let mut reused = project.clone();
        note_unrun_workers(
            &mut reused,
            &BTreeSet::from(["plane".to_owned()]),
            true,
            &[],
        );
        let notes = reused
            .manifest
            .limitations
            .iter()
            .filter(|limitation| limitation["wholeFile"] == true)
            .collect::<Vec<_>>();
        assert_eq!(notes.len(), 1);
        assert!(
            notes[0]["reason"]
                .as_str()
                .unwrap()
                .contains("wrangler dev --remote"),
            "{notes:?}"
        );
        note_unrun_workers(
            &mut reused,
            &BTreeSet::from(["plane".to_owned(), "runners".to_owned()]),
            false,
            &[],
        );
        assert_eq!(reused, empty);
        // Tests that did start `wrangler dev` are named, once each, with why
        // a configuration was not measured.
        let mut started = empty;
        let configs = [
            "`wrangler dev -c tests/w.toml`".to_owned(),
            "`wrangler dev -c tests/other.toml`, not measured because it names no `main`"
                .to_owned(),
            "`wrangler dev -c tests/w.toml`".to_owned(),
        ];
        let said = note_unrun_workers(&mut started, &BTreeSet::new(), false, &configs);
        assert_eq!(said.len(), 2);
        let reason = &said[0].1;
        assert!(
            reason.contains(
                "the tests started `wrangler dev -c tests/other.toml`, not measured because it names no `main`; `wrangler dev -c tests/w.toml`, and none loaded"
            ),
            "{reason}"
        );
        assert!(
            reason.contains("build the Worker in the measured command"),
            "{reason}"
        );
    }

    /// Without capture, a test's own lines are shown and libtest's frame of
    /// each one-test process is not.
    #[test]
    fn a_test_without_capture_shows_its_lines_and_not_each_frame() {
        for frame in [
            "\n",
            "running 1 test\n",
            "running 2 tests\n",
            "test result: ok. 1 passed\n",
        ] {
            assert!(!shown_without_capture(frame.as_bytes()), "{frame}");
        }
        for line in ["test a ... ok\n", "hello\n", "running late\n", "running"] {
            assert!(shown_without_capture(line.as_bytes()), "{line}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_test_artifact_must_be_a_file_cargo_built_in_the_target_directory() {
        let root = scratch("unsafe-artifacts");
        fs::create_dir_all(root.join("target/debug/deps/a-directory")).unwrap();
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/lib.rs"), "").unwrap();
        let project = PreparedRustProject::empty(root.clone());
        // Cargo, as far as its build's messages go: `messages` is printed.
        script(
            &root.join("cargo"),
            &format!("#!/bin/sh\ncat '{}'\n", root.join("messages").display()),
        );
        let built = |executable: &Path, test: bool| {
            fs::write(
                root.join("messages"),
                serde_json::json!({
                    "reason": "compiler-artifact",
                    "target": {"name": "fixture", "kind": ["lib"], "src_path": root.join("src/lib.rs")},
                    "profile": {"test": test},
                    "executable": executable,
                })
                .to_string(),
            )
            .unwrap();
            build_test_artifacts(
                &project,
                &[root.join("cargo").display().to_string(), "test".into()],
            )
            .map(|artifacts| artifacts.len())
            .map_err(|error| error.to_string())
        };
        let outside = fs::canonicalize("/bin/sh").unwrap();
        assert!(
            built(&outside, true)
                .unwrap_err()
                .contains(&outside.display().to_string())
        );
        let directory = root.join("target/debug/deps/a-directory");
        assert!(
            built(&directory, true)
                .unwrap_err()
                .contains(&directory.display().to_string())
        );
        // A build that made no test binary has nothing to run.
        assert!(
            built(&outside, false)
                .unwrap_err()
                .contains("Cargo emitted no libtest artifacts")
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn small_paths_and_words_are_read_as_their_tools_read_them() {
        // A Windows path is a file URL from its drive.
        assert_eq!(
            file_url(Path::new("C:\\hooks\\a b.mjs")),
            "file:///C:/hooks/a%20b.mjs"
        );
        // A source is a path below the workspace and nothing else.
        let root = Path::new("/work");
        assert!(relative_source(root, Path::new("/work")).is_err());
        assert!(relative_source(root, Path::new("/work/src/../../etc")).is_err());
        assert_eq!(
            relative_source(root, Path::new("/work/src/lib.rs")).unwrap(),
            "src/lib.rs"
        );
        // A trailing space ends no word.
        assert_eq!(shell_words("cargo test ").unwrap(), ["cargo", "test"]);
        // An executable that is the root, or sits in it, has fewer
        // directories to search.
        let library_path = |executable: &str| {
            let environment = dynamic_library_environment(Path::new("/lib"), Path::new(executable));
            std::env::split_paths(&environment[0].1)
                .take(3)
                .collect::<Vec<_>>()
        };
        assert_eq!(library_path("/")[0], PathBuf::from("/lib"));
        assert_eq!(
            library_path("/fixture")[..2],
            [PathBuf::from("/"), PathBuf::from("/lib")]
        );
        // `-vv` is nextest's verbosity; `-vx` is no option it knows.
        let unknown = CargoTestInvocation {
            program: "cargo".into(),
            kind: RustCargoCommandKind::NextestRun,
            arguments: ["nextest", "run", "-vx"].map(String::from).to_vec(),
            runner_arguments: Vec::new(),
        };
        assert!(
            nextest_list_invocation(&unknown)
                .unwrap_err()
                .to_string()
                .contains("does not recognize option -vx")
        );
    }

    #[test]
    fn a_test_binary_is_announced_as_cargo_announces_it() {
        // The source from its package, the binary from where Cargo would
        // have built it, and neither with the prefix Windows puts on a path
        // it has resolved.
        let artifact = |package: &str, source: &str, kind: &str| TestArtifact {
            executable: PathBuf::from(
                "/work/copy/.supercov/cache/rust-target/debug/deps/shop-0123456789abcdef",
            ),
            name: "shop".into(),
            kind: kind.into(),
            source: source.into(),
            package_directory: PathBuf::from(package),
        };
        let line = |artifact: &TestArtifact| {
            running_line(
                Path::new("/work/copy"),
                Path::new("/work/copy/.supercov/cache/rust-target"),
                artifact,
                false,
            )
            .replace('\\', "/")
        };
        assert_eq!(
            line(&artifact("/work/copy", "src/lib.rs", "unit")),
            "     Running unittests src/lib.rs (target/debug/deps/shop-0123456789abcdef)"
        );
        assert_eq!(
            line(&artifact(
                "/work/copy/crates/shop",
                "crates/shop/tests/outside.rs",
                "integration"
            )),
            "     Running tests/outside.rs (target/debug/deps/shop-0123456789abcdef)"
        );
        // As Windows gives them: the package resolved, the workspace not.
        let windows = running_line(
            Path::new("C:\\work\\copy"),
            Path::new("C:\\work\\copy\\target"),
            &artifact(
                "\\\\?\\C:\\work\\copy\\crates\\shop",
                "crates/shop/src/lib.rs",
                "unit",
            ),
            false,
        )
        .replace('\\', "/");
        assert!(
            windows.starts_with("     Running unittests src/lib.rs ("),
            "{windows}"
        );
        assert!(
            running_line(
                Path::new("/work/copy"),
                Path::new("/work/copy/.supercov/cache/rust-target"),
                &artifact("/work/copy", "src/lib.rs", "unit"),
                true,
            )
            .starts_with("\u{1b}[1m\u{1b}[92m     Running\u{1b}[0m unittests ")
        );
    }

    /// `npm test -- adds` runs the script with `adds` after it: the filter
    /// reaches Cargo, so only the tests it names run.
    #[test]
    fn a_filter_given_to_a_package_script_reaches_cargo() {
        let root = std::env::temp_dir().join(format!(
            "supercov-npm-cargo-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&root).unwrap();
        std::fs::write(
            root.join("package.json"),
            r#"{ "scripts": { "test": "cargo test --lib" } }"#,
        )
        .unwrap();
        let invocation = cargo_invocation(
            &root,
            &["npm".into(), "test".into(), "--".into(), "adds".into()],
        )
        .unwrap();
        std::fs::remove_dir_all(&root).unwrap();
        assert_eq!(invocation.kind, RustCargoCommandKind::CargoTest);
        assert_eq!(invocation.arguments, ["test", "--lib", "adds"]);
    }

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
    fn nextest_run_is_detected_without_reclassifying_its_filters_or_retries() {
        let invocation = cargo_invocation(
            Path::new("."),
            &[
                "cargo".into(),
                "+1.95.0".into(),
                "nextest".into(),
                "run".into(),
                "--retries".into(),
                "2".into(),
                "-E".into(),
                "test(/flaky/)".into(),
                "--".into(),
                "--nocapture".into(),
            ],
        )
        .unwrap();
        assert_eq!(invocation.kind, RustCargoCommandKind::NextestRun);
        assert_eq!(
            invocation.arguments,
            [
                "+1.95.0",
                "nextest",
                "run",
                "--retries",
                "2",
                "-E",
                "test(/flaky/)",
            ]
        );
        assert_eq!(invocation.runner_arguments, ["--nocapture"]);
        let execution = rust_cargo_execution_selection(&invocation).unwrap();
        assert!(execution.run_libtests);
        assert!(!execution.run_doctests);
        assert!(execution.doctest_arguments.is_empty());
        assert_eq!(
            nextest_list_invocation(&invocation).unwrap(),
            NextestListInvocation {
                arguments: vec![
                    "+1.95.0".into(),
                    "nextest".into(),
                    "list".into(),
                    "-E".into(),
                    "test(/flaky/)".into(),
                    "--message-format".into(),
                    "json".into(),
                ],
                runner_arguments: vec!["--nocapture".into()],
            }
        );
    }

    #[test]
    fn nextest_list_projection_preserves_selection_and_rejects_external_state() {
        let invocation = CargoTestInvocation {
            program: "cargo".into(),
            kind: RustCargoCommandKind::NextestRun,
            arguments: vec![
                "nextest".into(),
                "run".into(),
                "--package=fixture".into(),
                "--partition".into(),
                "hash:1/2".into(),
                "--test-threads=8".into(),
                "--failure-output".into(),
                "final".into(),
                "name".into(),
            ],
            runner_arguments: vec!["--exact".into(), "full::name".into()],
        };
        assert_eq!(
            nextest_list_invocation(&invocation).unwrap(),
            NextestListInvocation {
                arguments: vec![
                    "nextest".into(),
                    "list".into(),
                    "--package=fixture".into(),
                    "--partition".into(),
                    "hash:1/2".into(),
                    "name".into(),
                    "--message-format".into(),
                    "json".into(),
                ],
                runner_arguments: vec!["--exact".into(), "full::name".into()],
            }
        );

        let mut rerun = invocation;
        rerun.arguments.extend(["--rerun".into(), "latest".into()]);
        assert!(
            nextest_list_invocation(&rerun)
                .unwrap_err()
                .to_string()
                .contains("cannot yet be assigned exact selected-test identity")
        );
    }

    #[test]
    fn nextest_list_keeps_attached_selection_values_and_drops_run_only_options() {
        let list = |arguments: &[&str]| {
            nextest_list_invocation(&CargoTestInvocation {
                program: "cargo".into(),
                kind: RustCargoCommandKind::NextestRun,
                arguments: ["nextest", "run"]
                    .iter()
                    .chain(arguments)
                    .map(|argument| (*argument).to_owned())
                    .collect(),
                runner_arguments: Vec::new(),
            })
            .map(|listed| listed.arguments)
            .map_err(|error| error.to_string())
        };
        assert_eq!(
            list(&[
                "-pfoo",
                "-Ffeat",
                "-Eall()",
                "-Pci",
                "-Zflag",
                "-vv",
                "--timings=html",
                "-j4",
                "-j",
                "2",
                "--",
                "--exact",
                "x",
            ])
            .unwrap(),
            [
                "nextest",
                "list",
                "-pfoo",
                "-Ffeat",
                "-Eall()",
                "-Pci",
                "-Zflag",
                "-vv",
                "--timings=html",
                "--",
                "--exact",
                "x",
                "--message-format",
                "json",
            ]
        );
        for (arguments, refusal) in [
            (&["-p"][..], "nextest option -p has no value"),
            (
                &["--test-threads"][..],
                "nextest option --test-threads has no value",
            ),
            (&["--rerun"][..], "nextest option --rerun has no value"),
            (&["-"][..], "does not recognize option -"),
        ] {
            let error = list(arguments).unwrap_err();
            assert!(error.contains(refusal), "{arguments:?}: {error}");
        }
    }

    /// A package script's command split as `sh` runs it.
    #[test]
    fn a_script_command_splits_the_way_sh_splits_it() {
        let words = |text: &str| shell_words(text).unwrap();
        assert_eq!(
            words("cargo  test --workspace"),
            ["cargo", "test", "--workspace"]
        );
        assert_eq!(
            words(r#"cargo test --config 'target.x.runner="a b"' -- "two words""#),
            [
                "cargo",
                "test",
                "--config",
                r#"target.x.runner="a b""#,
                "--",
                "two words"
            ]
        );
        // An empty argument is an argument.
        assert_eq!(
            words(r#"cargo test -- '' """#),
            ["cargo", "test", "--", "", ""]
        );
        // In double quotes a backslash escapes only what `sh` lets it.
        assert_eq!(
            words(r#""C:\work\app" "say \"hi\" \$HOME""#),
            [r"C:\work\app", r#"say "hi" $HOME"#]
        );
        // Outside quotes it escapes anything; in single quotes nothing.
        assert_eq!(words(r"a\ b 'c\d'"), ["a b", r"c\d"]);
        assert_eq!(words("joined'' x\"\"y"), ["joined", "xy"]);
        assert_eq!(
            words("cargo test \\\n  --workspace \"a\\\nb\""),
            ["cargo", "test", "--workspace", "ab"]
        );
        for broken in [
            "cargo test 'open",
            "cargo test \"open",
            r"cargo test \",
            "cargo test \"open\\",
        ] {
            assert!(shell_words(broken).is_err(), "{broken}");
        }
        // Nothing is no words, and no arguments build as none.
        assert!(words("").is_empty());
        assert!(build_owned_arguments(Vec::new()).unwrap().is_empty());
    }

    /// Each nextest option is read for whether it takes the next word as
    /// its value: one written with `=` carries its own.
    #[test]
    fn each_nextest_option_says_whether_it_takes_a_value() {
        for (argument, run_only, shared) in [
            ("--retries", Some(true), None),
            ("--retries=2", Some(false), None),
            ("--fail-fast", Some(false), None),
            ("-j4", Some(false), None),
            ("--profile", None, Some(true)),
            ("--profile=ci", None, Some(false)),
            ("--no-pager", None, Some(false)),
            ("name", None, None),
        ] {
            assert_eq!(nextest_run_only_option(argument), run_only, "{argument}");
            assert_eq!(nextest_shared_option(argument), shared, "{argument}");
        }
        assert_eq!(nextest_unsupported_run_option("--no-run"), Some(false));
    }

    #[test]
    fn nextest_list_projection_keeps_short_options_with_attached_values() {
        let invocation = CargoTestInvocation {
            program: "cargo".into(),
            kind: RustCargoCommandKind::NextestRun,
            arguments: [
                "nextest",
                "run",
                "-pfixture",
                "-Ffast",
                "-Etest(=a)",
                "-Pci",
                "-Zbuild-std",
                "-vv",
                "name",
            ]
            .map(String::from)
            .to_vec(),
            runner_arguments: Vec::new(),
        };
        let listed = nextest_list_invocation(&invocation).unwrap();
        // Each option carries its own value, so the filter after them is
        // still read as the filter.
        assert_eq!(
            listed.arguments,
            [
                "nextest",
                "list",
                "-pfixture",
                "-Ffast",
                "-Etest(=a)",
                "-Pci",
                "-Zbuild-std",
                "-vv",
                "name",
                "--message-format",
                "json",
            ]
        );
    }

    #[test]
    fn nextest_list_projection_preserves_post_separator_libtest_selection() {
        let invocation = cargo_invocation(
            Path::new("."),
            &[
                "cargo".into(),
                "nextest".into(),
                "run".into(),
                "--timings".into(),
                "-vv".into(),
                "--".into(),
                "--include-ignored".into(),
                "--skip".into(),
                "slow".into(),
                "--exact".into(),
                "tests::selected".into(),
            ],
        )
        .unwrap();
        assert_eq!(
            nextest_list_invocation(&invocation).unwrap(),
            NextestListInvocation {
                arguments: vec![
                    "nextest".to_owned(),
                    "list".to_owned(),
                    "--timings".to_owned(),
                    "-vv".to_owned(),
                    "--message-format".to_owned(),
                    "json".to_owned(),
                ],
                runner_arguments: vec![
                    "--include-ignored".to_owned(),
                    "--skip".to_owned(),
                    "slow".to_owned(),
                    "--exact".to_owned(),
                    "tests::selected".to_owned(),
                ],
            }
        );
    }

    #[test]
    fn nextest_version_handshake_preserves_the_cargo_toolchain_selector() {
        let invocation = CargoTestInvocation {
            program: "cargo".into(),
            kind: RustCargoCommandKind::NextestRun,
            arguments: vec![
                "+1.95.0".into(),
                "nextest".into(),
                "run".into(),
                "-p".into(),
                "fixture".into(),
            ],
            runner_arguments: Vec::new(),
        };
        assert_eq!(
            nextest_version_arguments(&invocation),
            ["+1.95.0", "nextest", "--version"]
        );
    }

    #[test]
    fn stock_libtest_presentation_and_scheduling_options_do_not_change_discovery() {
        let invocation = CargoTestInvocation {
            program: "cargo".into(),
            kind: RustCargoCommandKind::CargoTest,
            arguments: vec!["test".into(), "cargo-filter".into()],
            runner_arguments: [
                "runner-filter",
                "--nocapture",
                "--show-output",
                "--format=json",
                "--color",
                "never",
                "--test-threads=4",
                "--fail-fast",
                "--shuffle-seed",
                "17",
                "-Zunstable-options",
                "--exclude-should-panic",
            ]
            .into_iter()
            .map(str::to_owned)
            .collect(),
        };
        let selection = rust_libtest_selection(&invocation).unwrap();
        assert_eq!(
            selection.list_arguments,
            [
                "cargo-filter",
                "runner-filter",
                "-Zunstable-options",
                "--exclude-should-panic"
            ]
        );
    }

    /// `-Z` and its feature as two words keep both, as one does.
    #[test]
    fn an_unstable_flag_and_its_feature_as_two_words_are_kept_together() {
        let invocation = CargoTestInvocation {
            program: "cargo".into(),
            kind: RustCargoCommandKind::CargoTest,
            arguments: vec!["test".into()],
            runner_arguments: ["-Z", "unstable-options", "--exclude-should-panic"]
                .map(str::to_owned)
                .to_vec(),
        };
        let selection = rust_libtest_selection(&invocation).unwrap();
        assert_eq!(
            selection.list_arguments,
            ["-Z", "unstable-options", "--exclude-should-panic"]
        );
    }

    #[test]
    fn cargo_test_options_are_not_mistaken_for_the_test_name_filter() {
        let invocation = CargoTestInvocation {
            program: "cargo".into(),
            kind: RustCargoCommandKind::CargoTest,
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
    }

    #[test]
    fn libtest_thread_count_is_preserved_as_runner_scheduling() {
        for arguments in [vec!["--test-threads", "1"], vec!["--test-threads=8"]] {
            let invocation = CargoTestInvocation {
                program: "cargo".into(),
                kind: RustCargoCommandKind::CargoTest,
                arguments: vec!["test".into()],
                runner_arguments: arguments.into_iter().map(str::to_owned).collect(),
            };
            let selection = rust_libtest_selection(&invocation).unwrap();
            assert!(selection.list_arguments.is_empty());
        }
    }

    /// Each libtest option that needs a value refuses to go without one,
    /// an attached value is kept or dropped by what it does, and a second
    /// `--` is the runner's own argument.
    #[test]
    fn libtest_options_keep_or_drop_their_values_by_what_they_do() {
        let select = |runner: &[&str]| {
            rust_libtest_selection(&CargoTestInvocation {
                program: "cargo".into(),
                kind: RustCargoCommandKind::CargoTest,
                arguments: vec!["test".into()],
                runner_arguments: runner.iter().map(|argument| argument.to_string()).collect(),
            })
            .map(|selection| selection.list_arguments)
            .map_err(|error| error.to_string())
        };
        assert_eq!(
            select(&[
                "--skip=slow",
                "-Zflag",
                "--format=terse",
                "--color",
                "always"
            ]),
            Ok(vec!["--skip=slow".to_owned(), "-Zflag".to_owned()])
        );
        for (runner, refusal) in [
            (&["--skip"][..], "libtest --skip has no filter value"),
            (&["-Z"][..], "libtest -Z has no feature value"),
            (&["--logfile"][..], "libtest --logfile has no value"),
            (&["--skip="][..], "does not recognize option --skip="),
            (&["--format="][..], "does not recognize option --format="),
            (&["--list"][..], "does not execute a test suite"),
        ] {
            let error = select(runner).unwrap_err();
            assert!(error.contains(refusal), "{runner:?}: {error}");
        }
        let words = |command: &str| {
            cargo_invocation(
                Path::new("."),
                &command.split(' ').map(str::to_owned).collect::<Vec<_>>(),
            )
            .unwrap()
            .runner_arguments
        };
        assert_eq!(words("cargo test -- a -- b"), ["a", "--", "b"]);
        // A nextest option that is refused says so before it says its value is missing.
        let list = |arguments: &[&str]| {
            nextest_list_invocation(&CargoTestInvocation {
                program: "cargo".into(),
                kind: RustCargoCommandKind::NextestRun,
                arguments: ["nextest", "run"]
                    .iter()
                    .chain(arguments)
                    .map(|argument| (*argument).to_owned())
                    .collect(),
                runner_arguments: Vec::new(),
            })
            .map_err(|error| error.to_string())
        };
        for (arguments, refusal) in [
            (
                &["--stress-count"][..],
                "nextest option --stress-count has no value",
            ),
            (
                &["--stress-count", "3"][..],
                "cannot yet be assigned exact selected-test identity",
            ),
            (
                &["--stress-count=3"][..],
                "cannot yet be assigned exact selected-test identity",
            ),
            (
                &["--no-run"][..],
                "cannot yet be assigned exact selected-test identity",
            ),
        ] {
            let error = list(arguments).unwrap_err();
            assert!(error.contains(refusal), "{arguments:?}: {error}");
        }
    }

    /// libtest is quiet with `-q`, `--quiet` or terse output in either
    /// spelling; a run that ran nothing is a skip only when it passed.
    #[test]
    fn libtest_quiet_and_skipped_runs_are_read_from_their_arguments_and_output() {
        let quiet = |runner: &[&str]| {
            libtest_quiet(&CargoTestInvocation {
                program: "cargo".into(),
                kind: RustCargoCommandKind::CargoTest,
                arguments: vec!["test".into()],
                runner_arguments: runner.iter().map(|argument| argument.to_string()).collect(),
            })
        };
        assert!(quiet(&["-q"]) && quiet(&["--quiet"]) && quiet(&["--format=terse"]));
        assert!(quiet(&["--format", "terse"]));
        assert!(!quiet(&["--format", "json"]) && !quiet(&["terse"]) && !quiet(&[]));
        assert!(libtest_skipped(0, "running 0 tests\n"));
        assert!(libtest_skipped(
            0,
            "test result: ok. 0 passed; 0 failed; 1 ignored; 0 measured"
        ));
        assert!(!libtest_skipped(101, "running 0 tests\n"));
        assert!(!libtest_skipped(0, "running 1 test\n"));
    }

    /// A record naming a probe the manifest does not have, or a decision
    /// with the wrong number of conditions, is refused; a branch outcome's
    /// hit and an assertion with what its thread wrote before it are kept.
    #[test]
    fn probe_records_are_read_against_the_manifest() {
        use crate::coverage_analysis::PointKind;
        use crate::coverage_report::{
            BranchAlternativeMeta, BranchMeta, CoverageManifest, DecisionMeta, PointMeta,
        };
        use crate::rust_runtime::{RustProbeEntry, RustProbeObservation as O};
        let manifest = CoverageManifest {
            unmeasured: Vec::new(),
            decisions: vec![DecisionMeta {
                id: "d".into(),
                file: "src/lib.rs".into(),
                line: 2,
                column: 4,
                source: "a && b".into(),
                conditions: vec!["a".into(), "b".into()],
                kind: "if".into(),
            }],
            points: vec![PointMeta {
                id: "p".into(),
                kind: PointKind::Statement,
                file: "src/lib.rs".into(),
                line: 1,
                column: 1,
                source: "assert!(a)".into(),
                label: None,
            }],
            branches: vec![BranchMeta {
                id: "b".into(),
                kind: "if".into(),
                file: "src/lib.rs".into(),
                line: 2,
                column: 4,
                source: "a && b".into(),
                alternatives: vec![BranchAlternativeMeta {
                    id: "alt".into(),
                    label: "true".into(),
                }],
            }],
            limitations: vec![],
            scope: None,
            assertion_sites: Vec::new(),
        };
        let entry = |thread: u64, observation: O| RustProbeEntry {
            thread,
            observation,
        };
        let read = |entries: Vec<RustProbeEntry>| snapshot_regions(&manifest, [entries], "attempt");
        for (entries, expected) in [
            (vec![entry(1, O::Hit { id: "zzz".into() })], "zzz"),
            (
                vec![entry(
                    1,
                    O::Decision {
                        id: "zzz".into(),
                        values: vec![],
                        outcome: true,
                    },
                )],
                "zzz",
            ),
            (vec![entry(1, O::Assertion { id: "zzz".into() })], "zzz"),
        ] {
            assert!(
                matches!(read(entries), Err(RustTestRunnerError::UnknownProbe(id)) if id == expected)
            );
        }
        assert!(matches!(
            read(vec![entry(
                1,
                O::Decision {
                    id: "d".into(),
                    values: vec![Some(true)],
                    outcome: true
                }
            )]),
            Err(RustTestRunnerError::InvalidVector {
                expected: 2,
                actual: 1,
                ..
            })
        ));
        let evidence = read(vec![
            entry(1, O::Hit { id: "alt".into() }),
            entry(
                2,
                O::Decision {
                    id: "d".into(),
                    values: vec![Some(true), Some(false)],
                    outcome: false,
                },
            ),
            entry(1, O::Assertion { id: "p".into() }),
        ]);
        assert_eq!(evidence.err().map(|error| error.to_string()), None);
    }

    #[test]
    fn invalid_or_duplicate_libtest_thread_counts_fail_closed() {
        for arguments in [
            vec!["--test-threads"],
            vec!["--test-threads=0"],
            vec!["--test-threads=abc"],
            vec!["--test-threads", "1", "--test-threads=2"],
        ] {
            let invocation = CargoTestInvocation {
                program: "cargo".into(),
                kind: RustCargoCommandKind::CargoTest,
                arguments: vec!["test".into()],
                runner_arguments: arguments.into_iter().map(str::to_owned).collect(),
            };
            assert!(rust_libtest_selection(&invocation).is_err());
        }
    }

    #[test]
    fn cargo_target_selection_reproduces_when_cargo_runs_doctests() {
        let invocation = CargoTestInvocation {
            program: "cargo".into(),
            kind: RustCargoCommandKind::CargoTest,
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
    fn a_module_the_build_never_compiles_leaves_the_denominator() {
        // `#[cfg(feature = ...)]` with the feature off resolves as a module
        // and is never compiled, so nothing in it can ever be covered.
        // memchr carries thirteen such files for other architectures.
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "supercov-rust-runner-cfg-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(
            root.join("Cargo.toml"),
            "[package]\nname='fixture'\nversion='0.0.0'\nedition='2024'\n\n[features]\nextra=[]\n",
        )
        .unwrap();
        fs::write(
            root.join("src/lib.rs"),
            r#"
#[cfg(feature = "extra")]
mod extra;

pub fn kept(value: i32) -> i32 {
    if value > 0 { value } else { -value }
}
#[cfg(test)]
mod tests {
    #[test] fn positive() { assert_eq!(super::kept(2), 2); }
    #[test] fn negative() { assert_eq!(super::kept(-2), 2); }
}
"#,
        )
        .unwrap();
        fs::write(
            root.join("src/extra.rs"),
            "pub fn never_built(value: i32) -> i32 {\n    if value > 0 { 1 } else { 0 }\n}\n",
        )
        .unwrap();
        let project = prepare_rust_project(&root, None).unwrap();
        // Discovery still reaches it: the module tree is what rustc resolves.
        assert!(
            project
                .source_files
                .iter()
                .any(|file| file == "src/extra.rs"),
            "{:?}",
            project.source_files
        );
        let run = run_prepared_rust_tests(
            &project,
            &["cargo".into(), "test".into(), "--lib".into()],
            "rust-fixture-cfg",
            "2026-08-26T00:00:00.000Z",
            None,
            &[],
            &mut Vec::new(),
        )
        .unwrap();
        assert_eq!(run.exit_code, 0);
        // Its obligations stay in the manifest, so the evidence token still
        // matches and the file is still addressable, but they are declined.
        let manifest = &run.request.manifest;
        assert!(
            manifest
                .points
                .iter()
                .any(|point| point.file == "src/extra.rs"),
            "the obligations must stay in the manifest"
        );
        let declined = manifest.unmeasured.iter().collect::<BTreeSet<_>>();
        for point in &manifest.points {
            assert_eq!(
                declined.contains(&point.id),
                point.file == "src/extra.rs",
                "{} in {}",
                point.id,
                point.file
            );
        }
        for decision in &manifest.decisions {
            assert_eq!(
                declined.contains(&decision.id),
                decision.file == "src/extra.rs"
            );
        }
        // The tests cover everything the build compiled.
        let report =
            crate::frontend_protocol::analyze_frontend_results(&run.declaration, &run.request)
                .unwrap();
        assert_eq!(
            report.view.summary.lines.percentage, 100.0,
            "declined obligations must not read as uncovered"
        );
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn assertion_occurrences_are_retained_without_automatic_asserted_credit() {
        // Records are appended in execution order, so what a thread wrote
        // before it passed an assertion was in scope for that check. Without
        // this every Rust line read "execution only", however thoroughly the
        // tests checked it.
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "supercov-rust-runner-assert-{}-{nonce}",
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
pub fn checked(value: i32) -> i32 {
    value * 2
}
pub fn unchecked(value: i32) -> i32 {
    value + 1
}
#[cfg(test)]
mod tests {
    #[test]
    fn asserts() {
        let doubled = super::checked(2);
        assert_eq!(doubled, 4);
    }
    #[test]
    fn asserts_nothing() {
        let _ = super::unchecked(1);
    }
}
"#,
        )
        .unwrap();
        let project = prepare_rust_project(&root, None).unwrap();
        let run = run_prepared_rust_tests(
            &project,
            &["cargo".into(), "test".into(), "--lib".into()],
            "rust-fixture-assert",
            "2026-08-26T00:00:00.000Z",
            None,
            &[],
            &mut Vec::new(),
        )
        .unwrap();
        assert_eq!(run.exit_code, 0);

        // The asserting test carries an assertion phase that passed; the
        // other carries none.
        let phases = |test: &str| {
            run.request
                .raw_results
                .iter()
                .find(|result| result.test.ends_with(test))
                .unwrap_or_else(|| panic!("no test {test}"))
                .phases
                .clone()
        };
        let asserting = phases("asserts");
        assert!(
            !asserting.is_empty(),
            "the asserting test recorded no phase"
        );
        assert!(
            asserting
                .iter()
                .all(|phase| phase.kind == "assertion" && phase.status.as_deref() == Some("passed"))
        );
        assert!(
            phases("asserts_nothing").is_empty(),
            "a test that checks nothing witnesses nothing"
        );

        let report =
            crate::frontend_protocol::analyze_frontend_results(&run.declaration, &run.request)
                .unwrap();
        let level = |line: usize| {
            report
                .view
                .lines
                .iter()
                .find(|entry| entry.file == "src/lib.rs" && entry.line == line)
                .map(|entry| entry.confidence.level.clone())
                .unwrap_or_else(|| panic!("no line {line}"))
        };
        // Both executed. Assertion meaning is supplied later by assertions.json.
        assert_eq!(level(3), "executed");
        assert_eq!(level(6), "executed");
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn libtest_filters_select_the_tests_plain_cargo_would_run() {
        // The user's filters were parsed and never applied: every artifact
        // was listed bare, so `--skip` and a name filter changed nothing and
        // Supercov ran a test plain Cargo did not (tokio, 1414 against 1413).
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "supercov-rust-runner-filters-{}-{nonce}",
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
pub fn value(flag: bool) -> i32 { if flag { 1 } else { 0 } }
#[cfg(test)]
mod tests {
    #[test] fn alpha() { assert_eq!(super::value(true), 1); }
    #[test] fn beta() { assert_eq!(super::value(false), 0); }
    #[test] #[ignore] fn gamma_slow() { assert_eq!(super::value(true), 1); }
}
"#,
        )
        .unwrap();
        let project = prepare_rust_project(&root, None).unwrap();
        let names = |run: &str, command: &[&str]| {
            let run = run_prepared_rust_tests(
                &project,
                &command
                    .iter()
                    .map(|word| (*word).into())
                    .collect::<Vec<String>>(),
                run,
                "2026-08-26T00:00:00.000Z",
                None,
                &[],
                &mut Vec::new(),
            )
            .unwrap();
            assert_eq!(run.exit_code, 0, "{:?}", run.request.raw_results);
            let mut names = run
                .request
                .raw_results
                .iter()
                .map(|result| {
                    (
                        result.test.rsplit("::").next().unwrap().to_owned(),
                        result.status.clone().unwrap(),
                    )
                })
                .collect::<Vec<_>>();
            names.sort();
            names
        };
        // Everything: the ignored test is listed and reported skipped.
        assert_eq!(
            names("rust-fixture-filters-1", &["cargo", "test", "--lib"]),
            [
                ("alpha".to_owned(), "passed".to_owned()),
                ("beta".to_owned(), "passed".to_owned()),
                ("gamma_slow".to_owned(), "skipped".to_owned()),
            ]
        );
        // A name filter and `--skip` narrow the enumeration as libtest does.
        assert_eq!(
            names(
                "rust-fixture-filters-2",
                &["cargo", "test", "--lib", "alpha"]
            ),
            [("alpha".to_owned(), "passed".to_owned())]
        );
        assert_eq!(
            names(
                "rust-fixture-filters-3",
                &["cargo", "test", "--lib", "--", "--skip", "beta"]
            ),
            [
                ("alpha".to_owned(), "passed".to_owned()),
                ("gamma_slow".to_owned(), "skipped".to_owned()),
            ]
        );
        // `--ignored` lists the ignored test, and then it has to RUN.
        assert_eq!(
            names(
                "rust-fixture-filters-4",
                &["cargo", "test", "--lib", "--", "--ignored"]
            ),
            [("gamma_slow".to_owned(), "passed".to_owned())]
        );
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn libtest_processes_run_in_their_package_directory() {
        // Cargo runs a test binary in its package's directory, and tokio's
        // `basic_fs` reads `Cargo.toml` from there; the workspace root has a
        // different manifest.
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "supercov-rust-runner-cwd-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir_all(root.join("member/src")).unwrap();
        fs::write(
            root.join("Cargo.toml"),
            "[workspace]\nmembers = ['member']\nresolver = '2'\n",
        )
        .unwrap();
        fs::write(
            root.join("member/Cargo.toml"),
            "[package]\nname='member'\nversion='0.0.0'\nedition='2024'\n",
        )
        .unwrap();
        fs::write(
            root.join("member/src/lib.rs"),
            r#"
pub fn manifest() -> String {
    std::fs::read_to_string("Cargo.toml").unwrap()
}
#[cfg(test)]
mod tests {
    #[test] fn reads_own_manifest() { assert!(super::manifest().contains("name='member'")); }
}
"#,
        )
        .unwrap();
        let project = prepare_rust_project(&root, None).unwrap();
        let run = run_prepared_rust_tests(
            &project,
            &["cargo".into(), "test".into(), "--lib".into()],
            "rust-fixture-cwd",
            "2026-08-26T00:00:00.000Z",
            None,
            &[],
            &mut Vec::new(),
        )
        .unwrap();
        assert_eq!(run.exit_code, 0, "{:?}", run.request.raw_results);
        assert_eq!(run.request.raw_results.len(), 1);
        assert_eq!(run.request.raw_results[0].status.as_deref(), Some("passed"));
        fs::remove_dir_all(&root).ok();
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
pub fn pick(value: i32) -> &'static str {
    match value {
        0 => "zero",
        1 => "one",
        _ => "many",
    }
}
pub fn total(values: &[i32]) -> i32 {
    let mut sum = 0;
    for value in values {
        sum += value;
    }
    sum
}
pub fn first_even(values: &[i32]) -> Option<i32> {
    let mut index = 0;
    while index < values.len() {
        if values[index] % 2 == 0 {
            return Some(values[index]);
        }
        index += 1;
    }
    None
}
pub fn parse_twice(text: &str) -> Option<i32> {
    let value: i32 = text.parse().ok()?;
    Some(value * 2)
}
pub fn describe(value: Option<i32>, flag: bool) -> &'static str {
    if let Some(inner) = value && inner > 0 && flag {
        "positive"
    } else {
        "other"
    }
}
pub fn depth(n: u32) -> Result<u32, String> {
    if n == 0 {
        Ok(0)
    } else {
        let below = depth(n - 1)?;
        Ok(below + 1)
    }
}
#[cfg(test)]
mod tests {
    #[test] fn false_path() { assert_eq!(super::choose(false, true), 0); }
    #[test] fn true_path() { assert_eq!(super::choose(true, true), 1); }
    #[test] #[ignore] fn ignored_path() { unreachable!(); }
    #[test] fn pick_zero() { assert_eq!(super::pick(0), "zero"); }
    #[test] fn pick_many() { assert_eq!(super::pick(7), "many"); }
    #[test] fn total_empty() { assert_eq!(super::total(&[]), 0); }
    #[test] fn total_some() { assert_eq!(super::total(&[1, 2]), 3); }
    #[test] fn first_even_empty() { assert_eq!(super::first_even(&[]), None); }
    #[test] fn first_even_found() { assert_eq!(super::first_even(&[1, 4]), Some(4)); }
    #[test] fn parse_ok() { assert_eq!(super::parse_twice("4"), Some(8)); }
    #[test] fn parse_bad() { assert_eq!(super::parse_twice("x"), None); }
    #[test] fn chain_taken() { assert_eq!(super::describe(Some(1), true), "positive"); }
    #[test] fn chain_pattern_fails() { assert_eq!(super::describe(None, true), "other"); }
    #[test] fn chain_negative() { assert_eq!(super::describe(Some(-1), true), "other"); }
    #[test] fn chain_flag_fails() { assert_eq!(super::describe(Some(1), false), "other"); }
    #[test] fn deep_recursion() { assert_eq!(super::depth(5_000), Ok(5_000)); }
}
"#,
        )
        .unwrap();
        let project = prepare_rust_project(&root, None).unwrap();
        // Doctests need the CLI binary standing in for rustdoc, which this
        // test binary cannot do; scripts/rust-public-cargo-integration.mjs
        // covers them end to end. `--lib` keeps this run to the libtests.
        let run = run_prepared_rust_tests(
            &project,
            &["cargo".into(), "test".into(), "--lib".into()],
            "rust-fixture",
            "2026-08-26T00:00:00.000Z",
            None,
            &[],
            &mut Vec::new(),
        )
        .unwrap();
        assert_eq!(run.exit_code, 0);
        assert_eq!(run.request.raw_results.len(), 16);
        let statuses = run
            .request
            .raw_results
            .iter()
            .filter_map(|result| result.status.as_deref())
            .collect::<Vec<_>>();
        assert_eq!(
            statuses
                .iter()
                .filter(|status| **status == "skipped")
                .count(),
            1
        );
        assert_eq!(
            statuses
                .iter()
                .filter(|status| **status == "passed")
                .count(),
            15
        );

        // The let chain's condition vectors, with the pattern's outcome
        // derived: [let Some(inner) = value, inner > 0, flag].
        let chain_vectors = |test: &str| {
            let result = run
                .request
                .raw_results
                .iter()
                .find(|result| result.test.ends_with(test))
                .unwrap_or_else(|| panic!("no test {test}"));
            let snapshot = result
                .runtime
                .iter()
                .flat_map(|snapshot| &snapshot.decisions)
                .find(|decision| decision.meta.source.starts_with("let Some(inner) = value"))
                .unwrap_or_else(|| panic!("{test} recorded no chain decision"));
            snapshot
                .vectors
                .iter()
                .map(|vector| (vector.values.clone(), vector.outcome))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            chain_vectors("chain_taken"),
            [(vec![Some(true), Some(true), Some(true)], true)]
        );
        assert_eq!(
            chain_vectors("chain_pattern_fails"),
            [(vec![Some(false), None, None], false)]
        );
        assert_eq!(
            chain_vectors("chain_negative"),
            [(vec![Some(true), Some(false), None], false)]
        );
        assert_eq!(
            chain_vectors("chain_flag_fails"),
            [(vec![Some(true), Some(true), Some(false)], false)]
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
            test_filter: None,
        })
        .unwrap();
        assert_eq!(report.view.tests.len(), 16);
        assert!(report.view.summary.lines.covered > 0);
        assert!(report.view.summary.decisions > 0);

        // Loops, the try operator and the logical operator, each with both
        // outcomes attributed to the test that produced it.
        let single = |kind: &str| {
            let mut found = report
                .view
                .branches
                .iter()
                .filter(|branch| branch.meta.kind == kind);
            let branch = found.next().unwrap_or_else(|| panic!("no {kind} branch"));
            assert!(found.next().is_none(), "more than one {kind} branch");
            branch
        };
        let tests_of = |branch: &crate::coverage_report::BranchResult, label: &str| {
            branch
                .alternatives
                .iter()
                .find(|alternative| alternative.label == label)
                .unwrap_or_else(|| panic!("{} has no alternative {label}", branch.meta.kind))
                .tests
                .clone()
        };
        let for_loop = single("for-loop");
        assert_eq!(
            tests_of(for_loop, "zero iterations"),
            ["src/lib.rs::tests::total_empty"]
        );
        assert_eq!(
            tests_of(for_loop, "entered"),
            ["src/lib.rs::tests::total_some"]
        );
        let while_loop = single("while-loop");
        assert_eq!(
            tests_of(while_loop, "zero iterations"),
            ["src/lib.rs::tests::first_even_empty"]
        );
        assert_eq!(
            tests_of(while_loop, "entered"),
            ["src/lib.rs::tests::first_even_found"]
        );
        // `depth` adds a second `?`; the one in `parse_twice` is the one
        // exercised both ways.
        let try_operator = report
            .view
            .branches
            .iter()
            .find(|branch| {
                branch.meta.kind == "try-operator" && branch.meta.source.contains("parse().ok()")
            })
            .expect("parse_twice's try operator");
        assert_eq!(
            tests_of(try_operator, "continued"),
            ["src/lib.rs::tests::parse_ok"]
        );
        assert_eq!(
            tests_of(try_operator, "early return"),
            ["src/lib.rs::tests::parse_bad"]
        );
        let mut logical = report
            .view
            .branches
            .iter()
            .filter(|branch| branch.meta.kind == "logical-and")
            .collect::<Vec<_>>();
        logical.sort_by_key(|branch| (branch.meta.line, branch.meta.column));
        // `left && right` in choose, then the chain's two operators.
        assert_eq!(logical.len(), 3);
        assert_eq!(
            tests_of(logical[0], "short-circuited"),
            ["src/lib.rs::tests::false_path"]
        );
        assert_eq!(
            tests_of(logical[0], "right operand evaluated"),
            ["src/lib.rs::tests::true_path"]
        );
        assert_eq!(
            tests_of(logical[1], "short-circuited"),
            ["src/lib.rs::tests::chain_pattern_fails"]
        );
        assert_eq!(
            tests_of(logical[1], "right operand evaluated"),
            [
                "src/lib.rs::tests::chain_flag_fails",
                "src/lib.rs::tests::chain_negative",
                "src/lib.rs::tests::chain_taken",
            ]
        );
        assert_eq!(
            tests_of(logical[2], "short-circuited"),
            [
                "src/lib.rs::tests::chain_negative",
                "src/lib.rs::tests::chain_pattern_fails",
            ]
        );
        assert_eq!(
            tests_of(logical[2], "right operand evaluated"),
            [
                "src/lib.rs::tests::chain_flag_fails",
                "src/lib.rs::tests::chain_taken",
            ]
        );
        assert!(for_loop.covered && while_loop.covered && try_operator.covered);
        assert!(logical.iter().all(|branch| branch.covered));

        // The match in `pick`: pick(0) selects the first arm; pick(7) passes
        // the first two over and selects the last. Nothing selects `1`.
        let mut arms = report
            .view
            .branches
            .iter()
            .filter(|branch| branch.meta.kind == "match-arm")
            .collect::<Vec<_>>();
        arms.sort_by_key(|branch| branch.meta.line);
        assert_eq!(arms.len(), 3);
        let alternative = |arm: usize, label: &str| {
            arms[arm]
                .alternatives
                .iter()
                .find(|alternative| alternative.label == label)
                .unwrap_or_else(|| panic!("arm {arm} has no alternative {label}"))
        };
        assert_eq!(
            alternative(0, "selected").tests,
            ["src/lib.rs::tests::pick_zero"]
        );
        assert_eq!(
            alternative(0, "not selected").tests,
            ["src/lib.rs::tests::pick_many"]
        );
        assert!(!alternative(1, "selected").covered);
        assert_eq!(
            alternative(1, "not selected").tests,
            ["src/lib.rs::tests::pick_many"]
        );
        assert_eq!(arms[2].alternatives.len(), 1);
        assert_eq!(
            alternative(2, "selected").tests,
            ["src/lib.rs::tests::pick_many"]
        );
        assert!(arms[0].covered && !arms[1].covered && arms[2].covered);

        fs::remove_dir_all(root).unwrap();
    }
}
