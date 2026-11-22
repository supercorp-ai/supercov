//! A strong pass's run, around its driver: the command over the copy, then
//! each further invocation the driver asks for in its results.
//!
//! The command here is a shell script that writes the records a driver
//! writes, so no runner is needed: what is tested is what the engine does
//! with them. The script is a POSIX shell's, so the file is Unix only.
#![cfg(unix)]

use std::path::PathBuf;

use supercov_engine::javascript_run::{
    DirectJavascriptRunError, DirectJavascriptRunRequest, StrongRun, run_strong_javascript,
};

mod common;

/// A strong pass's restarts take signals to this process: one pass at a
/// time, so a signal one sends reaches only its own.
fn one_at_a_time() -> std::sync::MutexGuard<'static, ()> {
    static PASSES: std::sync::Mutex<()> = std::sync::Mutex::new(());
    PASSES
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// A project of one source, and beside it a watchdog that says it is ready
/// and leaves: the engine's tests have no Supercov binary to watch with.
struct Pass {
    directory: common::TestDirectory,
    strong: StrongRun,
}

impl Pass {
    fn new(label: &str) -> Self {
        static UNIQUE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let directory = std::env::temp_dir().join(format!(
            "supercov-strong-run-{label}-{}-{}",
            std::process::id(),
            UNIQUE.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(directory.join("project/src")).unwrap();
        let directory = common::TestDirectory::adopt(directory);
        std::fs::write(
            directory.join("project/package.json"),
            r#"{ "name": "strong", "type": "module" }"#,
        )
        .unwrap();
        std::fs::write(
            directory.join("project/src/a.js"),
            "export const a = (x) => x > 1;\n",
        )
        .unwrap();
        let watchdog = directory.join("watchdog");
        std::fs::write(&watchdog, "#!/bin/sh\nprintf '\\001' >&3\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&watchdog, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let strong = StrongRun {
            plan: directory.join("plan.json"),
            results: directory.join("results.jsonl"),
        };
        Self { directory, strong }
    }

    /// The pass, its command `sh -c script`.
    fn run(&self, script: &str) -> Result<i32, String> {
        let request = DirectJavascriptRunRequest {
            root: self.directory.join("project"),
            command: vec!["/bin/sh".into(), "-c".into(), script.into()],
            run_id: None,
            started_at: None,
            watchdog_program: Some(self.directory.join("watchdog")),
            strong: Some(self.strong.clone()),
        };
        run_strong_javascript(&request, &mut Vec::new()).map_err(|error| match error {
            DirectJavascriptRunError::Failed(message) => message,
            other => other.to_string(),
        })
    }

    fn results(&self) -> Vec<String> {
        std::fs::read_to_string(&self.strong.results)
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    fn restart(&self, number: usize) -> PathBuf {
        self.strong
            .results
            .with_file_name(format!("restart-{number}.json"))
    }
}

/// What the command writes when the driver asks for restarts, and what each
/// restart writes: the mutant it was started with switched on, if any.
const ASKS_TWICE: &str = r#"
if [ -n "$SUPERCOV_STRONG_RESTART" ]; then
  printf '{"ran":"%s"}\n' "$SUPERCOV_ACTIVE_MUTANT" >> "$SUPERCOV_STRONG_RESULTS"
  exit 0
fi
printf '%s\n' \
  '{"restart":{"everywhere":true,"mutants":[{"mutant":"m1"}],"kind":"alone","reason":"server"}}' \
  '{"restart":{"everywhere":true}}' >> "$SUPERCOV_STRONG_RESULTS"
"#;

/// Each restart the driver asks for runs once, in its order, with the
/// mutant it names switched on in every process where it says everywhere;
/// one that names none starts with none. A plan that does not read
/// switches every file.
#[test]
fn each_restart_the_driver_asks_for_runs_once_with_its_mutant() {
    let _one = one_at_a_time();
    let pass = Pass::new("restarts");
    assert_eq!(pass.run(ASKS_TWICE), Ok(0));
    let results = pass.results();
    assert_eq!(results[2..], [r#"{"ran":"m1"}"#, r#"{"ran":""}"#]);
    assert!(pass.restart(1).exists());
    assert!(!pass.restart(2).exists());
}

/// A driver that asks again in every restart is stopped at a thousand.
#[test]
fn a_driver_that_always_asks_again_is_stopped_at_a_thousand_restarts() {
    let _one = one_at_a_time();
    let pass = Pass::new("bound");
    let always = r#"printf '%s\n' '{"restart":{}}' >> "$SUPERCOV_STRONG_RESULTS""#;
    assert_eq!(pass.run(always), Ok(0));
    assert!(pass.restart(999).exists());
    assert!(!pass.restart(1_000).exists());
    assert_eq!(pass.results().len(), 1_001);
}

/// A restart interrupted ends the pass there: the restarts after it do not
/// run, and the mutants left have no verdict.
#[test]
fn an_interrupted_restart_ends_the_pass() {
    let _one = one_at_a_time();
    let pass = Pass::new("interrupted");
    let script = format!(
        r#"
if [ -n "$SUPERCOV_STRONG_RESTART" ]; then
  kill -TERM {}
  sleep 10
  exit 0
fi
printf '%s\n' '{{"restart":{{}}}}' '{{"restart":{{}}}}' >> "$SUPERCOV_STRONG_RESULTS"
"#,
        std::process::id()
    );
    assert_eq!(pass.run(&script), Ok(0));
    assert!(pass.restart(0).exists());
    assert!(!pass.restart(1).exists());
}

/// Where the list of files to switch, a restart's job, or the run's last
/// state cannot be written, or a restart cannot be started, the pass fails
/// saying so.
#[test]
fn a_pass_that_cannot_write_or_start_what_it_needs_fails() {
    let _one = one_at_a_time();
    let cases = [
        // The list of the plan's files, for the processes that instrument.
        ("fs.write#javascript_run.rs:988", "strong-files.json"),
        // A restart's job.
        ("fs.write#javascript_run.rs:2837", "restart-0.json"),
        // The run's state, read back to say it is complete.
        ("fs.read#javascript_run.rs:1250", "state.json"),
        // The first restart's command, after the pass's own.
        ("spawn#process_supervision.rs:1490@2", "/bin/sh"),
    ];
    for (fault, says) in cases {
        let pass = Pass::new("faults");
        std::fs::write(&pass.strong.plan, r#"{"mutants": [{"file": "src/a.js"}]}"#).unwrap();
        let failed = supercov_engine::fault::during(fault, || pass.run(ASKS_TWICE));
        assert!(
            failed.as_ref().is_err_and(|message| message.contains(says)),
            "{fault}: {failed:?}"
        );
    }
}
