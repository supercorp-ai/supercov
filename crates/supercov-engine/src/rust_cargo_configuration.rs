//! Exact pre-execution resolution for Cargo target runners.
//!
//! Cargo remains the authority for building and ordering test artifacts, but
//! Supercov must place an already-configured target runner *inside* its
//! process-per-test boundary. This module resolves the user's original Cargo
//! configuration before the repository is copied into the isolated workspace.
//! Unsupported configuration surfaces fail before user code executes.

use std::{
    ffi::OsString,
    fs,
    path::{Component, Path, PathBuf},
    process::Command,
};

use cargo_config2::{Config, ResolveOptions, Walk};
use serde::{Deserialize, Serialize};

use crate::rust_test_runner::CargoTestInvocation;

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
pub struct RustCargoRunnerPlan {
    pub target: String,
    pub underlying_runner: Option<RustCargoUnderlyingRunner>,
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

fn command_targets(
    invocation: &CargoTestInvocation,
) -> Result<Vec<String>, RustCargoConfigurationError> {
    let test_position = invocation
        .arguments
        .iter()
        .position(|argument| argument == "test")
        .ok_or_else(|| {
            RustCargoConfigurationError::Invalid("Cargo test subcommand is missing".into())
        })?;
    toolchain_selector(invocation, test_position)?;
    if invocation.arguments[..test_position]
        .iter()
        .any(|argument| argument == "-Z" || argument.starts_with("-Z"))
    {
        return Err(RustCargoConfigurationError::Unsupported(
            "Cargo runner composition does not yet resolve pre-subcommand -Z configuration semantics exactly"
                .into(),
        ));
    }
    let mut targets = Vec::new();
    let mut index = 0;
    while index < invocation.arguments.len() {
        let argument = &invocation.arguments[index];
        if argument == "--config" || argument.starts_with("--config=") {
            return Err(RustCargoConfigurationError::Unsupported(
                "Cargo runner composition does not yet support Cargo --config; refusing to run before changing the configured runner"
                    .into(),
            ));
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
    Ok(targets)
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
        .output()
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
    let test_position = invocation
        .arguments
        .iter()
        .position(|argument| argument == "test")
        .ok_or_else(|| {
            RustCargoConfigurationError::Invalid("Cargo test subcommand is missing".into())
        })?;
    let Some(selector) = toolchain_selector(invocation, test_position)? else {
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

fn reject_unresolved_includes(root: &Path) -> Result<(), RustCargoConfigurationError> {
    for path in Walk::new(root) {
        let contents = fs::read_to_string(&path).map_err(|error| io_error(&path, error))?;
        let value: toml::Value = toml::from_str(&contents).map_err(|error| {
            RustCargoConfigurationError::Invalid(format!("{}: {error}", path.display()))
        })?;
        if value
            .as_table()
            .is_some_and(|table| table.contains_key("include"))
        {
            return Err(RustCargoConfigurationError::Unsupported(format!(
                "Cargo runner composition does not yet support Cargo config include in {}; refusing to ignore inherited configuration",
                path.display()
            )));
        }
    }
    Ok(())
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

fn resolve_with_options(
    root: &Path,
    invocation: &CargoTestInvocation,
    options: ResolveOptions,
) -> Result<RustCargoRunnerPlan, RustCargoConfigurationError> {
    let root = fs::canonicalize(root).map_err(|error| io_error(root, error))?;
    reject_unresolved_includes(&root)?;
    let targets = command_targets(invocation)?;
    let selected_cargo = selected_cargo_program(invocation)?;
    let config = Config::load_with_options(&root, options.cargo(selected_cargo))
        .map_err(|error| RustCargoConfigurationError::Invalid(error.to_string()))?;
    let targets = config
        .build_target_for_config(targets.iter())
        .map_err(|error| RustCargoConfigurationError::Invalid(error.to_string()))?;
    let [target] = targets.as_slice() else {
        return Err(RustCargoConfigurationError::Unsupported(format!(
            "Cargo selected {} targets; exact multi-target runner composition is not yet implemented",
            targets.len()
        )));
    };
    let target_name = target.triple().to_owned();
    let runner = config
        .runner(target)
        .map_err(|error| RustCargoConfigurationError::Invalid(error.to_string()))?
        .map(|runner| {
            let arguments = runner
                .args
                .into_iter()
                .map(OsString::into_string)
                .collect::<Result<Vec<_>, _>>()
                .map_err(|_| {
                    RustCargoConfigurationError::Invalid(
                        "Cargo runner arguments are not UTF-8".into(),
                    )
                })?;
            Ok(RustCargoUnderlyingRunner {
                program: runner_program(&root, runner.path)?,
                arguments,
            })
        })
        .transpose()?;
    Ok(RustCargoRunnerPlan {
        target: target_name,
        underlying_runner: runner,
    })
}

pub(crate) fn resolve_cargo_runner_plan(
    root: &Path,
    invocation: &CargoTestInvocation,
) -> Result<RustCargoRunnerPlan, RustCargoConfigurationError> {
    resolve_with_options(root, invocation, ResolveOptions::default())
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

    fn invocation(arguments: &[&str]) -> CargoTestInvocation {
        CargoTestInvocation {
            program: "cargo".into(),
            arguments: arguments.iter().map(|value| (*value).into()).collect(),
            runner_arguments: Vec::new(),
        }
    }

    fn options(root: &Path) -> ResolveOptions {
        options_with(root, [])
    }

    fn options_with<const N: usize>(
        root: &Path,
        additional: [(OsString, OsString); N],
    ) -> ResolveOptions {
        let mut environment = vec![(
            OsString::from("CARGO_HOME"),
            root.join("empty-home").into_os_string(),
        )];
        environment.extend(additional);
        ResolveOptions::default()
            .cargo_home(None)
            .host_triple("aarch64-apple-darwin")
            .env(environment)
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
        let plan = resolve_with_options(&root, &invocation(&["test"]), options(&root)).unwrap();
        assert_eq!(
            plan.underlying_runner,
            Some(RustCargoUnderlyingRunner {
                program: RustCargoRunnerProgram::SearchPath {
                    value: "exact-runner".into(),
                },
                arguments: vec!["--exact".into()],
            })
        );
        let plan = resolve_with_options(
            &root,
            &invocation(&["test"]),
            options_with(
                &root,
                [(
                    OsString::from("CARGO_TARGET_AARCH64_APPLE_DARWIN_RUNNER"),
                    OsString::from("environment-runner --environment"),
                )],
            ),
        )
        .unwrap();
        assert_eq!(
            plan.underlying_runner,
            Some(RustCargoUnderlyingRunner {
                program: RustCargoRunnerProgram::SearchPath {
                    value: "environment-runner".into(),
                },
                arguments: vec!["--environment".into()],
            })
        );
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
        let plan = resolve_with_options(&root, &invocation(&["test"]), options(&root)).unwrap();
        assert_eq!(plan.target, "aarch64-apple-darwin");
        assert_eq!(
            plan.underlying_runner,
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
    fn resolves_an_explicit_installed_rustup_toolchain_before_target_configuration() {
        let root = fixture();
        let rustc = Command::new("rustc")
            .args(["+1.95.0", "-vV"])
            .output()
            .unwrap();
        assert!(
            rustc.status.success(),
            "{}",
            String::from_utf8_lossy(&rustc.stderr)
        );
        let rustc = String::from_utf8(rustc.stdout).unwrap();
        let host = rustc
            .lines()
            .find_map(|line| line.strip_prefix("host: "))
            .unwrap();
        fs::write(
            root.join(".cargo/config.toml"),
            format!("[target.{host}]\nrunner=[\"selected-runner\",\"--selected\"]\n"),
        )
        .unwrap();
        let plan = resolve_with_options(
            &root,
            &invocation(&["+1.95.0", "test"]),
            ResolveOptions::default().cargo_home(None).env([(
                OsString::from("CARGO_HOME"),
                root.join("empty-home").into_os_string(),
            )]),
        )
        .unwrap();
        assert_eq!(plan.target, host);
        assert_eq!(
            plan.underlying_runner,
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
    fn rejects_include_cli_override_and_multiple_targets_before_execution() {
        let root = fixture();
        fs::write(root.join(".cargo/extra.toml"), "[build]\njobs=1\n").unwrap();
        fs::write(
            root.join(".cargo/config.toml"),
            "include=[\"extra.toml\"]\n",
        )
        .unwrap();
        assert!(
            resolve_with_options(&root, &invocation(&["test"]), options(&root))
                .unwrap_err()
                .to_string()
                .contains("config include")
        );
        fs::write(root.join(".cargo/config.toml"), "").unwrap();
        assert!(
            resolve_with_options(
                &root,
                &invocation(&["test", "--config", "build.jobs=1"]),
                options(&root),
            )
            .unwrap_err()
            .to_string()
            .contains("--config")
        );
        assert!(
            resolve_with_options(
                &root,
                &invocation(&["test", "--target=a", "--target=b"]),
                options(&root),
            )
            .unwrap_err()
            .to_string()
            .contains("2 targets")
        );
        assert!(
            command_targets(&invocation(&["--quiet", "+nightly", "test"]))
                .unwrap_err()
                .to_string()
                .contains("must be the first")
        );
        assert!(
            resolve_with_options(
                &root,
                &invocation(&["-Ztarget-applies-to-host", "test"]),
                options(&root),
            )
            .unwrap_err()
            .to_string()
            .contains("-Z configuration semantics")
        );
        fs::remove_dir_all(root).unwrap();
    }
}
