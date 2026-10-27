//! Exact pre-execution resolution for Cargo target runners.
//!
//! Cargo remains the authority for building and ordering test artifacts, but
//! Supercov must compose an already-configured target runner inside its
//! authenticated Cargo-runner boundary without changing Cargo or libtest
//! semantics. This module resolves the user's original Cargo configuration
//! before the repository is copied into the isolated workspace. Unsupported
//! configuration surfaces fail before user code executes.

use crate::fault_fs as fs;
use crate::fault_process::Launch;
use std::{
    collections::{BTreeSet, HashMap},
    ffi::OsString,
    path::{Component, Path, PathBuf},
    process::Command,
    str::FromStr,
};

use cargo_config2::cargo_home_with_cwd;
use cargo_platform::{Cfg, CfgExpr};
use serde::{Deserialize, Serialize};

use crate::{
    rust_cargo_config_model::{
        CargoConfigDefinition, CargoConfigKind, CargoConfigValue, load_cargo_configuration,
    },
    rust_test_runner::CargoTestInvocation,
};

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum RustCargoRunnerProgram {
    SearchPath { value: String },
    Absolute { value: PathBuf },
    WorkspaceRelative { value: PathBuf },
}

impl RustCargoRunnerProgram {
    pub fn resolve(&self, workspace: &Path) -> PathBuf {
        match self {
            Self::SearchPath { value } => PathBuf::from(value),
            Self::Absolute { value } => value.clone(),
            Self::WorkspaceRelative { value } => workspace.join(value),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RustCargoUnderlyingRunner {
    pub program: RustCargoRunnerProgram,
    pub arguments: Vec<String>,
}

impl RustCargoUnderlyingRunner {
    pub fn resolve(&self, workspace: &Path) -> RustCargoResolvedRunner {
        RustCargoResolvedRunner {
            program: self.program.resolve(workspace),
            arguments: self.arguments.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RustCargoResolvedRunner {
    pub program: PathBuf,
    pub arguments: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RustCargoTargetRunnerPlan {
    pub target: String,
    pub underlying_runner: Option<RustCargoUnderlyingRunner>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RustCargoRunnerPlan {
    pub compiler: RustCargoCompilerCommandPlan,
    pub targets: Vec<RustCargoTargetRunnerPlan>,
    /// The flags the configuration gives rustc where the environment sets
    /// no `RUSTFLAGS`; none when it gives none, or its targets differ.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rustflags: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RustCargoCompilerCommandPlan {
    pub rustc: RustCargoRunnerProgram,
    pub rustc_wrapper: Option<RustCargoRunnerProgram>,
    pub rustc_workspace_wrapper: Option<RustCargoRunnerProgram>,
}

impl RustCargoCompilerCommandPlan {
    fn resolved_programs(&self, workspace: &Path) -> Vec<PathBuf> {
        self.rustc_wrapper
            .iter()
            .chain(self.rustc_workspace_wrapper.iter())
            .chain(std::iter::once(&self.rustc))
            .map(|program| program.resolve(workspace))
            .collect()
    }

    fn command(&self, workspace: &Path) -> Command {
        let programs = self.resolved_programs(workspace);
        let mut command = Command::new(&programs[0]);
        command.args(&programs[1..]);
        command
    }
}

#[derive(Debug, Clone)]
struct CargoModelInputs {
    cargo_home: Option<PathBuf>,
    environment: HashMap<String, OsString>,
    host_override: Option<String>,
}

impl CargoModelInputs {
    fn ambient(root: &Path) -> Self {
        Self {
            cargo_home: cargo_home_with_cwd(root),
            environment: std::env::vars_os()
                .filter_map(|(key, value)| key.into_string().ok().map(|key| (key, value)))
                .collect(),
            host_override: None,
        }
    }

    fn environment(&self, key: &str) -> Option<&OsString> {
        self.environment.get(key)
    }
}

#[derive(Debug)]
pub enum RustCargoConfigurationError {
    Io { path: PathBuf, reason: String },
    Invalid(String),
    Unsupported(String),
}

impl std::fmt::Display for RustCargoConfigurationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io { path, reason } => write!(formatter, "{}: {reason}", path.display()),
            Self::Invalid(reason) => write!(formatter, "invalid Cargo configuration: {reason}"),
            Self::Unsupported(reason) => formatter.write_str(reason),
        }
    }
}

impl std::error::Error for RustCargoConfigurationError {}

fn io_error(path: &Path, error: impl std::fmt::Display) -> RustCargoConfigurationError {
    RustCargoConfigurationError::Io {
        path: path.to_owned(),
        reason: error.to_string(),
    }
}

/// What a Cargo command line itself says: its `--target`s, its `--config`
/// values, and whether it names a rustup toolchain.
#[derive(Debug, PartialEq)]
struct CommandLine {
    targets: Vec<String>,
    config: Vec<String>,
    toolchain: bool,
}

fn command_line(
    invocation: &CargoTestInvocation,
) -> Result<CommandLine, RustCargoConfigurationError> {
    let command_position = invocation.command_position();
    let toolchain = toolchain_selector(invocation, command_position)?.is_some();
    if invocation.arguments[..command_position]
        .iter()
        .any(|argument| argument == "-Z" || argument.starts_with("-Z"))
    {
        return Err(RustCargoConfigurationError::Unsupported(
            "Cargo runner composition does not yet resolve pre-subcommand -Z configuration semantics exactly"
                .into(),
        ));
    }
    let mut targets = Vec::new();
    let mut config = Vec::new();
    let mut index = 0;
    while index < invocation.arguments.len() {
        let argument = &invocation.arguments[index];
        if argument == "--config" {
            let value = invocation.arguments.get(index + 1).ok_or_else(|| {
                RustCargoConfigurationError::Invalid("--config has no value".into())
            })?;
            config.push(value.clone());
            index += 2;
            continue;
        }
        if let Some(value) = argument.strip_prefix("--config=") {
            if value.is_empty() {
                return Err(RustCargoConfigurationError::Invalid(
                    "--config has no value".into(),
                ));
            }
            config.push(value.to_owned());
            index += 1;
            continue;
        }
        if argument == "--target" {
            let target = invocation.arguments.get(index + 1).ok_or_else(|| {
                RustCargoConfigurationError::Invalid("--target has no value".into())
            })?;
            targets.push(target.clone());
            index += 2;
            continue;
        }
        if let Some(target) = argument.strip_prefix("--target=") {
            if target.is_empty() {
                return Err(RustCargoConfigurationError::Invalid(
                    "--target has no value".into(),
                ));
            }
            targets.push(target.to_owned());
        }
        index += 1;
    }
    Ok(CommandLine {
        targets,
        config,
        toolchain,
    })
}

fn toolchain_selector(
    invocation: &CargoTestInvocation,
    test_position: usize,
) -> Result<Option<&str>, RustCargoConfigurationError> {
    let prefix = &invocation.arguments[..test_position];
    let selectors = prefix
        .iter()
        .enumerate()
        .filter(|(_, argument)| argument.starts_with('+'))
        .collect::<Vec<_>>();
    match selectors.as_slice() {
        [] => Ok(None),
        [(0, selector)] if selector.len() > 1 => Ok(Some(&selector[1..])),
        [(0, _)] => Err(RustCargoConfigurationError::Invalid(
            "the rustup toolchain selector is empty".into(),
        )),
        _ => Err(RustCargoConfigurationError::Invalid(
            "the rustup +toolchain selector must be the first and only selector before the Cargo subcommand"
                .into(),
        )),
    }
}

fn command_stdout(
    program: &Path,
    arguments: &[&str],
    operation: &str,
) -> Result<String, RustCargoConfigurationError> {
    let output = Command::new(program)
        .args(arguments)
        .launch_output()
        .map_err(|error| io_error(program, error))?;
    if !output.status.success() {
        return Err(RustCargoConfigurationError::Invalid(format!(
            "{} failed while {operation} with status {}: {}",
            program.display(),
            output
                .status
                .code()
                .map_or_else(|| "signal".into(), |value| value.to_string()),
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    if !output.stderr.is_empty() {
        return Err(RustCargoConfigurationError::Invalid(format!(
            "{} wrote unexpected stderr while {operation}: {}",
            program.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    String::from_utf8(output.stdout).map_err(|_| {
        RustCargoConfigurationError::Invalid(format!(
            "{} produced non-UTF-8 output while {operation}",
            program.display()
        ))
    })
}

fn rustup_program(cargo: &Path) -> PathBuf {
    cargo
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .map_or_else(
            || PathBuf::from(format!("rustup{}", std::env::consts::EXE_SUFFIX)),
            |parent| parent.join(format!("rustup{}", std::env::consts::EXE_SUFFIX)),
        )
}

fn selected_cargo_program(
    invocation: &CargoTestInvocation,
) -> Result<OsString, RustCargoConfigurationError> {
    let command_position = invocation.command_position();
    let Some(selector) = toolchain_selector(invocation, command_position)? else {
        return Ok(invocation.program.clone().into());
    };
    let cargo_proxy = which::which(&invocation.program).map_err(|error| {
        RustCargoConfigurationError::Invalid(format!(
            "could not resolve the Cargo proxy {}: {error}",
            invocation.program
        ))
    })?;
    let rustup = rustup_program(&cargo_proxy);
    let selected = command_stdout(
        &rustup,
        &["which", "--toolchain", selector, "cargo"],
        "resolving the explicit rustup toolchain's Cargo",
    )?;
    let selected = PathBuf::from(selected.trim());
    if !selected.is_absolute() {
        return Err(RustCargoConfigurationError::Invalid(format!(
            "rustup returned a non-absolute Cargo path for +{selector}: {}",
            selected.display()
        )));
    }
    let selected = fs::canonicalize(&selected).map_err(|error| io_error(&selected, error))?;
    let metadata = fs::symlink_metadata(&selected).map_err(|error| io_error(&selected, error))?;
    if !metadata.file_type().is_file() {
        return Err(RustCargoConfigurationError::Invalid(format!(
            "rustup returned a non-regular Cargo path for +{selector}: {}",
            selected.display()
        )));
    }
    let proxy_version = command_stdout(
        &cargo_proxy,
        &[&format!("+{selector}"), "-Vv"],
        "verifying the explicit rustup Cargo selection",
    )?;
    let selected_version = command_stdout(
        &selected,
        &["-Vv"],
        "verifying the resolved Cargo executable",
    )?;
    if proxy_version != selected_version {
        return Err(RustCargoConfigurationError::Invalid(format!(
            "the +{selector} Cargo proxy and rustup's selected Cargo executable disagree"
        )));
    }
    Ok(selected.into_os_string())
}

fn model_target_config<'a>(
    model: &'a CargoConfigValue,
    target: &str,
) -> Option<&'a CargoConfigValue> {
    let targets = model.at(&["target"])?;
    if let Some(value) = targets.table()?.get(target) {
        return Some(value);
    }
    // `[target.thumbv8m.main-none-eabi]` is a table in a table to TOML.
    let mut value = targets;
    for component in target.split('.') {
        value = value.table()?.get(component)?;
    }
    Some(value)
}

fn model_exact_runner<'a>(
    model: &'a CargoConfigValue,
    target: &str,
) -> Option<&'a CargoConfigValue> {
    model_target_config(model, target)?.at(&["runner"])
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum ModelTargetKind {
    Tuple,
    Json,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct ModelTarget {
    kind: ModelTargetKind,
    name: String,
    cargo_argument: String,
}

fn model_targets(
    root: &Path,
    model: &CargoConfigValue,
    command_targets: Vec<String>,
    host: &str,
    inputs: &CargoModelInputs,
) -> Result<Vec<ModelTarget>, RustCargoConfigurationError> {
    let convert = |target: &str| {
        let target = target.trim();
        if target.is_empty() {
            return Err(RustCargoConfigurationError::Invalid(
                "Cargo target was empty".into(),
            ));
        }
        let target = if target == "host-tuple" { host } else { target };
        if target.ends_with(".json") {
            let requested = Path::new(target);
            let requested = if requested.is_absolute() {
                requested.to_owned()
            } else {
                root.join(requested)
            };
            let path = fs::canonicalize(&requested).map_err(|error| io_error(&requested, error))?;
            let name = path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .filter(|stem| !stem.is_empty())
                .ok_or_else(|| {
                    RustCargoConfigurationError::Invalid(format!(
                        "Cargo target specification has no UTF-8 file stem: {}",
                        path.display()
                    ))
                })?;
            let cargo_argument = path.to_str().ok_or_else(|| {
                RustCargoConfigurationError::Invalid(format!(
                    "Cargo target specification path is not UTF-8: {}",
                    path.display()
                ))
            })?;
            return Ok(ModelTarget {
                kind: ModelTargetKind::Json,
                name: name.to_owned(),
                cargo_argument: cargo_argument.to_owned(),
            });
        }
        Ok(ModelTarget {
            kind: ModelTargetKind::Tuple,
            name: target.to_owned(),
            cargo_argument: target.to_owned(),
        })
    };
    let targets = if !command_targets.is_empty() {
        command_targets
    } else if let Some(target) = inputs.environment("CARGO_BUILD_TARGET") {
        let target = target.clone().into_string().map_err(|_| {
            RustCargoConfigurationError::Invalid("CARGO_BUILD_TARGET is not UTF-8".into())
        })?;
        vec![target]
    } else if let Some(targets) = model.at(&["build", "target"]) {
        targets
            .string_list()
            .ok_or_else(|| {
                RustCargoConfigurationError::Invalid(
                    "build.target must be a string or string array".into(),
                )
            })?
            .into_iter()
            .map(str::to_owned)
            .collect()
    } else {
        vec![host.to_owned()]
    };
    let targets = targets
        .iter()
        .map(|target| convert(target))
        .collect::<Result<BTreeSet<_>, _>>()
        .map(BTreeSet::into_iter)
        .map(Iterator::collect::<Vec<_>>)?;
    let mut names = HashMap::new();
    targets.iter().try_for_each(|target| {
        match names.insert(&target.name, &target.cargo_argument) {
            Some(first) => Err(RustCargoConfigurationError::Invalid(format!(
                "Cargo targets {first} and {} have the same configuration identity {:?}",
                target.cargo_argument, target.name
            ))),
            None => Ok(()),
        }
    })?;
    Ok(targets)
}

fn environment_runner(
    target: &str,
    inputs: &CargoModelInputs,
) -> Result<Option<CargoConfigValue>, RustCargoConfigurationError> {
    let mut key = target.replace(['-', '.'], "_");
    key.make_ascii_uppercase();
    let key = format!("CARGO_TARGET_{key}_RUNNER");
    let Some(value) = inputs.environment(&key) else {
        return Ok(None);
    };
    let value = value
        .clone()
        .into_string()
        .map_err(|_| RustCargoConfigurationError::Invalid(format!("{key} is not UTF-8")))?;
    Ok(Some(CargoConfigValue {
        kind: CargoConfigKind::String(value),
        definition: CargoConfigDefinition::Environment(key),
    }))
}

/// `direct`: a variable Cargo reads itself, such as `RUSTC`, rather than a
/// configuration key such as `CARGO_BUILD_RUSTC`. Cargo takes a direct value
/// that is not UTF-8 as unset, and one holding either slash as a path on
/// every platform; a configuration key's value is a path only where it holds
/// a separator of the platform.
fn environment_tool(
    root: &Path,
    key: &str,
    inputs: &CargoModelInputs,
    empty_disables: bool,
    direct: bool,
) -> Result<Option<Option<RustCargoRunnerProgram>>, RustCargoConfigurationError> {
    let Some(value) = inputs.environment(key) else {
        return Ok(None);
    };
    let value = match value.clone().into_string() {
        Ok(value) => value,
        Err(_) if direct => return Ok(None),
        Err(_) => {
            return Err(RustCargoConfigurationError::Invalid(format!(
                "{key} is not UTF-8"
            )));
        }
    };
    if empty_disables && value.is_empty() {
        return Ok(Some(None));
    }
    let path =
        if direct && value.contains('\\') || crate::rust_cargo_config_model::names_a_path(&value) {
            root.join(value)
        } else {
            PathBuf::from(value)
        };
    Ok(Some(Some(runner_program(root, path)?)))
}

fn model_tool(
    root: &Path,
    value: &CargoConfigValue,
    empty_disables: bool,
) -> Result<Option<RustCargoRunnerProgram>, RustCargoConfigurationError> {
    let path = value
        .program_path(root)
        .map_err(|error| RustCargoConfigurationError::Invalid(error.to_string()))?;
    if empty_disables && path.as_os_str().is_empty() {
        return Ok(None);
    }
    runner_program(root, path).map(Some)
}

fn selected_compiler_tool(
    root: &Path,
    model: &CargoConfigValue,
    inputs: &CargoModelInputs,
    model_key: &str,
    direct_environment: &str,
    cargo_environment: &str,
    empty_disables: bool,
) -> Result<Option<RustCargoRunnerProgram>, RustCargoConfigurationError> {
    let configured = model.at(&["build", model_key]);
    if let Some(selected) =
        environment_tool(root, direct_environment, inputs, empty_disables, true)?
    {
        return Ok(selected);
    }
    if configured.is_some_and(|value| {
        matches!(
            value.definition,
            CargoConfigDefinition::CliFile(_) | CargoConfigDefinition::CliValue
        )
    }) {
        return model_tool(root, configured.expect("checked above"), empty_disables);
    }
    if let Some(selected) =
        environment_tool(root, cargo_environment, inputs, empty_disables, false)?
    {
        return Ok(selected);
    }
    configured
        .map(|value| model_tool(root, value, empty_disables))
        .transpose()
        .map(Option::flatten)
}

fn compiler_command_plan(
    root: &Path,
    model: &CargoConfigValue,
    default_rustc: RustCargoRunnerProgram,
    inputs: &CargoModelInputs,
) -> Result<RustCargoCompilerCommandPlan, RustCargoConfigurationError> {
    let rustc = selected_compiler_tool(
        root,
        model,
        inputs,
        "rustc",
        "RUSTC",
        "CARGO_BUILD_RUSTC",
        false,
    )?
    .unwrap_or(default_rustc);
    let rustc_wrapper = selected_compiler_tool(
        root,
        model,
        inputs,
        "rustc-wrapper",
        "RUSTC_WRAPPER",
        "CARGO_BUILD_RUSTC_WRAPPER",
        true,
    )?;
    let rustc_workspace_wrapper = selected_compiler_tool(
        root,
        model,
        inputs,
        "rustc-workspace-wrapper",
        "RUSTC_WORKSPACE_WRAPPER",
        "CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER",
        true,
    )?;
    Ok(RustCargoCompilerCommandPlan {
        rustc,
        rustc_wrapper,
        rustc_workspace_wrapper,
    })
}

fn compiler_host(
    root: &Path,
    compiler: &RustCargoCompilerCommandPlan,
) -> Result<String, RustCargoConfigurationError> {
    let output = compiler
        .command(root)
        .arg("-vV")
        .launch_output()
        .map_err(|error| io_error(&compiler.rustc.resolve(root), error))?;
    if !output.status.success() {
        return Err(RustCargoConfigurationError::Invalid(format!(
            "Cargo's configured compiler command failed while resolving its host with status {}: {}",
            output
                .status
                .code()
                .map_or_else(|| "signal".into(), |value| value.to_string()),
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    let stdout = String::from_utf8(output.stdout).map_err(|_| {
        RustCargoConfigurationError::Invalid(
            "Cargo's configured compiler command produced non-UTF-8 verbose version output".into(),
        )
    })?;
    stdout
        .lines()
        .find_map(|line| line.strip_prefix("host: "))
        .filter(|host| !host.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| {
            RustCargoConfigurationError::Invalid(
                "Cargo's configured compiler command did not report a host in rustc -vV output"
                    .into(),
            )
        })
}

fn parsed_model_runner(
    root: &Path,
    runner: &CargoConfigValue,
) -> Result<RustCargoUnderlyingRunner, RustCargoConfigurationError> {
    let (program, arguments) = runner
        .program_and_arguments(root)
        .map_err(|error| RustCargoConfigurationError::Invalid(error.to_string()))?;
    Ok(RustCargoUnderlyingRunner {
        program: runner_program(root, program)?,
        arguments,
    })
}

fn target_cfg(
    configuration_root: &Path,
    execution_root: &Path,
    compiler: &RustCargoCompilerCommandPlan,
    target: &ModelTarget,
) -> Result<Vec<Cfg>, RustCargoConfigurationError> {
    let target_argument = match target.kind {
        ModelTargetKind::Tuple => PathBuf::from(&target.cargo_argument),
        ModelTargetKind::Json => {
            let source = Path::new(&target.cargo_argument);
            source.strip_prefix(configuration_root).map_or_else(
                |_| source.to_owned(),
                |relative| execution_root.join(relative),
            )
        }
    };
    let output = compiler
        .command(execution_root)
        .args(["--print", "cfg", "--target"])
        .arg(&target_argument)
        .launch_output()
        .map_err(|error| io_error(&compiler.rustc.resolve(execution_root), error))?;
    if !output.status.success() {
        return Err(RustCargoConfigurationError::Invalid(format!(
            "Cargo's configured compiler command failed while resolving cfg values for target {} with status {}: {}",
            target.cargo_argument,
            output
                .status
                .code()
                .map_or_else(|| "signal".into(), |value| value.to_string()),
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    let stdout = String::from_utf8(output.stdout).map_err(|_| {
        RustCargoConfigurationError::Invalid(format!(
            "Cargo's configured compiler command produced non-UTF-8 cfg output for target {}",
            target.cargo_argument
        ))
    })?;
    stdout
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            Cfg::from_str(line.trim()).map_err(|error| {
                RustCargoConfigurationError::Invalid(format!(
                    "rustc emitted an invalid cfg value {line:?}: {error}"
                ))
            })
        })
        .collect()
}

fn cfg_runner<'a>(
    model: &'a CargoConfigValue,
    cfg: &[Cfg],
) -> Result<Option<&'a CargoConfigValue>, RustCargoConfigurationError> {
    let Some(targets) = model.at(&["target"]).and_then(CargoConfigValue::table) else {
        return Ok(None);
    };
    let mut matches = Vec::new();
    for (key, target) in targets {
        let Some(expression) = key
            .strip_prefix("cfg(")
            .and_then(|key| key.strip_suffix(')'))
        else {
            continue;
        };
        let expression = CfgExpr::from_str(expression).map_err(|error| {
            RustCargoConfigurationError::Invalid(format!(
                "invalid Cargo target cfg key {key:?}: {error}"
            ))
        })?;
        if expression.matches(cfg)
            && let Some(runner) = target.at(&["runner"])
        {
            matches.push((key, runner));
        }
    }
    match matches.as_slice() {
        [] => Ok(None),
        [(_, runner)] => Ok(Some(*runner)),
        [(first, _), (second, _), ..] => Err(RustCargoConfigurationError::Invalid(format!(
            "several matching instances of target.'cfg(..)'.runner: {first} and {second}"
        ))),
    }
}

fn model_runner(
    configuration_root: &Path,
    execution_root: &Path,
    model: &CargoConfigValue,
    compiler: &RustCargoCompilerCommandPlan,
    target: &ModelTarget,
    inputs: &CargoModelInputs,
) -> Result<Option<RustCargoUnderlyingRunner>, RustCargoConfigurationError> {
    let exact = model_exact_runner(model, &target.name);
    let environment = environment_runner(&target.name, inputs)?;
    let selected = match exact {
        Some(value)
            if matches!(
                value.definition,
                CargoConfigDefinition::CliFile(_) | CargoConfigDefinition::CliValue
            ) =>
        {
            Some(value)
        }
        _ => environment.as_ref().or(exact),
    };
    if let Some(runner) = selected {
        return parsed_model_runner(configuration_root, runner).map(Some);
    }
    let cfg = target_cfg(configuration_root, execution_root, compiler, target)?;
    cfg_runner(model, &cfg)?
        .map(|runner| parsed_model_runner(configuration_root, runner))
        .transpose()
}

fn runner_program(
    root: &Path,
    path: PathBuf,
) -> Result<RustCargoRunnerProgram, RustCargoConfigurationError> {
    if path.is_absolute() {
        if let Ok(relative) = path.strip_prefix(root) {
            if relative.as_os_str().is_empty()
                || relative
                    .components()
                    .any(|component| !matches!(component, Component::Normal(_)))
            {
                return Err(RustCargoConfigurationError::Invalid(format!(
                    "runner path is not a regular workspace-relative path: {}",
                    path.display()
                )));
            }
            return Ok(RustCargoRunnerProgram::WorkspaceRelative {
                value: relative.to_owned(),
            });
        }
        return Ok(RustCargoRunnerProgram::Absolute { value: path });
    }
    if path.components().count() == 1 {
        let value = path.into_os_string().into_string().map_err(|_| {
            RustCargoConfigurationError::Invalid("runner search path is not UTF-8".into())
        })?;
        return Ok(RustCargoRunnerProgram::SearchPath { value });
    }
    Err(RustCargoConfigurationError::Invalid(format!(
        "Cargo returned an unresolved relative runner path: {}",
        path.display()
    )))
}

/// What a test command builds with: its resolved configuration, compiler
/// command and targets.
struct ResolvedTargets {
    root: PathBuf,
    execution_root: PathBuf,
    model: CargoConfigValue,
    compiler: RustCargoCompilerCommandPlan,
    targets: Vec<ModelTarget>,
}

fn resolve_with_inputs(
    root: &Path,
    execution_root: &Path,
    invocation: &CargoTestInvocation,
    model_inputs: CargoModelInputs,
) -> Result<RustCargoRunnerPlan, RustCargoConfigurationError> {
    let ResolvedTargets {
        root,
        execution_root,
        model,
        compiler,
        targets,
    } = resolve_targets(root, execution_root, invocation, &model_inputs)?;
    let mut rustflags = targets
        .iter()
        .map(|target| {
            model_rustflags(
                &root,
                &execution_root,
                &model,
                &compiler,
                target,
                &model_inputs,
            )
        })
        .collect::<Result<Vec<_>, RustCargoConfigurationError>>()?;
    rustflags.dedup();
    let targets = targets
        .iter()
        .map(|target| {
            Ok(RustCargoTargetRunnerPlan {
                target: target.name.clone(),
                underlying_runner: model_runner(
                    &root,
                    &execution_root,
                    &model,
                    &compiler,
                    target,
                    &model_inputs,
                )?,
            })
        })
        .collect::<Result<Vec<_>, RustCargoConfigurationError>>()?;
    Ok(RustCargoRunnerPlan {
        compiler,
        targets,
        // One `RUSTFLAGS` cannot carry two targets' different flags.
        rustflags: match rustflags.as_slice() {
            [flags] => flags.clone(),
            _ => None,
        },
    })
}

/// A configured list of flags (`name`) as Cargo reads one: a string is split
/// on whitespace, and the words of its variable `environment_key` follow.
fn flag_list(
    value: Option<&CargoConfigValue>,
    name: &str,
    environment_key: Option<&str>,
    inputs: &CargoModelInputs,
) -> Result<Option<Vec<String>>, RustCargoConfigurationError> {
    let mut flags = match value {
        None => None,
        Some(CargoConfigValue {
            kind: CargoConfigKind::String(text),
            ..
        }) => Some(text.split_whitespace().map(str::to_owned).collect()),
        Some(value) => Some(
            value
                .string_list()
                .ok_or_else(|| {
                    RustCargoConfigurationError::Invalid(format!(
                        "{name} must be a string or string array"
                    ))
                })?
                .into_iter()
                .map(str::to_owned)
                .collect::<Vec<_>>(),
        ),
    };
    if let Some((environment_key, words)) =
        environment_key.and_then(|key| inputs.environment(key).map(|words| (key, words)))
    {
        let words = words.clone().into_string().map_err(|_| {
            RustCargoConfigurationError::Invalid(format!("{environment_key} is not UTF-8"))
        })?;
        flags
            .get_or_insert_with(Vec::new)
            .extend(words.split_whitespace().map(str::to_owned));
    }
    Ok(flags)
}

/// The flags Cargo's configuration gives rustc for one target when the
/// environment sets no `RUSTFLAGS`: its `target.<triple>.rustflags` and
/// every matching `target.'cfg(..)'.rustflags` together, else
/// `build.rustflags`. Setting `RUSTFLAGS` replaces them, so whatever sets it
/// starts from these.
fn model_rustflags(
    configuration_root: &Path,
    execution_root: &Path,
    model: &CargoConfigValue,
    compiler: &RustCargoCompilerCommandPlan,
    target: &ModelTarget,
    inputs: &CargoModelInputs,
) -> Result<Option<Vec<String>>, RustCargoConfigurationError> {
    let mut key = target.name.replace(['-', '.'], "_");
    key.make_ascii_uppercase();
    let mut flags = flag_list(
        model_target_config(model, &target.name).and_then(|config| config.at(&["rustflags"])),
        &format!("target.{}.rustflags", target.name),
        Some(&format!("CARGO_TARGET_{key}_RUSTFLAGS")),
        inputs,
    )?
    .unwrap_or_default();
    let cfg_flags = model
        .at(&["target"])
        .and_then(CargoConfigValue::table)
        .into_iter()
        .flatten()
        .filter_map(|(key, value)| {
            let expression = key.strip_prefix("cfg(")?.strip_suffix(')')?;
            Some((key, expression, value.at(&["rustflags"])?))
        })
        .collect::<Vec<_>>();
    if !cfg_flags.is_empty() {
        let cfg = target_cfg(configuration_root, execution_root, compiler, target)?;
        cfg_flags
            .into_iter()
            .try_for_each(|(key, expression, value)| {
                let expression = CfgExpr::from_str(expression).map_err(|error| {
                    RustCargoConfigurationError::Invalid(format!(
                        "invalid Cargo target cfg key {key:?}: {error}"
                    ))
                })?;
                if expression.matches(&cfg) {
                    flags.extend(
                        flag_list(
                            Some(value),
                            &format!("target.{key}.rustflags"),
                            None,
                            inputs,
                        )?
                        .unwrap_or_default(),
                    );
                }
                Ok::<(), RustCargoConfigurationError>(())
            })?;
    }
    if !flags.is_empty() {
        return Ok(Some(flags));
    }
    flag_list(
        model.at(&["build", "rustflags"]),
        "build.rustflags",
        Some("CARGO_BUILD_RUSTFLAGS"),
        inputs,
    )
}

/// The cfg values of the one target a test command builds for: what decides
/// which `#[cfg(..)]` items the compiler leaves out. None when the command
/// builds for several targets or they cannot be read, so nothing is assumed.
pub(crate) fn active_target_cfg(root: &Path, invocation: &CargoTestInvocation) -> Option<Vec<Cfg>> {
    let inputs = CargoModelInputs::ambient(root);
    let resolved = resolve_targets(root, root, invocation, &inputs).ok()?;
    let [target] = resolved.targets.as_slice() else {
        return None;
    };
    target_cfg(
        &resolved.root,
        &resolved.execution_root,
        &resolved.compiler,
        target,
    )
    .ok()
}

fn resolve_targets(
    root: &Path,
    execution_root: &Path,
    invocation: &CargoTestInvocation,
    model_inputs: &CargoModelInputs,
) -> Result<ResolvedTargets, RustCargoConfigurationError> {
    let root = fs::canonicalize(root).map_err(|error| io_error(root, error))?;
    let execution_root =
        fs::canonicalize(execution_root).map_err(|error| io_error(execution_root, error))?;
    let CommandLine {
        targets: command_targets,
        config: command_config,
        toolchain: explicit_toolchain,
    } = command_line(invocation)?;
    let selected_cargo = selected_cargo_program(invocation)?;
    let model = load_cargo_configuration(&root, model_inputs.cargo_home.clone(), &command_config)
        .map_err(|error| RustCargoConfigurationError::Invalid(error.to_string()))?;
    let selected_cargo_path = PathBuf::from(&selected_cargo);
    let default_rustc = if explicit_toolchain {
        selected_cargo_path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .map(|parent| parent.join(format!("rustc{}", std::env::consts::EXE_SUFFIX)))
            .filter(|candidate| candidate.is_file())
            .map(|candidate| runner_program(&root, candidate))
            .transpose()?
            .unwrap_or_else(|| RustCargoRunnerProgram::SearchPath {
                value: format!("rustc{}", std::env::consts::EXE_SUFFIX),
            })
    } else {
        RustCargoRunnerProgram::SearchPath {
            value: format!("rustc{}", std::env::consts::EXE_SUFFIX),
        }
    };
    let compiler = compiler_command_plan(&root, &model, default_rustc, model_inputs)?;
    let host = model_inputs
        .host_override
        .clone()
        .map(Ok)
        .unwrap_or_else(|| compiler_host(&execution_root, &compiler))?;
    let targets = model_targets(&root, &model, command_targets, &host, model_inputs)?;
    Ok(ResolvedTargets {
        root,
        execution_root,
        model,
        compiler,
        targets,
    })
}

pub(crate) fn resolve_cargo_runner_plan(
    root: &Path,
    execution_root: &Path,
    invocation: &CargoTestInvocation,
) -> Result<RustCargoRunnerPlan, RustCargoConfigurationError> {
    resolve_with_inputs(
        root,
        execution_root,
        invocation,
        CargoModelInputs::ambient(root),
    )
}

#[cfg(test)]
mod tests {
    use std::{
        sync::atomic::{AtomicU64, Ordering},
        time::{SystemTime, UNIX_EPOCH},
    };

    use super::*;

    static FIXTURE_ID: AtomicU64 = AtomicU64::new(0);

    fn fixture() -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "supercov-cargo-configuration-{}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            FIXTURE_ID.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(root.join(".cargo/bin with spaces")).unwrap();
        root
    }

    /// A runner path is relative to the workspace when inside it, absolute
    /// when outside, a name to search for when bare, and refused when it is
    /// the workspace itself, climbs out of it, or is left relative.
    #[test]
    fn a_runner_path_is_read_against_the_workspace() {
        use crate::test_directory::native;
        let workspace = native("/w");
        let root = Path::new(&workspace);
        let read = |path: &str| runner_program(root, PathBuf::from(path));
        assert_eq!(
            read(&native("/w/bin/run")).unwrap(),
            RustCargoRunnerProgram::WorkspaceRelative {
                value: "bin/run".into()
            }
        );
        assert_eq!(
            read(&native("/usr/bin/run")).unwrap(),
            RustCargoRunnerProgram::Absolute {
                value: native("/usr/bin/run").into()
            }
        );
        assert_eq!(
            read("wasm-runner").unwrap(),
            RustCargoRunnerProgram::SearchPath {
                value: "wasm-runner".into()
            }
        );
        for refused in [native("/w"), native("/w/../x"), "tools/run".to_owned()] {
            assert!(read(&refused).is_err(), "{refused}");
        }
    }

    /// `--config` takes its value from the next word or after `=`, and one
    /// with no value is refused; a toolchain selector must come first, alone,
    /// and name something; and `-Z` before the subcommand is not resolved.
    #[test]
    fn command_line_configuration_and_toolchains_are_read_exactly() {
        assert_eq!(
            command_line(&invocation(&[
                "+nightly",
                "--config",
                "a=1",
                "test",
                "--config=b=2"
            ]))
            .unwrap(),
            CommandLine {
                targets: Vec::new(),
                config: vec!["a=1".into(), "b=2".into()],
                toolchain: true,
            }
        );
        assert!(command_line(&invocation(&["test", "--config"])).is_err());
        assert!(command_line(&invocation(&["test", "--config="])).is_err());
        let selector = |arguments: &[&str]| {
            let invocation = invocation(arguments);
            let position = invocation.command_position();
            toolchain_selector(&invocation, position).map(|value| value.map(str::to_owned))
        };
        assert_eq!(
            selector(&["+nightly", "test"]).unwrap().as_deref(),
            Some("nightly")
        );
        assert_eq!(selector(&["test"]).unwrap(), None);
        assert!(selector(&["+", "test"]).is_err());
        assert!(selector(&["-v", "+nightly", "test"]).is_err());
        assert!(command_line(&invocation(&["-Zunstable-options", "test"])).is_err());
        assert!(command_line(&invocation(&["-Z", "x", "test"])).is_err());
    }

    fn invocation(arguments: &[&str]) -> CargoTestInvocation {
        CargoTestInvocation {
            program: "cargo".into(),
            kind: crate::rust_test_runner::RustCargoCommandKind::CargoTest,
            arguments: arguments.iter().map(|value| (*value).into()).collect(),
            runner_arguments: Vec::new(),
        }
    }

    fn model_inputs() -> CargoModelInputs {
        model_inputs_with([])
    }

    fn model_inputs_with<const N: usize>(
        additional: [(OsString, OsString); N],
    ) -> CargoModelInputs {
        CargoModelInputs {
            cargo_home: None,
            environment: additional
                .into_iter()
                .filter_map(|(key, value)| key.into_string().ok().map(|key| (key, value)))
                .collect(),
            host_override: Some("aarch64-apple-darwin".into()),
        }
    }

    #[test]
    fn compiler_tools_follow_cargo_cli_and_environment_precedence() {
        let root = fixture();
        fs::write(
            root.join(".cargo/config.toml"),
            concat!(
                "[build]\n",
                "rustc=\"./file-rustc\"\n",
                "rustc-wrapper=\"./file-wrapper\"\n",
                "rustc-workspace-wrapper=\"./file-workspace-wrapper\"\n",
            ),
        )
        .unwrap();
        let cli = [
            "build.rustc=\"./cli-rustc\"".into(),
            "build.rustc-wrapper=\"./cli-wrapper\"".into(),
            "build.rustc-workspace-wrapper=\"./cli-workspace-wrapper\"".into(),
        ];
        let model = load_cargo_configuration(&root, None, &cli).unwrap();
        let plan = compiler_command_plan(
            &root,
            &model,
            RustCargoRunnerProgram::SearchPath {
                value: "rustc".into(),
            },
            &model_inputs_with([
                (OsString::from("RUSTC"), OsString::from("./direct-rustc")),
                (
                    OsString::from("CARGO_BUILD_RUSTC_WRAPPER"),
                    OsString::from("./cargo-wrapper"),
                ),
                (OsString::from("RUSTC_WORKSPACE_WRAPPER"), OsString::new()),
            ]),
        )
        .unwrap();
        assert_eq!(
            plan.rustc,
            RustCargoRunnerProgram::WorkspaceRelative {
                value: "direct-rustc".into()
            }
        );
        assert_eq!(
            plan.rustc_wrapper,
            Some(RustCargoRunnerProgram::WorkspaceRelative {
                value: "cli-wrapper".into()
            })
        );
        assert_eq!(plan.rustc_workspace_wrapper, None);
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn direct_non_utf8_tool_environment_falls_back_but_config_environment_is_invalid() {
        use std::os::unix::ffi::OsStringExt;

        let root = fixture();
        fs::write(root.join(".cargo/config.toml"), "").unwrap();
        let model =
            load_cargo_configuration(&root, None, &["build.rustc=\"./cli-rustc\"".into()]).unwrap();
        let plan = compiler_command_plan(
            &root,
            &model,
            RustCargoRunnerProgram::SearchPath {
                value: "rustc".into(),
            },
            &model_inputs_with([(OsString::from("RUSTC"), OsString::from_vec(vec![0xff]))]),
        )
        .unwrap();
        assert_eq!(
            plan.rustc,
            RustCargoRunnerProgram::WorkspaceRelative {
                value: "cli-rustc".into()
            }
        );

        let empty_model = load_cargo_configuration(&root, None, &[]).unwrap();
        let error = compiler_command_plan(
            &root,
            &empty_model,
            RustCargoRunnerProgram::SearchPath {
                value: "rustc".into(),
            },
            &model_inputs_with([(
                OsString::from("CARGO_BUILD_RUSTC"),
                OsString::from_vec(vec![0xff]),
            )]),
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("CARGO_BUILD_RUSTC is not UTF-8"), "{error}");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn every_compiler_wrapper_layer_is_preserved_in_the_compiler_plan() {
        let root = fixture();
        fs::write(
            root.join(".cargo/config.toml"),
            "[build]\nrustc-wrapper=\"wrapper\"\n",
        )
        .unwrap();
        let model = load_cargo_configuration(&root, None, &[]).unwrap();
        let plan = compiler_command_plan(
            &root,
            &model,
            RustCargoRunnerProgram::SearchPath {
                value: "rustc".into(),
            },
            &model_inputs(),
        )
        .unwrap();
        assert_eq!(
            plan.rustc_wrapper,
            Some(RustCargoRunnerProgram::SearchPath {
                value: "wrapper".into()
            })
        );

        fs::write(root.join(".cargo/config.toml"), "").unwrap();
        let model = load_cargo_configuration(
            &root,
            None,
            &["build.rustc-workspace-wrapper=\"workspace-wrapper\"".into()],
        )
        .unwrap();
        let plan = compiler_command_plan(
            &root,
            &model,
            RustCargoRunnerProgram::SearchPath {
                value: "rustc".into(),
            },
            &model_inputs(),
        )
        .unwrap();
        assert_eq!(
            plan.rustc_workspace_wrapper,
            Some(RustCargoRunnerProgram::SearchPath {
                value: "workspace-wrapper".into()
            })
        );

        let model = load_cargo_configuration(&root, None, &[]).unwrap();
        let plan = compiler_command_plan(
            &root,
            &model,
            RustCargoRunnerProgram::SearchPath {
                value: "rustc".into(),
            },
            &model_inputs_with([(
                OsString::from("RUSTC_WRAPPER"),
                OsString::from("environment-wrapper"),
            )]),
        )
        .unwrap();
        assert_eq!(
            plan.rustc_wrapper,
            Some(RustCargoRunnerProgram::SearchPath {
                value: "environment-wrapper".into()
            })
        );
        fs::remove_dir_all(root).unwrap();
    }

    /// Write an executable script by way of another process. A file this
    /// process has open for writing is inherited by a child another test
    /// thread forks, and held until that child execs; running the script
    /// meanwhile fails with "Text file busy". Written by `cp`, the script
    /// never has a descriptor here.
    #[cfg(unix)]
    fn write_script(path: &Path, text: &str) {
        use std::os::unix::fs::PermissionsExt;

        let draft = path.with_extension("draft");
        fs::write(&draft, text).unwrap();
        let copied = std::process::Command::new("cp")
            .arg(&draft)
            .arg(path)
            .status()
            .unwrap();
        assert!(copied.success(), "cp {} failed", draft.display());
        fs::remove_file(&draft).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn included_build_rustc_drives_host_and_cfg_runner_selection() {
        let root = fixture();
        let rustc = which::which("rustc").unwrap();
        let proxy = root.join("compiler-proxy");
        let log = root.join("compiler.log");
        write_script(
            &proxy,
            &format!(
                "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"{}\"\nexec \"{}\" \"$@\"\n",
                log.display(),
                rustc.display()
            ),
        );
        fs::write(
            root.join(".cargo/compiler.toml"),
            concat!(
                "[build]\nrustc=\"./compiler-proxy\"\n",
                "[target.'cfg(unix)']\nrunner=[\"cfg-runner\",\"--cfg\"]\n",
            ),
        )
        .unwrap();
        fs::write(
            root.join(".cargo/config.toml"),
            "include=[\"compiler.toml\"]\n",
        )
        .unwrap();
        let mut inputs = model_inputs();
        inputs.host_override = None;
        let plan = resolve_with_inputs(&root, &root, &invocation(&["test"]), inputs).unwrap();
        assert_eq!(
            plan.compiler.rustc,
            RustCargoRunnerProgram::WorkspaceRelative {
                value: "compiler-proxy".into()
            }
        );
        assert_eq!(
            plan.targets[0].underlying_runner,
            Some(RustCargoUnderlyingRunner {
                program: RustCargoRunnerProgram::SearchPath {
                    value: "cfg-runner".into()
                },
                arguments: vec!["--cfg".into()]
            })
        );
        let invocations = fs::read_to_string(&log).unwrap();
        assert!(invocations.lines().any(|line| line == "-vV"));
        assert!(
            invocations
                .lines()
                .any(|line| line.contains("--print cfg --target"))
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn exact_target_beats_cfg_and_environment_beats_both() {
        let root = fixture();
        fs::write(
            root.join(".cargo/config.toml"),
            concat!(
                "[target.'cfg(target_vendor = \"apple\")']\n",
                "runner=[\"cfg-runner\",\"--cfg\"]\n",
                "[target.aarch64-apple-darwin]\n",
                "runner=\"exact-runner --exact\"\n",
            ),
        )
        .unwrap();
        let plan =
            resolve_with_inputs(&root, &root, &invocation(&["test"]), model_inputs()).unwrap();
        assert_eq!(
            plan.targets[0].underlying_runner,
            Some(RustCargoUnderlyingRunner {
                program: RustCargoRunnerProgram::SearchPath {
                    value: "exact-runner".into(),
                },
                arguments: vec!["--exact".into()],
            })
        );
        let plan = resolve_with_inputs(
            &root,
            &root,
            &invocation(&["test"]),
            model_inputs_with([(
                OsString::from("CARGO_TARGET_AARCH64_APPLE_DARWIN_RUNNER"),
                OsString::from("environment-runner --environment"),
            )]),
        )
        .unwrap();
        assert_eq!(
            plan.targets[0].underlying_runner,
            Some(RustCargoUnderlyingRunner {
                program: RustCargoRunnerProgram::SearchPath {
                    value: "environment-runner".into(),
                },
                arguments: vec!["--environment".into()],
            })
        );
        fs::remove_dir_all(root).unwrap();
    }

    /// A target specification named in UTF-8 that leads to a file or a
    /// directory whose name is not is refused by what it is. (macOS names
    /// none.)
    #[cfg(target_os = "linux")]
    #[test]
    fn a_target_specification_that_leads_to_no_utf8_name_is_refused() {
        use std::os::unix::ffi::OsStrExt;
        let root = fixture();
        let odd = std::ffi::OsStr::from_bytes(b"\xff");
        fs::write(root.join(odd).with_extension("json"), "{}").unwrap();
        std::os::unix::fs::symlink(
            root.join(odd).with_extension("json"),
            root.join("stem.json"),
        )
        .unwrap();
        fs::create_dir(root.join(odd)).unwrap();
        fs::write(root.join(odd).join("spec.json"), "{}").unwrap();
        std::os::unix::fs::symlink(root.join(odd).join("spec.json"), root.join("path.json"))
            .unwrap();
        for (target, said) in [
            ("stem.json", "no UTF-8 file stem"),
            ("path.json", "path is not UTF-8"),
        ] {
            let error = resolve_with_inputs(
                &root,
                &root,
                &invocation(&["test", "--target", target]),
                model_inputs(),
            )
            .unwrap_err()
            .to_string();
            assert!(error.contains(said), "{target}: {error}");
        }
        fs::remove_dir_all(root).unwrap();
    }

    /// A target table that is not one, and a key that only begins like a
    /// cfg, set no runner.
    #[test]
    fn target_tables_of_odd_shapes_set_no_runner() {
        let root = fixture();
        for config in [
            "target = 1\n",
            "[target.'cfg(unix']\nrustflags = [\"-x\"]\n",
        ] {
            fs::write(root.join(".cargo/config.toml"), config).unwrap();
            let plan =
                resolve_with_inputs(&root, &root, &invocation(&["test"]), model_inputs()).unwrap();
            assert_eq!(plan.targets[0].underlying_runner, None, "{config}");
        }
        // A target whose name has a dot is read through the tables TOML
        // makes of it; one of them missing, or no table, sets no runner.
        let dotted = invocation(&["test", "--target", "thumbv8m.main-none-eabi"]);
        for (config, runner) in [
            (
                "[target.thumbv8m.main-none-eabi]\nrunner = \"probe\"\n",
                true,
            ),
            ("[target.thumbv8m.other]\nrunner = \"probe\"\n", false),
            ("[target]\nthumbv8m = 1\n", false),
        ] {
            fs::write(root.join(".cargo/config.toml"), config).unwrap();
            let plan = resolve_with_inputs(&root, &root, &dotted, model_inputs()).unwrap();
            assert_eq!(plan.targets[0].target, "thumbv8m.main-none-eabi");
            assert_eq!(
                plan.targets[0].underlying_runner.is_some(),
                runner,
                "{config}"
            );
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn resolves_exact_array_runner_and_relocates_workspace_program() {
        let root = fixture();
        fs::write(
            root.join(".cargo/config.toml"),
            "[target.aarch64-apple-darwin]\nrunner=[\"bin with spaces/runner\",\"--fixed\"]\n",
        )
        .unwrap();
        let plan =
            resolve_with_inputs(&root, &root, &invocation(&["test"]), model_inputs()).unwrap();
        assert_eq!(plan.targets[0].target, "aarch64-apple-darwin");
        assert_eq!(
            plan.targets[0].underlying_runner,
            Some(RustCargoUnderlyingRunner {
                program: RustCargoRunnerProgram::WorkspaceRelative {
                    value: PathBuf::from("bin with spaces/runner")
                },
                arguments: vec!["--fixed".into()],
            })
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn ordinary_files_use_cargo_195_runner_parsing_and_duplicate_cfg_rules() {
        let root = fixture();
        fs::write(
            root.join(".cargo/config.toml"),
            "[target.aarch64-apple-darwin]\nrunner=\"runner\\t--one\\n--two\"\n",
        )
        .unwrap();
        let plan =
            resolve_with_inputs(&root, &root, &invocation(&["test"]), model_inputs()).unwrap();
        assert_eq!(
            plan.targets[0].underlying_runner,
            Some(RustCargoUnderlyingRunner {
                program: RustCargoRunnerProgram::SearchPath {
                    value: "runner".into()
                },
                arguments: vec!["--one".into(), "--two".into()]
            })
        );
        fs::write(
            root.join(".cargo/config.toml"),
            concat!(
                "[target.'cfg(target_vendor = \"apple\")']\nrunner=\"vendor-cfg\"\n",
                "[target.'cfg(target_os = \"macos\")']\nrunner=\"os-cfg\"\n",
            ),
        )
        .unwrap();
        let error = resolve_with_inputs(&root, &root, &invocation(&["test"]), model_inputs())
            .unwrap_err()
            .to_string();
        assert!(error.contains("several matching instances"), "{error}");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn cargo_duplicate_and_host_tuple_targets_collapse_before_runner_selection() {
        let root = fixture();
        fs::write(
            root.join(".cargo/config.toml"),
            "[target.aarch64-apple-darwin]\nrunner=\"runner\"\n",
        )
        .unwrap();
        let plan = resolve_with_inputs(
            &root,
            &root,
            &invocation(&[
                "test",
                "--target=host-tuple",
                "--target=aarch64-apple-darwin",
                "--target=aarch64-apple-darwin",
            ]),
            model_inputs(),
        )
        .unwrap();
        assert_eq!(plan.targets[0].target, "aarch64-apple-darwin");
        assert_eq!(
            plan.targets[0].underlying_runner,
            Some(RustCargoUnderlyingRunner {
                program: RustCargoRunnerProgram::SearchPath {
                    value: "runner".into()
                },
                arguments: Vec::new()
            })
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn custom_target_paths_are_project_relative_and_name_collisions_fail_closed() {
        let root = fixture();
        fs::create_dir_all(root.join("targets/first")).unwrap();
        fs::create_dir_all(root.join("targets/second")).unwrap();
        fs::write(root.join("targets/first/custom.json"), "{}").unwrap();
        fs::write(root.join("targets/second/custom.json"), "{}").unwrap();
        let model = CargoConfigValue {
            kind: CargoConfigKind::Table(std::collections::BTreeMap::new()),
            definition: CargoConfigDefinition::BuiltIn,
        };
        let inputs = model_inputs();
        let target = model_targets(
            &root,
            &model,
            vec!["targets/first/custom.json".into()],
            "aarch64-apple-darwin",
            &inputs,
        )
        .unwrap();
        assert_eq!(target[0].name, "custom");
        assert_eq!(
            Path::new(&target[0].cargo_argument),
            fs::canonicalize(root.join("targets/first/custom.json")).unwrap()
        );
        let error = model_targets(
            &root,
            &model,
            vec![
                "targets/first/custom.json".into(),
                "targets/second/custom.json".into(),
            ],
            "aarch64-apple-darwin",
            &inputs,
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("same configuration identity"), "{error}");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn direct_cargo_path_keeps_cargos_default_rustc_search_semantics() {
        let root = fixture();
        fs::write(root.join(".cargo/config.toml"), "").unwrap();
        let cargo = which::which("cargo").unwrap();
        let plan = resolve_with_inputs(
            &root,
            &root,
            &CargoTestInvocation {
                program: cargo.to_string_lossy().into_owned(),
                kind: crate::rust_test_runner::RustCargoCommandKind::CargoTest,
                arguments: vec!["test".into()],
                runner_arguments: Vec::new(),
            },
            model_inputs(),
        )
        .unwrap();
        assert_eq!(
            plan.compiler.rustc,
            RustCargoRunnerProgram::SearchPath {
                value: format!("rustc{}", std::env::consts::EXE_SUFFIX)
            }
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn resolves_an_explicit_installed_rustup_toolchain_before_target_configuration() {
        let root = fixture();
        // The toolchain has to be really installed for `+1.95.0` to resolve to
        // a real rustc, which is what this test is about.
        let rustc = Command::new("rustc")
            .args(["+1.95.0", "-vV"])
            .output()
            .unwrap();
        assert!(
            rustc.status.success(),
            "{}",
            String::from_utf8_lossy(&rustc.stderr)
        );
        // The target comes from `model_inputs`, which pins the host the way
        // every other test in this file does. Reading it from the machine
        // instead made the test pass only on an Apple silicon Mac: everywhere
        // else the plan honoured the pinned host and the assertion compared it
        // against the real one.
        let host = "aarch64-apple-darwin";
        fs::write(
            root.join(".cargo/config.toml"),
            format!("[target.{host}]\nrunner=[\"selected-runner\",\"--selected\"]\n"),
        )
        .unwrap();
        let plan = resolve_with_inputs(
            &root,
            &root,
            &invocation(&["+1.95.0", "test"]),
            model_inputs(),
        )
        .unwrap();
        let rustc_executable = format!("rustc{}", std::env::consts::EXE_SUFFIX);
        assert!(matches!(
            &plan.compiler.rustc,
            RustCargoRunnerProgram::Absolute { value }
                if value.file_name().and_then(|name| name.to_str())
                    == Some(rustc_executable.as_str())
        ));
        assert_eq!(plan.targets[0].target, host);
        assert_eq!(
            plan.targets[0].underlying_runner,
            Some(RustCargoUnderlyingRunner {
                program: RustCargoRunnerProgram::SearchPath {
                    value: "selected-runner".into(),
                },
                arguments: vec!["--selected".into()],
            })
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn resolves_include_cli_cfg_and_multiple_target_runners_but_rejects_open_surfaces() {
        let root = fixture();
        fs::write(
            root.join(".cargo/extra.toml"),
            "[target.aarch64-apple-darwin]\nrunner=[\"included\",\"--included\"]\n",
        )
        .unwrap();
        fs::write(
            root.join(".cargo/config.toml"),
            "include=[\"extra.toml\"]\n",
        )
        .unwrap();
        assert_eq!(
            resolve_with_inputs(&root, &root, &invocation(&["test"]), model_inputs(),)
                .unwrap()
                .targets[0]
                .underlying_runner,
            Some(RustCargoUnderlyingRunner {
                program: RustCargoRunnerProgram::SearchPath {
                    value: "included".into()
                },
                arguments: vec!["--included".into()]
            })
        );
        assert_eq!(
            resolve_with_inputs(
                &root,
                &root,
                &invocation(&[
                    "test",
                    "--config",
                    "target.aarch64-apple-darwin.runner=[\"cli\",\"--cli\"]",
                ]),
                model_inputs_with([(
                    OsString::from("CARGO_TARGET_AARCH64_APPLE_DARWIN_RUNNER"),
                    OsString::from("environment"),
                )]),
            )
            .unwrap()
            .targets[0]
                .underlying_runner,
            Some(RustCargoUnderlyingRunner {
                program: RustCargoRunnerProgram::SearchPath {
                    value: "cli".into()
                },
                arguments: vec!["--cli".into()]
            })
        );
        let multi_target_environment = [
            (
                OsString::from("CARGO_TARGET_A_RUNNER"),
                OsString::from("runner-a"),
            ),
            (
                OsString::from("CARGO_TARGET_B_RUNNER"),
                OsString::from("runner-b"),
            ),
        ];
        let plan = resolve_with_inputs(
            &root,
            &root,
            &invocation(&["test", "--target=a", "--target=b"]),
            model_inputs_with(multi_target_environment),
        );
        let plan = plan.unwrap();
        assert_eq!(
            plan.targets
                .iter()
                .map(|target| target.target.as_str())
                .collect::<Vec<_>>(),
            ["a", "b"]
        );
        assert_eq!(
            plan.targets
                .iter()
                .map(|target| {
                    target
                        .underlying_runner
                        .as_ref()
                        .map(|runner| &runner.program)
                })
                .collect::<Vec<_>>(),
            [
                Some(&RustCargoRunnerProgram::SearchPath {
                    value: "runner-a".into()
                }),
                Some(&RustCargoRunnerProgram::SearchPath {
                    value: "runner-b".into()
                })
            ]
        );
        fs::write(
            root.join(".cargo/extra.toml"),
            "[target.'cfg(target_vendor = \"apple\")']\nrunner=\"included-cfg\"\n",
        )
        .unwrap();
        assert_eq!(
            resolve_with_inputs(&root, &root, &invocation(&["test"]), model_inputs(),)
                .unwrap()
                .targets[0]
                .underlying_runner,
            Some(RustCargoUnderlyingRunner {
                program: RustCargoRunnerProgram::SearchPath {
                    value: "included-cfg".into()
                },
                arguments: Vec::new()
            })
        );
        fs::write(
            root.join(".cargo/extra.toml"),
            concat!(
                "[target.'cfg(target_vendor = \"apple\")']\nrunner=\"vendor-cfg\"\n",
                "[target.'cfg(target_os = \"macos\")']\nrunner=\"os-cfg\"\n",
            ),
        )
        .unwrap();
        let error = resolve_with_inputs(&root, &root, &invocation(&["test"]), model_inputs())
            .unwrap_err()
            .to_string();
        assert!(error.contains("several matching instances"), "{error}");
        assert!(
            command_line(&invocation(&["--quiet", "+nightly", "test"]))
                .unwrap_err()
                .to_string()
                .contains("must be the first")
        );
        assert!(
            resolve_with_inputs(
                &root,
                &root,
                &invocation(&["-Ztarget-applies-to-host", "test"]),
                model_inputs(),
            )
            .unwrap_err()
            .to_string()
            .contains("-Z configuration semantics")
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn a_backslash_makes_a_compiler_tool_a_path_only_in_a_direct_variable() {
        let root = fixture();
        fs::write(
            root.join(".cargo/config.toml"),
            "[build]\nrustc-workspace-wrapper='tools\\workspace-wrapper'\n",
        )
        .unwrap();
        let model = load_cargo_configuration(&root, None, &[]).unwrap();
        let plan = compiler_command_plan(
            &root,
            &model,
            RustCargoRunnerProgram::SearchPath {
                value: "rustc".into(),
            },
            &model_inputs_with([
                (OsString::from("RUSTC"), OsString::from("tools\\rustc")),
                (
                    OsString::from("CARGO_BUILD_RUSTC_WRAPPER"),
                    OsString::from("tools\\wrapper"),
                ),
            ]),
        )
        .unwrap();
        // Cargo joins `RUSTC` to the directory it runs in whichever slash it
        // holds, but on Linux and macOS looks a configured `tools\wrapper` up
        // on `PATH`, as it does a runner.
        assert_eq!(
            plan.rustc,
            RustCargoRunnerProgram::WorkspaceRelative {
                value: "tools\\rustc".into()
            }
        );
        assert_eq!(
            plan.rustc_wrapper,
            Some(RustCargoRunnerProgram::SearchPath {
                value: "tools\\wrapper".into()
            })
        );
        assert_eq!(
            plan.rustc_workspace_wrapper,
            Some(RustCargoRunnerProgram::SearchPath {
                value: "tools\\workspace-wrapper".into()
            })
        );
        fs::write(
            root.join(".cargo/config.toml"),
            "[target.aarch64-apple-darwin]\nrunner='tools\\runner'\n",
        )
        .unwrap();
        assert_eq!(
            resolve_with_inputs(&root, &root, &invocation(&["test"]), model_inputs())
                .unwrap()
                .targets[0]
                .underlying_runner,
            Some(RustCargoUnderlyingRunner {
                program: RustCargoRunnerProgram::SearchPath {
                    value: "tools\\runner".into()
                },
                arguments: Vec::new()
            })
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn an_empty_configured_wrapper_is_no_wrapper() {
        let root = fixture();
        fs::write(
            root.join(".cargo/config.toml"),
            "[build]\nrustc-wrapper=''\n",
        )
        .unwrap();
        let model = load_cargo_configuration(&root, None, &[]).unwrap();
        let plan = compiler_command_plan(
            &root,
            &model,
            RustCargoRunnerProgram::SearchPath {
                value: "rustc".into(),
            },
            &model_inputs(),
        )
        .unwrap();
        assert_eq!(plan.rustc_wrapper, None);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn targets_come_from_the_command_then_the_environment_then_the_configuration() {
        let root = fixture();
        let targets = |config: &str, command: Vec<String>, inputs: CargoModelInputs| {
            fs::write(root.join(".cargo/config.toml"), config).unwrap();
            let model = load_cargo_configuration(&root, None, &[]).unwrap();
            model_targets(&root, &model, command, "host-triple", &inputs)
                .map(|targets| {
                    targets
                        .into_iter()
                        .map(|target| (target.kind, target.name))
                        .collect::<Vec<_>>()
                })
                .map_err(|error| error.to_string())
        };
        let tuple = |name: &str| (ModelTargetKind::Tuple, name.to_owned());
        assert_eq!(
            targets(
                "[build]\ntarget=['a','b']\n",
                Vec::new(),
                model_inputs_with([(
                    OsString::from("CARGO_BUILD_TARGET"),
                    OsString::from("environment")
                )])
            ),
            Ok(vec![tuple("environment")])
        );
        assert_eq!(
            targets(
                "[build]\ntarget=['a','host-tuple']\n",
                Vec::new(),
                model_inputs()
            ),
            Ok(vec![tuple("a"), tuple("host-triple")])
        );
        assert_eq!(
            targets("[build]\ntarget=[' ']\n", Vec::new(), model_inputs()),
            Err("invalid Cargo configuration: Cargo target was empty".into())
        );
        // A target specification named by its absolute path keeps it.
        let specification = root.join("custom.json");
        fs::write(&specification, "{}").unwrap();
        assert_eq!(
            targets(
                "",
                vec![specification.display().to_string()],
                model_inputs()
            ),
            Ok(vec![(ModelTargetKind::Json, "custom".into())])
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn a_compiler_that_fails_to_describe_itself_or_its_target_stops_the_plan() {
        let root = fixture();
        // Says its host, and fails to print any target's cfg values.
        write_script(
            &root.join("half-rustc"),
            "#!/bin/sh\nif [ \"$1\" = -vV ]; then echo 'host: x86_64-unknown-linux-gnu'; exit 0; fi\necho 'no cfg here' >&2\nexit 3\n",
        );
        write_script(
            &root.join("broken-rustc"),
            "#!/bin/sh\necho 'broken' >&2\nexit 4\n",
        );
        let mut inputs = model_inputs();
        inputs.host_override = None;
        let error = |rustc: &str| {
            fs::write(
                root.join(".cargo/config.toml"),
                format!("[build]\nrustc='./{rustc}'\n"),
            )
            .unwrap();
            resolve_with_inputs(&root, &root, &invocation(&["test"]), inputs.clone())
                .unwrap_err()
                .to_string()
        };
        assert_eq!(
            error("broken-rustc"),
            "invalid Cargo configuration: Cargo's configured compiler command failed while resolving its host with status 4: broken"
        );
        assert_eq!(
            error("half-rustc"),
            "invalid Cargo configuration: Cargo's configured compiler command failed while resolving cfg values for target x86_64-unknown-linux-gnu with status 3: no cfg here"
        );
        let host = "if [ \"$1\" = -vV ]; then echo 'host: x86_64-unknown-linux-gnu'; exit 0; fi\n";
        for (rustc, script, said) in [
            (
                "hostless-rustc",
                "echo 'release: 1.95.0'\n".to_owned(),
                "did not report a host in rustc -vV output",
            ),
            (
                "binary-rustc",
                "printf '\\377'\n".to_owned(),
                "produced non-UTF-8 verbose version output",
            ),
            (
                "killed-rustc",
                "kill -9 $$\n".to_owned(),
                "while resolving its host with status signal",
            ),
            (
                "binary-cfg-rustc",
                format!("{host}printf '\\377'\n"),
                "produced non-UTF-8 cfg output for target x86_64-unknown-linux-gnu",
            ),
            (
                "bad-cfg-rustc",
                format!("{host}echo '1x='\n"),
                "rustc emitted an invalid cfg value",
            ),
            ("absent-rustc", String::new(), "absent-rustc"),
        ] {
            if !script.is_empty() {
                write_script(&root.join(rustc), &format!("#!/bin/sh\n{script}"));
            }
            let said_error = error(rustc);
            assert!(said_error.contains(said), "{rustc}: {said_error}");
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_cfg_target_applies_only_when_it_matches_and_sets_a_runner() {
        let root = fixture();
        fs::write(
            root.join(".cargo/config.toml"),
            concat!(
                "[target.'cfg(target_os = \"none\")']\nrunner='never'\n",
                "[target.'cfg(all())']\nrustflags=['--no-runner-here']\n",
            ),
        )
        .unwrap();
        let mut inputs = model_inputs();
        inputs.host_override = None;
        let plan =
            resolve_with_inputs(&root, &root, &invocation(&["test"]), inputs.clone()).unwrap();
        assert_eq!(plan.targets.len(), 1);
        assert_eq!(plan.targets[0].underlying_runner, None);
        // An empty target table configures nothing.
        fs::write(root.join(".cargo/config.toml"), "[target]\n").unwrap();
        let plan =
            resolve_with_inputs(&root, &root, &invocation(&["test"]), inputs.clone()).unwrap();
        assert_eq!(plan.targets[0].underlying_runner, None);
        // A custom target's cfg values are asked for by its spec, from where
        // Cargo runs the compiler.
        #[cfg(unix)]
        {
            fs::write(root.join("board.json"), "{}").unwrap();
            write_script(
                &root.join("spec-rustc"),
                "#!/bin/sh\nif [ \"$1\" = -vV ]; then echo 'host: x86_64-unknown-linux-gnu'; exit 0; fi\n[ -f \"$4\" ] && echo 'target_os=\"none\"'\n",
            );
            fs::write(
                root.join(".cargo/config.toml"),
                "[build]\nrustc='./spec-rustc'\n[target.'cfg(target_os = \"none\")']\nrunner='board-runner'\n",
            )
            .unwrap();
            let plan = resolve_with_inputs(
                &root,
                &root,
                &invocation(&["test", "--target", "board.json"]),
                inputs,
            )
            .unwrap();
            assert!(plan.targets[0].underlying_runner.is_some(), "{plan:?}");
        }
        fs::remove_dir_all(root).unwrap();
    }

    /// A Cargo proxy and a rustup beside it, standing in for an explicit
    /// `+toolchain` selection: rustup prints `rustup.out` and exits with
    /// `rustup.status`; the proxy and the Cargo rustup names each print a
    /// version of their own.
    #[cfg(unix)]
    fn selected_toolchain(root: &Path, rustup: &str, status: i32) -> Result<OsString, String> {
        let bin = root.join("bin");
        fs::create_dir_all(&bin).unwrap();
        write_script(
            &bin.join("cargo"),
            "#!/bin/sh\necho 'cargo 1.95.0 (proxy)'\n",
        );
        write_script(
            &bin.join("rustup"),
            &format!("#!/bin/sh\n{rustup}\nexit {status}\n"),
        );
        write_script(
            &root.join("selected-cargo"),
            "#!/bin/sh\necho 'cargo 1.95.0 (selected)'\n",
        );
        selected_cargo_program(&CargoTestInvocation {
            program: bin.join("cargo").display().to_string(),
            kind: crate::rust_test_runner::RustCargoCommandKind::CargoTest,
            arguments: vec!["+stable".into(), "test".into()],
            runner_arguments: Vec::new(),
        })
        .map_err(|error| error.to_string())
    }

    #[test]
    fn rustflags_come_from_the_target_tables_else_the_build_table() {
        let root = fixture();
        let flags = |config: &str, command: &[&str], inputs: CargoModelInputs| {
            fs::write(root.join(".cargo/config.toml"), config).unwrap();
            resolve_with_inputs(&root, &root, &invocation(command), inputs)
                .unwrap()
                .rustflags
        };
        let words = |words: &[&str]| Some(words.iter().map(|word| (*word).to_owned()).collect());
        assert_eq!(flags("", &["test"], model_inputs()), None);
        // A string is split on whitespace; a variable's words follow.
        assert_eq!(
            flags(
                "[build]\nrustflags='--cfg configured'\n",
                &["test"],
                model_inputs_with([(
                    OsString::from("CARGO_BUILD_RUSTFLAGS"),
                    OsString::from("-C debuginfo=1"),
                )])
            ),
            words(&["--cfg", "configured", "-C", "debuginfo=1"])
        );
        // The target tables together replace the build table.
        assert_eq!(
            flags(
                concat!(
                    "[build]\nrustflags=['--cfg', 'built']\n",
                    "[target.aarch64-apple-darwin]\nrustflags=['--cfg', 'exact']\n",
                    "[target.'cfg(target_vendor = \"apple\")']\nrustflags='--cfg apple'\n",
                    "[target.'cfg(windows)']\nrustflags=['--cfg', 'windows']\n",
                ),
                &["test"],
                model_inputs_with([(
                    OsString::from("CARGO_TARGET_AARCH64_APPLE_DARWIN_RUSTFLAGS"),
                    OsString::from("--cfg environment"),
                )])
            ),
            words(&["--cfg", "exact", "--cfg", "environment", "--cfg", "apple"])
        );
        // Two targets given different flags cannot share one `RUSTFLAGS`.
        assert_eq!(
            flags(
                "[target.aarch64-apple-darwin]\nrustflags=['--cfg', 'apple']\n",
                &[
                    "test",
                    "--target=aarch64-apple-darwin",
                    "--target=x86_64-unknown-linux-gnu"
                ],
                model_inputs()
            ),
            None
        );
        fs::write(root.join(".cargo/config.toml"), "[build]\nrustflags=7\n").unwrap();
        assert_eq!(
            resolve_with_inputs(&root, &root, &invocation(&["test"]), model_inputs())
                .unwrap_err()
                .to_string(),
            "invalid Cargo configuration: build.rustflags must be a string or string array"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn a_toolchain_rustup_cannot_name_exactly_stops_the_run() {
        let root = fixture();
        let error = selected_toolchain(&root, "echo 'no such toolchain' >&2", 1).unwrap_err();
        assert!(
            error.ends_with(
                "rustup failed while resolving the explicit rustup toolchain's Cargo with status 1: no such toolchain"
            ),
            "{error}"
        );
        let error = selected_toolchain(
            &root,
            &format!(
                "echo '{}'; echo 'warning: noisy' >&2",
                root.join("selected-cargo").display()
            ),
            0,
        )
        .unwrap_err();
        assert!(
            error.ends_with(
                "rustup wrote unexpected stderr while resolving the explicit rustup toolchain's Cargo: warning: noisy"
            ),
            "{error}"
        );
        assert_eq!(
            selected_toolchain(&root, "echo 'relative/cargo'", 0).unwrap_err(),
            "invalid Cargo configuration: rustup returned a non-absolute Cargo path for +stable: relative/cargo"
        );
        let directory = fs::canonicalize(&root).unwrap();
        assert_eq!(
            selected_toolchain(&root, &format!("echo '{}'", directory.display()), 0).unwrap_err(),
            format!(
                "invalid Cargo configuration: rustup returned a non-regular Cargo path for +stable: {}",
                directory.display()
            )
        );
        assert_eq!(
            selected_toolchain(
                &root,
                &format!("echo '{}'", root.join("selected-cargo").display()),
                0
            )
            .unwrap_err(),
            "invalid Cargo configuration: the +stable Cargo proxy and rustup's selected Cargo executable disagree"
        );
        fs::remove_dir_all(root).unwrap();
    }

    /// An explicit toolchain is refused at whichever step of finding its
    /// Cargo fails: an empty selector, a proxy that is nowhere, a Cargo
    /// rustup names that is not there or cannot be read, and a proxy or a
    /// selected Cargo that cannot say its version.
    #[cfg(unix)]
    #[test]
    fn an_explicit_toolchain_fails_at_each_step_it_takes() {
        let root = fixture();
        let bin = root.join("bin");
        fs::create_dir_all(&bin).unwrap();
        let selected = root.join("selected-cargo");
        let setup = |proxy: &str, rustup: &str, chosen: &str| {
            write_script(&bin.join("cargo"), &format!("#!/bin/sh\n{proxy}\n"));
            write_script(&bin.join("rustup"), &format!("#!/bin/sh\n{rustup}\n"));
            write_script(&selected, &format!("#!/bin/sh\n{chosen}\n"));
        };
        let select = |program: &Path, arguments: &[&str]| {
            selected_cargo_program(&CargoTestInvocation {
                program: program.display().to_string(),
                kind: crate::rust_test_runner::RustCargoCommandKind::CargoTest,
                arguments: arguments
                    .iter()
                    .map(|argument| (*argument).to_owned())
                    .collect(),
                runner_arguments: Vec::new(),
            })
            .map_err(|error| error.to_string())
        };
        let names = format!("echo '{}'", selected.display());
        let same = "echo 'cargo 1.95.0 (same)'";
        setup(same, &names, same);
        assert!(select(&bin.join("cargo"), &["+stable", "test"]).is_ok());
        let failing = |faults: &str| {
            crate::fault::during(faults, || select(&bin.join("cargo"), &["+stable", "test"]))
        };
        // Reading the named Cargo fails after it was found.
        assert!(failing("fs@2").unwrap_err().contains("selected-cargo"));
        assert!(
            select(&bin.join("cargo"), &["+", "test"])
                .unwrap_err()
                .contains("the rustup toolchain selector is empty")
        );
        assert!(
            select(&root.join("nowhere/cargo"), &["+stable", "test"])
                .unwrap_err()
                .contains("could not resolve the Cargo proxy")
        );
        setup(same, "echo '/nowhere/supercov-cargo'", same);
        assert!(
            select(&bin.join("cargo"), &["+stable", "test"])
                .unwrap_err()
                .contains("/nowhere/supercov-cargo")
        );
        setup("exit 3", &names, same);
        assert!(
            select(&bin.join("cargo"), &["+stable", "test"])
                .unwrap_err()
                .contains("verifying the explicit rustup Cargo selection")
        );
        setup(same, &names, "exit 3");
        assert!(
            select(&bin.join("cargo"), &["+stable", "test"])
                .unwrap_err()
                .contains("verifying the resolved Cargo executable")
        );
        fs::remove_dir_all(root).unwrap();
    }

    /// Each way a configuration or the environment can be written wrong is
    /// refused by what is wrong in it, before anything runs.
    #[test]
    fn each_malformed_configuration_is_refused_by_name() {
        let root = fixture();
        let refused = |config: &str, arguments: &[&str], inputs: CargoModelInputs| {
            fs::write(root.join(".cargo/config.toml"), config).unwrap();
            match resolve_with_inputs(&root, &root, &invocation(arguments), inputs) {
                Err(error) => error.to_string(),
                Ok(plan) => panic!("{config} {arguments:?} was accepted: {plan:?}"),
            }
        };
        let mut real_host = model_inputs();
        real_host.host_override = None;
        for (config, arguments, said) in [
            (
                "[target.'cfg(foo bar)']\nrunner='never'\n",
                &["test"][..],
                "invalid Cargo target cfg key",
            ),
            (
                "[target.'cfg(foo bar)']\nrustflags=['--cfg', 'x']\n",
                &["test"][..],
                "invalid Cargo target cfg key",
            ),
            ("[build]\ntarget=5\n", &["test"][..], "build.target"),
            ("[build\n", &["test"][..], "invalid Cargo configuration"),
            ("[build]\nrustflags=5\n", &["test"][..], "rustflags"),
            (
                "[target.'cfg(all())']\nrustflags=5\n",
                &["test"][..],
                "rustflags",
            ),
            (
                "[target.x86_64-unknown-linux-gnu]\nrunner=''\n[target.aarch64-apple-darwin]\nrunner=''\n[target.x86_64-pc-windows-msvc]\nrunner=''\n",
                &["test"][..],
                "cannot be empty",
            ),
            (
                "[target.x86_64-unknown-linux-gnu]\nrunner='../../outside/run'\n[target.aarch64-apple-darwin]\nrunner='../../outside/run'\n",
                &["test"][..],
                "outside/run",
            ),
            (
                "",
                &["test", "--target", "missing-spec.json"][..],
                "missing-spec.json",
            ),
        ] {
            let error = refused(config, arguments, real_host.clone());
            assert!(error.contains(said), "{config} {arguments:?}: {error}");
        }
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStringExt;
            let not_utf8 = || OsString::from_vec(vec![0xff, 0xfe]);
            for (variable, said) in [
                ("CARGO_BUILD_TARGET", "CARGO_BUILD_TARGET is not UTF-8"),
                ("CARGO_BUILD_RUSTFLAGS", "is not UTF-8"),
                (
                    "CARGO_TARGET_AARCH64_APPLE_DARWIN_RUNNER",
                    "CARGO_TARGET_AARCH64_APPLE_DARWIN_RUNNER is not UTF-8",
                ),
            ] {
                let error = refused(
                    "",
                    &["test"],
                    model_inputs_with([(OsString::from(variable), not_utf8())]),
                );
                assert!(error.contains(said), "{variable}: {error}");
            }
        }
        fs::remove_dir_all(root).unwrap();
    }
}
