//! Strong mutation's drivers, one per language
//! (`docs/design/mutation-drivers.md`).
//!
//! `supercov runs <id> mutants test` plans every language's mutants alike
//! (`strong_plan`), then hands each language's share of the plan to that
//! language's driver as files: the driver reads `plan.json`, runs each
//! mutant against its candidate tests, and appends one verdict per mutant to
//! `results.jsonl`. A driver chooses its runner from the plan's tests; the
//! command learns which mutant is on from the environment below.

use std::path::{Path, PathBuf};

/// Set to `1` in every process that instruments: switches (mutant
/// schemata) instead of probes.
pub const STRONG: &str = "SUPERCOV_STRONG";
/// A JSON list's path: the only files that get switches. Others are left
/// as written.
pub const STRONG_FILES: &str = "SUPERCOV_STRONG_FILES";
/// The plan the driver runs (`plan.json`).
pub const STRONG_PLAN: &str = "SUPERCOV_STRONG_PLAN";
/// Where the driver appends its verdicts (`results.jsonl`).
pub const STRONG_RESULTS: &str = "SUPERCOV_STRONG_RESULTS";
/// `<file>#<index>`: the mutant switched on in a process from its start;
/// `-` or unset for none. `file` is the mutant's file as `mutants.json`
/// keys it, `index` its place in that file's list.
pub const ACTIVE_MUTANT: &str = "SUPERCOV_ACTIVE_MUTANT";
/// How many times the active mutant may be evaluated in one process before
/// its switch raises the timeout error (default `DEFAULT_HIT_LIMIT`).
pub const MUTANT_HIT_LIMIT: &str = "SUPERCOV_MUTANT_HIT_LIMIT";
/// The default of `MUTANT_HIT_LIMIT`.
pub const DEFAULT_HIT_LIMIT: u64 = 1_000_000;
/// What the timeout error carries (its `code`, or its message where a
/// language has no codes), so a test that fails on it is told apart.
pub const MUTANT_TIMEOUT_CODE: &str = "SUPERCOV_MUTANT_TIMEOUT";

/// The suffix of the file beside the results where a process appends a
/// line for each mutant that passed its hit limit, the error caught or not:
/// `{"mutant": "<file>#<index>", "attemptId": <id or null>, "pass": <pass>}`.
pub const TIMEOUTS_SUFFIX: &str = ".timeouts.jsonl";

/// What a strong pass hands a driver: the plan it runs, and where it writes
/// each mutant's verdict.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StrongRun {
    pub plan: PathBuf,
    pub results: PathBuf,
}

impl StrongRun {
    /// The files that hold a mutant the plan runs: the only ones that need
    /// switches. `None` when the plan cannot be read, which switches every
    /// file.
    pub fn switched_files(&self) -> Option<std::collections::HashSet<String>> {
        let plan =
            serde_json::from_slice::<serde_json::Value>(&std::fs::read(&self.plan).ok()?).ok()?;
        Some(
            plan["mutants"]
                .as_array()?
                .iter()
                .filter(|mutant| mutant.get("decided").is_none_or(serde_json::Value::is_null))
                .filter_map(|mutant| mutant["file"].as_str().map(str::to_owned))
                .collect(),
        )
    }

    /// Where the pass lists its switched files for other processes
    /// (`STRONG_FILES`).
    pub fn switched_files_path(&self) -> PathBuf {
        self.results.with_file_name("strong-files.json")
    }

    /// The file a process appends a mutant's timeout to.
    pub fn timeouts_path(&self) -> PathBuf {
        let mut path = self.results.clone().into_os_string();
        path.push(TIMEOUTS_SUFFIX);
        PathBuf::from(path)
    }
}

/// One language's strong pass: the command over a copy of the project with
/// the plan's mutants behind switches.
#[derive(Debug, Clone)]
pub struct StrongPass {
    pub root: PathBuf,
    pub command: Vec<String>,
    pub run_id: String,
    /// The executable that contains a crashed run's processes.
    pub watchdog_program: Option<PathBuf>,
    pub strong: StrongRun,
}

/// Whether `language` has a strong driver.
pub fn has_driver(language: &str) -> bool {
    matches!(
        language,
        crate::mutant_list::JAVASCRIPT | crate::ruby_mutants::RUBY
    )
}

/// Runs `language`'s share of a plan through that language's driver, and
/// returns the command's exit code. Its verdicts are in `pass.strong.results`.
pub fn run_strong_pass(
    language: &str,
    pass: &StrongPass,
    diagnostics: &mut dyn std::io::Write,
) -> Result<i32, String> {
    match language {
        crate::mutant_list::JAVASCRIPT => {
            let request = crate::javascript_run::DirectJavascriptRunRequest {
                root: pass.root.clone(),
                command: pass.command.clone(),
                run_id: Some(pass.run_id.clone()),
                started_at: None,
                watchdog_program: pass.watchdog_program.clone(),
                strong: Some(pass.strong.clone()),
            };
            crate::javascript_run::run_strong_javascript(&request, diagnostics)
                .map_err(|error| error.to_string())
        }
        crate::ruby_mutants::RUBY => crate::ruby_run::run_strong_ruby(pass, diagnostics),
        other => Err(format!("no strong mutation driver for {other} yet")),
    }
}

/// The directory a pass over run `run_id` works in, under `root`.
pub fn work_directory(root: &Path, run_id: &str, nonce: u128) -> PathBuf {
    root.join(".supercov/strong")
        .join(format!("{run_id}-{nonce}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pass_switches_the_files_of_the_mutants_it_runs() {
        let directory = std::env::temp_dir().join(format!(
            "supercov-strong-driver-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let strong = StrongRun {
            plan: directory.join("plan.json"),
            results: directory.join("results.jsonl"),
        };
        assert_eq!(strong.switched_files(), None);
        std::fs::write(
            &strong.plan,
            r#"{"mutants": [{"file": "a.js"}, {"file": "b.js", "decided": "survived"}]}"#,
        )
        .unwrap();
        assert_eq!(
            strong.switched_files(),
            Some(std::collections::HashSet::from(["a.js".to_owned()]))
        );
        assert_eq!(
            strong.switched_files_path(),
            directory.join("strong-files.json")
        );
        assert_eq!(
            strong.timeouts_path(),
            directory.join("results.jsonl.timeouts.jsonl")
        );
        let _ = std::fs::remove_dir_all(&directory);
    }

    #[test]
    fn only_a_language_with_a_driver_runs() {
        assert!(has_driver("javascript"));
        assert!(!has_driver("cobol"));
        let pass = StrongPass {
            root: PathBuf::from("."),
            command: vec!["true".into()],
            run_id: "r".into(),
            watchdog_program: None,
            strong: StrongRun {
                plan: PathBuf::from("plan.json"),
                results: PathBuf::from("results.jsonl"),
            },
        };
        assert_eq!(
            run_strong_pass("cobol", &pass, &mut Vec::new()),
            Err("no strong mutation driver for cobol yet".into())
        );
    }
}
