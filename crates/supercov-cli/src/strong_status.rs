//! What a running strong pass is doing, for its progress lines: the
//! Playwright driver's invocations each write a status file
//! (`playwrightStrongReporter.mjs`: the round, the attempts so far, the tests
//! running and since when), and the command's output goes to a log beside
//! the pass's results. A pass that goes quiet says what it waits on. Each
//! invocation also counts the work its tests have left (how many mutants
//! wait for each, at its baseline), so the lines name the costliest tests
//! and the time the round has left.

use std::path::Path;

use serde_json::Value;

/// How long nothing may end before the pass says so.
const QUIET_MS: u64 = 120_000;

/// The running tests a line names at most.
const RUNNING_SHOWN: usize = 3;

/// The costliest tests left the lines name at most.
const COSTLY_SHOWN: usize = 3;

/// The invocations of the latest round, together.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct DriverStatus {
    pub(crate) round: String,
    pub(crate) invocations: usize,
    pub(crate) baselines: u64,
    pub(crate) controls: u64,
    pub(crate) failed_controls: u64,
    pub(crate) attempts: u64,
    /// When the first attempt with a mutant began (ms since the epoch).
    pub(crate) first_mutant_at: Option<u64>,
    /// When the round's last invocation began, or an attempt last ended.
    pub(crate) last_event_at: u64,
    pub(crate) running: Vec<Running>,
    /// The time the round's work left takes: its slowest invocation's, each
    /// at its own pace.
    pub(crate) left_ms: Option<u64>,
    /// The tests with the most work left, the costliest first.
    pub(crate) costly: Vec<Costly>,
}

/// A test with mutants waiting for it.
#[derive(Debug, PartialEq)]
pub(crate) struct Costly {
    pub(crate) title: String,
    /// How many mutants wait for it.
    pub(crate) waiting: u64,
    /// How long it takes without a mutant, and with one at most.
    pub(crate) baseline_ms: u64,
    pub(crate) limit_ms: u64,
}

impl Costly {
    fn work(&self) -> u64 {
        self.waiting.saturating_mul(self.baseline_ms)
    }
}

/// A test running now.
#[derive(Debug, PartialEq)]
pub(crate) struct Running {
    pub(crate) title: String,
    /// Its mutant, or what it runs without one (`baseline`, `control`).
    pub(crate) what: String,
    pub(crate) since: u64,
    /// A mutant's time limit (ms from `since`), past which it is a timeout.
    pub(crate) limit: Option<u64>,
}

fn number(value: &Value) -> u64 {
    value.as_u64().unwrap_or(0)
}

/// The status files under `directory` (`<work>/playwright/status`), the
/// latest round's together; `None` before any invocation wrote one.
pub(crate) fn read(directory: &Path) -> Option<DriverStatus> {
    let files = std::fs::read_dir(directory)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| std::fs::read(entry.path()).ok())
        .filter_map(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
        .collect::<Vec<_>>();
    let latest = files.iter().max_by_key(|file| number(&file["updatedAt"]))?;
    let round = latest["round"].as_str().unwrap_or("main").to_owned();
    let mut status = DriverStatus {
        round,
        ..DriverStatus::default()
    };
    // The latest's round has one file at least, the latest.
    files
        .iter()
        .filter(|file| file["round"].as_str().unwrap_or("main") == status.round)
        .for_each(|file| {
            let counts = &file["counts"];
            status.invocations += 1;
            status.baselines += number(&counts["baselines"]);
            status.controls += number(&counts["controls"]);
            status.failed_controls += number(&counts["failedControls"]);
            status.attempts += number(&counts["attempts"]);
            if let Some(at) = file["firstMutantAt"].as_u64() {
                status.first_mutant_at =
                    Some(status.first_mutant_at.map_or(at, |first| first.min(at)));
            }
            let event = number(&file["lastEndedAt"]).max(number(&file["startedAt"]));
            status.last_event_at = status.last_event_at.max(event);
            let queued = &file["queued"];
            if let Some(left) = queued["leftMs"].as_u64() {
                status.left_ms = Some(status.left_ms.map_or(left, |most| most.max(left)));
            }
            for test in queued["tests"].as_array().into_iter().flatten() {
                status.costly.push(Costly {
                    title: test["title"].as_str().unwrap_or("?").to_owned(),
                    waiting: number(&test["waiting"]),
                    baseline_ms: number(&test["baselineMs"]),
                    limit_ms: number(&test["limitMs"]),
                });
            }
            for test in file["running"].as_array().into_iter().flatten() {
                status.running.push(Running {
                    title: test["title"].as_str().unwrap_or("?").to_owned(),
                    what: test["mutant"]
                        .as_str()
                        .or_else(|| test["kind"].as_str())
                        .unwrap_or("starting")
                        .to_owned(),
                    since: number(&test["startedAt"]).max(number(&test["begunAt"])),
                    limit: test["limitMs"].as_u64(),
                });
            }
        });
    status.running.sort_by_key(|test| test.since);
    // A test several invocations run is named once, with its most work.
    status
        .costly
        .sort_by(|a, b| b.work().cmp(&a.work()).then_with(|| a.title.cmp(&b.title)));
    let mut named = std::collections::BTreeSet::new();
    status
        .costly
        .retain(|test| named.insert(test.title.clone()));
    status.costly.truncate(COSTLY_SHOWN);
    Some(status)
}

/// A span of time as the progress line says it.
fn ago(milliseconds: u64) -> String {
    supercov_engine::process_supervision::format_duration(u128::from(milliseconds))
}

/// The lines that follow the progress line: what the latest round is doing
/// and, where nothing ended for a while, what it waits on; before any
/// invocation of Playwright began, or for a driver that writes no status,
/// the command's last line while nothing is decided.
pub(crate) fn lines(
    status: Option<&DriverStatus>,
    now: u64,
    decided: usize,
    last_line: Option<&str>,
) -> Vec<String> {
    let said = |line: Option<&str>| {
        line.map_or_else(|| "nothing yet".to_owned(), |line| format!("{line:?}"))
    };
    let Some(status) = status else {
        if decided > 0 {
            return Vec::new();
        }
        return vec![format!(
            "[supercov]   starting: the command's last line: {}",
            said(last_line)
        )];
    };
    let mut lines = vec![format!(
        "[supercov]   {} round, {} invocation(s): {} baseline(s), {} mutant attempt(s), {} control(s){}",
        status.round,
        status.invocations,
        status.baselines,
        status.attempts,
        status.controls,
        if status.failed_controls > 0 {
            format!(" ({} failed without a mutant)", status.failed_controls)
        } else {
            String::new()
        },
    )];
    // The tests whose runs left take longest: a slow test many mutants
    // still wait for is what the round's end waits on.
    for test in &status.costly {
        lines.push(format!(
            "[supercov]   costliest test left: {:?}, {} mutant(s) wait for it, {} a run (its limit {})",
            test.title,
            test.waiting,
            ago(test.baseline_ms),
            ago(test.limit_ms)
        ));
    }
    for test in status.running.iter().take(RUNNING_SHOWN) {
        let running = now.saturating_sub(test.since);
        // A mutant's attempt says its limit, and when it nears it: a test
        // that waits for what the mutant broke ends there, a timeout.
        let limit = match test.limit {
            Some(limit) if running.saturating_mul(4) >= limit.saturating_mul(3) => {
                format!(" of {}, near its time limit", ago(limit))
            }
            Some(limit) => format!(" of {}", ago(limit)),
            None => String::new(),
        };
        lines.push(format!(
            "[supercov]   running for {}{limit}: {:?} with {}",
            ago(running),
            test.title,
            test.what
        ));
    }
    let quiet = now.saturating_sub(status.last_event_at);
    if quiet >= QUIET_MS {
        lines.push(if status.running.is_empty() {
            format!(
                "[supercov]   nothing has ended for {} and no test is running: the command's last line: {}",
                ago(quiet),
                said(last_line)
            )
        } else {
            format!(
                "[supercov]   nothing has ended for {}: a test runs until its time is up (a mutant's is its test's baseline times the timeout factor, + 5 s)",
                ago(quiet)
            )
        });
    }
    lines
}

/// The last line the command printed: the newest log beside the pass's
/// results (the first invocation's, or a later round's).
pub(crate) fn last_output_line(work: &Path) -> Option<String> {
    let newest = std::fs::read_dir(work)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|extension| extension == "log"))
        .filter_map(|path| {
            std::fs::metadata(&path)
                .and_then(|metadata| metadata.modified())
                .ok()
                .map(|modified| (modified, path))
        })
        .max()?
        .1;
    let bytes = std::fs::read(newest).ok()?;
    let tail = &bytes[bytes.len().saturating_sub(4_096)..];
    let text = String::from_utf8_lossy(tail);
    let line = text
        .lines()
        .rev()
        .map(strip_ansi)
        .find(|line| !line.trim().is_empty())?;
    let line = line.trim();
    Some(if line.chars().count() > 160 {
        format!("{}…", line.chars().take(159).collect::<String>())
    } else {
        line.to_owned()
    })
}

/// A line without its terminal colours.
fn strip_ansi(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars();
    while let Some(character) = chars.next() {
        if character == '\u{1b}' {
            // `ESC [ ... <letter>`
            for code in chars.by_ref() {
                if code.is_ascii_alphabetic() {
                    break;
                }
            }
            continue;
        }
        out.push(character);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!(
            "supercov-strong-status-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    /// The latest round's invocations are added up; an earlier round's
    /// file, and one that is not JSON, are left out.
    #[test]
    fn the_latest_round_s_invocations_are_read_together() {
        let directory = scratch("read");
        assert_eq!(read(&directory.join("none")), None);
        assert_eq!(read(&directory), None);
        let write = |name: &str, value: Value| {
            std::fs::write(directory.join(name), value.to_string()).unwrap();
        };
        write(
            "1.json",
            serde_json::json!({ "round": "main", "updatedAt": 10, "startedAt": 5,
                "counts": { "baselines": 9, "attempts": 90, "controls": 9, "failedControls": 1 },
                "firstMutantAt": 7, "lastEndedAt": 9, "running": [] }),
        );
        write(
            "2.json",
            serde_json::json!({ "round": "restart-0", "updatedAt": 30, "startedAt": 20,
                "counts": { "baselines": 0, "attempts": 4, "controls": 2, "failedControls": 0 },
                "firstMutantAt": 25, "lastEndedAt": 28,
                "running": [{ "title": "pays", "mutant": "a.ts:1", "startedAt": 26, "begunAt": 25, "limitMs": 9500 }],
                "queued": { "ms": 90_000, "leftMs": 120_000, "tests": [
                    { "title": "polls", "waiting": 9, "baselineMs": 8_000, "limitMs": 17_000 },
                    { "title": "lists", "waiting": 4, "baselineMs": 1_000, "limitMs": 6_500 },
                    { "title": "opens", "waiting": 2, "baselineMs": 2_000, "limitMs": 8_000 },
                ] } }),
        );
        write(
            "3.json",
            serde_json::json!({ "round": "restart-0", "updatedAt": 29, "startedAt": 21,
                "counts": { "attempts": 1 }, "firstMutantAt": 22,
                "running": [{ "title": "lists", "kind": "control", "begunAt": 24 }, { "begunAt": 27 }],
                "queued": { "ms": 6_000, "leftMs": 7_000, "tests": [
                    { "title": "lists", "waiting": 6, "baselineMs": 1_000, "limitMs": 6_500 },
                    { "waiting": 1 },
                ] } }),
        );
        write("4.json.draft", "{".into());
        let status = read(&directory).unwrap();
        assert_eq!(
            status,
            DriverStatus {
                round: "restart-0".into(),
                invocations: 2,
                baselines: 0,
                controls: 2,
                failed_controls: 0,
                attempts: 5,
                first_mutant_at: Some(22),
                last_event_at: 28,
                left_ms: Some(120_000),
                // The costliest first; a test two invocations name once,
                // with its most work; three at most.
                costly: vec![
                    Costly {
                        title: "polls".into(),
                        waiting: 9,
                        baseline_ms: 8_000,
                        limit_ms: 17_000,
                    },
                    Costly {
                        title: "lists".into(),
                        waiting: 6,
                        baseline_ms: 1_000,
                        limit_ms: 6_500,
                    },
                    Costly {
                        title: "opens".into(),
                        waiting: 2,
                        baseline_ms: 2_000,
                        limit_ms: 8_000,
                    },
                ],
                running: vec![
                    Running {
                        title: "lists".into(),
                        what: "control".into(),
                        since: 24,
                        limit: None,
                    },
                    Running {
                        title: "pays".into(),
                        what: "a.ts:1".into(),
                        since: 26,
                        limit: Some(9_500),
                    },
                    Running {
                        title: "?".into(),
                        what: "starting".into(),
                        since: 27,
                        limit: None,
                    },
                ],
            }
        );
        // A file with no round is the main round's; one with no work counted
        // has no time left to say.
        write("5.json", serde_json::json!({ "updatedAt": 40 }));
        let main = read(&directory).unwrap();
        assert_eq!(main.round, "main");
        assert_eq!((main.left_ms, main.costly.len()), (None, 0));
        let _ = std::fs::remove_dir_all(&directory);
    }

    /// Before any invocation began the command's last line is said, until a
    /// verdict comes; then what the round does, what runs, and whether it
    /// went quiet.
    #[test]
    fn the_lines_say_what_the_pass_does_and_waits_on() {
        assert_eq!(
            lines(None, 0, 0, Some("[e2e] building")),
            vec!["[supercov]   starting: the command's last line: \"[e2e] building\""]
        );
        assert_eq!(
            lines(None, 0, 0, None),
            vec!["[supercov]   starting: the command's last line: nothing yet"]
        );
        assert!(lines(None, 0, 1, None).is_empty());
        let mut status = DriverStatus {
            round: "main".into(),
            invocations: 6,
            baselines: 100,
            controls: 3,
            failed_controls: 0,
            attempts: 400,
            first_mutant_at: Some(1),
            last_event_at: 100_000,
            left_ms: None,
            costly: Vec::new(),
            running: (0..4)
                .map(|at| Running {
                    title: format!("t{at}"),
                    what: "a.ts:1".into(),
                    since: 90_000,
                    limit: None,
                })
                .collect(),
        };
        let said = lines(Some(&status), 110_000, 3, None);
        assert_eq!(
            said,
            vec![
                "[supercov]   main round, 6 invocation(s): 100 baseline(s), 400 mutant attempt(s), 3 control(s)",
                "[supercov]   running for 20s: \"t0\" with a.ts:1",
                "[supercov]   running for 20s: \"t1\" with a.ts:1",
                "[supercov]   running for 20s: \"t2\" with a.ts:1",
            ]
        );
        // A mutant's attempt with its time limit, and one near it (three
        // quarters of it gone).
        status.running[0].limit = Some(80_000);
        status.running[1].limit = Some(26_000);
        status.running[2].limit = Some(27_000);
        let limited = lines(Some(&status), 110_000, 3, None);
        assert_eq!(
            limited[1..],
            [
                "[supercov]   running for 20s of 1m20s: \"t0\" with a.ts:1",
                "[supercov]   running for 20s of 26s, near its time limit: \"t1\" with a.ts:1",
                "[supercov]   running for 20s of 27s: \"t2\" with a.ts:1",
            ]
        );
        // The costliest tests left come before the running ones.
        status.costly.push(Costly {
            title: "polls".into(),
            waiting: 9,
            baseline_ms: 8_000,
            limit_ms: 17_000,
        });
        let costly = lines(Some(&status), 110_000, 3, None);
        assert_eq!(
            costly[1],
            "[supercov]   costliest test left: \"polls\", 9 mutant(s) wait for it, 8s a run (its limit 17s)"
        );
        assert_eq!(costly[2], limited[1]);
        status.costly.clear();
        status.failed_controls = 1;
        let quiet = lines(Some(&status), 230_000, 3, None);
        assert!(
            quiet[0].ends_with("3 control(s) (1 failed without a mutant)"),
            "{quiet:?}"
        );
        assert!(
            quiet[4].starts_with("[supercov]   nothing has ended for 2m10s: a test runs until"),
            "{quiet:?}"
        );
        status.running.clear();
        let stuck = lines(Some(&status), 230_000, 3, Some("waiting for the server"));
        assert_eq!(
            stuck[1],
            "[supercov]   nothing has ended for 2m10s and no test is running: the command's last line: \"waiting for the server\""
        );
    }

    /// The newest log's last line that says something, without its colours
    /// and cut short where it is long.
    #[test]
    fn the_last_line_is_the_newest_log_s() {
        let work = scratch("log");
        assert_eq!(last_output_line(&work), None);
        std::fs::write(work.join("results.jsonl"), "{}\n").unwrap();
        std::fs::write(work.join("results.log"), "first\n").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(
            work.join("restart-0.log"),
            "\u{1b}[32m[slot 1]\u{1b}[0m ready\n  \n",
        )
        .unwrap();
        assert_eq!(last_output_line(&work).as_deref(), Some("[slot 1] ready"));
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(work.join("restart-1.log"), "x".repeat(200)).unwrap();
        let long = last_output_line(&work).unwrap();
        assert_eq!(long.chars().count(), 160);
        assert!(long.ends_with('…'));
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(work.join("restart-2.log"), "\n\n").unwrap();
        assert_eq!(last_output_line(&work), None);
        // A colour code cut short at the line's end; a log that cannot be
        // read (a directory).
        assert_eq!(strip_ansi("ready\u{1b}"), "ready");
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::create_dir(work.join("restart-3.log")).unwrap();
        assert_eq!(last_output_line(&work), None);
        let _ = std::fs::remove_dir_all(&work);
    }
}
