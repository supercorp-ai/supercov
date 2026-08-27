//! Private production-shaped Cargo orchestration for the exact rustc companion.
//!
//! Cargo itself supplies the compiler path to `RUSTC_WORKSPACE_WRAPPER`; the
//! Supercov wrapper selects an exact companion at that boundary. This avoids
//! guessing which toolchain a working command, custom `RUSTC`, or rustup
//! override will actually use.

use std::{
    collections::BTreeMap,
    ffi::OsString,
    fs::{self, OpenOptions},
    io::{self, Write},
    path::{Component, Path, PathBuf},
    process::Command,
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};

use crate::{
    process_supervision::{
        CommandSpec, ForwardedSignal, ProcessSupervisor, SupervisedOutput, SupervisionOptions,
    },
    rust_cargo_configuration::{RustCargoResolvedTargetRunner, RustCargoRunnerPlan},
    rust_compiler_ctfe::{RustCompilerCtfeUnit, read_rust_compiler_ctfe},
    rust_compiler_manifest::{NormalizedRustCompilerManifest, normalize_rust_compiler_candidates},
    rust_compiler_selection::{SelectedRustCompilerCompanion, select_rust_compiler_companion},
    rust_compiler_test_runner::{
        RUST_CARGO_RUNNER_CONFIG_ENV, RUST_CARGO_RUNNER_VERSION, RustCargoRunnerConfig,
        RustCargoRunnerUnit, read_cargo_runner_units,
    },
    rust_doctest::{
        RustdocOutcomeResolution, join_rustdoc_outcomes, read_rustdoc_outcome_units,
        resolve_merged_doctest_candidates,
    },
    rust_test_runner::{cargo_invocation, rust_cargo_execution_selection},
};

fn inherited_environment(
    overrides: impl IntoIterator<Item = (OsString, OsString)>,
) -> Vec<(OsString, OsString)> {
    let mut environment = std::env::vars_os().collect::<BTreeMap<_, _>>();
    environment.extend(overrides);
    environment.into_iter().collect()
}

fn supervised_success(output: &SupervisedOutput) -> bool {
    output.result.status == Some(0)
        && output.result.signal.is_none()
        && !output.result.timed_out
        && output.result.interrupted_signal.is_none()
}

fn interrupted_error(output: &SupervisedOutput) -> Option<RustCompilerOrchestrationError> {
    output.result.interrupted_signal.map(|signal| {
        let signal = match signal {
            ForwardedSignal::Sighup => "SIGHUP",
            ForwardedSignal::Sigint => "SIGINT",
            ForwardedSignal::Sigterm => "SIGTERM",
        };
        RustCompilerOrchestrationError::Interrupted {
            code: output.result.exit_code(),
            signal: signal.into(),
        }
    })
}

fn cargo_runner_configuration_arguments(
    wrapper: &Path,
    plan: &RustCargoRunnerPlan,
) -> Result<Vec<String>, RustCompilerOrchestrationError> {
    let wrapper = wrapper.to_str().ok_or_else(|| {
        RustCompilerOrchestrationError::InvalidRequest(
            "the Cargo runner executable path is not UTF-8".into(),
        )
    })?;
    let wrapper = serde_json::to_string(wrapper)
        .map_err(|error| RustCompilerOrchestrationError::InvalidRequest(error.to_string()))?;
    let mut seen = BTreeMap::new();
    let mut arguments = Vec::with_capacity(plan.targets.len() * 2);
    for target in &plan.targets {
        if seen.insert(target.target.as_str(), ()).is_some() {
            return Err(RustCompilerOrchestrationError::InvalidRequest(format!(
                "Cargo runner plan contains duplicate target identity: {}",
                target.target
            )));
        }
        let target = serde_json::to_string(&target.target)
            .map_err(|error| RustCompilerOrchestrationError::InvalidRequest(error.to_string()))?;
        arguments.extend([
            "--config".into(),
            format!("target.{target}.runner=[{wrapper},\"__cargo-test-runner\",{target}]"),
        ]);
    }
    if arguments.is_empty() {
        return Err(RustCompilerOrchestrationError::InvalidRequest(
            "Cargo runner plan has no selected targets".into(),
        ));
    }
    Ok(arguments)
}

pub const RUST_COMPILER_WRAPPER_CONFIG_ENV: &str = "SUPERCOV_RUST_COMPILER_WRAPPER_CONFIG";
pub const RUST_COMPILER_OUTPUT_ENV: &str = "SUPERCOV_RUST_COMPILER_OUTPUT";
pub const RUST_SOURCE_ROOT_ENV: &str = "SUPERCOV_RUST_SOURCE_ROOT";
pub const RUST_TARGET_ROOT_ENV: &str = "SUPERCOV_RUST_TARGET_ROOT";
pub const RUST_INSTRUMENT_MIR_ENV: &str = "SUPERCOV_RUST_INSTRUMENT_MIR";
pub const RUST_INSTRUMENT_CTFE_ENV: &str = "SUPERCOV_RUST_INSTRUMENT_CTFE";
pub const RUST_STATIC_RUNTIME_DIRECTORY_ENV: &str = "SUPERCOV_RUST_STATIC_RUNTIME_DIRECTORY";
pub const RUSTDOC_WRAPPER_MODE_ENV: &str = "SUPERCOV_RUSTDOC_WRAPPER_MODE";
pub const RUST_REAL_RUSTDOC_ENV: &str = "SUPERCOV_RUST_REAL_RUSTDOC";
pub const RUST_COMPANION_PATH_ENV: &str = "SUPERCOV_RUST_COMPANION_PATH";
pub const RUSTDOC_CAPTURE_OUTCOMES_ENV: &str = "SUPERCOV_RUSTDOC_CAPTURE_OUTCOMES";
pub const RUSTDOC_ENGINE_PATH_ENV: &str = "SUPERCOV_RUSTDOC_ENGINE_PATH";
const SHARED_RUNTIME_TEMPLATE: &str = include_str!("../runtime-assets/rust-mmap-runtime.rs");
const SHARED_RUNTIME_EXPORTS: &str = r#"
#[unsafe(no_mangle)] pub extern "C" fn __supercov_rt_ordinal_hit(ordinal: u64) { __supercov_shared_runtime::ordinal_hit(ordinal) }
#[unsafe(no_mangle)] pub extern "C" fn __supercov_rt_enter_context(context_id: u64) -> u64 { __supercov_shared_runtime::enter_context(context_id) }
#[unsafe(no_mangle)] pub extern "C" fn __supercov_rt_exit_context(previous: u64) { __supercov_shared_runtime::exit_context(previous) }
#[unsafe(no_mangle)] pub extern "C" fn __supercov_rt_enter_assertion_context(id_high: u64, id_low: u32) -> u64 { __supercov_shared_runtime::enter_assertion_context(id_high, id_low) }
#[unsafe(no_mangle)] pub extern "C" fn __supercov_rt_decision_start(id_high: u64, id_low: u32, conditions: u64) -> u64 { __supercov_shared_runtime::mir_decision_start(id_high, id_low, conditions) }
#[unsafe(no_mangle)] pub extern "C" fn __supercov_rt_decision_condition(token: u64, index: u64, value: bool) { __supercov_shared_runtime::mir_decision_condition(token, index, value) }
#[unsafe(no_mangle)] pub extern "C" fn __supercov_rt_decision_finish(token: u64, outcome: bool) { __supercov_shared_runtime::mir_decision_finish(token, outcome) }
#[unsafe(no_mangle)] pub extern "C" fn __supercov_rt_branch_start() -> u64 { __supercov_shared_runtime::mir_branch_start() }
#[unsafe(no_mangle)] pub extern "C" fn __supercov_rt_branch_hit(token: u64, ordinal: u64) { __supercov_shared_runtime::mir_branch_hit(token, ordinal) }
"#;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RustCompilerWrapperConfig {
    pub candidates: Vec<PathBuf>,
    pub require_public_capabilities: bool,
    pub selection_directory: PathBuf,
    pub shared_runtime_directory: PathBuf,
    pub target_runners: Vec<RustCargoResolvedTargetRunner>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RustCompilerBuildRequest {
    pub project_root: PathBuf,
    pub command: Vec<String>,
    pub run_id: String,
    pub wrapper_path: PathBuf,
    pub companion_candidates: Vec<PathBuf>,
    pub require_public_capabilities: bool,
    pub cargo_runner_plan: RustCargoRunnerPlan,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RustCompilerTestArtifact {
    pub executable: PathBuf,
    pub package: String,
    pub target_name: String,
    pub target_kinds: Vec<String>,
    pub source_path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RustCompilerBuild {
    pub selection: SelectedRustCompilerCompanion,
    pub normalized: NormalizedRustCompilerManifest,
    pub artifacts: Vec<RustCompilerTestArtifact>,
    pub target_directory: PathBuf,
    pub compiler_output_directory: PathBuf,
    pub ctfe_units: Vec<RustCompilerCtfeUnit>,
    pub doctest_outcomes: RustdocOutcomeResolution,
    pub cargo_runner_units: Vec<RustCargoRunnerUnit>,
    pub run_libtests: bool,
    pub run_doctests: bool,
    pub execution_exit_code: i32,
    pub execution_stdout: Vec<u8>,
    pub execution_stderr: Vec<u8>,
    pub build_started_at_ms: i64,
    pub build_ended_at_ms: i64,
    pub build_ms: f64,
    pub execution_ms: f64,
}

#[derive(Debug)]
pub enum RustCompilerOrchestrationError {
    InvalidRequest(String),
    Io { path: PathBuf, reason: String },
    ExistingCompilerWrapper,
    Cargo(String),
    CargoOutput(String),
    CompilerOutput(String),
    Selection(String),
    Manifest(String),
    Interrupted { code: i32, signal: String },
}

impl std::fmt::Display for RustCompilerOrchestrationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidRequest(reason) => write!(formatter, "invalid Rust compiler build: {reason}"),
            Self::Io { path, reason } => write!(formatter, "{}: {reason}", path.display()),
            Self::ExistingCompilerWrapper => formatter.write_str(
                "a configured Cargo compiler wrapper cannot yet be composed without changing wrapper-visible compiler semantics",
            ),
            Self::Cargo(reason) => write!(formatter, "Cargo compiler build failed: {reason}"),
            Self::CargoOutput(reason) => write!(formatter, "invalid Cargo compiler output: {reason}"),
            Self::CompilerOutput(reason) => write!(formatter, "invalid Rust compiler output: {reason}"),
            Self::Selection(reason) => write!(formatter, "Rust compiler selection failed: {reason}"),
            Self::Manifest(reason) => write!(formatter, "Rust compiler manifest failed: {reason}"),
            Self::Interrupted { signal, .. } => {
                write!(formatter, "Rust compiler run was interrupted by {signal}")
            }
        }
    }
}

impl std::error::Error for RustCompilerOrchestrationError {}

#[derive(Debug, Deserialize)]
struct CargoMessage {
    reason: String,
    #[serde(default)]
    manifest_path: Option<PathBuf>,
    #[serde(default)]
    target: Option<CargoTarget>,
    #[serde(default)]
    profile: Option<CargoProfile>,
    executable: Option<PathBuf>,
    #[serde(default)]
    message: Option<CargoDiagnostic>,
}

#[derive(Debug, Deserialize)]
struct CargoDiagnostic {
    rendered: Option<String>,
}

#[derive(Debug, Deserialize)]
struct CargoTarget {
    name: String,
    kind: Vec<String>,
    src_path: PathBuf,
}

#[derive(Debug, Deserialize)]
struct CargoProfile {
    test: bool,
}

fn io_error(path: &Path, error: impl std::fmt::Display) -> RustCompilerOrchestrationError {
    RustCompilerOrchestrationError::Io {
        path: path.to_path_buf(),
        reason: error.to_string(),
    }
}

fn epoch_ms() -> Result<i64, RustCompilerOrchestrationError> {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| RustCompilerOrchestrationError::Cargo(error.to_string()))?
        .as_millis();
    i64::try_from(millis).map_err(|error| RustCompilerOrchestrationError::Cargo(error.to_string()))
}

fn valid_run_id(value: &str) -> bool {
    !value.is_empty()
        && value != "."
        && value != ".."
        && value
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
}

fn ensure_directories(
    root: &Path,
    relative: &Path,
) -> Result<PathBuf, RustCompilerOrchestrationError> {
    if relative.is_absolute()
        || relative
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(RustCompilerOrchestrationError::InvalidRequest(format!(
            "unsafe storage path {}",
            relative.display()
        )));
    }
    let mut current = root.to_path_buf();
    for component in relative.components() {
        current.push(component.as_os_str());
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_dir() => {}
            Ok(_) => return Err(io_error(&current, "expected a non-symlink directory")),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                fs::create_dir(&current).map_err(|error| io_error(&current, error))?;
            }
            Err(error) => return Err(io_error(&current, error)),
        }
    }
    Ok(current)
}

fn regular_executable(path: &Path) -> Result<PathBuf, RustCompilerOrchestrationError> {
    let path = fs::canonicalize(path).map_err(|error| io_error(path, error))?;
    let metadata = fs::symlink_metadata(&path).map_err(|error| io_error(&path, error))?;
    if !metadata.file_type().is_file() {
        return Err(io_error(&path, "expected a regular executable"));
    }
    Ok(path)
}

fn write_json_config<T: Serialize>(
    path: &Path,
    config: &T,
) -> Result<(), RustCompilerOrchestrationError> {
    let bytes = serde_json::to_vec(config)
        .map_err(|error| RustCompilerOrchestrationError::InvalidRequest(error.to_string()))?;
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let mut file = options.open(path).map_err(|error| io_error(path, error))?;
    file.write_all(&bytes)
        .map_err(|error| io_error(path, error))?;
    file.sync_all().map_err(|error| io_error(path, error))
}

pub fn publish_compiler_selection_attestation(
    directory: &Path,
    selection: &SelectedRustCompilerCompanion,
) -> Result<PathBuf, RustCompilerOrchestrationError> {
    if !fs::symlink_metadata(directory).is_ok_and(|metadata| metadata.file_type().is_dir()) {
        return Err(io_error(
            directory,
            "compiler selection root is not a directory",
        ));
    }
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| RustCompilerOrchestrationError::Selection(error.to_string()))?
        .as_nanos();
    let stem = format!("selection-{}-{now}", std::process::id());
    let partial = directory.join(format!(".{stem}.partial"));
    let final_path = directory.join(format!("{stem}.json"));
    let bytes = serde_json::to_vec(selection)
        .map_err(|error| RustCompilerOrchestrationError::Selection(error.to_string()))?;
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let mut cleanup = RemoveFileOnDrop(Some(partial.clone()));
    let mut file = options
        .open(&partial)
        .map_err(|error| io_error(&partial, error))?;
    file.write_all(&bytes)
        .map_err(|error| io_error(&partial, error))?;
    file.sync_all().map_err(|error| io_error(&partial, error))?;
    drop(file);
    fs::rename(&partial, &final_path).map_err(|error| io_error(&final_path, error))?;
    sync_directory(directory)?;
    cleanup.0 = None;
    Ok(final_path)
}

fn write_shared_runtime_source(directory: &Path) -> Result<(), RustCompilerOrchestrationError> {
    let source = directory.join("runtime.rs");
    let runtime = format!(
        "{}\n{}",
        SHARED_RUNTIME_TEMPLATE.replace("__SUPERCOV_MODULE__", "__supercov_shared_runtime"),
        SHARED_RUNTIME_EXPORTS
    );
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let mut file = options
        .open(&source)
        .map_err(|error| io_error(&source, error))?;
    file.write_all(runtime.as_bytes())
        .map_err(|error| io_error(&source, error))?;
    file.sync_all().map_err(|error| io_error(&source, error))
}

fn shared_runtime_archive(directory: &Path) -> PathBuf {
    #[cfg(windows)]
    let name = "supercov_runtime.lib";
    #[cfg(not(windows))]
    let name = "libsupercov_runtime.a";
    directory.join(name)
}

fn valid_shared_runtime_archive(path: &Path) -> bool {
    fs::symlink_metadata(path)
        .is_ok_and(|metadata| metadata.file_type().is_file() && metadata.len() != 0)
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> Result<(), RustCompilerOrchestrationError> {
    let directory = OpenOptions::new()
        .read(true)
        .open(path)
        .map_err(|error| io_error(path, error))?;
    directory.sync_all().map_err(|error| io_error(path, error))
}

#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> Result<(), RustCompilerOrchestrationError> {
    Ok(())
}

struct RemoveFileOnDrop(Option<PathBuf>);

impl Drop for RemoveFileOnDrop {
    fn drop(&mut self) {
        if let Some(path) = self.0.take() {
            let _ = fs::remove_file(path);
        }
    }
}

struct RuntimeBuildLock {
    path: PathBuf,
    file: Option<fs::File>,
}

impl Drop for RuntimeBuildLock {
    fn drop(&mut self) {
        drop(self.file.take());
        let _ = fs::remove_file(&self.path);
    }
}

/// Compile the one process-wide Rust probe runtime with the exact rustc path
/// Cargo supplied to its wrapper. Concurrent rustc wrapper processes converge
/// on one atomically published archive; a killed builder cannot create a
/// partially valid archive or make peers wait indefinitely.
pub fn prepare_shared_rust_runtime(
    rustc: &Path,
    directory: &Path,
) -> Result<PathBuf, RustCompilerOrchestrationError> {
    let metadata = fs::symlink_metadata(directory).map_err(|error| io_error(directory, error))?;
    if !metadata.file_type().is_dir() {
        return Err(io_error(
            directory,
            "shared Rust runtime root is not a directory",
        ));
    }
    let source = directory.join("runtime.rs");
    if !fs::symlink_metadata(&source).is_ok_and(|metadata| metadata.file_type().is_file()) {
        return Err(io_error(
            &source,
            "shared Rust runtime source is not a regular file",
        ));
    }
    let archive = shared_runtime_archive(directory);
    if valid_shared_runtime_archive(&archive) {
        return Ok(archive);
    }
    let lock = directory.join("build.lock");
    let started = Instant::now();
    loop {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }
        match options.open(&lock) {
            Ok(lock_file) => {
                let mut lock_guard = RuntimeBuildLock {
                    path: lock.clone(),
                    file: Some(lock_file),
                };
                writeln!(
                    lock_guard.file.as_mut().expect("runtime build lock file"),
                    "{}",
                    std::process::id()
                )
                .and_then(|()| {
                    lock_guard
                        .file
                        .as_ref()
                        .expect("runtime build lock file")
                        .sync_all()
                })
                .map_err(|error| io_error(&lock, error))?;
                let partial = directory.join(format!(
                    ".supercov-runtime-{}-{}.partial",
                    std::process::id(),
                    SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .map_err(|error| RustCompilerOrchestrationError::Cargo(error.to_string()))?
                        .as_nanos()
                ));
                let mut partial_cleanup = RemoveFileOnDrop(Some(partial.clone()));
                let output = Command::new(rustc)
                    .args([
                        "--edition=2024",
                        "--crate-name=supercov_runtime",
                        "--crate-type=staticlib",
                        "-o",
                    ])
                    .arg(&partial)
                    .arg(&source)
                    .env_remove("RUSTC_WRAPPER")
                    .env_remove("RUSTC_WORKSPACE_WRAPPER")
                    .env_remove(RUST_COMPILER_WRAPPER_CONFIG_ENV)
                    .env_remove(RUST_INSTRUMENT_MIR_ENV)
                    .env_remove(RUST_INSTRUMENT_CTFE_ENV)
                    .output()
                    .map_err(|error| io_error(rustc, error));
                let result = match output {
                    Ok(output) if output.status.success() => {
                        let file = OpenOptions::new()
                            .read(true)
                            .open(&partial)
                            .map_err(|error| io_error(&partial, error))?;
                        file.sync_all().map_err(|error| io_error(&partial, error))?;
                        fs::rename(&partial, &archive)
                            .map_err(|error| io_error(&archive, error))?;
                        sync_directory(directory)?;
                        partial_cleanup.0 = None;
                        Ok(archive.clone())
                    }
                    Ok(output) => Err(RustCompilerOrchestrationError::Cargo(format!(
                        "exact rustc could not compile the shared Supercov runtime: {}{}",
                        String::from_utf8_lossy(&output.stderr),
                        String::from_utf8_lossy(&output.stdout)
                    ))),
                    Err(error) => Err(error),
                };
                return result;
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                if valid_shared_runtime_archive(&archive) {
                    return Ok(archive);
                }
                if started.elapsed() >= Duration::from_secs(30) {
                    return Err(io_error(
                        &lock,
                        "timed out waiting for the exact shared Rust runtime build",
                    ));
                }
                thread::sleep(Duration::from_millis(10));
            }
            Err(error) => return Err(io_error(&lock, error)),
        }
    }
}

fn compiler_candidates(
    directory: &Path,
) -> Result<crate::rust_doctest::RustdocResolvedCandidates, RustCompilerOrchestrationError> {
    let mut manifests = BTreeMap::<String, PathBuf>::new();
    let mut snapshots = BTreeMap::<String, PathBuf>::new();
    let mut merged_maps = Vec::new();
    let entries = fs::read_dir(directory)
        .map_err(|error| io_error(directory, error))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| io_error(directory, error))?;
    for entry in entries {
        let path = entry.path();
        let metadata = entry.file_type().map_err(|error| io_error(&path, error))?;
        if !metadata.is_file() {
            return Err(RustCompilerOrchestrationError::CompilerOutput(format!(
                "compiler output contains a non-file entry: {}",
                path.display()
            )));
        }
        let name = entry.file_name().into_string().map_err(|_| {
            RustCompilerOrchestrationError::CompilerOutput(
                "compiler output contains a non-UTF-8 name".into(),
            )
        })?;
        if name.starts_with("doctest-map-") && name.ends_with(".json") {
            merged_maps.push(fs::read(&path).map_err(|error| io_error(&path, error))?);
            continue;
        }
        let destination = if let Some(key) = name
            .strip_prefix("manifest-")
            .and_then(|name| name.strip_suffix(".json"))
        {
            Some((&mut manifests, key))
        } else {
            name.strip_prefix("sources-")
                .and_then(|name| name.strip_suffix(".json"))
                .map(|key| (&mut snapshots, key))
        };
        if let Some((destination, key)) = destination
            && destination.insert(key.into(), path.clone()).is_some()
        {
            return Err(RustCompilerOrchestrationError::CompilerOutput(format!(
                "duplicate compiler output identity {key}"
            )));
        }
    }
    if manifests.is_empty() || manifests.keys().ne(snapshots.keys()) {
        return Err(RustCompilerOrchestrationError::CompilerOutput(format!(
            "manifest/source snapshot identities differ (manifests: {}, snapshots: {})",
            manifests.len(),
            snapshots.len()
        )));
    }
    let pairs = manifests
        .into_iter()
        .map(|(key, manifest)| {
            let snapshot = &snapshots[&key];
            let manifest = fs::read(&manifest).map_err(|error| io_error(&manifest, error))?;
            let snapshot = fs::read(snapshot).map_err(|error| io_error(snapshot, error))?;
            Ok((manifest, snapshot))
        })
        .collect::<Result<Vec<_>, RustCompilerOrchestrationError>>()?;
    resolve_merged_doctest_candidates(pairs, merged_maps)
        .map_err(|error| RustCompilerOrchestrationError::Manifest(error.to_string()))
}

pub fn verified_compiler_selection(
    directory: &Path,
    candidates: &[PathBuf],
    require_public_capabilities: bool,
    allow_in_progress: bool,
) -> Result<Option<SelectedRustCompilerCompanion>, RustCompilerOrchestrationError> {
    let mut attestations = Vec::new();
    let entries = fs::read_dir(directory)
        .map_err(|error| io_error(directory, error))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| io_error(directory, error))?;
    for entry in entries {
        let path = entry.path();
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            return Err(RustCompilerOrchestrationError::CompilerOutput(
                "selection output contains a non-UTF-8 name".into(),
            ));
        };
        if name.starts_with(".selection-") && name.ends_with(".partial") && allow_in_progress {
            continue;
        }
        if !name.starts_with("selection-") || !name.ends_with(".json") {
            return Err(RustCompilerOrchestrationError::CompilerOutput(format!(
                "unexpected selection output {name}"
            )));
        }
        if !entry
            .file_type()
            .map_err(|error| io_error(&path, error))?
            .is_file()
        {
            return Err(RustCompilerOrchestrationError::CompilerOutput(format!(
                "selection output is not a regular file: {}",
                path.display()
            )));
        }
        let bytes = fs::read(&path).map_err(|error| io_error(&path, error))?;
        attestations.push(
            serde_json::from_slice::<SelectedRustCompilerCompanion>(&bytes).map_err(|error| {
                RustCompilerOrchestrationError::Selection(format!(
                    "invalid wrapper attestation {}: {error}",
                    path.display()
                ))
            })?,
        );
    }
    let Some(first) = attestations.first() else {
        return Ok(None);
    };
    if attestations.iter().any(|selection| selection != first) {
        return Err(RustCompilerOrchestrationError::Selection(
            "Cargo used more than one compiler identity or companion".into(),
        ));
    }
    let verified =
        select_rust_compiler_companion(&first.rustc_path, candidates, require_public_capabilities)
            .map_err(|error| RustCompilerOrchestrationError::Selection(error.to_string()))?;
    if &verified != first {
        return Err(RustCompilerOrchestrationError::Selection(
            "wrapper attestation changed during post-build verification".into(),
        ));
    }
    Ok(Some(verified))
}

fn cargo_artifacts(
    stdout: &[u8],
    target_directory: &Path,
    project_root: &Path,
) -> Result<Vec<RustCompilerTestArtifact>, RustCompilerOrchestrationError> {
    let canonical_target =
        fs::canonicalize(target_directory).map_err(|error| io_error(target_directory, error))?;
    let canonical_project =
        fs::canonicalize(project_root).map_err(|error| io_error(project_root, error))?;
    let mut artifacts = Vec::new();
    for line in stdout
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
    {
        let message: CargoMessage = serde_json::from_slice(line)
            .map_err(|error| RustCompilerOrchestrationError::CargoOutput(error.to_string()))?;
        if message.reason != "compiler-artifact"
            || !message.profile.as_ref().is_some_and(|profile| profile.test)
        {
            continue;
        }
        let (Some(executable), Some(manifest_path), Some(target)) =
            (message.executable, message.manifest_path, message.target)
        else {
            continue;
        };
        let executable =
            fs::canonicalize(&executable).map_err(|error| io_error(&executable, error))?;
        let metadata =
            fs::symlink_metadata(&executable).map_err(|error| io_error(&executable, error))?;
        if !executable.starts_with(&canonical_target) || !metadata.file_type().is_file() {
            return Err(RustCompilerOrchestrationError::CargoOutput(format!(
                "test artifact escaped the private target: {}",
                executable.display()
            )));
        }
        let manifest_metadata = fs::symlink_metadata(&manifest_path)
            .map_err(|error| io_error(&manifest_path, error))?;
        let manifest =
            fs::canonicalize(&manifest_path).map_err(|error| io_error(&manifest_path, error))?;
        let package_root = manifest
            .parent()
            .and_then(|path| path.strip_prefix(&canonical_project).ok())
            .filter(|_| {
                manifest_metadata.file_type().is_file()
                    && manifest
                        .file_name()
                        .is_some_and(|name| name == "Cargo.toml")
            })
            .ok_or_else(|| {
                RustCompilerOrchestrationError::CargoOutput(format!(
                    "test artifact manifest escaped the owned project: {}",
                    manifest.display()
                ))
            })?;
        let package = if package_root.as_os_str().is_empty() {
            "package:.".to_owned()
        } else if package_root
            .components()
            .all(|component| matches!(component, std::path::Component::Normal(_)))
        {
            format!(
                "package:{}",
                package_root.to_string_lossy().replace('\\', "/")
            )
        } else {
            return Err(RustCompilerOrchestrationError::CargoOutput(format!(
                "test artifact has a noncanonical package root: {}",
                package_root.display()
            )));
        };
        artifacts.push(RustCompilerTestArtifact {
            executable,
            package,
            target_name: target.name,
            target_kinds: target.kind,
            source_path: target.src_path,
        });
    }
    artifacts.sort_by(|left, right| left.executable.cmp(&right.executable));
    artifacts.dedup_by(|left, right| left.executable == right.executable);
    if artifacts.is_empty() {
        return Err(RustCompilerOrchestrationError::CargoOutput(
            "Cargo emitted no executable test artifacts".into(),
        ));
    }
    Ok(artifacts)
}

fn rendered_cargo_diagnostics(stdout: &[u8]) -> String {
    stdout
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .filter_map(|line| serde_json::from_slice::<CargoMessage>(line).ok())
        .filter_map(|message| message.message.and_then(|message| message.rendered))
        .collect::<Vec<_>>()
        .join("")
}

pub fn build_with_rust_compiler_companion(
    request: &RustCompilerBuildRequest,
) -> Result<RustCompilerBuild, RustCompilerOrchestrationError> {
    let supervisor = ProcessSupervisor::new()
        .map_err(|error| RustCompilerOrchestrationError::Cargo(error.to_string()))?;
    let options = SupervisionOptions::from_environment()
        .map_err(|error| RustCompilerOrchestrationError::Cargo(error.to_string()))?;
    build_with_rust_compiler_companion_supervised(request, &supervisor, options, &mut io::sink())
}

pub fn build_with_rust_compiler_companion_supervised(
    request: &RustCompilerBuildRequest,
    supervisor: &ProcessSupervisor,
    options: SupervisionOptions,
    diagnostics: &mut dyn Write,
) -> Result<RustCompilerBuild, RustCompilerOrchestrationError> {
    if request.command.is_empty()
        || !valid_run_id(&request.run_id)
        || request.companion_candidates.is_empty()
    {
        return Err(RustCompilerOrchestrationError::InvalidRequest(
            "command, safe run ID and companion candidates are required".into(),
        ));
    }
    if request.cargo_runner_plan.compiler.rustc_wrapper.is_some()
        || request
            .cargo_runner_plan
            .compiler
            .rustc_workspace_wrapper
            .is_some()
    {
        return Err(RustCompilerOrchestrationError::ExistingCompilerWrapper);
    }
    if std::env::var_os("RUSTDOC").is_some() {
        return Err(RustCompilerOrchestrationError::InvalidRequest(
            "an existing RUSTDOC executable cannot yet be composed without changing rustdoc semantics"
                .into(),
        ));
    }
    let project_root = fs::canonicalize(&request.project_root)
        .map_err(|error| io_error(&request.project_root, error))?;
    if !fs::symlink_metadata(&project_root).is_ok_and(|metadata| metadata.file_type().is_dir()) {
        return Err(io_error(&project_root, "expected a project directory"));
    }
    let wrapper = regular_executable(&request.wrapper_path)?;
    let run_root = ensure_directories(
        &project_root,
        &PathBuf::from(".supercov/work").join(&request.run_id),
    )?;
    let compiler_output_directory = run_root.join("rust-compiler");
    fs::create_dir(&compiler_output_directory)
        .map_err(|error| io_error(&compiler_output_directory, error))?;
    let selection_directory = compiler_output_directory.join("selections");
    let candidate_directory = compiler_output_directory.join("candidates");
    fs::create_dir(&selection_directory).map_err(|error| io_error(&selection_directory, error))?;
    fs::create_dir(&candidate_directory).map_err(|error| io_error(&candidate_directory, error))?;
    let target_directory = run_root.join("rust-target");
    fs::create_dir(&target_directory).map_err(|error| io_error(&target_directory, error))?;
    let shared_runtime_directory = compiler_output_directory.join("shared-runtime");
    fs::create_dir(&shared_runtime_directory)
        .map_err(|error| io_error(&shared_runtime_directory, error))?;
    let cargo_runner_directory = compiler_output_directory.join("cargo-runner");
    fs::create_dir(&cargo_runner_directory)
        .map_err(|error| io_error(&cargo_runner_directory, error))?;
    write_shared_runtime_source(&shared_runtime_directory)?;
    let target_runners = request
        .cargo_runner_plan
        .targets
        .iter()
        .map(|target| target.resolve(&project_root))
        .collect::<Vec<_>>();
    let config_path = compiler_output_directory.join("wrapper.json");
    write_json_config(
        &config_path,
        &RustCompilerWrapperConfig {
            candidates: request.companion_candidates.clone(),
            require_public_capabilities: request.require_public_capabilities,
            selection_directory: selection_directory.clone(),
            shared_runtime_directory: shared_runtime_directory.clone(),
            target_runners: target_runners.clone(),
        },
    )?;
    let cargo_runner_config_path = compiler_output_directory.join("cargo-runner.json");
    write_json_config(
        &cargo_runner_config_path,
        &RustCargoRunnerConfig {
            version: RUST_CARGO_RUNNER_VERSION,
            run_id: request.run_id.clone(),
            target_directory: target_directory.clone(),
            output_directory: cargo_runner_directory.clone(),
            target_runners,
        },
    )?;

    let mut invocation = cargo_invocation(&project_root, &request.command)
        .map_err(|error| RustCompilerOrchestrationError::InvalidRequest(error.to_string()))?;
    let execution = rust_cargo_execution_selection(&invocation)
        .map_err(|error| RustCompilerOrchestrationError::InvalidRequest(error.to_string()))?;
    let execution_arguments = invocation.arguments.clone();
    let build_started_at_ms = epoch_ms()?;
    let started = Instant::now();
    let output = if execution.run_libtests {
        invocation.arguments.retain(|argument| {
            argument != "--no-run" && !argument.starts_with("--message-format=")
        });
        invocation
            .arguments
            .extend(["--no-run".into(), "--message-format=json".into()]);
        let environment = inherited_environment([
            (
                OsString::from("CARGO_TARGET_DIR"),
                target_directory.clone().into_os_string(),
            ),
            (
                OsString::from("RUSTC_WORKSPACE_WRAPPER"),
                wrapper.clone().into_os_string(),
            ),
            (
                OsString::from(RUST_COMPILER_WRAPPER_CONFIG_ENV),
                config_path.clone().into_os_string(),
            ),
            (
                OsString::from(RUST_COMPILER_OUTPUT_ENV),
                candidate_directory.clone().into_os_string(),
            ),
            (
                OsString::from(RUST_SOURCE_ROOT_ENV),
                project_root.clone().into_os_string(),
            ),
            (
                OsString::from(RUST_TARGET_ROOT_ENV),
                target_directory.clone().into_os_string(),
            ),
            (OsString::from(RUST_INSTRUMENT_MIR_ENV), OsString::from("1")),
            (
                OsString::from(RUST_INSTRUMENT_CTFE_ENV),
                OsString::from("1"),
            ),
        ]);
        let output = supervisor
            .supervise_captured(
                &CommandSpec {
                    program: invocation.program.clone().into(),
                    arguments: invocation.arguments.iter().map(OsString::from).collect(),
                    cwd: project_root.clone(),
                    environment: Some(environment),
                    captured_output: None,
                },
                options,
                diagnostics,
            )
            .map_err(|error| RustCompilerOrchestrationError::Cargo(error.to_string()))?;
        if let Some(error) = interrupted_error(&output) {
            return Err(error);
        }
        if !supervised_success(&output) {
            let rendered = rendered_cargo_diagnostics(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(RustCompilerOrchestrationError::Cargo(
                format!("{stderr}{rendered}").trim().to_owned(),
            ));
        }
        Some(output)
    } else {
        None
    };
    let mut full_arguments = execution_arguments;
    full_arguments.extend(cargo_runner_configuration_arguments(
        &wrapper,
        &request.cargo_runner_plan,
    )?);
    if !invocation.runner_arguments.is_empty() {
        full_arguments.push("--".into());
        full_arguments.extend(invocation.runner_arguments.iter().cloned());
    }
    let execution_started = Instant::now();
    let execution_output = supervisor
        .supervise_captured(
            &CommandSpec {
                program: invocation.program.clone().into(),
                arguments: full_arguments.into_iter().map(OsString::from).collect(),
                cwd: project_root.clone(),
                environment: Some(inherited_environment([
                    (
                        OsString::from("CARGO_TARGET_DIR"),
                        target_directory.clone().into_os_string(),
                    ),
                    (
                        OsString::from("RUSTC_WORKSPACE_WRAPPER"),
                        wrapper.clone().into_os_string(),
                    ),
                    (
                        OsString::from(RUST_COMPILER_WRAPPER_CONFIG_ENV),
                        config_path.clone().into_os_string(),
                    ),
                    (
                        OsString::from(RUST_COMPILER_OUTPUT_ENV),
                        candidate_directory.clone().into_os_string(),
                    ),
                    (
                        OsString::from(RUST_SOURCE_ROOT_ENV),
                        project_root.clone().into_os_string(),
                    ),
                    (
                        OsString::from(RUST_TARGET_ROOT_ENV),
                        target_directory.clone().into_os_string(),
                    ),
                    (OsString::from(RUST_INSTRUMENT_MIR_ENV), OsString::from("1")),
                    (
                        OsString::from(RUST_INSTRUMENT_CTFE_ENV),
                        OsString::from("1"),
                    ),
                    (
                        OsString::from(RUST_STATIC_RUNTIME_DIRECTORY_ENV),
                        shared_runtime_directory.clone().into_os_string(),
                    ),
                    (OsString::from("RUSTDOC"), wrapper.clone().into_os_string()),
                    (
                        OsString::from(RUSTDOC_WRAPPER_MODE_ENV),
                        OsString::from("1"),
                    ),
                    (
                        OsString::from(RUST_CARGO_RUNNER_CONFIG_ENV),
                        cargo_runner_config_path.clone().into_os_string(),
                    ),
                ])),
                captured_output: None,
            },
            options,
            diagnostics,
        )
        .map_err(|error| RustCompilerOrchestrationError::Cargo(error.to_string()))?;
    if let Some(error) = interrupted_error(&execution_output) {
        return Err(error);
    }
    let execution_ms = execution_started.elapsed().as_secs_f64() * 1000.0;
    let build_ms = started.elapsed().as_secs_f64() * 1000.0;
    let build_ended_at_ms = epoch_ms()?;
    let selection = verified_compiler_selection(
        &selection_directory,
        &request.companion_candidates,
        request.require_public_capabilities,
        false,
    )?
    .ok_or_else(|| {
        RustCompilerOrchestrationError::Selection(
            "Cargo invoked no authenticated compiler companion".into(),
        )
    })?;
    let resolved = compiler_candidates(&candidate_directory)?;
    let normalized = normalize_rust_compiler_candidates(resolved.candidates)
        .map_err(|error| RustCompilerOrchestrationError::Manifest(error.to_string()))?;
    let ctfe_units =
        read_rust_compiler_ctfe(&candidate_directory, &normalized, build_started_at_ms)
            .map_err(|error| RustCompilerOrchestrationError::CompilerOutput(error.to_string()))?;
    let doctest_outcomes = read_rustdoc_outcome_units(&candidate_directory)
        .map_err(|error| RustCompilerOrchestrationError::CompilerOutput(error.to_string()))?;
    if doctest_outcomes
        .iter()
        .any(|unit| unit.companion_build_id != selection.handshake.companion_build_id)
    {
        return Err(RustCompilerOrchestrationError::CompilerOutput(
            "rustdoc outcome unit was produced by a different compiler companion".into(),
        ));
    }
    let doctest_outcomes = join_rustdoc_outcomes(resolved.merged_units, doctest_outcomes)
        .map_err(|error| RustCompilerOrchestrationError::CompilerOutput(error.to_string()))?;
    let artifacts = output
        .as_ref()
        .map(|output| cargo_artifacts(&output.stdout, &target_directory, &project_root))
        .transpose()?
        .unwrap_or_default();
    let expected_targets = request
        .cargo_runner_plan
        .targets
        .iter()
        .map(|target| target.target.clone())
        .collect::<Vec<_>>();
    let cargo_runner_units =
        read_cargo_runner_units(&cargo_runner_directory, &request.run_id, &expected_targets)
            .map_err(|error| {
                let stderr = String::from_utf8_lossy(&execution_output.stderr);
                RustCompilerOrchestrationError::CompilerOutput(
                    format!("{error}\n{stderr}").trim().to_owned(),
                )
            })?;
    let execution_exit_code = execution_output.result.exit_code();
    Ok(RustCompilerBuild {
        selection,
        normalized,
        artifacts,
        target_directory,
        compiler_output_directory,
        ctfe_units,
        doctest_outcomes,
        cargo_runner_units,
        run_libtests: execution.run_libtests,
        run_doctests: execution.run_doctests,
        execution_exit_code,
        execution_stdout: execution_output.stdout,
        execution_stderr: execution_output.stderr,
        build_started_at_ms,
        build_ended_at_ms,
        build_ms,
        execution_ms,
    })
}

#[cfg(test)]
mod tests {
    use std::{
        collections::BTreeSet,
        sync::atomic::{AtomicU64, Ordering},
    };

    use super::*;

    struct TemporaryDirectory(PathBuf);

    static TEMPORARY_DIRECTORY_NONCE: AtomicU64 = AtomicU64::new(0);

    impl TemporaryDirectory {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "supercov-shared-rust-runtime-{}-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_nanos(),
                TEMPORARY_DIRECTORY_NONCE.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for TemporaryDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn exact_rustc_concurrently_publishes_one_shared_runtime_without_debris() {
        let directory = TemporaryDirectory::new();
        write_shared_runtime_source(&directory.0).unwrap();
        let archives = std::thread::scope(|scope| {
            (0..4)
                .map(|_| {
                    scope.spawn(|| {
                        prepare_shared_rust_runtime(Path::new("rustc"), &directory.0).unwrap()
                    })
                })
                .collect::<Vec<_>>()
                .into_iter()
                .map(|thread| thread.join().unwrap())
                .collect::<Vec<_>>()
        });
        assert!(archives.windows(2).all(|pair| pair[0] == pair[1]));
        assert!(valid_shared_runtime_archive(&archives[0]));
        let names = fs::read_dir(&directory.0)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect::<BTreeSet<_>>();
        assert_eq!(
            names,
            BTreeSet::from([
                "runtime.rs".into(),
                archives[0]
                    .file_name()
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .to_owned()
            ])
        );
    }

    #[test]
    fn failed_shared_runtime_builder_leaves_no_lock_or_partial_archive() {
        let directory = TemporaryDirectory::new();
        write_shared_runtime_source(&directory.0).unwrap();
        let missing_rustc = directory.0.join("missing-rustc");
        let error = prepare_shared_rust_runtime(&missing_rustc, &directory.0).unwrap_err();
        assert!(error.to_string().contains("missing-rustc"));
        let names = fs::read_dir(&directory.0)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect::<BTreeSet<_>>();
        assert_eq!(names, BTreeSet::from(["runtime.rs".into()]));
    }

    #[test]
    fn cargo_artifacts_bind_relocatable_workspace_package_identity() {
        fn fixture(root: &Path) -> (PathBuf, Vec<u8>) {
            let target = root.join("target");
            fs::create_dir(&target).unwrap();
            let mut messages = Vec::new();
            for (index, package_root) in [Path::new("."), Path::new("crates/sibling")]
                .into_iter()
                .enumerate()
            {
                let package = root.join(package_root);
                let source = package.join("src/lib.rs");
                fs::create_dir_all(source.parent().unwrap()).unwrap();
                fs::write(
                    package.join("Cargo.toml"),
                    "[package]\nname='fixture'\nversion='0.0.0'\n",
                )
                .unwrap();
                fs::write(&source, "#[test] fn same_name() {}\n").unwrap();
                let executable = target.join(format!("same-target-{index}"));
                fs::write(&executable, b"artifact").unwrap();
                messages.extend(
                    serde_json::to_vec(&serde_json::json!({
                        "reason": "compiler-artifact",
                        "package_id": format!("opaque-{index}"),
                        "manifest_path": package.join("Cargo.toml"),
                        "target": {
                            "name": "same_target",
                            "kind": ["lib"],
                            "src_path": source,
                        },
                        "profile": { "test": true },
                        "executable": executable,
                    }))
                    .unwrap(),
                );
                messages.push(b'\n');
            }
            (target, messages)
        }

        let first = TemporaryDirectory::new();
        let second = TemporaryDirectory::new();
        let (first_target, first_messages) = fixture(&first.0);
        let (second_target, second_messages) = fixture(&second.0);
        let first_artifacts = cargo_artifacts(&first_messages, &first_target, &first.0).unwrap();
        let second_artifacts =
            cargo_artifacts(&second_messages, &second_target, &second.0).unwrap();
        assert_eq!(
            first_artifacts
                .iter()
                .map(|artifact| artifact.package.as_str())
                .collect::<BTreeSet<_>>(),
            BTreeSet::from(["package:.", "package:crates/sibling"])
        );
        assert_eq!(
            first_artifacts
                .iter()
                .map(|artifact| &artifact.package)
                .collect::<Vec<_>>(),
            second_artifacts
                .iter()
                .map(|artifact| &artifact.package)
                .collect::<Vec<_>>(),
            "package identities changed when the workspace moved"
        );
    }

    #[test]
    fn cargo_runner_configuration_is_target_indexed_and_rejects_aliases() {
        use crate::rust_cargo_configuration::{
            RustCargoCompilerCommandPlan, RustCargoRunnerProgram, RustCargoTargetRunnerPlan,
        };

        let plan = RustCargoRunnerPlan {
            compiler: RustCargoCompilerCommandPlan {
                rustc: RustCargoRunnerProgram::SearchPath {
                    value: "rustc".into(),
                },
                rustc_wrapper: None,
                rustc_workspace_wrapper: None,
            },
            targets: vec![
                RustCargoTargetRunnerPlan {
                    target: "aarch64-apple-darwin".into(),
                    underlying_runner: None,
                },
                RustCargoTargetRunnerPlan {
                    target: "x86_64-unknown-linux-gnu".into(),
                    underlying_runner: None,
                },
            ],
        };
        assert_eq!(
            cargo_runner_configuration_arguments(Path::new("/opt/super cov"), &plan).unwrap(),
            [
                "--config",
                "target.\"aarch64-apple-darwin\".runner=[\"/opt/super cov\",\"__cargo-test-runner\",\"aarch64-apple-darwin\"]",
                "--config",
                "target.\"x86_64-unknown-linux-gnu\".runner=[\"/opt/super cov\",\"__cargo-test-runner\",\"x86_64-unknown-linux-gnu\"]",
            ]
        );
        let duplicate = RustCargoRunnerPlan {
            compiler: plan.compiler.clone(),
            targets: vec![plan.targets[0].clone(), plan.targets[0].clone()],
        };
        assert!(
            cargo_runner_configuration_arguments(Path::new("/opt/supercov"), &duplicate)
                .unwrap_err()
                .to_string()
                .contains("duplicate target identity")
        );
    }
}
