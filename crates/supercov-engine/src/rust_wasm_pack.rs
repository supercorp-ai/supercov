//! `wasm-pack test`: what it hands Cargo, learned by running it.
//!
//! wasm-pack resolves the browser driver (downloading `chromedriver` or
//! `geckodriver` when asked to), installs the matching
//! `wasm-bindgen-test-runner`, and then runs `cargo test --target
//! wasm32-unknown-unknown` with the runner and the driver in the environment.
//! Supercov runs a project's tests itself, test by test, so it runs wasm-pack
//! once with a `cargo` of its own first on `PATH`: `cargo metadata` passes
//! through, the `cargo build --tests` that only warms a cache is skipped, and
//! `cargo test` is recorded rather than run. The run then measures that
//! `cargo test`, with the runner named on its command line and the driver's
//! variables given to each test's runner.

// On Windows `learn` refuses at once, and nothing below it is called.
#![cfg_attr(windows, allow(dead_code, unused_imports))]

use crate::fault_fs as fs;
use std::{
    path::{Path, PathBuf},
    process::Command,
};

/// What `wasm-pack test` would have run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WasmPackTest {
    /// `cargo test ...`, with the target runner given by `--config`.
    pub command: Vec<String>,
    /// The variables the runner reads: the driver, headless or not, which
    /// environments to run in.
    pub runner_environment: Vec<(String, String)>,
}

/// The variables of wasm-pack's `cargo test` that its runner reads.
const RUNNER_VARIABLES: &[&str] = &[
    "CHROMEDRIVER",
    "CHROMEDRIVER_ARGS",
    "GECKODRIVER",
    "GECKODRIVER_ARGS",
    "SAFARIDRIVER",
    "SAFARIDRIVER_ARGS",
    "MSEDGEDRIVER",
    "MSEDGEDRIVER_ARGS",
    "NO_HEADLESS",
    "WASM_BINDGEN_TEST_ONLY_WEB",
    "WASM_BINDGEN_TEST_ONLY_NODE",
];

/// `wasm-pack test ...`.
pub fn is_wasm_pack_test(command: &[String]) -> bool {
    command.first().is_some_and(|program| {
        Path::new(program)
            .file_stem()
            .is_some_and(|name| name == "wasm-pack")
    }) && command.get(1).is_some_and(|word| word == "test")
}

/// Run `command`, a `wasm-pack test`, in `root` with Supercov's `cargo`
/// first on `PATH`, and read back the `cargo test` it would have run.
pub fn learn(root: &Path, command: &[String], scratch: &Path) -> Result<WasmPackTest, String> {
    #[cfg(windows)]
    {
        let _ = (root, command, scratch);
        Err("`wasm-pack test` is measured on Linux and macOS; on Windows, run the `cargo test --target wasm32-unknown-unknown` it runs".into())
    }
    #[cfg(not(windows))]
    learn_through_cargo(root, command, scratch)
}

#[cfg(not(windows))]
fn learn_through_cargo(
    root: &Path,
    command: &[String],
    scratch: &Path,
) -> Result<WasmPackTest, String> {
    let cargo = which("cargo").ok_or("no `cargo` on PATH for `wasm-pack test`")?;
    let bin = scratch.join("wasm-pack-cargo");
    fs::create_dir_all(&bin).map_err(|error| format!("{}: {error}", bin.display()))?;
    let record = scratch.join("wasm-pack-test.txt");
    let _ = fs::remove_file(&record);
    write_cargo(&bin, &cargo, &record)?;
    let mut path = vec![bin.clone()];
    path.extend(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    ));
    let output = Command::new(&command[0])
        .args(&command[1..])
        .current_dir(root)
        .env(
            "PATH",
            std::env::join_paths(path).map_err(|error| error.to_string())?,
        )
        .output()
        .map_err(|error| format!("could not run {}: {error}", command[0]))?;
    let text = fs::read_to_string(&record).map_err(|_| {
        let said = String::from_utf8_lossy(&output.stderr);
        let tail = said.lines().rev().take(8).collect::<Vec<_>>();
        format!(
            "`wasm-pack test` stopped before it ran `cargo test`:\n{}",
            tail.into_iter().rev().collect::<Vec<_>>().join("\n")
        )
    })?;
    parse_record(root, &text)
}

/// The recorded call: the working directory, then each argument, then
/// `NAME=value` for each runner variable, separated by a line `--env`.
fn parse_record(root: &Path, text: &str) -> Result<WasmPackTest, String> {
    let mut lines = text.lines();
    let directory = PathBuf::from(lines.next().ok_or("an empty wasm-pack record")?);
    let mut arguments = Vec::new();
    let mut environment = Vec::new();
    let mut in_environment = false;
    for line in lines {
        if line == "--env" {
            in_environment = true;
        } else if in_environment {
            if let Some((name, value)) = line.split_once('=') {
                environment.push((name.to_owned(), value.to_owned()));
            }
        } else {
            arguments.push(line.to_owned());
        }
    }
    let mut command = vec!["cargo".to_owned()];
    command.extend(arguments);
    // The runner wasm-pack chose, named where the run reads runners from.
    let mut runner_environment = Vec::new();
    for (name, value) in environment {
        if let Some(target) = name
            .strip_prefix("CARGO_TARGET_")
            .and_then(|rest| rest.strip_suffix("_RUNNER"))
        {
            let triple = target.to_ascii_lowercase().replace('_', "-");
            command.push("--config".into());
            // A string, as most projects write a runner: given as an array it
            // could not be merged with a `.cargo/config.toml` string one.
            command.push(format!(
                "target.{triple}.runner={}",
                serde_json::to_string(&value).map_err(|error| error.to_string())?
            ));
        } else if RUNNER_VARIABLES.contains(&name.as_str()) {
            runner_environment.push((name, value));
        }
    }
    // A crate below the project root is the one wasm-pack ran in.
    let canonical_root = fs::canonicalize(root).unwrap_or_else(|_| root.to_owned());
    let canonical_directory = fs::canonicalize(&directory).unwrap_or(directory);
    if canonical_directory != canonical_root
        && let Ok(relative) = canonical_directory.strip_prefix(&canonical_root)
    {
        command.push("--manifest-path".into());
        command.push(relative.join("Cargo.toml").to_string_lossy().into_owned());
    }
    Ok(WasmPackTest {
        command,
        runner_environment,
    })
}

fn write_cargo(bin: &Path, cargo: &Path, record: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let quote = |path: &Path| format!("'{}'", path.to_string_lossy().replace('\'', r"'\''"));
        let variables = RUNNER_VARIABLES.join("|");
        let script = format!(
            r#"#!/bin/sh
# Written by Supercov: records the `cargo test` wasm-pack runs, instead of
# running it, and skips the `cargo build --tests` that only warms a cache.
case "$1" in
  test)
    {{
      pwd
      for argument in "$@"; do printf '%s\n' "$argument"; done
      echo --env
      env | grep -E '^(CARGO_TARGET_[A-Z0-9_]+_RUNNER|{variables})='
    }} > {record}
    exit 0
    ;;
  build) exit 0 ;;
esac
exec {cargo} "$@"
"#,
            record = quote(record),
            cargo = quote(cargo),
        );
        let path = bin.join("cargo");
        fs::write(&path, script).map_err(|error| format!("{}: {error}", path.display()))?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755))
            .map_err(|error| format!("{}: {error}", path.display()))?;
    }
    #[cfg(not(unix))]
    {
        let _ = (bin, cargo, record);
    }
    Ok(())
}

fn which(name: &str) -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|directory| directory.join(name))
        .find(|candidate| fs::metadata(candidate).is_ok_and(|metadata| metadata.is_file()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_wasm_pack_test_is_learned() {
        let words = |text: &str| text.split(' ').map(str::to_owned).collect::<Vec<_>>();
        assert!(is_wasm_pack_test(&words(
            "wasm-pack test --headless --chrome"
        )));
        assert!(is_wasm_pack_test(&words("/usr/bin/wasm-pack test --node")));
        assert!(!is_wasm_pack_test(&words("wasm-pack build")));
        assert!(!is_wasm_pack_test(&words("cargo test")));
    }

    /// The runner becomes a `--config` on the command, the driver's
    /// variables go to the runner, and a crate below the root is named.
    #[test]
    fn a_record_becomes_the_cargo_test_it_stands_for() {
        let root = std::env::temp_dir().join(format!("supercov-wasm-pack-{}", std::process::id()));
        fs::create_dir_all(root.join("crates/web")).unwrap();
        let record = format!(
            "{}\ntest\n--target\nwasm32-unknown-unknown\n--env\nCARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUNNER=/c/wasm-bindgen-test-runner\nCHROMEDRIVER=/c/chromedriver\nWASM_BINDGEN_TEST_ONLY_WEB=1\n",
            root.join("crates/web").display()
        );
        let learned = parse_record(&root, &record).unwrap();
        // The manifest path in the platform's own separators.
        let manifest = Path::new("crates").join("web").join("Cargo.toml");
        assert_eq!(
            learned.command,
            [
                "cargo",
                "test",
                "--target",
                "wasm32-unknown-unknown",
                "--config",
                "target.wasm32-unknown-unknown.runner=\"/c/wasm-bindgen-test-runner\"",
                "--manifest-path",
                &manifest.to_string_lossy(),
            ]
        );
        assert_eq!(
            learned.runner_environment,
            [
                ("CHROMEDRIVER".to_owned(), "/c/chromedriver".to_owned()),
                ("WASM_BINDGEN_TEST_ONLY_WEB".to_owned(), "1".to_owned()),
            ]
        );
        fs::remove_dir_all(&root).unwrap();
    }

    /// A line of the environment with no `=` and a variable no runner reads
    /// are passed over, and a crate outside the project's root is not named.
    #[test]
    fn a_record_keeps_only_the_runner_s_variables_and_a_crate_below_the_root() {
        let root =
            std::env::temp_dir().join(format!("supercov-wasm-pack-outside-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        let record = "/elsewhere/web\ntest\n--env\nnot a variable\nHOME=/root\nNO_HEADLESS=1\n";
        let learned = parse_record(&root, record).unwrap();
        assert_eq!(learned.command, ["cargo", "test"]);
        assert_eq!(
            learned.runner_environment,
            [("NO_HEADLESS".to_owned(), "1".to_owned())]
        );
        fs::remove_dir_all(&root).unwrap();
    }
}
