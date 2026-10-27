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

/// A report as Cargo and libtest write one between them: Cargo's line for
/// the binary on standard error, and the binary's own report on standard
/// output. `cargo test > results.txt` holds the reports and not Cargo's
/// lines, and it does under Supercov.
pub(crate) struct Announced {
    pub heading: Option<String>,
    pub report: String,
}

/// Write each report where `cargo test` writes it.
pub(crate) fn write_reports(
    diagnostics: &mut dyn std::io::Write,
    reports: &[Announced],
) -> std::io::Result<()> {
    write_reports_to(diagnostics, &mut std::io::stdout().lock(), reports)
}

/// [`write_reports`], each report to `output`.
fn write_reports_to(
    diagnostics: &mut dyn std::io::Write,
    output: &mut dyn std::io::Write,
    reports: &[Announced],
) -> std::io::Result<()> {
    for announced in reports {
        if let Some(heading) = &announced.heading {
            writeln!(diagnostics, "{heading}")?;
            diagnostics.flush()?;
        }
        output.write_all(announced.report.as_bytes())?;
        output.flush()?;
    }
    Ok(())
}

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
fn result_line<'a>(stdout: &'a str, name: &str) -> Option<&'a str> {
    // The line for this test: a test that runs a suite of its own and passes
    // its output on (as Supercov's do) prints such lines for other tests.
    stdout.lines().find(|line| {
        line.strip_prefix("test ")
            .and_then(|rest| rest.strip_prefix(name))
            .is_some_and(|rest| rest.starts_with(" ... ") || rest.starts_with(" - "))
    })
}

/// What a failed test printed, under libtest's heading for it.
fn failure_detail(stdout: &str) -> Option<&str> {
    let start = stdout.find("\nfailures:\n\n")? + "\nfailures:\n\n".len();
    let end = stdout.rfind("\n\nfailures:\n")?;
    (start < end).then(|| &stdout[start..end])
}

/// Text without what a terminal reads as colour: on one, libtest writes
/// `ok` as `ESC[32mokESC(BESC[m`.
fn plain(text: &str) -> std::borrow::Cow<'_, str> {
    let Some(mut at) = text.find('\u{1b}') else {
        return text.into();
    };
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    loop {
        out.push_str(&rest[..at]);
        let sequence = &rest[at + 1..];
        let length = match sequence.as_bytes().first() {
            // A colour or a reset, ended by its letter.
            Some(b'[') => sequence
                .find(|next: char| next.is_ascii_alphabetic())
                .map_or(sequence.len(), |end| end + 1),
            // The character set a reset also names.
            Some(b'(') => 2.min(sequence.len()),
            _ => 0,
        };
        rest = &sequence[length..];
        match rest.find('\u{1b}') {
            Some(next) => at = next,
            None => break,
        }
    }
    out.push_str(rest);
    out.into()
}

/// The colour a word was written in: what stands before it and after it.
fn colour_of(written: &str) -> (&str, &str) {
    let word = plain(written);
    match written.find(word.as_ref()) {
        Some(at) if !word.is_empty() => (&written[..at], &written[at + word.len()..]),
        _ => ("", ""),
    }
}

fn counts(stdout: &str) -> Option<Counts> {
    let line = stdout
        .lines()
        .rev()
        .find(|line| line.starts_with("test result: "))?;
    let line = plain(line);
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
pub(crate) fn merged_report(reports: &[&str], quiet: bool, known: &[(&str, &str)]) -> String {
    // One report is rustdoc's own, with whatever it says after the result
    // (how long the doctests took to compile, when it compiles them as one).
    if let ([only], false) = (reports, quiet) {
        return (*only).to_owned();
    }
    let mut lines = reports
        .iter()
        .flat_map(|report| {
            report
                .lines()
                .take_while(|line| *line != "failures:")
                .filter(|line| line.starts_with("test ") && line.contains(" ... "))
                .map(str::to_owned)
        })
        .collect::<Vec<_>>();
    // A quiet harness prints a mark per case and no name, and a merged one
    // cannot be asked for them; the cases it ran are known all the same.
    if lines.is_empty() && quiet {
        lines = known
            .iter()
            .map(|(name, status)| {
                let result = match *status {
                    "failed" => "FAILED",
                    "skipped" => "ignored",
                    _ => "ok",
                };
                format!("test {name} ... {result}")
            })
            .collect();
    }
    let mut report = format!(
        "\nrunning {} test{}\n",
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
    if quiet {
        report.push_str(&terse_lines(lines.iter().map(|line| {
            let (name, result) = line["test ".len()..]
                .split_once(" ... ")
                .unwrap_or((line, ""));
            (
                name.trim_end_matches(" - should panic"),
                match result {
                    "FAILED" => 'F',
                    ignored if ignored.starts_with("ignored") => 'i',
                    _ => '.',
                },
                ("", ""),
            )
        })));
    } else {
        for line in &lines {
            let _ = writeln!(report, "{line}");
        }
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
        failed.iter().for_each(|name| {
            let _ = writeln!(report, "    {name}");
        });
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

/// libtest's terse form of a run: a character for each test, and a failed
/// test's name on a line of its own. What follows ends the last line.
fn terse_lines<'a>(
    results: impl ExactSizeIterator<Item = (&'a str, char, (&'a str, &'a str))>,
) -> String {
    let total = results.len();
    let mut out = String::new();
    let mut column = 0_usize;
    let mut open = false;
    for (reported, (name, result, (before, after))) in results.enumerate() {
        if result == 'F' {
            if open {
                let _ = writeln!(out, " {reported}/{total}");
            }
            let _ = writeln!(out, "{name} --- {before}FAILED{after}");
            open = false;
        } else {
            let _ = write!(out, "{before}{result}{after}");
            column += 1;
            open = column % TERSE_LINE != TERSE_LINE - 1;
            if !open {
                let _ = writeln!(out, " {}/{total}", reported + 1);
            }
        }
    }
    out
}

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
    let mut results = Vec::new();
    for case in cases {
        let line = result_line(case.stdout, case.name);
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
        if quiet {
            results.push((
                case.name,
                if failed_here {
                    'F'
                } else if line.is_some_and(|line| plain(line).contains(" ... ignored")) {
                    'i'
                } else {
                    '.'
                },
                line.and_then(|line| line.split_once(" ... "))
                    .map_or(("", ""), |(_, result)| colour_of(result)),
            ));
            continue;
        }
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
    }
    report.push_str(&terse_lines(results.into_iter()));
    // A terse line is ended by what follows it.
    if !failed.is_empty() {
        report.push_str("\nfailures:\n\n");
        // A failed case left its detail.
        details.iter().for_each(|detail| {
            report.push_str(detail);
            if !detail.ends_with('\n') {
                report.push('\n');
            }
            report.push('\n');
        });
        report.push_str("\nfailures:\n");
        failed.sort_unstable();
        failed.iter().for_each(|name| {
            let _ = writeln!(report, "    {name}");
        });
    }
    // Each process filtered out every test but its own.
    let filtered = (filtered.unwrap_or(0) + 1).saturating_sub(cases.len());
    // The word as a process wrote it: in its colour, on a terminal.
    let word = if failed.is_empty() { "ok" } else { "FAILED" };
    let word = cases
        .iter()
        .filter_map(|case| {
            let line = case
                .stdout
                .lines()
                .rev()
                .find(|line| line.starts_with("test result: "))?;
            let end = line.find(". ")?;
            Some(&line["test result: ".len()..end])
        })
        .find(|written| plain(written) == word)
        .unwrap_or(word);
    let _ = writeln!(
        report,
        "\ntest result: {}. {} passed; {} failed; {} ignored; {} measured; {} filtered out; finished in {:.2}s\n",
        word,
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

    /// No report at all is a run of no doctest.
    /// A stream that takes `writes` writes and then refuses them, and
    /// refuses to flush when `flushes` is false.
    struct Refusing {
        writes: usize,
        flushes: bool,
    }

    impl std::io::Write for Refusing {
        fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
            if self.writes == 0 {
                return Err(std::io::Error::other("closed"));
            }
            self.writes -= 1;
            Ok(buffer.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            if self.flushes {
                Ok(())
            } else {
                Err(std::io::Error::other("closed"))
            }
        }
    }

    /// A report whose heading or text cannot be written, or not flushed,
    /// says so rather than going on.
    #[test]
    fn a_report_that_cannot_be_written_says_so() {
        let reports = [Announced {
            heading: Some("     Running tests".into()),
            report: "ok\n".into(),
        }];
        let write = |diagnostics: Refusing, output: Refusing| {
            let (mut diagnostics, mut output) = (diagnostics, output);
            write_reports_to(&mut diagnostics, &mut output, &reports).is_ok()
        };
        let fine = || Refusing {
            writes: usize::MAX,
            flushes: true,
        };
        assert!(write(fine(), fine()));
        assert!(!write(
            Refusing {
                writes: 0,
                flushes: true
            },
            fine()
        ));
        assert!(!write(
            Refusing {
                writes: usize::MAX,
                flushes: false
            },
            fine()
        ));
        assert!(!write(
            fine(),
            Refusing {
                writes: 0,
                flushes: true
            }
        ));
        assert!(!write(
            fine(),
            Refusing {
                writes: usize::MAX,
                flushes: false
            }
        ));
    }

    #[test]
    fn no_report_merges_into_a_run_of_nothing() {
        let report = merged_report(&[], false, &[]);
        assert!(report.starts_with("\nrunning 0 tests\n"), "{report}");
        assert!(
            report.contains("test result: ok. 0 passed; 0 failed"),
            "{report}"
        );
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
            merged_report(&[passed, &failed], false, &[]),
            "
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
    fn a_report_keeps_the_colours_its_processes_wrote() {
        // On a terminal `cargo test` writes `ok` in green and `FAILED` in
        // red, and the processes are asked for the same.
        let green = "\nrunning 1 test\ntest a ... \u{1b}[32mok\u{1b}(B\u{1b}[m\n\ntest result: \u{1b}[32mok\u{1b}(B\u{1b}[m. 1 passed; 0 failed; 0 ignored; 0 measured; 1 filtered out; finished in 0.00s\n\n";
        let red = "\nrunning 1 test\ntest b ... \u{1b}[31mFAILED\u{1b}(B\u{1b}[m\n\nfailures:\n\n---- b stdout ----\nno\n\n\nfailures:\n    b\n\ntest result: \u{1b}[31mFAILED\u{1b}(B\u{1b}[m. 0 passed; 1 failed; 0 ignored; 0 measured; 1 filtered out; finished in 0.00s\n\n";
        let cases = [
            CaseOutput {
                name: "a",
                exit: 0,
                stdout: green,
                stderr: "",
            },
            CaseOutput {
                name: "b",
                exit: 101,
                stdout: red,
                stderr: "",
            },
        ];
        let report = libtest_report(&cases, false);
        assert!(
            report.contains("test a ... \u{1b}[32mok\u{1b}(B\u{1b}[m\n"),
            "{report}"
        );
        assert!(
            report.contains("test result: \u{1b}[31mFAILED\u{1b}(B\u{1b}[m. 1 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out;"),
            "{report}"
        );
        let terse = libtest_report(&cases, true);
        assert!(
            terse.contains(
                "\u{1b}[32m.\u{1b}(B\u{1b}[m 1/2\nb --- \u{1b}[31mFAILED\u{1b}(B\u{1b}[m\n"
            ),
            "{terse}"
        );
    }

    #[test]
    fn quiet_doctests_are_one_terse_report() {
        // `cargo test -q` printed a report for each doctest.
        let passed = "\nrunning 1 test\ntest src/lib.rs - seal (line 3) ... ok\n\ntest result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 1 filtered out; finished in 0.20s\n\n";
        assert_eq!(
            merged_report(&[passed, passed], true, &[]),
            "\nrunning 2 tests\n..\ntest result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.40s\n\n"
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
        // One that died after libtest listed its failure but before the
        // result: all of what it wrote is shown, not the listing alone.
        let stdout = "\nrunning 1 test\ntest t ... FAILED\n\nfailures:\n\n---- t stdout ----\nboom\n\n\nfailures:\n    t\n\n";
        let report = libtest_report(
            &[CaseOutput {
                name: "t",
                exit: 134,
                stdout,
                stderr: "fatal runtime error: failed to initiate panic\n",
            }],
            false,
        );
        assert!(
            report.contains(&format!(
                "---- t ----\n{stdout}fatal runtime error: failed to initiate panic\n"
            )),
            "{report}"
        );
    }

    /// A test's line is its own name's, colour is what stands around the
    /// word, and a result line libtest would not write reads as none.
    #[test]
    fn lines_colours_and_counts_are_read_as_libtest_writes_them() {
        assert_eq!(result_line("test foo_bar ... ok\n", "foo"), None);
        assert_eq!(
            result_line(
                "test foo_bar ... ok\ntest foo - should panic ... ok\n",
                "foo"
            ),
            Some("test foo - should panic ... ok")
        );
        assert_eq!(colour_of("\u{1b}[0m"), ("", ""));
        assert_eq!(
            colour_of("\u{1b}[32mok\u{1b}(B\u{1b}[m"),
            ("\u{1b}[32m", "\u{1b}(B\u{1b}[m")
        );
        assert_eq!(plain("a\u{1b}xb"), "axb");
        assert_eq!(plain("a\u{1b}[1"), "a");
        assert_eq!(plain("a\u{1b}("), "a");
        for refused in [
            "test result: ok",
            "test result: ok. 1 passed; 2 weird",
            "test result: ok. 1 passed; finished in soon",
            "test result: ok. x passed",
            "test result: ok. 1",
            "nothing",
        ] {
            assert!(counts(refused).is_none(), "{refused}");
        }
    }

    /// Reports with no lines and no result still make one report; a quiet
    /// harness's cases come from what is known of them.
    #[test]
    fn empty_and_quiet_doctest_reports_still_read_as_one() {
        let zero = "\nrunning 0 tests\n\ntest result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s\n\n";
        assert_eq!(merged_report(&["", ""], false, &[]), zero);
        let known = merged_report(
            &["", ""],
            true,
            &[("a", "failed"), ("b", "skipped"), ("c", "passed")],
        );
        assert!(
            known.starts_with("\nrunning 3 tests\na --- FAILED\ni.\nfailures:\n"),
            "{known}"
        );
        assert!(known.contains("\nfailures:\n    a\n"), "{known}");
        assert!(known.contains("test result: FAILED."), "{known}");
        assert_eq!(libtest_report(&[], false), zero);
        assert_eq!(libtest_report(&[], true), zero);
    }

    /// A quiet binary breaks its line where libtest does, a failure ends the
    /// open line, an ignored test is `i`, and a process that reported its
    /// line but no result is shown as it ended.
    #[test]
    fn quiet_binaries_break_lines_and_mark_each_result() {
        let names = (0..88).map(|at| format!("t{at}")).collect::<Vec<_>>();
        let mut cases = names
            .iter()
            .map(|name| CaseOutput {
                name,
                exit: 0,
                stdout: "",
                stderr: "",
            })
            .collect::<Vec<_>>();
        cases.push(CaseOutput {
            name: "fail",
            exit: 1,
            stdout: "test fail ... FAILED\n",
            stderr: "boom",
        });
        let report = libtest_report(&cases, true);
        assert!(
            report.contains(&format!("{} 87/89\n.", ".".repeat(87))),
            "{report}"
        );
        assert!(report.contains(" 88/89\nfail --- FAILED\n"), "{report}");
        assert!(
            report.contains("---- fail ----\ntest fail ... FAILED\nboom\n\n"),
            "{report}"
        );
        assert!(
            report.contains("test result: FAILED. 88 passed; 1 failed;"),
            "{report}"
        );
        let loud = libtest_report(&cases[87..], false);
        assert!(
            loud.contains("test t87 ... ok\ntest fail ... FAILED\n"),
            "{loud}"
        );
        let ignored = libtest_report(
            &[CaseOutput {
                name: "ig",
                exit: 0,
                stdout: "test ig ... ignored\n\ntest result: ok. 0 passed; 0 failed; 1 ignored; 0 measured; 3 filtered out; finished in 0.00s\n",
                stderr: "",
            }],
            true,
        );
        assert_eq!(
            ignored,
            "\nrunning 1 test\ni\ntest result: ok. 0 passed; 0 failed; 1 ignored; 0 measured; 3 filtered out; finished in 0.00s\n\n"
        );
    }
}
