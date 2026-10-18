//! Doctests for the owned Rust frontend, measured one doctest at a time.
//!
//! `cargo test` runs doctests, and for a long time the owned frontend did not:
//! it built with `cargo test --no-run`, ran each libtest case in its own
//! process, and never asked rustdoc for anything. A crate whose only test was
//! a doctest reported zero tests and succeeded.
//!
//! rustdoc gives a stable toolchain exactly two handles on doctest execution,
//! and this module is built on nothing else:
//!
//! * Cargo honours `RUSTDOC`, so this program stands in for rustdoc during
//!   `cargo test --doc`, sees the exact arguments for each package, and calls
//!   the real rustdoc as many times as it needs.
//! * rustdoc runs every compiled doctest binary through the `--test-runtool`
//!   it is given, so this program receives each binary before it runs.
//!
//! What a binary is depends on the edition. From edition 2024 rustdoc merges
//! a package's doctests into one libtest harness whose arguments are baked in
//! at compile time -- `--list` and `--exact` on the command line are ignored
//! -- and which runs one case per process only through rustdoc's own
//! protocol: with `RUSTDOC_DOCTEST_BIN_PATH` set it spawns that program once
//! per case with `RUSTDOC_DOCTEST_RUN_NB_TEST` naming the case's index, and a
//! process with only the index set runs that one case and exits. rustdoc
//! sorts doctests by name before generating the harness, so the index is the
//! position in sorted order, which is also the order the harness prints. The
//! child this module supplies runs its case under its own evidence directory,
//! and the runtool reads the case's name off the harness's own output. Every
//! non-ignored, non-`no_run` case spawns a child, so nothing is inferred.
//!
//! Earlier editions, and cases rustdoc cannot merge, compile to one binary per
//! doctest, and rustdoc names none of them for the runtool. Those cases run
//! one rustdoc invocation each, filtered to that name with `--exact`, with the
//! name in the runtool's environment; a list pass first learns the names. It
//! costs one crate scan per doctest and nothing is guessed.
//!
//! Every case ends up with the name rustdoc gives it -- `src/lib.rs - classify
//! (line 1)` -- its status as rustdoc's harness reported it, and the evidence
//! directory its own process wrote, or none for a case that never ran. When
//! the accounting does not balance the run fails rather than attribute
//! evidence to a guess.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Output},
    sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};

use serde::{Deserialize, Serialize};

use crate::{
    coverage_report::{ExecutionScope, RawTestResult, TestProvenance},
    rust_project::PreparedRustProject,
    rust_test_runner::{
        CargoTestInvocation, RustCargoExecutionSelection, RustTestRunnerError, capped_rustflags,
        instrumented_stack_environment, io_error, relative_source, rustc_sysroot, snapshot,
    },
};

/// Set on `cargo test --doc` so this program, named as `RUSTDOC`, knows it is
/// the wrapper and where the run collects results.
pub const WRAPPER_ROOT_ENV: &str = "SUPERCOV_DOCTEST_WRAPPER_ROOT";
/// The rustdoc the wrapper calls.
pub const REAL_RUSTDOC_ENV: &str = "SUPERCOV_DOCTEST_REAL_RUSTDOC";
/// The one doctest a per-name rustdoc invocation runs; the runtool inherits it.
pub const NAME_ENV: &str = "SUPERCOV_DOCTEST_NAME";
/// Where a merged harness's children record themselves, and which binary they run.
pub const CHILD_DIR_ENV: &str = "SUPERCOV_DOCTEST_CHILD_DIR";
pub const CHILD_BINARY_ENV: &str = "SUPERCOV_DOCTEST_CHILD_BINARY";
/// rustdoc's own protocol for merged doctest harnesses.
pub const RUSTDOC_BIN_PATH_ENV: &str = "RUSTDOC_DOCTEST_BIN_PATH";
pub const RUSTDOC_RUN_NB_TEST_ENV: &str = "RUSTDOC_DOCTEST_RUN_NB_TEST";
const EVIDENCE_DIR_ENV: &str = "SUPERCOV_RUST_EVIDENCE_DIR";
/// The argument that puts this program in runtool mode.
pub const RUNTOOL_MODE_ARGUMENT: &str = "__doctest-runner";

// ---------------------------------------------------------------- records

/// One doctest as the run reports it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DoctestCase {
    pub name: String,
    /// `passed`, `failed` or `skipped`, from the harness line rustdoc printed.
    pub status: String,
    /// The evidence directory the case's own process wrote; none for a case
    /// that never ran (ignored, `no_run`, `compile_fail`).
    pub evidence: Option<PathBuf>,
}

/// Everything the wrapper learned about one package.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackageDoctests {
    pub package: String,
    pub manifest_dir: PathBuf,
    pub cases: Vec<DoctestCase>,
    /// How many doctests the package has, selected or not.
    #[serde(default)]
    pub listed: usize,
    /// rustdoc's combined exit code over every pass.
    pub exit_code: i32,
}

/// What the runtool recorded for one binary it was handed.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
enum Invocation {
    /// A merged harness: its printed lines, in order, and the child each
    /// spawned (by index).
    Merged { cases: Vec<MergedCase> },
    /// One doctest binary in a per-name pass.
    Standalone {
        name: String,
        exit_code: i32,
        evidence: PathBuf,
    },
    /// A merged harness that matched no case in a per-name pass.
    EmptyHarness,
    /// A binary run in the list pass; nothing to attribute.
    Listed,
}

#[derive(Debug, Serialize, Deserialize)]
struct MergedCase {
    name: String,
    status: String,
    evidence: Option<PathBuf>,
}

/// What a merged harness's child recorded for its case.
#[derive(Debug, Serialize, Deserialize)]
struct ChildRecord {
    index: usize,
    exit_code: i32,
    evidence: PathBuf,
}

// ------------------------------------------------------------ engine side

/// rustdoc's reports on Cargo's standard output, one for each package under
/// the `Doc-tests <package>` line Cargo wrote for it on standard error. Cargo
/// runs the packages one after another, so both are in that order, and a
/// report is its package's while every doctest it names is one of that
/// package's. When the two cannot be paired the reports are given as they
/// came.
fn doctest_reports(
    stdout: &str,
    stderr: &str,
    packages: &[PackageDoctests],
    quiet: bool,
) -> Vec<crate::rust_libtest_report::Announced> {
    use crate::rust_libtest_report::Announced;
    let mut reports = Vec::<String>::new();
    for line in stdout.split_inclusive('\n') {
        if line.starts_with("running ") || reports.is_empty() {
            reports.push(String::new());
        }
        reports
            .last_mut()
            .expect("a report was just started")
            .push_str(line);
    }
    reports.retain(|report| !report.trim().is_empty());
    let named = |name: &str| name.replace('-', "_");
    // Cargo may colour its word for the line.
    let plain = |line: &str| {
        let mut text = String::with_capacity(line.len());
        let mut rest = line;
        while let Some(at) = rest.find('\u{1b}') {
            text.push_str(&rest[..at]);
            rest = rest[at..].find('m').map_or("", |end| &rest[at + end + 1..]);
        }
        text.push_str(rest);
        text
    };
    let headings = stderr
        .lines()
        .filter(|line| plain(line).trim_start().starts_with("Doc-tests "))
        .collect::<Vec<_>>();
    // Under `cargo test -q` Cargo writes no line for a package, and the
    // packages are taken in the order it ran them.
    let mut announced = Vec::new();
    if headings.is_empty() {
        announced.extend(packages.iter().map(|package| (None, package)));
    }
    for heading in &headings {
        let written = plain(heading);
        let package = written.trim_start().trim_start_matches("Doc-tests ").trim();
        let Some(package) = packages
            .iter()
            .find(|candidate| named(&candidate.package) == named(package))
        else {
            announced.clear();
            break;
        };
        announced.push((Some((*heading).to_owned()), package));
    }
    let mut merged = Vec::new();
    let mut next = 0;
    for (heading, package) in announced {
        let its = reports[next..]
            .iter()
            .take_while(|report| {
                crate::rust_libtest_report::reported_tests(report)
                    .iter()
                    .all(|test| package.cases.iter().any(|case| case.name == *test))
            })
            .map(String::as_str)
            .collect::<Vec<_>>();
        next += its.len();
        merged.push(Announced {
            heading,
            // rustdoc is not run for a package none of whose doctests were
            // asked for; it would have reported that it ran none.
            report: if its.is_empty() {
                crate::rust_libtest_report::unasked_report(package.listed)
            } else {
                crate::rust_libtest_report::merged_report(
                    &its,
                    quiet,
                    &package
                        .cases
                        .iter()
                        .map(|case| (case.name.as_str(), case.status.as_str()))
                        .collect::<Vec<_>>(),
                )
            },
        });
    }
    if next == reports.len() && !merged.is_empty() {
        merged
    } else if reports.is_empty() {
        Vec::new()
    } else {
        vec![Announced {
            heading: None,
            report: reports.concat(),
        }]
    }
}

/// Run the doctests through `cargo test --doc` with this program as rustdoc,
/// then read back what the wrapper recorded for each package.
pub(crate) fn run_doctests(
    project: &PreparedRustProject,
    invocation: &CargoTestInvocation,
    selection: &RustCargoExecutionSelection,
    root: &Path,
    run_id: &str,
    diagnostics: &mut dyn Write,
    overall_exit: &mut i32,
) -> Result<Vec<RawTestResult>, RustTestRunnerError> {
    fs::create_dir_all(root.join("packages")).map_err(io_error)?;
    let program = std::env::current_exe()
        .and_then(fs::canonicalize)
        .map_err(io_error)?;
    let real_rustdoc = real_rustdoc()?;
    let output = crate::child_signal_guard::output(
        Command::new(&invocation.program)
            .args(&selection.doctest_arguments)
            .current_dir(&project.workspace_root)
            .env("CARGO_TARGET_DIR", &project.target_directory)
            .env("RUSTFLAGS", capped_rustflags(project, invocation))
            .env("RUSTDOC", &program)
            .env(WRAPPER_ROOT_ENV, root)
            .env(REAL_RUSTDOC_ENV, &real_rustdoc),
    )
    .map_err(|error| RustTestRunnerError::Launch(error.to_string()))?;
    let exit = output.status.code().unwrap_or(1);

    let mut packages = Vec::new();
    let mut errors = Vec::new();
    let mut entries = fs::read_dir(root.join("packages"))
        .map_err(io_error)?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .collect::<Vec<_>>();
    entries.sort();
    for path in entries {
        match path.extension().and_then(|extension| extension.to_str()) {
            Some("json") => {
                let package: PackageDoctests =
                    serde_json::from_slice(&fs::read(&path).map_err(io_error)?)?;
                packages.push(package);
            }
            Some("error") => errors.push(fs::read_to_string(&path).map_err(io_error)?),
            _ => {}
        }
    }
    if !errors.is_empty() {
        return Err(RustTestRunnerError::Context(format!(
            "doctest attribution could not be established: {}",
            errors.join("; ")
        )));
    }
    let any_failed = packages
        .iter()
        .any(|package| package.cases.iter().any(|case| case.status == "failed"));
    // `cargo test` runs doctests only for the selected packages whose library
    // can have them, and runs none, quietly, when no package can: a crate
    // that is only a `cdylib`, as a Worker or a plugin is. `--doc` makes
    // that same case an error, which the command never asked for.
    if exit != 0
        && selection.implicit_doctests
        && packages.is_empty()
        && String::from_utf8_lossy(&output.stderr).contains("no library targets found in package")
    {
        return Ok(Vec::new());
    }
    if exit != 0 && !any_failed {
        // Doctests that do not compile, or rustdoc itself failing, is a build
        // failure, not a test failure.
        return Err(RustTestRunnerError::CargoFailed(
            String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        ));
    }
    if exit != 0 {
        *overall_exit = exit;
    }
    // rustdoc's report of each package's doctests, under Cargo's heading for
    // it, as `cargo test` prints them. A failed doctest was one line naming
    // it, with nothing of what it printed.
    let reported = doctest_reports(
        &String::from_utf8_lossy(&output.stdout),
        &String::from_utf8_lossy(&output.stderr),
        &packages,
        crate::rust_test_runner::cargo_quiet(invocation)
            || crate::rust_test_runner::libtest_quiet(invocation),
    );
    crate::rust_libtest_report::write_reports(diagnostics, &reported).map_err(io_error)?;
    let reported = reported
        .iter()
        .map(|announced| announced.report.as_str())
        .collect::<String>();

    let mut results = Vec::new();
    let mut index = 0_usize;
    for package in packages {
        for case in package.cases {
            // rustdoc names a case by its path relative to the package; the
            // manifest is keyed by paths relative to the workspace.
            let path_in_package = case
                .name
                .split_once(" - ")
                .map_or(case.name.as_str(), |(path, _)| path);
            let test_file = relative_source(
                &project.workspace_root,
                &package.manifest_dir.join(path_in_package),
            )
            .unwrap_or_else(|_| path_in_package.to_owned());
            if case.status == "failed" && !reported.contains(&case.name) {
                writeln!(diagnostics, "[supercov] Rust doctest failed: {}", case.name)
                    .map_err(io_error)?;
            }
            // A case that never ran has no evidence directory; an empty one
            // yields the empty snapshot the report expects.
            let evidence = match &case.evidence {
                Some(directory) => directory.clone(),
                None => {
                    let directory = root.join("none").join(format!("{index:08}"));
                    fs::create_dir_all(&directory).map_err(io_error)?;
                    directory
                }
            };
            // rustdoc writes the path with the platform's separator; test
            // identities use `/` everywhere, as the libtest path does.
            let name = case.name.replace('\\', "/");
            let attempt_id = format!("{run_id}:doctest:{index:08}");
            let recorded = snapshot(&project.manifest, &project.probes, &evidence, &attempt_id)?;
            results.push(RawTestResult {
                span: None,
                test_id: Some(name.clone()),
                scope: Some(ExecutionScope {
                    version: 1,
                    run_id: run_id.into(),
                    worker_id: format!("doctest-{index:04}"),
                    test_id: name.clone(),
                    test_key: name.clone(),
                    retry: 0,
                    attempt_id: attempt_id.clone(),
                }),
                test: name.clone(),
                test_file: Some(test_file),
                title: Some(name),
                retry: Some(0),
                status: Some(case.status.clone()),
                expected_status: Some("passed".into()),
                flaky: false,
                provenance: TestProvenance {
                    runner: "rustdoc".into(),
                    kind: "doctest".into(),
                    project: Some(package.package.clone()),
                    source: "supercov-owned-process-per-test".into(),
                    tags: Vec::new(),
                },
                role: "test".into(),
                attribution: crate::coverage_report::ATTRIBUTION_EXACT.into(),
                phases: recorded.phases,
                runtime: vec![recorded.snapshot],
                browser: Vec::new(),
                server: Vec::new(),
            });
            index += 1;
        }
    }
    Ok(results)
}

/// The rustdoc Cargo would have run: whatever `RUSTDOC` already named, else
/// the one beside the selected toolchain's rustc.
fn real_rustdoc() -> Result<PathBuf, RustTestRunnerError> {
    if let Some(configured) = std::env::var_os("RUSTDOC") {
        return Ok(PathBuf::from(configured));
    }
    let sysroot = rustc_sysroot()?;
    let rustdoc = sysroot
        .join("bin")
        .join(format!("rustdoc{}", std::env::consts::EXE_SUFFIX));
    if !rustdoc.is_file() {
        return Err(RustTestRunnerError::Launch(format!(
            "the selected toolchain has no rustdoc at {}",
            rustdoc.display()
        )));
    }
    Ok(rustdoc)
}

// ------------------------------------------------------------- the wrapper

/// The arguments Cargo gave rustdoc, taken apart: everything that describes
/// the crate, the harness arguments the user passed after `--`, and any
/// runtool Cargo configured from a target runner.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct RustdocArguments {
    crate_arguments: Vec<String>,
    harness_arguments: Vec<String>,
    test: bool,
}

fn parse_rustdoc_arguments(arguments: &[String]) -> RustdocArguments {
    let mut parsed = RustdocArguments::default();
    let mut index = 0;
    while index < arguments.len() {
        let argument = &arguments[index];
        if argument == "--test" {
            parsed.test = true;
            parsed.crate_arguments.push(argument.clone());
        } else if argument == "--test-args"
            || argument == "--test-runtool"
            || argument == "--test-runtool-arg"
        {
            if let Some(value) = arguments.get(index + 1) {
                if argument == "--test-args" {
                    parsed.harness_arguments.push(value.clone());
                }
                index += 1;
            }
        } else if let Some(value) = argument.strip_prefix("--test-args=") {
            parsed.harness_arguments.push(value.to_owned());
        } else if argument.starts_with("--test-runtool") {
            // `--test-runtool=X` / `--test-runtool-arg=X`: replaced below.
        } else {
            parsed.crate_arguments.push(argument.clone());
        }
        index += 1;
    }
    parsed
}

/// libtest's harness arguments, taken apart the way libtest reads them:
/// positional filters, `--skip` patterns, whether `--exact` was given, and
/// everything else passed through unchanged.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct HarnessArguments {
    filters: Vec<String>,
    skips: Vec<String>,
    exact: bool,
    list: bool,
    passthrough: Vec<String>,
}

fn parse_harness_arguments(arguments: &[String]) -> HarnessArguments {
    const TAKES_VALUE: &[&str] = &[
        "--skip",
        "--test-threads",
        "--logfile",
        "--format",
        "--color",
        "--shuffle-seed",
        "-Z",
    ];
    let mut parsed = HarnessArguments::default();
    let mut index = 0;
    while index < arguments.len() {
        let argument = &arguments[index];
        if let Some(pattern) = argument.strip_prefix("--skip=") {
            parsed.skips.push(pattern.to_owned());
        } else if argument == "--skip" {
            if let Some(pattern) = arguments.get(index + 1) {
                parsed.skips.push(pattern.clone());
                index += 1;
            }
        } else if matches!(argument.as_str(), "-q" | "--quiet") || argument.starts_with("--format=")
        {
            // How results print, not which run: each pass is read by the
            // name libtest prints, and `cargo test -q` would print a dot.
        } else if argument == "--format" {
            index += 1;
        } else if argument == "--exact" {
            parsed.exact = true;
        } else if argument == "--list" {
            parsed.list = true;
        } else if TAKES_VALUE.contains(&argument.as_str()) {
            parsed.passthrough.push(argument.clone());
            if let Some(value) = arguments.get(index + 1) {
                parsed.passthrough.push(value.clone());
                index += 1;
            }
        } else if argument.starts_with('-') {
            parsed.passthrough.push(argument.clone());
        } else {
            parsed.filters.push(argument.clone());
        }
        index += 1;
    }
    parsed
}

impl HarnessArguments {
    /// libtest's selection: a test runs when it matches any filter (all, if
    /// there are none) and no skip; `--exact` compares whole names, otherwise
    /// a filter is a substring.
    fn selects(&self, name: &str) -> bool {
        let matches = |pattern: &String| {
            if self.exact {
                name == pattern
            } else {
                name.contains(pattern.as_str())
            }
        };
        (self.filters.is_empty() || self.filters.iter().any(matches))
            && !self.skips.iter().any(matches)
    }
}

/// rustdoc splits every `--test-args` value on whitespace before handing the
/// pieces to libtest; read them the same way.
fn split_test_args(arguments: &[String]) -> Vec<String> {
    arguments
        .iter()
        .flat_map(|argument| argument.split_whitespace())
        .map(str::to_owned)
        .collect()
}

/// A substring filter and skips that select `name` alone among `names`:
/// libtest matches filters and skips as substrings, and every piece reaching
/// it is whitespace-free, so a piece of the name is the filter and every
/// other doctest it also matches is skipped by a piece of its own the name
/// lacks. None when some other doctest has no such piece.
fn isolate(name: &str, names: &[String]) -> Option<(String, Vec<String>)> {
    let mut best: Option<(String, Vec<String>)> = None;
    for token in name.split_whitespace() {
        let mut skips = Vec::new();
        let mut isolated = true;
        for other in names
            .iter()
            .filter(|other| other.as_str() != name && other.contains(token))
        {
            match other.split_whitespace().find(|piece| !name.contains(piece)) {
                Some(piece) => skips.push(piece.to_owned()),
                None => {
                    isolated = false;
                    break;
                }
            }
        }
        if !isolated {
            continue;
        }
        skips.sort();
        skips.dedup();
        if best
            .as_ref()
            .is_none_or(|(_, current)| skips.len() < current.len())
        {
            best = Some((token.to_owned(), skips));
        }
    }
    best
}

/// The lines libtest prints for each test, as `(name, status)`.
fn harness_lines(text: &str) -> Vec<(String, String)> {
    text.lines()
        .filter_map(|line| {
            let rest = line.strip_prefix("test ")?;
            let (name, outcome) = rest.rsplit_once(" ... ")?;
            // libtest appends the test mode to `no_run`, `compile_fail` and
            // `should_panic` doctests when it prints them; the listing names
            // them plainly.
            let name = without_test_mode(name);
            let status = if outcome.starts_with("ok") {
                "passed"
            } else if outcome.starts_with("FAILED") {
                "failed"
            } else if outcome.starts_with("ignored") {
                "skipped"
            } else {
                return None;
            };
            Some((name.to_owned(), status.to_owned()))
        })
        .collect()
}

/// What a quiet merged harness reported, named from what Supercov knows.
///
/// `cargo test -q` makes libtest print a dot per case and no name, and a
/// merged harness has its arguments compiled in, so it cannot be asked for
/// them instead. A case that ran is named by its child's index in the
/// harness's order; one that failed is named in the `failures:` list the
/// summary still prints; one that spawned nothing is skipped if its fence
/// says `ignore` and compiled (`no_run`, `compile_fail`) otherwise. None at
/// all unless the counts that come out are the counts the harness printed.
fn quiet_cases(
    text: &str,
    order: &[String],
    selected: &BTreeSet<String>,
    ran: &BTreeSet<usize>,
    manifest_dir: &Path,
) -> Option<Vec<(String, String)>> {
    let summary = text
        .lines()
        .rev()
        .find_map(|line| line.strip_prefix("test result: "))?;
    let count = |label: &str| -> Option<usize> {
        summary
            .split([';', '.'])
            .find_map(|part| part.trim().strip_suffix(label)?.trim().parse().ok())
    };
    let (passed, failed, ignored) = (count("passed")?, count("failed")?, count("ignored")?);
    let failures = text
        .rsplit_once("\nfailures:\n")
        .map(|(_, rest)| {
            rest.lines()
                .skip_while(|line| line.trim().is_empty())
                .take_while(|line| line.starts_with("    "))
                .map(|line| without_test_mode(line.trim()).to_owned())
                .collect::<BTreeSet<_>>()
        })
        .unwrap_or_default();
    let cases = order
        .iter()
        .enumerate()
        .filter(|(_, name)| selected.contains(*name))
        .map(|(index, name)| {
            let status = if failures.contains(name) {
                "failed"
            } else if ran.contains(&index) || !fence_says_ignore(name, manifest_dir) {
                "passed"
            } else {
                "skipped"
            };
            (name.clone(), status.to_owned())
        })
        .collect::<Vec<_>>();
    let tally = |status: &str| cases.iter().filter(|(_, case)| case == status).count();
    (tally("passed") == passed && tally("failed") == failed && tally("skipped") == ignored)
        .then_some(cases)
}

/// Whether the fence a doctest is named for (`src/lib.rs - f (line 12)`)
/// marks it `ignore`.
fn fence_says_ignore(name: &str, manifest_dir: &Path) -> bool {
    let Some((file, rest)) = name.split_once(" - ") else {
        return false;
    };
    let Some(line) = rest
        .rsplit_once("(line ")
        .and_then(|(_, number)| number.strip_suffix(')'))
        .and_then(|number| number.parse::<usize>().ok())
    else {
        return false;
    };
    fs::read_to_string(manifest_dir.join(file))
        .ok()
        .and_then(|source| {
            source
                .lines()
                .nth(line.saturating_sub(1))
                .map(str::to_owned)
        })
        .and_then(|fence| {
            fence
                .split_once("```")
                .map(|(_, attributes)| attributes.to_owned())
        })
        .is_some_and(|attributes| {
            attributes
                .split(|c: char| c == ',' || c.is_whitespace())
                .any(|attribute| attribute == "ignore" || attribute.starts_with("ignore-"))
        })
}

/// A case's name without the mode libtest appends when it prints one.
fn without_test_mode(name: &str) -> &str {
    name.strip_suffix(" - compile fail")
        .or_else(|| name.strip_suffix(" - compile"))
        .or_else(|| name.strip_suffix(" - should panic"))
        .unwrap_or(name)
}

/// The names `--list --format terse` prints.
fn listed_names(text: &str) -> Vec<String> {
    text.lines()
        .filter_map(|line| line.strip_suffix(": test"))
        .map(str::to_owned)
        .collect()
}

fn next_sequence(directory: &Path, prefix: &str) -> Result<usize, RustTestRunnerError> {
    fs::create_dir_all(directory).map_err(io_error)?;
    for candidate in 0..1_000_000_usize {
        let marker = directory.join(format!("{prefix}{candidate:08}.claim"));
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&marker)
        {
            Ok(_) => return Ok(candidate),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(io_error(error)),
        }
    }
    Err(RustTestRunnerError::Io(
        "too many doctest sequence claims".into(),
    ))
}

fn relay(output: &Output) {
    let mut stdout = std::io::stdout();
    let mut stderr = std::io::stderr();
    let _ = stdout.write_all(&output.stdout);
    let _ = stderr.write_all(&output.stderr);
    let _ = stdout.flush();
    let _ = stderr.flush();
}

/// This program standing in for rustdoc during `cargo test --doc`.
pub fn rustdoc_wrapper(arguments: Vec<String>) -> i32 {
    match rustdoc_wrapper_inner(&arguments) {
        Ok(code) => code,
        Err(error) => {
            eprintln!("[supercov] {error}");
            101
        }
    }
}

fn rustdoc_wrapper_inner(arguments: &[String]) -> Result<i32, RustTestRunnerError> {
    let root = PathBuf::from(std::env::var_os(WRAPPER_ROOT_ENV).ok_or_else(|| {
        RustTestRunnerError::Context("the doctest wrapper has no results root".into())
    })?);
    let real = PathBuf::from(std::env::var_os(REAL_RUSTDOC_ENV).ok_or_else(|| {
        RustTestRunnerError::Context("the doctest wrapper does not know the real rustdoc".into())
    })?);
    let program = std::env::current_exe()
        .and_then(fs::canonicalize)
        .map_err(io_error)?;
    let parsed = parse_rustdoc_arguments(arguments);
    let harness_arguments = split_test_args(&parsed.harness_arguments);
    let harness = parse_harness_arguments(&harness_arguments);
    // Anything but a doctest run -- or the user asking for a listing -- is
    // rustdoc's business alone.
    if !parsed.test || harness.list {
        let status = Command::new(&real)
            .args(arguments)
            .env_remove(WRAPPER_ROOT_ENV)
            .env_remove(REAL_RUSTDOC_ENV)
            .status()
            .map_err(|error| RustTestRunnerError::Launch(error.to_string()))?;
        return Ok(status.code().unwrap_or(1));
    }

    let package = std::env::var("CARGO_PKG_NAME").unwrap_or_else(|_| "package".into());
    let manifest_dir = std::env::var_os("CARGO_MANIFEST_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    let sequence = next_sequence(&root.join("packages"), "")?;
    let package_dir = root.join("packages").join(format!("{sequence:08}"));
    fs::create_dir_all(package_dir.join("invocations")).map_err(io_error)?;

    let rustdoc = |harness_arguments: &[String], mode: &str, environment: &[(&str, &str)]| {
        let mut command = Command::new(&real);
        command
            .args(&parsed.crate_arguments)
            .env_remove(WRAPPER_ROOT_ENV)
            .env_remove(REAL_RUSTDOC_ENV);
        for argument in harness_arguments {
            command.args(["--test-args", argument]);
        }
        command
            .args(["--test-runtool", &program.to_string_lossy()])
            .args(["--test-runtool-arg", RUNTOOL_MODE_ARGUMENT])
            .args(["--test-runtool-arg", &package_dir.to_string_lossy()])
            .args(["--test-runtool-arg", mode]);
        for (key, value) in environment {
            command.env(key, value);
        }
        command
            .output()
            .map_err(|error| RustTestRunnerError::Launch(error.to_string()))
    };

    // List pass: every doctest of the package, unfiltered, so a merged
    // harness index maps to its name. rustdoc lists the cases it would run
    // standalone; a merged harness lists its own through the runtool. The
    // user's harness arguments stay out of it: the listing must be complete.
    let listing = rustdoc(
        &["--list".into(), "--format".into(), "terse".into()],
        "list",
        &[],
    )?;
    if !listing.status.success() {
        relay(&listing);
        return Ok(listing.status.code().unwrap_or(1));
    }
    let all_names = listed_names(&String::from_utf8_lossy(&listing.stdout));
    let merged_listed = read_listed_names(&package_dir)?;
    let merged_set = merged_listed.iter().cloned().collect::<BTreeSet<_>>();
    let mut merged_sorted = merged_listed.clone();
    merged_sorted.sort();
    merged_sorted.dedup();
    let standalone = all_names
        .iter()
        .filter(|name| !merged_set.contains(*name))
        .cloned()
        .collect::<Vec<_>>();

    let mut cases = Vec::new();
    let mut exit_code = 0;

    // Merged pass: one rustdoc invocation with the user's own harness
    // arguments, so libtest selects exactly what it would have; each case
    // runs in a child of its own and is attributed by its index.
    // `cargo test -- --ignored` failed outright on a package with a doctest:
    // "the pass for doctest … ran 0 doctest(s) instead of that one alone".
    let only_ignored = harness
        .passthrough
        .iter()
        .any(|argument| argument == "--ignored");
    let selected_merged = merged_sorted
        .iter()
        .filter(|name| harness.selects(name))
        .cloned()
        .collect::<BTreeSet<_>>();
    if !selected_merged.is_empty() {
        fs::write(
            package_dir.join("merged-order.json"),
            serde_json::to_vec(&merged_sorted)?,
        )
        .map_err(io_error)?;
        // What a quiet harness leaves unnamed is named from these.
        fs::write(
            package_dir.join("merged-selected.json"),
            serde_json::to_vec(&selected_merged)?,
        )
        .map_err(io_error)?;
        fs::write(
            package_dir.join("manifest-dir"),
            manifest_dir.to_string_lossy().as_bytes(),
        )
        .map_err(io_error)?;
        let output = rustdoc(&harness_arguments, "merged", &[])?;
        relay(&output);
        if !output.status.success() {
            exit_code = output.status.code().unwrap_or(1);
        }
        let mut reported = BTreeSet::new();
        for (_, invocation) in read_invocations(&package_dir)? {
            if let Invocation::Merged { cases: merged } = invocation {
                for case in merged {
                    if !merged_set.contains(&case.name) {
                        return fail_package(
                            &package_dir,
                            format!(
                                "the merged doctest harness reported a case the listing lacks: {}",
                                case.name
                            ),
                        );
                    }
                    if !reported.insert(case.name.clone()) {
                        return fail_package(
                            &package_dir,
                            format!("doctest {} was reported more than once", case.name),
                        );
                    }
                    cases.push(DoctestCase {
                        name: case.name,
                        status: case.status,
                        evidence: case.evidence,
                    });
                }
            }
        }
        // libtest's selection and the wrapper's reading of it must agree, or
        // a doctest could run unattributed; when they differ, say so.
        let agrees = if only_ignored {
            reported.is_subset(&selected_merged)
        } else {
            reported == selected_merged
        };
        if !agrees && output.status.success() {
            let difference = selected_merged
                .symmetric_difference(&reported)
                .cloned()
                .collect::<Vec<_>>();
            return fail_package(
                &package_dir,
                format!(
                    "the merged doctest harness ran a different selection than its arguments \
                     describe: {}",
                    difference.join(", ")
                ),
            );
        }
    }

    // Standalone passes: one rustdoc invocation per selected name, in
    // parallel, each isolating its doctest with a substring filter and skips
    // (rustdoc splits harness arguments on whitespace, so the name itself
    // can never be a filter) and named in the runtool's environment.
    let selected_standalone = standalone
        .iter()
        .filter(|name| harness.selects(name))
        .cloned()
        .collect::<Vec<_>>();
    let mut plans = Vec::new();
    for name in &selected_standalone {
        let Some((filter, skips)) = isolate(name, &all_names) else {
            return fail_package(
                &package_dir,
                format!("doctest {name} cannot be told apart from another doctest by a filter"),
            );
        };
        let mut arguments = harness.passthrough.clone();
        arguments.push(filter);
        for skip in skips {
            arguments.push("--skip".to_owned());
            arguments.push(skip);
        }
        plans.push((name.clone(), arguments));
    }
    let outputs = Mutex::new(Vec::<(String, Output)>::new());
    let next = AtomicUsize::new(0);
    let workers = std::thread::available_parallelism()
        .map(usize::from)
        .unwrap_or(1)
        .min(plans.len().max(1));
    let launch_error = Mutex::new(None::<RustTestRunnerError>);
    std::thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| {
                loop {
                    let index = next.fetch_add(1, Ordering::Relaxed);
                    let Some((name, arguments)) = plans.get(index) else {
                        break;
                    };
                    match rustdoc(arguments, "named", &[(NAME_ENV, name)]) {
                        Ok(output) => outputs
                            .lock()
                            .expect("doctest outputs lock")
                            .push((name.clone(), output)),
                        Err(error) => {
                            *launch_error.lock().expect("doctest error lock") = Some(error);
                        }
                    }
                }
            });
        }
    });
    if let Some(error) = launch_error.into_inner().expect("doctest error lock") {
        return Err(error);
    }
    let mut outputs = outputs.into_inner().expect("doctest outputs lock");
    outputs.sort_by(|left, right| left.0.cmp(&right.0));
    let mut standalone_evidence = BTreeMap::new();
    for (_, invocation) in read_invocations(&package_dir)? {
        if let Invocation::Standalone { name, evidence, .. } = invocation
            && standalone_evidence.insert(name.clone(), evidence).is_some()
        {
            return fail_package(&package_dir, format!("doctest {name} ran more than once"));
        }
    }
    for (name, output) in &outputs {
        let text = String::from_utf8_lossy(&output.stdout);
        let lines = harness_lines(&text);
        // Under `--ignored` libtest leaves out every doctest that is not
        // ignored, which no listing tells apart: its pass ran none, and the
        // doctest was not asked for.
        if only_ignored && lines.is_empty() && output.status.success() {
            continue;
        }
        relay(output);
        if !output.status.success() {
            exit_code = output.status.code().unwrap_or(1);
        }
        // The pass must have run this doctest and nothing else; the filter
        // is a substring, so check rather than trust.
        let [(printed, status)] = lines.as_slice() else {
            let ran = lines
                .iter()
                .map(|(printed, _)| printed.as_str())
                .collect::<Vec<_>>();
            return fail_package(
                &package_dir,
                format!(
                    "the pass for doctest {name} ran {} doctest(s) instead of that one alone: {}",
                    ran.len(),
                    ran.join(", ")
                ),
            );
        };
        if printed != name {
            return fail_package(
                &package_dir,
                format!("the pass for doctest {name} ran {printed} instead"),
            );
        }
        cases.push(DoctestCase {
            evidence: standalone_evidence.remove(name),
            name: name.clone(),
            status: status.clone(),
        });
    }
    if let Some((name, _)) = standalone_evidence.into_iter().next() {
        return fail_package(
            &package_dir,
            format!("a doctest ran that no pass selected: {name}"),
        );
    }

    cases.sort_by(|left, right| left.name.cmp(&right.name));
    let record = PackageDoctests {
        package,
        manifest_dir,
        cases,
        listed: all_names.len(),
        exit_code,
    };
    fs::write(
        root.join("packages").join(format!("{sequence:08}.json")),
        serde_json::to_vec(&record)?,
    )
    .map_err(io_error)?;
    Ok(exit_code)
}

fn fail_package(package_dir: &Path, reason: String) -> Result<i32, RustTestRunnerError> {
    let error_path = package_dir.parent().unwrap_or(package_dir).join(format!(
        "{}.error",
        package_dir
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("package")
    ));
    fs::write(&error_path, &reason).map_err(io_error)?;
    eprintln!("[supercov] {reason}");
    Ok(101)
}

fn read_invocations(package_dir: &Path) -> Result<Vec<(usize, Invocation)>, RustTestRunnerError> {
    let directory = package_dir.join("invocations");
    let mut records = Vec::new();
    let Ok(entries) = fs::read_dir(&directory) else {
        return Ok(records);
    };
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        if path
            .extension()
            .is_some_and(|extension| extension == "json")
        {
            let index = path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .and_then(|stem| stem.parse::<usize>().ok())
                .unwrap_or(usize::MAX);
            let invocation: Invocation =
                serde_json::from_slice(&fs::read(&path).map_err(io_error)?)?;
            records.push((index, invocation));
        }
    }
    records.sort_by_key(|(index, _)| *index);
    Ok(records)
}

fn read_listed_names(package_dir: &Path) -> Result<Vec<String>, RustTestRunnerError> {
    let directory = package_dir.join("listed");
    let mut names = Vec::new();
    let Ok(entries) = fs::read_dir(&directory) else {
        return Ok(names);
    };
    for entry in entries.filter_map(Result::ok) {
        names.extend(listed_names(
            &fs::read_to_string(entry.path()).map_err(io_error)?,
        ));
    }
    Ok(names)
}

// ------------------------------------------------------------- the runtool

/// This program as rustdoc's `--test-runtool`: `__doctest-runner <package
/// dir> <mode> <binary>`, where the mode says which pass is running.
pub fn doctest_runtool(arguments: Vec<String>) -> i32 {
    match doctest_runtool_inner(&arguments) {
        Ok(code) => code,
        Err(error) => {
            eprintln!("[supercov] {error}");
            101
        }
    }
}

fn doctest_runtool_inner(arguments: &[String]) -> Result<i32, RustTestRunnerError> {
    let [package_dir, mode, binary, ..] = arguments else {
        return Err(RustTestRunnerError::UnsupportedCommand(
            "the doctest runtool needs a package directory, a mode and a binary".into(),
        ));
    };
    let package_dir = PathBuf::from(package_dir);
    let binary = PathBuf::from(binary);
    let index = next_sequence(&package_dir.join("invocations"), "")?;
    let directory = package_dir.join("invocations").join(format!("{index:08}"));
    fs::create_dir_all(&directory).map_err(io_error)?;
    let record_path = package_dir
        .join("invocations")
        .join(format!("{index:08}.json"));
    let write_record = |record: &Invocation| -> Result<(), RustTestRunnerError> {
        fs::write(&record_path, serde_json::to_vec(record)?).map_err(io_error)
    };

    match mode.as_str() {
        "list" => {
            // A merged harness lists its cases; a standalone binary would run
            // its doctest, but the list pass never reaches one: rustdoc lists
            // standalone cases itself and does not invoke the runtool.
            let output = Command::new(&binary)
                .output()
                .map_err(|error| RustTestRunnerError::Launch(error.to_string()))?;
            let text = String::from_utf8_lossy(&output.stdout);
            if !listed_names(&text).is_empty() {
                let listed = package_dir.join("listed");
                fs::create_dir_all(&listed).map_err(io_error)?;
                fs::write(listed.join(format!("{index:08}.txt")), text.as_bytes())
                    .map_err(io_error)?;
            }
            write_record(&Invocation::Listed)?;
            relay(&output);
            Ok(output.status.code().unwrap_or(1))
        }
        "merged" => {
            let program = std::env::current_exe()
                .and_then(fs::canonicalize)
                .map_err(io_error)?;
            let children = directory.join("children");
            fs::create_dir_all(&children).map_err(io_error)?;
            let output = Command::new(&binary)
                .env(RUSTDOC_BIN_PATH_ENV, &program)
                .env(CHILD_DIR_ENV, &children)
                .env(CHILD_BINARY_ENV, &binary)
                .output()
                .map_err(|error| RustTestRunnerError::Launch(error.to_string()))?;
            let text = String::from_utf8_lossy(&output.stdout);
            let lines = harness_lines(&text);
            // rustdoc built the harness from doctests sorted by name, so a
            // child's index is its position in that order; the order itself
            // was written by the wrapper from the list pass.
            let order: Vec<String> = serde_json::from_slice(
                &fs::read(package_dir.join("merged-order.json")).map_err(io_error)?,
            )?;
            let mut by_index = BTreeMap::new();
            if let Ok(entries) = fs::read_dir(&children) {
                for entry in entries.filter_map(Result::ok) {
                    let path = entry.path();
                    if path
                        .extension()
                        .is_some_and(|extension| extension == "json")
                    {
                        let child: ChildRecord =
                            serde_json::from_slice(&fs::read(&path).map_err(io_error)?)?;
                        by_index.insert(child.index, child);
                    }
                }
            }
            let lines = if lines.is_empty() {
                let selected: BTreeSet<String> = fs::read(package_dir.join("merged-selected.json"))
                    .ok()
                    .and_then(|bytes| serde_json::from_slice(&bytes).ok())
                    .unwrap_or_default();
                let manifest_dir = fs::read_to_string(package_dir.join("manifest-dir"))
                    .map(PathBuf::from)
                    .unwrap_or_else(|_| PathBuf::from("."));
                let ran = by_index.keys().copied().collect::<BTreeSet<_>>();
                quiet_cases(&text, &order, &selected, &ran, &manifest_dir).unwrap_or_default()
            } else {
                lines
            };
            let mut cases = Vec::new();
            for (name, status) in lines {
                let position = order.iter().position(|candidate| candidate == &name);
                let evidence = position
                    .and_then(|position| by_index.remove(&position))
                    .map(|child| child.evidence);
                cases.push(MergedCase {
                    name,
                    status,
                    evidence,
                });
            }
            if let Some((index, _)) = by_index.into_iter().next() {
                return Err(RustTestRunnerError::Context(format!(
                    "merged doctest child {index} ran for a case the harness did not report"
                )));
            }
            write_record(&Invocation::Merged { cases })?;
            relay(&output);
            Ok(output.status.code().unwrap_or(1))
        }
        "named" => {
            let evidence = directory.join("evidence");
            fs::create_dir_all(&evidence).map_err(io_error)?;
            let output = Command::new(&binary)
                .envs(instrumented_stack_environment())
                .env(EVIDENCE_DIR_ENV, &evidence)
                .output()
                .map_err(|error| RustTestRunnerError::Launch(error.to_string()))?;
            let text = String::from_utf8_lossy(&output.stdout);
            let exit = output.status.code().unwrap_or(1);
            // A merged harness that the per-name filter matched nothing in
            // announces itself; anything else is the named doctest.
            if text.contains("running 0 tests") {
                write_record(&Invocation::EmptyHarness)?;
            } else {
                let name = std::env::var(NAME_ENV).map_err(|_| {
                    RustTestRunnerError::Context(
                        "a per-name doctest pass did not name its doctest".into(),
                    )
                })?;
                write_record(&Invocation::Standalone {
                    name,
                    exit_code: exit,
                    evidence,
                })?;
            }
            relay(&output);
            Ok(exit)
        }
        other => Err(RustTestRunnerError::UnsupportedCommand(format!(
            "unknown doctest runtool mode {other}"
        ))),
    }
}

// --------------------------------------------------------------- the child

/// This program as the child a merged harness spawns for one case: run the
/// harness binary in rustdoc's single-case mode under this case's own
/// evidence directory, record how it went, and exit as it exited.
pub fn doctest_child() -> i32 {
    match doctest_child_inner() {
        Ok(code) => code,
        Err(error) => {
            eprintln!("[supercov] {error}");
            101
        }
    }
}

fn doctest_child_inner() -> Result<i32, RustTestRunnerError> {
    let index = std::env::var(RUSTDOC_RUN_NB_TEST_ENV)
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .ok_or_else(|| {
            RustTestRunnerError::Context("the doctest child has no case index".into())
        })?;
    let children = PathBuf::from(std::env::var_os(CHILD_DIR_ENV).ok_or_else(|| {
        RustTestRunnerError::Context("the doctest child has no record directory".into())
    })?);
    let binary = PathBuf::from(std::env::var_os(CHILD_BINARY_ENV).ok_or_else(|| {
        RustTestRunnerError::Context("the doctest child has no harness binary".into())
    })?);
    let evidence = children.join(format!("{index:08}"));
    fs::create_dir_all(&evidence).map_err(io_error)?;
    // The harness cleared RUSTDOC_DOCTEST_BIN_PATH before spawning us; make
    // sure of it, or the case would run the whole harness again.
    let output = Command::new(&binary)
        .env_remove(RUSTDOC_BIN_PATH_ENV)
        .env_remove(CHILD_DIR_ENV)
        .env_remove(CHILD_BINARY_ENV)
        .envs(instrumented_stack_environment())
        .env(EVIDENCE_DIR_ENV, &evidence)
        .output()
        .map_err(|error| RustTestRunnerError::Launch(error.to_string()))?;
    let exit = output.status.code().unwrap_or(1);
    fs::write(
        children.join(format!("{index:08}.json")),
        serde_json::to_vec(&ChildRecord {
            index,
            exit_code: exit,
            evidence,
        })?,
    )
    .map_err(io_error)?;
    relay(&output);
    Ok(exit)
}

/// Whether this process was started by a merged harness as a case's child.
pub fn is_doctest_child() -> bool {
    std::env::var_os(RUSTDOC_RUN_NB_TEST_ENV).is_some() && std::env::var_os(CHILD_DIR_ENV).is_some()
}

/// Whether this process is standing in for rustdoc.
pub fn is_rustdoc_wrapper(arguments: &[String]) -> bool {
    std::env::var_os(WRAPPER_ROOT_ENV).is_some()
        && arguments
            .first()
            .is_none_or(|first| first != RUNTOOL_MODE_ARGUMENT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn isolation_picks_a_filter_and_skips_for_one_doctest() {
        let names = vec![
            "src/lib.rs - classify (line 1)".to_owned(),
            "src/lib.rs - classify (line 11)".to_owned(),
            "src/lib.rs - classify_more (line 1)".to_owned(),
            "src/other.rs - render (line 3)".to_owned(),
        ];
        for name in &names {
            let (filter, skips) = isolate(name, &names).expect("isolated");
            assert!(name.contains(&filter));
            for other in names.iter().filter(|other| other != &name) {
                let selected =
                    other.contains(&filter) && !skips.iter().any(|skip| other.contains(skip));
                assert!(!selected, "{name}: {other} would also run");
            }
            assert!(!skips.iter().any(|skip| name.contains(skip)));
        }
        let twins = vec!["a b".to_owned(), "b a".to_owned()];
        assert_eq!(isolate("a b", &twins), None);
    }

    #[test]
    fn test_args_split_on_whitespace_like_rustdoc() {
        assert_eq!(
            split_test_args(&[
                "--exact  src/lib.rs - f (line 1)".to_owned(),
                "x".to_owned()
            ]),
            ["--exact", "src/lib.rs", "-", "f", "(line", "1)", "x"]
        );
    }

    #[test]
    fn rustdoc_arguments_separate_crate_harness_and_runtool() {
        let parsed = parse_rustdoc_arguments(&[
            "--edition=2024".into(),
            "--crate-name".into(),
            "probe".into(),
            "--test".into(),
            "src/lib.rs".into(),
            "--test-args".into(),
            "--nocapture".into(),
            "--test-args=foo".into(),
            "--test-runtool".into(),
            "/usr/bin/env".into(),
            "--test-runtool-arg".into(),
            "x".into(),
            "-L".into(),
            "dependency=deps".into(),
        ]);
        assert!(parsed.test);
        assert_eq!(parsed.harness_arguments, ["--nocapture", "foo"]);
        assert_eq!(
            parsed.crate_arguments,
            [
                "--edition=2024",
                "--crate-name",
                "probe",
                "--test",
                "src/lib.rs",
                "-L",
                "dependency=deps"
            ]
        );
    }

    #[test]
    fn harness_arguments_select_the_way_libtest_does() {
        let harness = parse_harness_arguments(&[
            "--nocapture".into(),
            "--skip".into(),
            "slow".into(),
            "--test-threads".into(),
            "2".into(),
            "classify".into(),
        ]);
        assert_eq!(harness.filters, ["classify"]);
        assert_eq!(harness.skips, ["slow"]);
        assert_eq!(harness.passthrough, ["--nocapture", "--test-threads", "2"]);
        assert!(harness.selects("src/lib.rs - classify (line 1)"));
        assert!(!harness.selects("src/lib.rs - classify_slow (line 9)"));
        assert!(!harness.selects("src/lib.rs - doubled (line 12)"));
        let exact =
            parse_harness_arguments(&["--exact".into(), "src/lib.rs - doubled (line 12)".into()]);
        assert!(exact.selects("src/lib.rs - doubled (line 12)"));
        assert!(!exact.selects("src/lib.rs - doubled (line 120)"));
        assert!(parse_harness_arguments(&[]).selects("anything"));
        // `cargo test -q` passes `--quiet`: dots instead of the names each
        // pass is checked by, so every doctest pass read as running none.
        let quiet = parse_harness_arguments(&[
            "--quiet".into(),
            "-q".into(),
            "--format".into(),
            "terse".into(),
            "--format=pretty".into(),
            "--nocapture".into(),
        ]);
        assert_eq!(quiet.passthrough, ["--nocapture"]);
        assert!(quiet.filters.is_empty());
    }

    #[test]
    fn a_quiet_merged_harness_is_named_from_its_children_failures_and_fences() {
        let root = std::env::temp_dir().join(format!(
            "supercov-quiet-doctests-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(
            root.join("src/lib.rs"),
            "/// ```\n/// a();\n/// ```\n/// ```ignore\n/// b();\n/// ```\n/// ```no_run\n/// c();\n/// ```\n/// ```\n/// d();\n/// ```\n",
        )
        .unwrap();
        let order = [
            "src/lib.rs - a (line 1)",
            "src/lib.rs - b (line 4)",
            "src/lib.rs - c (line 7)",
            "src/lib.rs - d (line 10)",
        ]
        .map(str::to_owned);
        let selected = order.iter().cloned().collect::<BTreeSet<_>>();
        let ran = BTreeSet::from([0, 3]);
        let text = "\nrunning 4 tests\n.i.\nsrc/lib.rs - d (line 10) --- FAILED\n\nfailures:\n\n---- src/lib.rs - d (line 10) stdout ----\nboom\n\nfailures:\n    src/lib.rs - d (line 10)\n\ntest result: FAILED. 2 passed; 1 failed; 1 ignored; 0 measured; 0 filtered out; finished in 0.00s\n";
        let cases = quiet_cases(text, &order, &selected, &ran, &root).unwrap();
        assert_eq!(
            cases,
            [
                ("src/lib.rs - a (line 1)", "passed"),
                ("src/lib.rs - b (line 4)", "skipped"),
                ("src/lib.rs - c (line 7)", "passed"),
                ("src/lib.rs - d (line 10)", "failed"),
            ]
            .map(|(name, status)| (name.to_owned(), status.to_owned()))
        );
        // Counts that do not add up to what the harness printed name nothing.
        let wrong = text.replace("2 passed", "3 passed");
        assert!(quiet_cases(&wrong, &order, &selected, &ran, &root).is_none());
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn harness_lines_drop_the_test_mode_libtest_appends() {
        assert_eq!(
            harness_lines(
                "test src/lib.rs - f (line 9) - compile ... ok\ntest src/lib.rs - g (line 2) - compile fail ... FAILED\ntest src/map.rs - h (line 1632) - should panic ... ok\n"
            ),
            [
                ("src/lib.rs - f (line 9)".to_owned(), "passed".to_owned()),
                ("src/lib.rs - g (line 2)".to_owned(), "failed".to_owned()),
                ("src/map.rs - h (line 1632)".to_owned(), "passed".to_owned()),
            ]
        );
    }

    #[test]
    fn harness_lines_and_listings_are_read_as_libtest_prints_them() {
        let text = "\nrunning 3 tests\ntest src/lib.rs - a (line 1) ... ok\ntest src/lib.rs - b (line 5) ... FAILED\ntest src/lib.rs - c (line 9) ... ignored, needs network\n\ntest result: FAILED. 1 passed; 1 failed; 1 ignored; 0 measured; 0 filtered out\n";
        assert_eq!(
            harness_lines(text),
            [
                ("src/lib.rs - a (line 1)".to_owned(), "passed".to_owned()),
                ("src/lib.rs - b (line 5)".to_owned(), "failed".to_owned()),
                ("src/lib.rs - c (line 9)".to_owned(), "skipped".to_owned()),
            ]
        );
        assert_eq!(
            listed_names(
                "src/lib.rs - a (line 1): test\nsrc/lib.rs - b (line 5): test\n\n3 tests, 0 benchmarks\n"
            ),
            ["src/lib.rs - a (line 1)", "src/lib.rs - b (line 5)"]
        );
    }

    fn strings(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| value.to_string()).collect()
    }

    /// rustdoc's own arguments and the harness's, taken apart the way each
    /// reads them; a flag left without its value at the end is dropped.
    #[test]
    fn rustdoc_and_harness_arguments_are_taken_apart_as_each_reads_them() {
        let parsed = parse_rustdoc_arguments(&strings(&[
            "--test",
            "--test-args",
            "a b",
            "--test-runtool",
            "x",
            "--test-runtool-arg",
            "y",
            "--test-args=c",
            "--test-runtool=z",
            "--crate-name",
            "k",
            "--test-args",
        ]));
        assert!(parsed.test);
        assert_eq!(parsed.crate_arguments, ["--test", "--crate-name", "k"]);
        assert_eq!(parsed.harness_arguments, ["a b", "c"]);
        assert!(!parse_rustdoc_arguments(&strings(&["--crate-name", "k"])).test);
        let harness = parse_harness_arguments(&strings(&[
            "--skip=s1",
            "--skip",
            "s2",
            "-q",
            "--format=terse",
            "--format",
            "json",
            "--exact",
            "--list",
            "--test-threads",
            "2",
            "-Z",
            "x",
            "--nocapture",
            "filter",
            "--logfile",
        ]));
        assert_eq!(harness.filters, ["filter"]);
        assert_eq!(harness.skips, ["s1", "s2"]);
        assert!(harness.exact && harness.list);
        assert_eq!(
            harness.passthrough,
            ["--test-threads", "2", "-Z", "x", "--nocapture", "--logfile"]
        );
        assert!(
            parse_harness_arguments(&strings(&["--skip"]))
                .skips
                .is_empty()
        );
    }

    /// libtest's result lines, read by name and outcome, the mode it
    /// appends taken off; a fence marks a doctest ignored by its attributes.
    #[test]
    fn harness_lines_and_fence_attributes_are_read_as_libtest_prints_them() {
        let lines = harness_lines(
            "running 5 tests\ntest a ... ok\ntest b ... FAILED\ntest c ... ignored\ntest d ... bench\nnoise\ntest src/lib.rs - f (line 3) - compile fail ... ok\n",
        );
        assert_eq!(
            lines,
            [
                ("a".to_owned(), "passed".to_owned()),
                ("b".to_owned(), "failed".to_owned()),
                ("c".to_owned(), "skipped".to_owned()),
                ("src/lib.rs - f (line 3)".to_owned(), "passed".to_owned()),
            ]
        );
        let root = std::env::temp_dir().join(format!(
            "supercov-fence-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(root.join("src")).unwrap();
        let root = crate::test_directory::TestDirectory::adopt(root);
        fs::write(
            root.join("src/lib.rs"),
            "/// ```ignore\n/// ```rust,ignore-wasm32\n/// ```rust no_run\n/// ```\n",
        )
        .unwrap();
        for (name, ignored) in [
            ("src/lib.rs - f (line 1)", true),
            ("src/lib.rs - f (line 2)", true),
            ("src/lib.rs - f (line 3)", false),
            ("src/lib.rs - f (line 4)", false),
            ("src/lib.rs - f (line 9)", false),
            ("src/missing.rs - f (line 1)", false),
            ("src/lib.rs - f", false),
            ("plain", false),
        ] {
            assert_eq!(fence_says_ignore(name, &root), ignored, "{name}");
        }
    }
}
