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

/// The runners whose tests a strong pass runs: those a language's driver
/// drives. JavaScript's: Vitest and Playwright drive themselves from within
/// the command (`vitestStrong*.mjs`, `playwrightStrong*.mjs`), the others
/// through `strongJobs.mjs` (`JAVASCRIPT_JOB_RUNNERS`). A language adds its
/// runners as it adds its driver.
pub const STRONG_RUNNERS: &[&str] = &[
    "vitest",
    "playwright",
    "jest",
    "mocha",
    "node:test",
    "ava",
    "tap",
    "bun:test",
    "vitest-browser",
    // Rust's (`rust_strong`): libtest and nextest tests by name through
    // their binaries, doctests through `cargo test --doc`.
    "rust-libtest",
    "nextest",
    "rustdoc",
];

/// The JavaScript runners whose strong pass is `strongJobs.mjs`: each job
/// starts the command again with one mutant on, as these runners start a
/// process (or a worker) per test file anyway.
pub const JAVASCRIPT_JOB_RUNNERS: &[&str] = &[
    "jest",
    "mocha",
    "node:test",
    "ava",
    "tap",
    "bun:test",
    "vitest-browser",
];

/// The JavaScript runners whose tests the runtime hands a wasm instance its
/// mutant under: in Node, and in Playwright's pages.
const WASM_JAVASCRIPT_RUNNERS: &[&str] = &[
    "playwright",
    "vitest",
    "jest",
    "mocha",
    "node:test",
    "ava",
    "tap",
    "bun:test",
];

/// The driver that runs `mutant`: its language's, but for a Rust mutant
/// whose candidates are all JavaScript tests (Rust compiled to wasm that
/// they load), which the JavaScript driver runs, switching the mutant on
/// through the wasm's exports. One a runner the runtime cannot hand the
/// mutant under stays Rust's, which records it `error`.
pub fn driver_of<'a>(
    mutant: &'a crate::strong_plan::PlannedMutant,
    tests: &[crate::strong_plan::PlannedTest],
) -> &'a str {
    let javascript_only = !mutant.candidates.is_empty()
        && mutant.candidates.iter().all(|candidate| {
            tests
                .get(candidate.test)
                .is_some_and(|test| WASM_JAVASCRIPT_RUNNERS.contains(&test.runner.as_str()))
        });
    if mutant.language == crate::mutant_list::RUST && javascript_only {
        crate::mutant_list::JAVASCRIPT
    } else {
        &mutant.language
    }
}

/// Whether the driver of `language` (as `driver_of` names it) runs tests
/// under `runner`.
pub fn drives(language: &str, runner: &str) -> bool {
    match language {
        crate::mutant_list::JAVASCRIPT => {
            STRONG_RUNNERS.contains(&runner)
                && !matches!(runner, "rust-libtest" | "nextest" | "rustdoc")
        }
        crate::mutant_list::RUST => matches!(runner, "rust-libtest" | "nextest" | "rustdoc"),
        _ => false,
    }
}

/// Whether `language` has a strong driver.
pub fn has_driver(language: &str) -> bool {
    matches!(
        language,
        crate::mutant_list::JAVASCRIPT | crate::mutant_list::RUST
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
            // Interrupted: the signal's exit code, as the command's would be,
            // so the caller keeps what the pass decided and stops.
            match crate::javascript_run::run_strong_javascript(&request, diagnostics) {
                Err(crate::javascript_run::DirectJavascriptRunError::Interrupted {
                    exit_code,
                    ..
                }) => Ok(exit_code),
                finished => finished.map_err(|error| error.to_string()),
            }
        }
        crate::mutant_list::RUST => crate::rust_strong::run_strong_rust(
            &crate::rust_strong::StrongRustRequest {
                root: pass.root.clone(),
                command: pass.command.clone(),
                plan: pass.strong.plan.clone(),
                results: pass.strong.results.clone(),
                workers: crate::rust_strong::workers(),
            },
            diagnostics,
        ),
        other => Err(format!("no strong mutation driver for {other} yet")),
    }
}

/// The directory a pass over run `run_id` works in, under `root`.
pub fn work_directory(root: &Path, run_id: &str, nonce: u128) -> PathBuf {
    root.join(".supercov/strong")
        .join(format!("{run_id}-{nonce}"))
}

/// The file in a pass's directory naming the process that works there.
const OWNER_FILE: &str = "owner";

/// Makes the directory a pass works in, owned by this process.
pub fn claim_work_directory(work: &Path) -> std::io::Result<()> {
    crate::fault_fs::create_dir_all(work)?;
    crate::fault_fs::write(work.join(OWNER_FILE), std::process::id().to_string())
}

/// What a finished pass keeps: its logs (the command's output, a Playwright
/// pass's further invocations'). Its plan, results and each job's evidence
/// are gone once its verdicts are saved into the run; a pass over nanoid's
/// node:test suite left 1.2 GB.
pub fn finish_work_directory(work: &Path) {
    let Ok(entries) = std::fs::read_dir(work) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_some_and(|extension| extension == "log") {
            continue;
        }
        let _ = if path.is_dir() {
            std::fs::remove_dir_all(&path)
        } else {
            std::fs::remove_file(&path)
        };
    }
}

/// Removes the directories of passes whose process has ended, in whatever
/// way it ended: a pass that was killed left its whole directory. A pass of
/// another process still running keeps its own.
pub fn prune_work_directories(root: &Path) {
    let Ok(entries) = std::fs::read_dir(root.join(".supercov/strong")) else {
        return;
    };
    for entry in entries.flatten() {
        let owner = std::fs::read_to_string(entry.path().join(OWNER_FILE))
            .ok()
            .and_then(|pid| pid.trim().parse::<u32>().ok());
        match owner {
            Some(pid) if pid == std::process::id() || crate::lifecycle::process_alive(pid) => {}
            _ => {
                let _ = std::fs::remove_dir_all(entry.path());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A Rust mutant all of whose candidates are JavaScript tests run in
    /// Node (Rust compiled to wasm that they load) is the JavaScript
    /// driver's; one with a Rust or browser test among them, or none, is
    /// Rust's, and another language's mutant its own.
    #[test]
    fn rust_mutants_under_javascript_tests_are_the_javascript_driver_s() {
        use crate::strong_plan::{Candidate, CandidateReason, PlannedMutant, PlannedTest};
        let tests = [
            "vitest",
            "jest",
            "rust-libtest",
            "pytest",
            "vitest-browser",
            "playwright",
        ]
        .map(|runner| PlannedTest {
            id: runner.into(),
            file: None,
            name: runner.into(),
            title: None,
            runner: runner.into(),
            duration_ms: None,
            span: None,
            runnable: true,
        })
        .to_vec();
        let mutant = |language: &str, candidates: &[usize]| PlannedMutant {
            id: "m".into(),
            file: "src/lib.rs".into(),
            language: language.into(),
            index: 0,
            mutator: "FnValue".into(),
            weak: "unknown",
            decided: None,
            reason: None,
            candidates: candidates
                .iter()
                .map(|test| Candidate {
                    test: *test,
                    reason: CandidateReason::Infected,
                })
                .collect(),
            whole_files: Vec::new(),
            server_wide: None,
            covering: 0,
            statement: None,
            function: None,
            function_hit_by: Default::default(),
        };
        let rust = crate::mutant_list::RUST;
        let javascript = crate::mutant_list::JAVASCRIPT;
        assert_eq!(driver_of(&mutant(rust, &[0, 1]), &tests), javascript);
        assert_eq!(driver_of(&mutant(rust, &[0, 2]), &tests), rust);
        // A runner no driver runs, a test the plan does not have, or none.
        assert_eq!(driver_of(&mutant(rust, &[0, 3]), &tests), rust);
        // Playwright's pages get the mutant; Browser Mode's do not yet.
        assert_eq!(driver_of(&mutant(rust, &[0, 5]), &tests), javascript);
        assert_eq!(driver_of(&mutant(rust, &[0, 4]), &tests), rust);
        assert_eq!(driver_of(&mutant(rust, &[0, 9]), &tests), rust);
        assert_eq!(driver_of(&mutant(rust, &[]), &tests), rust);
        assert_eq!(driver_of(&mutant(javascript, &[2]), &tests), javascript);
        // Which runners each driver runs.
        assert!(drives(javascript, "jest") && !drives(javascript, "nextest"));
        assert!(drives(rust, "nextest") && !drives(rust, "jest"));
        assert!(!drives("python", "pytest"));
    }

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

    /// A finished pass keeps its logs alone; a later pass removes every
    /// pass's directory whose process has ended, or that names none, and
    /// keeps its own and a running process's.
    #[cfg(unix)]
    #[test]
    fn a_pass_s_directory_keeps_its_logs_and_goes_when_its_process_has() {
        let root = std::env::temp_dir().join(format!(
            "supercov-strong-work-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let work = work_directory(&root, "run_1", 7);
        claim_work_directory(&work).unwrap();
        std::fs::write(work.join("restart-0.log"), "said").unwrap();
        std::fs::write(work.join("plan.json"), "{}").unwrap();
        std::fs::create_dir_all(work.join("results.jsonl.jobs/1/evidence")).unwrap();
        finish_work_directory(&work);
        let mut left = std::fs::read_dir(&work)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect::<Vec<_>>();
        left.sort();
        assert_eq!(left, ["restart-0.log"]);
        // Gone already: nothing to finish.
        finish_work_directory(&root.join("gone"));
        // Another process's: one that runs (this test's parent, say), one
        // that ended, one that named none, one that named nonsense.
        let strong = root.join(".supercov/strong");
        let owned = |name: &str, owner: Option<String>| {
            std::fs::create_dir_all(strong.join(name)).unwrap();
            if let Some(owner) = owner {
                std::fs::write(strong.join(name).join(OWNER_FILE), owner).unwrap();
            }
        };
        let mut ended = std::process::Command::new("true").spawn().unwrap();
        let ended_pid = ended.id();
        ended.wait().unwrap();
        owned(
            "running",
            Some(std::os::unix::process::parent_id().to_string()),
        );
        owned("ended", Some(ended_pid.to_string()));
        owned("unowned", None);
        // A pass of this process still at work.
        claim_work_directory(&work_directory(&root, "run_1", 8)).unwrap();
        owned("nonsense", Some("x".into()));
        std::fs::write(strong.join("a-file"), "").unwrap();
        prune_work_directories(&root);
        let mut kept = std::fs::read_dir(&strong)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect::<Vec<_>>();
        kept.sort();
        // The finished pass's own directory, its owner gone with the rest,
        // is removed by the pass after it.
        assert_eq!(kept, ["a-file", "run_1-8", "running"]);
        // A project with no passes yet.
        prune_work_directories(&root.join("none"));
        let _ = std::fs::remove_dir_all(&root);
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
