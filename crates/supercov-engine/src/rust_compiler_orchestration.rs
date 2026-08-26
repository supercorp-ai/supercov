//! Private production-shaped Cargo orchestration for the exact rustc companion.
//!
//! Cargo itself supplies the compiler path to `RUSTC_WORKSPACE_WRAPPER`; the
//! Supercov wrapper selects an exact companion at that boundary. This avoids
//! guessing which toolchain a working command, custom `RUSTC`, or rustup
//! override will actually use.

use std::{
    collections::BTreeMap,
    fs::{self, OpenOptions},
    io::Write,
    path::{Component, Path, PathBuf},
    process::Command,
    time::{Instant, SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};

use crate::{
    rust_compiler_ctfe::{RustCompilerCtfeUnit, read_rust_compiler_ctfe},
    rust_compiler_manifest::{NormalizedRustCompilerManifest, normalize_rust_compiler_candidates},
    rust_compiler_selection::{SelectedRustCompilerCompanion, select_rust_compiler_companion},
    rust_doctest::{RustdocMergedUnit, resolve_merged_doctest_candidates},
    rust_test_runner::cargo_invocation,
};

pub const RUST_COMPILER_WRAPPER_CONFIG_ENV: &str = "SUPERCOV_RUST_COMPILER_WRAPPER_CONFIG";
pub const RUST_COMPILER_OUTPUT_ENV: &str = "SUPERCOV_RUST_COMPILER_OUTPUT";
pub const RUST_SOURCE_ROOT_ENV: &str = "SUPERCOV_RUST_SOURCE_ROOT";
pub const RUST_TARGET_ROOT_ENV: &str = "SUPERCOV_RUST_TARGET_ROOT";
pub const RUST_INSTRUMENT_MIR_ENV: &str = "SUPERCOV_RUST_INSTRUMENT_MIR";
pub const RUST_INSTRUMENT_CTFE_ENV: &str = "SUPERCOV_RUST_INSTRUMENT_CTFE";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RustCompilerWrapperConfig {
    pub candidates: Vec<PathBuf>,
    pub require_public_capabilities: bool,
    pub selection_directory: PathBuf,
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
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RustCompilerTestArtifact {
    pub executable: PathBuf,
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
    pub doctest_units: Vec<RustdocMergedUnit>,
    pub build_started_at_ms: i64,
    pub build_ended_at_ms: i64,
    pub build_ms: f64,
}

#[derive(Debug)]
pub enum RustCompilerOrchestrationError {
    InvalidRequest(String),
    Io { path: PathBuf, reason: String },
    ExistingWorkspaceWrapper,
    Cargo(String),
    CargoOutput(String),
    CompilerOutput(String),
    Selection(String),
    Manifest(String),
}

impl std::fmt::Display for RustCompilerOrchestrationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidRequest(reason) => write!(formatter, "invalid Rust compiler build: {reason}"),
            Self::Io { path, reason } => write!(formatter, "{}: {reason}", path.display()),
            Self::ExistingWorkspaceWrapper => formatter.write_str(
                "an existing RUSTC_WORKSPACE_WRAPPER cannot yet be composed without changing compiler semantics",
            ),
            Self::Cargo(reason) => write!(formatter, "Cargo compiler build failed: {reason}"),
            Self::CargoOutput(reason) => write!(formatter, "invalid Cargo compiler output: {reason}"),
            Self::CompilerOutput(reason) => write!(formatter, "invalid Rust compiler output: {reason}"),
            Self::Selection(reason) => write!(formatter, "Rust compiler selection failed: {reason}"),
            Self::Manifest(reason) => write!(formatter, "Rust compiler manifest failed: {reason}"),
        }
    }
}

impl std::error::Error for RustCompilerOrchestrationError {}

#[derive(Debug, Deserialize)]
struct CargoMessage {
    reason: String,
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

fn write_wrapper_config(
    path: &Path,
    config: &RustCompilerWrapperConfig,
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

fn selections(
    directory: &Path,
    candidates: &[PathBuf],
    require_public_capabilities: bool,
) -> Result<SelectedRustCompilerCompanion, RustCompilerOrchestrationError> {
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
    let first = attestations.first().ok_or_else(|| {
        RustCompilerOrchestrationError::Selection(
            "Cargo invoked no authenticated compiler companion".into(),
        )
    })?;
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
    Ok(verified)
}

fn cargo_artifacts(
    stdout: &[u8],
    target_directory: &Path,
) -> Result<Vec<RustCompilerTestArtifact>, RustCompilerOrchestrationError> {
    let canonical_target =
        fs::canonicalize(target_directory).map_err(|error| io_error(target_directory, error))?;
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
        let (Some(executable), Some(target)) = (message.executable, message.target) else {
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
        artifacts.push(RustCompilerTestArtifact {
            executable,
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
    if request.command.is_empty()
        || !valid_run_id(&request.run_id)
        || request.companion_candidates.is_empty()
    {
        return Err(RustCompilerOrchestrationError::InvalidRequest(
            "command, safe run ID and companion candidates are required".into(),
        ));
    }
    if std::env::var_os("RUSTC_WORKSPACE_WRAPPER").is_some() {
        return Err(RustCompilerOrchestrationError::ExistingWorkspaceWrapper);
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
    let config_path = compiler_output_directory.join("wrapper.json");
    write_wrapper_config(
        &config_path,
        &RustCompilerWrapperConfig {
            candidates: request.companion_candidates.clone(),
            require_public_capabilities: request.require_public_capabilities,
            selection_directory: selection_directory.clone(),
        },
    )?;

    let mut invocation = cargo_invocation(&project_root, &request.command)
        .map_err(|error| RustCompilerOrchestrationError::InvalidRequest(error.to_string()))?;
    invocation
        .arguments
        .retain(|argument| argument != "--no-run" && !argument.starts_with("--message-format="));
    invocation
        .arguments
        .extend(["--no-run".into(), "--message-format=json".into()]);
    let build_started_at_ms = epoch_ms()?;
    let started = Instant::now();
    let output = Command::new(&invocation.program)
        .args(&invocation.arguments)
        .current_dir(&project_root)
        .env("CARGO_TARGET_DIR", &target_directory)
        .env("RUSTC_WORKSPACE_WRAPPER", &wrapper)
        .env(RUST_COMPILER_WRAPPER_CONFIG_ENV, &config_path)
        .env(RUST_COMPILER_OUTPUT_ENV, &candidate_directory)
        .env(RUST_SOURCE_ROOT_ENV, &project_root)
        .env(RUST_TARGET_ROOT_ENV, &target_directory)
        .env(RUST_INSTRUMENT_MIR_ENV, "1")
        .env(RUST_INSTRUMENT_CTFE_ENV, "1")
        .output()
        .map_err(|error| RustCompilerOrchestrationError::Cargo(error.to_string()))?;
    let build_ms = started.elapsed().as_secs_f64() * 1000.0;
    let build_ended_at_ms = epoch_ms()?;
    if !output.status.success() {
        let rendered = rendered_cargo_diagnostics(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(RustCompilerOrchestrationError::Cargo(
            format!("{stderr}{rendered}").trim().to_owned(),
        ));
    }
    let selection = selections(
        &selection_directory,
        &request.companion_candidates,
        request.require_public_capabilities,
    )?;
    let resolved = compiler_candidates(&candidate_directory)?;
    let normalized = normalize_rust_compiler_candidates(resolved.candidates)
        .map_err(|error| RustCompilerOrchestrationError::Manifest(error.to_string()))?;
    let ctfe_units =
        read_rust_compiler_ctfe(&candidate_directory, &normalized, build_started_at_ms)
            .map_err(|error| RustCompilerOrchestrationError::CompilerOutput(error.to_string()))?;
    let artifacts = cargo_artifacts(&output.stdout, &target_directory)?;
    Ok(RustCompilerBuild {
        selection,
        normalized,
        artifacts,
        target_directory,
        compiler_output_directory,
        ctfe_units,
        doctest_units: resolved.merged_units,
        build_started_at_ms,
        build_ended_at_ms,
        build_ms,
    })
}
