//! Python's strong pass (`docs/design/python-mutants.md`): the schemata the
//! runtime switches mutants with, and the driver that runs the command with
//! them.
//!
//! The engine enumerates each file the pass switches again
//! (`python_mutants::enumerate`, the order `mutants.json` lists) and writes,
//! for each function that holds mutants, where it is and each mutant's edit:
//! the bytes it replaces and mutmut's text there. The runtime
//! (`runtime/python/supercov_strong.py`) parses each mutant's copy of the
//! function from that edit as the file is imported and puts one switch at
//! the function's entry. The command runs as the user wrote it; the
//! runtime's pytest plugin, or its patch of unittest's `TestProgram`, takes
//! over once the tests are collected and runs `plan.json`, appending each
//! verdict to `results.jsonl`.

use std::{
    collections::BTreeMap,
    ffi::OsString,
    path::{Path, PathBuf},
};

use serde_json::{Value, json};

use crate::{
    orchestration::{ExecutionPhase, ExecutionPlan, PhaseKind},
    process_supervision::CommandSpec,
    python_mutants::{Enumeration, enumerate},
    strong_driver::StrongPass,
};

/// The schemata's format version: the runtime refuses any other.
pub const SCHEMATA_VERSION: u32 = 1;

/// Where the runtime finds the schemata.
pub const SCHEMATA_ENV: &str = "SUPERCOV_PYTHON_SCHEMATA";

/// The runners Python's driver runs: pytest's and unittest's tests.
pub const RUNNERS: &[&str] = &["pytest", "unittest"];

/// One file's switches: the functions that hold its mutants, each with its
/// `def`'s line and byte column (as `ast` gives them), its end, and its
/// mutants' edits.
fn file_schemata(source: &str, enumeration: &Enumeration) -> Value {
    let starts = std::iter::once(0)
        .chain(
            source
                .bytes()
                .enumerate()
                .filter_map(|(index, byte)| (byte == b'\n').then_some(index + 1)),
        )
        .collect::<Vec<_>>();
    let units = enumeration
        .units
        .iter()
        .enumerate()
        .map(|(unit_index, unit)| {
            let line = starts.partition_point(|start| *start <= unit.def);
            let mutants = enumeration
                .mutants
                .iter()
                .enumerate()
                .filter(|(_, mutant)| mutant.unit == unit_index)
                .map(|(index, mutant)| {
                    json!({
                        "index": index,
                        "at": [mutant.splice.0, mutant.splice.1],
                        "text": mutant.replacement,
                        "default": mutant.default_of.as_ref().map(|(name, keyword_only)| json!([name, keyword_only])),
                    })
                })
                .collect::<Vec<_>>();
            json!({
                "key": unit.key,
                "line": line,
                "column": unit.def - starts[line - 1],
                "end": unit.end,
                "mutants": mutants,
            })
        })
        .collect::<Vec<_>>();
    json!({
        "size": source.len(),
        "crc32": crc32fast::hash(source.as_bytes()),
        "units": units,
    })
}

/// The schemata of `files` under `root`: a file that cannot be read or
/// parsed gets none, and the runtime reports its mutants as errors.
pub fn schemata(root: &Path, files: &[String]) -> Value {
    let mut switched = serde_json::Map::new();
    for file in files {
        let Ok(source) = std::fs::read_to_string(root.join(file)) else {
            continue;
        };
        let Ok(enumeration) = enumerate(&source) else {
            continue;
        };
        switched.insert(file.clone(), file_schemata(&source, &enumeration));
    }
    json!({
        "version": SCHEMATA_VERSION,
        "root": root.to_string_lossy(),
        "files": switched,
    })
}

/// `entry` first on a path list, keeping what the list held.
fn prepend_path(existing: Option<OsString>, entry: &Path) -> OsString {
    let mut paths = vec![entry.to_path_buf()];
    if let Some(existing) = existing {
        paths.extend(std::env::split_paths(&existing));
    }
    std::env::join_paths(paths).unwrap_or_else(|_| entry.as_os_str().to_owned())
}

/// The environment of the command in a strong pass: Supercov's runtime
/// first on the path, the strong plugin for pytest, and the files and plan
/// of the pass; no coverage runtime.
fn environment(
    runtime: &Path,
    pass: &StrongPass,
    schemata: &Path,
    ambient: impl Iterator<Item = (OsString, OsString)>,
) -> Vec<(OsString, OsString)> {
    let mut variables = ambient.collect::<BTreeMap<_, _>>();
    for key in [
        "SUPERCOV_PYTHON_PLAN",
        "SUPERCOV_PYTHON_EVIDENCE_DIR",
        "SUPERCOV_PYTHON_SOURCE_ROOT",
        "SUPERCOV_CONTEXT",
        crate::strong_driver::ACTIVE_MUTANT,
    ] {
        variables.remove(&OsString::from(key));
    }
    let python_path = prepend_path(variables.remove(&OsString::from("PYTHONPATH")), runtime);
    let plugins = match variables.remove(&OsString::from("PYTEST_PLUGINS")) {
        Some(existing) if !existing.is_empty() => {
            let mut value = existing;
            value.push(",supercov_strong_pytest");
            value
        }
        _ => OsString::from("supercov_strong_pytest"),
    };
    variables.insert("PYTHONPATH".into(), python_path);
    variables.insert("PYTEST_PLUGINS".into(), plugins);
    variables.insert(crate::strong_driver::STRONG.into(), "1".into());
    variables.insert(
        crate::strong_driver::STRONG_PLAN.into(),
        pass.strong.plan.clone().into_os_string(),
    );
    variables.insert(
        crate::strong_driver::STRONG_RESULTS.into(),
        pass.strong.results.clone().into_os_string(),
    );
    variables.insert(
        crate::strong_driver::STRONG_FILES.into(),
        pass.strong.switched_files_path().into_os_string(),
    );
    variables.insert(SCHEMATA_ENV.into(), schemata.as_os_str().to_owned());
    variables.into_iter().collect()
}

/// Runs a strong pass's Python mutants: the command with the plan's files
/// switched, its runner driven by the runtime. Returns the command's exit
/// code; the verdicts are in `pass.strong.results`.
pub fn run_strong_python(
    pass: &StrongPass,
    diagnostics: &mut dyn std::io::Write,
) -> Result<i32, String> {
    if pass.command.is_empty() {
        return Err("test command must not be empty".into());
    }
    let work = pass
        .strong
        .results
        .parent()
        .map_or_else(|| pass.root.join(".supercov/strong"), Path::to_path_buf);
    let runtime = work.join("python-runtime");
    crate::python_run::write_runtime(&runtime)?;
    let mut files = pass
        .strong
        .switched_files()
        .unwrap_or_default()
        .into_iter()
        .filter(|file| {
            crate::mutant_list::language_of_path(file) == Some(crate::python_mutants::PYTHON)
        })
        .collect::<Vec<_>>();
    files.sort();
    let listed = pass.strong.switched_files_path();
    std::fs::write(
        &listed,
        serde_json::to_vec(&files).expect("plain data serializes"),
    )
    .map_err(|error| format!("{}: {error}", listed.display()))?;
    let schemata_path: PathBuf = work.join("schemata.json");
    std::fs::write(
        &schemata_path,
        serde_json::to_vec(&schemata(&pass.root, &files)).expect("plain data serializes"),
    )
    .map_err(|error| format!("{}: {error}", schemata_path.display()))?;
    let _ = writeln!(
        diagnostics,
        "[supercov] strong pass: switches in {} Python source file(s)",
        files.len()
    );
    let plan = ExecutionPlan {
        preparation: Vec::new(),
        test: ExecutionPhase {
            name: "strong".into(),
            kind: PhaseKind::Test,
            command: CommandSpec {
                program: pass.command[0].clone().into(),
                arguments: pass.command[1..].iter().map(OsString::from).collect(),
                cwd: pass.root.clone(),
                environment: Some(environment(
                    &runtime,
                    pass,
                    &schemata_path,
                    std::env::vars_os(),
                )),
                // What the tests print is held beside the results, and
                // shown with Supercov's diagnostics where the command fails:
                // the pass's output is Supercov's.
                captured_output: Some(work.join("python.log")),
                console: None,
            },
        },
    };
    let execution =
        crate::python_run::run_supervised(&plan, diagnostics, "this pass saved no verdicts")?;
    Ok(execution.exit_code)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each function that holds mutants is named by its `def`'s line and
    /// byte column, as `ast` gives them, with each mutant's index in the
    /// file's list and its edit; a default's mutant names its parameter.
    #[test]
    fn a_file_s_schemata_name_its_functions_and_their_mutants_edits() {
        let source = "import os\n\nclass A:\n    async def f(self, x=1):\n        return x + 2\n\ndef g():\n    return 'é' + 'b'\n";
        let enumeration = enumerate(source).unwrap();
        let value = file_schemata(source, &enumeration);
        assert_eq!(value["size"], source.len());
        assert_eq!(value["crc32"], crc32fast::hash(source.as_bytes()));
        let units = value["units"].as_array().unwrap();
        assert_eq!(units.len(), 2);
        assert_eq!(units[0]["key"], "xǁAǁf");
        assert_eq!(
            (units[0]["line"].as_u64(), units[0]["column"].as_u64()),
            (Some(4), Some(4))
        );
        assert_eq!(units[1]["key"], "x_g");
        assert_eq!(
            (units[1]["line"].as_u64(), units[1]["column"].as_u64()),
            (Some(7), Some(0))
        );
        let first = &units[0]["mutants"][0];
        assert_eq!(first["index"], 0);
        assert_eq!(first["text"], "2");
        assert_eq!(first["default"], json!(["x", false]));
        assert_eq!(units[0]["mutants"][1]["default"], Value::Null);
        let indexes = units
            .iter()
            .flat_map(|unit| unit["mutants"].as_array().unwrap())
            .map(|mutant| mutant["index"].as_u64().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(
            indexes,
            (0..enumeration.mutants.len() as u64).collect::<Vec<_>>()
        );
        // An edit's bytes are the UTF-8 source's.
        let last = units[1]["mutants"].as_array().unwrap().last().unwrap();
        let at = last["at"].as_array().unwrap();
        let (start, end) = (
            at[0].as_u64().unwrap() as usize,
            at[1].as_u64().unwrap() as usize,
        );
        assert!(source.is_char_boundary(start) && source.is_char_boundary(end));
    }

    /// A pass writes the runtime, the switched files' list and their
    /// schemata beside its results, and returns the command's exit code; an
    /// empty command is refused.
    #[cfg(unix)]
    #[test]
    fn a_pass_runs_its_command_beside_the_schemata() {
        let root = std::env::temp_dir().join(format!(
            "supercov-python-strong-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let work = root.join(".supercov/strong/r-1");
        std::fs::create_dir_all(&work).unwrap();
        std::fs::write(root.join("a.py"), "def f(x):\n    return x + 1\n").unwrap();
        std::fs::write(
            work.join("plan.json"),
            r#"{"mutants": [{"file": "a.py"}, {"file": "b.js"}, {"file": "c.py", "decided": "survived"}]}"#,
        )
        .unwrap();
        let mut pass = StrongPass {
            root: root.clone(),
            command: vec!["sh".into(), "-c".into(), "exit 3".into()],
            run_id: "r".into(),
            watchdog_program: None,
            strong: crate::strong_driver::StrongRun {
                plan: work.join("plan.json"),
                results: work.join("results.jsonl"),
            },
        };
        let mut said = Vec::new();
        assert_eq!(run_strong_python(&pass, &mut said), Ok(3));
        assert!(String::from_utf8_lossy(&said).contains("switches in 1 Python source file(s)"));
        assert!(work.join("python-runtime/supercov_strong.py").exists());
        let listed: Vec<String> =
            serde_json::from_slice(&std::fs::read(work.join("strong-files.json")).unwrap())
                .unwrap();
        assert_eq!(listed, ["a.py"]);
        let schemata: Value =
            serde_json::from_slice(&std::fs::read(work.join("schemata.json")).unwrap()).unwrap();
        assert!(schemata["files"]["a.py"]["units"].is_array());
        // What the command prints is Supercov's diagnostics where it fails,
        // never its output.
        pass.command = vec!["sh".into(), "-c".into(), "echo printed; exit 4".into()];
        let mut said = Vec::new();
        assert_eq!(run_strong_python(&pass, &mut said), Ok(4));
        assert!(String::from_utf8_lossy(&said).contains("printed"));
        assert!(!work.join("python.log").exists());
        // Each file the pass writes that cannot be written ends it.
        for blocked in ["schemata.json", "strong-files.json"] {
            std::fs::remove_file(work.join(blocked)).unwrap();
            std::fs::create_dir(work.join(blocked)).unwrap();
            let refused = run_strong_python(&pass, &mut Vec::new()).unwrap_err();
            assert!(refused.contains(blocked), "{refused}");
        }
        let elsewhere = StrongPass {
            strong: crate::strong_driver::StrongRun {
                plan: work.join("plan.json"),
                results: root.join("a.py").join("results.jsonl"),
            },
            ..pass.clone()
        };
        assert!(run_strong_python(&elsewhere, &mut Vec::new()).is_err());
        pass.command.clear();
        assert_eq!(
            run_strong_python(&pass, &mut Vec::new()),
            Err("test command must not be empty".into())
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A file that cannot be read or parsed gets no switches.
    #[test]
    fn a_file_that_cannot_be_enumerated_gets_no_schemata() {
        let root =
            std::env::temp_dir().join(format!("supercov-python-schemata-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("bad.py"), "def (:\n").unwrap();
        std::fs::write(root.join("good.py"), "def f():\n    return 1\n").unwrap();
        let value = schemata(
            &root,
            &["bad.py".into(), "good.py".into(), "missing.py".into()],
        );
        assert_eq!(value["version"], SCHEMATA_VERSION);
        let files = value["files"].as_object().unwrap();
        assert_eq!(files.keys().collect::<Vec<_>>(), ["good.py"]);
        assert!(
            schemata(&root, &[])["files"]
                .as_object()
                .unwrap()
                .is_empty()
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The command's environment: the runtime first on the path, the
    /// strong plugin after the user's, the pass's files, and no coverage
    /// runtime or inherited mutant.
    #[test]
    fn the_environment_arms_the_strong_runtime_and_not_the_coverage_one() {
        let pass = StrongPass {
            root: PathBuf::from("/project"),
            command: vec!["pytest".into()],
            run_id: "r".into(),
            watchdog_program: None,
            strong: crate::strong_driver::StrongRun {
                plan: PathBuf::from("/work/plan.json"),
                results: PathBuf::from("/work/results.jsonl"),
            },
        };
        let ambient = [
            ("PYTHONPATH", "/user/lib"),
            ("PYTEST_PLUGINS", "theirs"),
            ("SUPERCOV_PYTHON_PLAN", "/old/plan.json"),
            ("SUPERCOV_ACTIVE_MUTANT", "a.py#1"),
            ("HOME", "/home"),
        ]
        .map(|(key, value)| (OsString::from(key), OsString::from(value)));
        let variables = environment(
            Path::new("/work/python-runtime"),
            &pass,
            Path::new("/work/schemata.json"),
            ambient.into_iter(),
        )
        .into_iter()
        .map(|(key, value)| {
            (
                key.to_string_lossy().into_owned(),
                value.to_string_lossy().into_owned(),
            )
        })
        .collect::<BTreeMap<_, _>>();
        assert_eq!(
            variables["PYTHONPATH"],
            std::env::join_paths(["/work/python-runtime", "/user/lib"])
                .unwrap()
                .to_string_lossy()
        );
        assert_eq!(variables["PYTEST_PLUGINS"], "theirs,supercov_strong_pytest");
        assert_eq!(variables["SUPERCOV_STRONG"], "1");
        assert_eq!(variables["SUPERCOV_STRONG_PLAN"], "/work/plan.json");
        assert_eq!(variables["SUPERCOV_STRONG_RESULTS"], "/work/results.jsonl");
        assert_eq!(
            variables["SUPERCOV_STRONG_FILES"],
            "/work/strong-files.json"
        );
        assert_eq!(variables[SCHEMATA_ENV], "/work/schemata.json");
        assert_eq!(variables["HOME"], "/home");
        assert!(!variables.contains_key("SUPERCOV_PYTHON_PLAN"));
        assert!(!variables.contains_key("SUPERCOV_ACTIVE_MUTANT"));
        let bare = environment(Path::new("/r"), &pass, Path::new("/s"), std::iter::empty())
            .into_iter()
            .collect::<BTreeMap<_, _>>();
        assert_eq!(
            bare[&OsString::from("PYTEST_PLUGINS")],
            "supercov_strong_pytest"
        );
        assert_eq!(bare[&OsString::from("PYTHONPATH")], "/r");
        // An empty plugin list is no list.
        let emptied = environment(
            Path::new("/r"),
            &pass,
            Path::new("/s"),
            std::iter::once(("PYTEST_PLUGINS".into(), OsString::new())),
        )
        .into_iter()
        .collect::<BTreeMap<_, _>>();
        assert_eq!(
            emptied[&OsString::from("PYTEST_PLUGINS")],
            "supercov_strong_pytest"
        );
    }
}
