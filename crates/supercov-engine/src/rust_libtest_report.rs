//! One libtest report for a test binary, made of the reports of its tests.
//!
//! Supercov runs each test in a process of its own, so that what a test ran
//! is known to be that test's. libtest then writes one report for each
//! process: `running 1 test`, the test, `2 filtered out`. Printing those as
//! they came gave a failing suite one such block for each failure and no line
//! at all for a test that passed, where `cargo test` prints every test under
//! one heading, the failures together and one result. This puts the
//! processes' reports back into the one the binary would have written.

use std::fmt::Write as _;

/// What one test's process wrote.
pub(crate) struct CaseOutput<'a> {
    pub name: &'a str,
    pub exit: i32,
    pub stdout: &'a str,
    pub stderr: &'a str,
}

const BACKTRACE_NOTE: &str =
    "note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace\n";

#[derive(Default)]
struct Counts {
    passed: usize,
    failed: usize,
    ignored: usize,
    measured: usize,
    filtered: usize,
    seconds: f64,
}

/// libtest's own line for the test: `test name ... ok`, with whatever it adds
/// (`- should panic`, an ignore reason).
fn result_line(stdout: &str) -> Option<&str> {
    stdout
        .lines()
        .find(|line| line.starts_with("test ") && line.contains(" ... "))
}

/// What a failed test printed, under libtest's heading for it.
fn failure_detail(stdout: &str) -> Option<&str> {
    let start = stdout.find("\nfailures:\n\n")? + "\nfailures:\n\n".len();
    let end = stdout.rfind("\n\nfailures:\n")?;
    (start < end).then(|| &stdout[start..end])
}

fn counts(stdout: &str) -> Option<Counts> {
    let line = stdout
        .lines()
        .rev()
        .find(|line| line.starts_with("test result: "))?;
    let (_, rest) = line.split_once(". ")?;
    let mut counts = Counts::default();
    for part in rest.split("; ") {
        if let Some(seconds) = part.strip_prefix("finished in ") {
            counts.seconds = seconds.trim_end_matches('s').parse().ok()?;
            continue;
        }
        let (number, what) = part.split_once(' ')?;
        let number = number.parse::<usize>().ok()?;
        match what {
            "passed" => counts.passed = number,
            "failed" => counts.failed = number,
            "ignored" => counts.ignored = number,
            "measured" => counts.measured = number,
            "filtered out" => counts.filtered = number,
            _ => return None,
        }
    }
    Some(counts)
}

/// One report for the doctests of a package, of the reports rustdoc wrote for
/// it: one for each doctest where each ran alone, one for several where it
/// ran them together.
pub(crate) fn merged_report(heading: &str, reports: &[&str]) -> String {
    // One report is rustdoc's own, with whatever it says after the result
    // (how long the doctests took to compile, when it compiles them as one).
    if let [only] = reports {
        return format!("{heading}\n{only}");
    }
    let lines = reports
        .iter()
        .flat_map(|report| {
            report
                .lines()
                .take_while(|line| *line != "failures:")
                .filter(|line| line.starts_with("test ") && line.contains(" ... "))
        })
        .collect::<Vec<_>>();
    let mut report = format!(
        "{heading}\n\nrunning {} test{}\n",
        lines.len(),
        if lines.len() == 1 { "" } else { "s" }
    );
    let mut total = Counts::default();
    // Every pass saw the same doctests: the ones it ran and the ones it
    // filtered out.
    let mut all = lines.len();
    for text in reports {
        if let Some(read) = counts(text) {
            all = all.max(read.passed + read.failed + read.ignored + read.measured + read.filtered);
            total.passed += read.passed;
            total.failed += read.failed;
            total.ignored += read.ignored;
            total.measured += read.measured;
            total.seconds += read.seconds;
        }
    }
    for line in &lines {
        let _ = writeln!(report, "{line}");
    }
    let mut failed = lines
        .iter()
        .filter_map(|line| line.strip_suffix(" ... FAILED"))
        .filter_map(|line| line.strip_prefix("test "))
        .collect::<Vec<_>>();
    if !failed.is_empty() {
        report.push_str("\nfailures:\n\n");
        // Each doctest is a program of its own, with its own note.
        for detail in reports.iter().filter_map(|text| failure_detail(text)) {
            report.push_str(detail);
            report.push('\n');
        }
        report.push_str("\nfailures:\n");
        failed.sort_unstable();
        for name in &failed {
            let _ = writeln!(report, "    {name}");
        }
    }
    let _ = writeln!(
        report,
        "\ntest result: {}. {} passed; {} failed; {} ignored; {} measured; {} filtered out; finished in {:.2}s\n",
        if failed.is_empty() { "ok" } else { "FAILED" },
        total.passed,
        total.failed,
        total.ignored,
        total.measured,
        all - lines.len(),
        total.seconds
    );
    report
}

/// The names a report says it ran.
pub(crate) fn reported_tests(report: &str) -> Vec<&str> {
    report
        .lines()
        .take_while(|line| *line != "failures:")
        .filter_map(|line| line.strip_prefix("test "))
        .filter_map(|line| line.split_once(" ... "))
        .map(|(name, _)| name.trim_end_matches(" - should panic"))
        .collect()
}

/// What a binary reports when none of its tests were asked for.
pub(crate) fn unasked_report(tests: usize) -> String {
    format!(
        "\nrunning 0 tests\n\ntest result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; {tests} filtered out; finished in 0.00s\n\n"
    )
}

/// libtest writes a progress count and a new line after this many results on
/// a terse line.
const TERSE_LINE: usize = 88;

/// The report of one binary. `quiet` is libtest's terse form: a character for
/// each test, and a failed test's name on a line of its own.
pub(crate) fn libtest_report(cases: &[CaseOutput<'_>], quiet: bool) -> String {
    let mut report = format!(
        "\nrunning {} test{}\n",
        cases.len(),
        if cases.len() == 1 { "" } else { "s" }
    );
    let mut total = Counts::default();
    let mut filtered = None::<usize>;
    let mut details = Vec::new();
    let mut failed = Vec::new();
    let mut column = 0_usize;
    let mut open = false;
    for (reported, case) in cases.iter().enumerate() {
        let line = result_line(case.stdout);
        let read = counts(case.stdout);
        let lost = case.exit != 0 && (line.is_none() || read.is_none());
        if let Some(read) = &read {
            total.passed += read.passed;
            total.ignored += read.ignored;
            total.measured += read.measured;
            total.seconds += read.seconds;
            filtered = Some(filtered.map_or(read.filtered, |seen| seen.max(read.filtered)));
        } else if case.exit == 0 {
            total.passed += 1;
        }
        if case.exit != 0 {
            total.failed += 1;
            failed.push(case.name);
            // A process that died before libtest could report -- an abort, a
            // stack overflow -- is shown as it ended.
            let detail = match failure_detail(case.stdout) {
                Some(detail) if !lost => format!("{detail}{}", case.stderr),
                _ => format!("---- {} ----\n{}{}", case.name, case.stdout, case.stderr),
            };
            // One process prints the note once; so does the report.
            details.push(if details.is_empty() {
                detail
            } else {
                detail.replacen(BACKTRACE_NOTE, "", 1)
            });
        }
        let failed_here = case.exit != 0;
        if !quiet {
            match line {
                Some(line) if !lost => {
                    let _ = writeln!(report, "{line}");
                }
                _ => {
                    let _ = writeln!(
                        report,
                        "test {} ... {}",
                        case.name,
                        if failed_here { "FAILED" } else { "ok" }
                    );
                }
            }
        } else if failed_here {
            if open {
                let _ = writeln!(report, " {reported}/{}", cases.len());
            }
            let _ = writeln!(report, "{} --- FAILED", case.name);
            open = false;
        } else {
            report.push(if line.is_some_and(|line| line.contains(" ... ignored")) {
                'i'
            } else {
                '.'
            });
            column += 1;
            open = column % TERSE_LINE != TERSE_LINE - 1;
            if !open {
                let _ = writeln!(report, " {}/{}", reported + 1, cases.len());
            }
        }
    }
    // A terse line is ended by what follows it.
    if !failed.is_empty() {
        report.push_str("\nfailures:\n\n");
        for detail in &details {
            report.push_str(detail);
            if !detail.ends_with('\n') {
                report.push('\n');
            }
            report.push('\n');
        }
        report.push_str("\nfailures:\n");
        failed.sort_unstable();
        for name in &failed {
            let _ = writeln!(report, "    {name}");
        }
    }
    // Each process filtered out every test but its own.
    let filtered = (filtered.unwrap_or(0) + 1).saturating_sub(cases.len());
    let _ = writeln!(
        report,
        "\ntest result: {}. {} passed; {} failed; {} ignored; {} measured; {} filtered out; finished in {:.2}s\n",
        if failed.is_empty() { "ok" } else { "FAILED" },
        total.passed,
        total.failed,
        total.ignored,
        total.measured,
        if cases.is_empty() { 0 } else { filtered },
        total.seconds
    );
    report
}

#[cfg(test)]
mod tests {
    use super::*;

    fn passed(name: &str) -> String {
        format!(
            "\nrunning 1 test\ntest {name} ... ok\n\ntest result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 2 filtered out; finished in 0.01s\n\n"
        )
    }

    fn panicked(name: &str, message: &str) -> String {
        format!(
            "\nrunning 1 test\ntest {name} ... FAILED\n\nfailures:\n\n---- {name} stdout ----\n\nthread '{name}' (7) panicked at src/lib.rs:8:9:\n{message}\n{BACKTRACE_NOTE}\n\nfailures:\n    {name}\n\ntest result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 2 filtered out; finished in 0.02s\n\n"
        )
    }

    #[test]
    fn three_processes_read_as_the_one_binary_cargo_runs() {
        // What `cargo test` prints for the same three tests, but for the
        // order of the two failures, which libtest takes as they finish.
        let seals = passed("tests::seals_a_body");
        let refuses = panicked("tests::refuses_a_short_key", "key too short");
        let compares = panicked("tests::compares", "assertion `left == right` failed");
        let report = libtest_report(
            &[
                CaseOutput {
                    name: "tests::seals_a_body",
                    exit: 0,
                    stdout: &seals,
                    stderr: "",
                },
                CaseOutput {
                    name: "tests::refuses_a_short_key",
                    exit: 101,
                    stdout: &refuses,
                    stderr: "",
                },
                CaseOutput {
                    name: "tests::compares",
                    exit: 101,
                    stdout: &compares,
                    stderr: "",
                },
            ],
            false,
        );
        assert_eq!(
            report,
            "
running 3 tests
test tests::seals_a_body ... ok
test tests::refuses_a_short_key ... FAILED
test tests::compares ... FAILED

failures:

---- tests::refuses_a_short_key stdout ----

thread 'tests::refuses_a_short_key' (7) panicked at src/lib.rs:8:9:
key too short
note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace

---- tests::compares stdout ----

thread 'tests::compares' (7) panicked at src/lib.rs:8:9:
assertion `left == right` failed


failures:
    tests::compares
    tests::refuses_a_short_key

test result: FAILED. 1 passed; 2 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.05s

"
        );
    }

    #[test]
    fn a_suite_that_passes_lists_its_tests() {
        // It printed nothing at all for them.
        let first = passed("a");
        let second = "\nrunning 1 test\ntest b ... ignored, slow\n\ntest result: ok. 0 passed; 0 failed; 1 ignored; 0 measured; 4 filtered out; finished in 0.00s\n\n";
        let cases = [
            CaseOutput {
                name: "a",
                exit: 0,
                stdout: &first,
                stderr: "",
            },
            CaseOutput {
                name: "b",
                exit: 0,
                stdout: second,
                stderr: "",
            },
        ];
        let report = libtest_report(&cases, false);
        assert!(report.contains("running 2 tests\ntest a ... ok\ntest b ... ignored, slow\n"));
        // Five tests in the binary, two asked for.
        assert!(report.ends_with(
            "test result: ok. 1 passed; 0 failed; 1 ignored; 0 measured; 3 filtered out; finished in 0.01s\n\n"
        ));
        // libtest's terse line is ended by the result.
        let terse = libtest_report(&cases, true);
        assert!(
            terse.contains("running 2 tests\n.i\ntest result: ok."),
            "{terse}"
        );
    }

    #[test]
    fn the_doctests_of_a_package_read_as_one_report() {
        let passed = "\nrunning 1 test\ntest src/lib.rs - seal (line 3) ... ok\n\ntest result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 1 filtered out; finished in 0.20s\n\n";
        let failed = format!(
            "\nrunning 1 test\ntest src/lib.rs - seal (line 7) ... FAILED\n\nfailures:\n\n---- src/lib.rs - seal (line 7) stdout ----\nTest executable failed (exit status: 101).\n\nstderr:\n\nthread 'main' (7) panicked at src/lib.rs:5:1:\nassertion `left == right` failed\n{BACKTRACE_NOTE}\n\n\nfailures:\n    src/lib.rs - seal (line 7)\n\ntest result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 1 filtered out; finished in 0.30s\n\n"
        );
        assert_eq!(reported_tests(&failed), ["src/lib.rs - seal (line 7)"]);
        assert_eq!(
            merged_report("   Doc-tests shop", &[passed, &failed]),
            "   Doc-tests shop

running 2 tests
test src/lib.rs - seal (line 3) ... ok
test src/lib.rs - seal (line 7) ... FAILED

failures:

---- src/lib.rs - seal (line 7) stdout ----
Test executable failed (exit status: 101).

stderr:

thread 'main' (7) panicked at src/lib.rs:5:1:
assertion `left == right` failed
note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace



failures:
    src/lib.rs - seal (line 7)

test result: FAILED. 1 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.50s

"
        );
    }

    #[test]
    fn a_process_that_died_is_shown_as_it_ended() {
        let report = libtest_report(
            &[CaseOutput {
                name: "tests::overflows",
                exit: 134,
                stdout: "\nrunning 1 test\n",
                stderr: "thread 'tests::overflows' has overflowed its stack\n",
            }],
            false,
        );
        assert!(report.contains("test tests::overflows ... FAILED\n"));
        assert!(report.contains(
            "---- tests::overflows ----\n\nrunning 1 test\nthread 'tests::overflows' has overflowed its stack\n"
        ));
        assert!(report.contains("test result: FAILED. 0 passed; 1 failed;"));
    }
}
