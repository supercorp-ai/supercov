//! Public, isolated Go coverage run lifecycle.
//!
//! Supercov owns the probes here, which means it rewrites the module's own
//! sources. That cannot happen in the user's tree, so the run copies the
//! project into an isolated workspace, instruments it there, and runs the
//! user's command against the copy. Nothing in the tree the author edits is
//! touched.
//!
//! Two facts about `go test` shape the rest:
//!
//! - It builds and runs **one test binary per package**, each its own process
//!   with its own probe array. So each test package writes its own evidence
//!   file and the run merges them, rather than every process racing to write
//!   one path.
//! - It **caches** successful results. A cached package does not run, so it
//!   records nothing, and a run that silently measured half a suite is worse
//!   than one that took longer. The run adds `-count=1` when the command does
//!   not already say otherwise, and says so.

use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsString,
    fs,
    io::Write,
    path::{Path, PathBuf},
    time::Instant,
};

use serde::{Deserialize, Serialize};

use crate::{
    evidence_archive::write_archive,
    frontend_protocol::validate_frontend_report_request,
    go_instrumenter::{RUNTIME_ALIAS, RUNTIME_IMPORT, rewrite},
    go_project::{PreparedGoProject, go_integrity_inputs, module_path, prepare_go_project},
    go_test_harness::{instrument_test_file, probe_array_file, synthesized_harness},
    integrity::{FrontendIntegrityInputs, create_explicit_run_integrity},
    lifecycle::{
        ProjectLock, finalize_published_run, note_kept_evidence, publish_run,
        recover_abandoned_runs, remove_stored_tree_deferred,
    },
    orchestration::{ExecutionPhase, ExecutionPlan, PhaseKind, execute_plan},
    owned_evidence::{
        OwnedRunInputs, OwnedTestOutcome, build_frontend_run, go_coverage_model, go_declaration,
        merge_evidence, read_evidence,
    },
    process_supervision::{CommandSpec, SupervisionOptions},
    run_store::{RawEvidenceMetadata, RunMetadata, RunTimings},
    workspace::{canonicalize_simplified, prepare_cached_workspace, recover_cached_workspace},
};

/// Where the runtime package lands inside the workspace.
///
/// A directory whose name begins with `_`: the Go tool leaves those out of
/// `./...` and builds them when a file imports one, which every rewritten
/// file does. Under a plain name the package was listed with the project's
/// own in every run, `?  example.com/shop/supercov_runtime  [no test files]`.
/// Go 1.22 to 1.27 import it alike.
const RUNTIME_DIRECTORY: &str = "_supercov_runtime";

const RUNTIME_SOURCE: &str = include_str!("../runtime-assets/go/supercov/supercov.go");
/// The generic wrappers a decision used as a value calls, in a file of its
/// own behind `go1.18`, so a module older than generics still builds the rest.
const RUNTIME_VALUES_SOURCE: &str = include_str!("../runtime-assets/go/supercov/values.go");

/// The generated file that declares a package's probe array.
const PROBE_FILE: &str = "supercov_probes.go";

/// The generated file that arms the runtime and binds each test.
const HARNESS_FILE: &str = "supercov_generated_test.go";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DirectGoRunRequest {
    pub root: PathBuf,
    pub command: Vec<String>,
    pub run_id: String,
    pub started_at: String,
    /// Credit every test with what it reached, by running the package's tests
    /// one at a time.
    ///
    /// Probes are a store into one array the whole process shares, so a test
    /// that called `t.Parallel()` can only be credited with its own work if
    /// nothing else is running while it does it. `-parallel 1` is what buys
    /// that, and what it costs is the suite's own parallelism -- which is why
    /// it is asked for rather than assumed.
    #[serde(default)]
    pub exact_attribution: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DirectGoRunResult {
    pub run_id: String,
    pub run_directory: PathBuf,
    pub exit_code: i32,
    pub tests: usize,
    /// Test functions `go test` ran that no evidence can be credited to: one
    /// that called `t.Parallel()`, and every Example or Fuzz target. What they
    /// reached counts run-wide, so a run of nothing but these is a measured
    /// run with no per-test attribution -- which is a different thing from an
    /// empty suite, and the summary has to be able to say which it was.
    pub unattributed: usize,
    pub source_files: usize,
    /// Source files that did not parse, and so carry no obligations. A hole in
    /// the denominator travels with the number it was taken out of.
    pub unparseable_sources: usize,
    /// Test files that did not parse. Their tests still run and still reach
    /// what they reach; what is lost is the announcement that would name them
    /// for it.
    pub unparseable_tests: usize,
    pub packages: usize,
    pub recovered_runs: Vec<String>,
    pub metadata: RunMetadata,
}

fn elapsed_ms(started: Instant) -> f64 {
    (started.elapsed().as_secs_f64() * 10_000.0).round() / 10.0
}

/// One package whose tests will run, and where its evidence lands.
#[derive(Debug, Clone)]
struct GoTestPackage {
    /// Directory relative to the workspace root, `/`-separated, `.` for the
    /// module root.
    directory: String,
    evidence: PathBuf,
    /// Which file declared each test, so a result can point at its source.
    declared_in: BTreeMap<String, String>,
    /// What ran here and announced nothing: parallel tests, Examples, Fuzz
    /// targets.
    unattributed: BTreeSet<String>,
}

struct InstrumentedWorkspace {
    project: PreparedGoProject,
    packages: Vec<GoTestPackage>,
    /// Test files that did not parse, with the reason. They cost the tests
    /// they declare their attribution and nothing else.
    unparseable_tests: Vec<(String, String)>,
}

fn directory_of(relative: &str) -> String {
    match relative.rsplit_once('/') {
        Some((directory, _)) => directory.to_owned(),
        None => ".".to_owned(),
    }
}

/// The `package` a Go file declares.
///
/// Scanned rather than parsed: the clause is the first thing in a Go file that
/// is not a comment, so stripping comments and reading the next word gets the
/// same answer as a parse for a fraction of the cost — and preparing a project
/// already parses every file once, which is enough.
fn package_name(source: &str) -> Option<String> {
    let bytes = source.as_bytes();
    let mut at = 0;
    while at < bytes.len() {
        let rest = &source[at..];
        if rest.starts_with("//") {
            at += rest.find('\n').map_or(rest.len(), |end| end + 1);
        } else if rest.starts_with("/*") {
            // An unterminated comment is not Go; there is no clause to find.
            at += rest.find("*/").map_or(rest.len(), |end| end + 2);
        } else if bytes[at].is_ascii_whitespace() {
            at += 1;
        } else {
            return rest
                .strip_prefix("package")
                .and_then(|rest| rest.strip_prefix(|c: char| c.is_ascii_whitespace()))
                .and_then(|rest| rest.split_whitespace().next())
                // The instrumented text ends the clause with `;`, as the
                // rewriter writes an import on the same line.
                .map(|name| name.trim_end_matches(';').to_owned());
        }
    }
    None
}

fn read(path: &Path) -> Result<String, String> {
    fs::read_to_string(path).map_err(|error| format!("{}: {error}", path.display()))
}

/// A copy's file under the name Go is to report it by: the project's own.
///
/// Go puts a file's path, as the compiler read it, into every stack trace,
/// so a panic under Supercov named the copy: `…/.supercov/workspaces/
/// workspace/shop/crypto.go:24`. A line directive is how Go is told that a
/// file stands for another. It is the comment form, in front of the package
/// clause on the clause's own line and naming that line, so the file gains
/// no line: what reads the file as it lies, a coverage profile, counts the
/// same lines as what reads it through the directive, a stack trace. The
/// rewriting adds no line of its own either.
fn named_for_the_project(project: &Path, relative: &str, text: &str) -> String {
    let Ok(tree) = crate::go_instrumenter::parse(text) else {
        return text.to_owned();
    };
    let mut cursor = tree.root_node().walk();
    let Some(package) = tree
        .root_node()
        .children(&mut cursor)
        .find(|child| child.kind() == "package_clause")
    else {
        return text.to_owned();
    };
    let at = package.start_byte();
    let line = text[..at].rfind('\n').map_or(0, |end| end + 1);
    if text[line..at].starts_with(LINE_DIRECTIVE) {
        return text.to_owned();
    }
    let position = package.start_position();
    // With the platform's separator all along: Go writes the file's name as
    // it is given here, and on Windows `pkg/inner/inner.go` after the
    // project's directory came out as `pkg/inner/inner.go` in a compile
    // error where Go alone writes `pkg\inner\inner.go`.
    let named = relative
        .split('/')
        .fold(project.to_path_buf(), |path, part| path.join(part));
    format!(
        "{}{LINE_DIRECTIVE}{}:{}:{}*/{}",
        &text[..at],
        named.display(),
        position.row + 1,
        position.column + 1,
        &text[at..]
    )
}

const LINE_DIRECTIVE: &str = "/*line ";

fn write(path: &Path, contents: &str) -> Result<(), String> {
    // Every path written is a name joined onto a directory.
    let parent = path.parent().unwrap_or(path);
    fs::create_dir_all(parent).map_err(|error| format!("{}: {error}", parent.display()))?;
    fs::write(path, contents).map_err(|error| format!("{}: {error}", path.display()))
}

/// A file-system-safe name for a package's evidence, so two packages cannot
/// collide on one path.
fn evidence_name(directory: &str) -> String {
    let slug = directory
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect::<String>();
    format!("{slug}.bin")
}

/// Rewrite the workspace in place: instrumented sources, the runtime package
/// they import, a probe array per package, and a harness per test package.
/// A workspace's sources rewritten with probes: the project, each module's
/// runtime import, the probe count every package reserves, and the package
/// of each source directory.
struct InstrumentedSources {
    project: PreparedGoProject,
    runtimes: Vec<(String, String)>,
    probe_count: usize,
    source_packages: BTreeMap<String, String>,
}

impl InstrumentedSources {
    /// The runtime a file imports: its module's.
    fn import_for(&self, relative: &str) -> &str {
        runtime_for(&self.runtimes, relative)
    }
}

/// The runtime a file imports, of the modules' runtimes longest first.
/// `relative` names a file, never a module's directory itself.
fn runtime_for<'a>(runtimes: &'a [(String, String)], relative: &str) -> &'a str {
    runtimes
        .iter()
        .find(|(directory, _)| directory == "." || relative.starts_with(&format!("{directory}/")))
        .map(|(_, import)| import.as_str())
        .unwrap_or_default()
}

fn instrument_sources(
    project_root: &Path,
    workspace: &Path,
    roots: Option<&crate::source_discovery::ExplicitSourceRoots>,
) -> Result<InstrumentedSources, String> {
    let project = prepare_go_project(workspace, roots)?;

    // A go.work repository has several modules and no module at its root, so
    // each gets a runtime of its own: an import is resolved against the module
    // that names it, and one module cannot import a package inside another.
    let members = crate::go_project::workspace_modules(workspace);
    let directories = if members.is_empty() {
        vec![".".to_owned()]
    } else {
        members.into_iter().collect::<Vec<_>>()
    };
    let mut runtimes: Vec<(String, String)> = Vec::new();
    for directory in &directories {
        let at = workspace.join(directory);
        let module = module_path(&at).ok_or_else(|| {
            format!(
                "{}: no module path in go.mod, so Supercov cannot name the package its runtime is imported from",
                at.display()
            )
        })?;
        write(
            &at.join(RUNTIME_DIRECTORY).join("supercov.go"),
            RUNTIME_SOURCE,
        )?;
        write(
            &at.join(RUNTIME_DIRECTORY).join("values.go"),
            RUNTIME_VALUES_SOURCE,
        )?;
        runtimes.push((directory.clone(), format!("{module}/{RUNTIME_DIRECTORY}")));
    }
    // Longest first, so a nested module wins over the root it sits under.
    runtimes.sort_by_key(|(directory, _)| std::cmp::Reverse(directory.len()));
    let mut sources = InstrumentedSources {
        project,
        runtimes,
        probe_count: 0,
        source_packages: BTreeMap::new(),
    };

    // Probe ids are handed out across the whole module, so every package
    // reserves the same total: a probe from one package is the same index in
    // the binary that links it as a dependency.
    sources.probe_count = sources
        .project
        .probes
        .keys()
        .max()
        .map_or(0, |highest| *highest as usize + 1);

    // A directory's probe file joins the package most of its files declare:
    // one behind a constraint the build leaves out can name another (`package
    // main` for a tool), and a probe file in that package stops the real one
    // compiling. Its tests vote too, an external `p_test` for `p`, since they
    // are built with the package the probe file has to join.
    let mut declared: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for relative in &sources.project.files.tests {
        if let Some(name) = package_name(&read(&project_root.join(relative))?) {
            let name = name
                .strip_suffix("_test")
                .map_or(name.clone(), str::to_owned);
            declared
                .entry(directory_of(relative))
                .or_default()
                .push(name);
        }
    }
    for (relative, instrumented) in &sources.project.instrumented {
        let instrumented = instrumented.replace(RUNTIME_IMPORT, sources.import_for(relative));
        let path = workspace.join(relative);
        if let Some(name) = package_name(&instrumented) {
            declared
                .entry(directory_of(relative))
                .or_default()
                .push(name);
        }
        write(
            &path,
            &named_for_the_project(project_root, relative, &instrumented),
        )?;
    }
    let measured: BTreeSet<String> = sources
        .project
        .instrumented
        .iter()
        .map(|(relative, _)| directory_of(relative))
        .collect();
    let source_packages: BTreeMap<String, String> = declared
        .into_iter()
        .filter(|(directory, _)| measured.contains(directory))
        .map(|(directory, names)| {
            let mut counts: Vec<(usize, String)> = Vec::new();
            for name in names {
                match counts.iter_mut().find(|(_, seen)| *seen == name) {
                    Some((count, _)) => *count += 1,
                    None => counts.push((1, name)),
                }
            }
            // The first of the most declared, so a tie goes to the tests.
            let mut best = 0;
            for (index, (count, _)) in counts.iter().enumerate() {
                if *count > counts[best].0 {
                    best = index;
                }
            }
            (directory, counts.swap_remove(best).1)
        })
        .collect();
    for (directory, package) in &source_packages {
        write(
            &workspace.join(directory).join(PROBE_FILE),
            &probe_array_file(
                package,
                RUNTIME_ALIAS,
                sources.import_for(&format!("{directory}/x.go")),
                sources.probe_count,
            ),
        )?;
    }
    sources.source_packages = source_packages;
    Ok(sources)
}

fn instrument_workspace(
    project_root: &Path,
    workspace: &Path,
    evidence_directory: &Path,
    serialised: bool,
    roots: Option<&crate::source_discovery::ExplicitSourceRoots>,
) -> Result<InstrumentedWorkspace, String> {
    let InstrumentedSources {
        mut project,
        runtimes,
        probe_count,
        source_packages,
    } = instrument_sources(project_root, workspace, roots)?;
    let import_for = |relative: &str| runtime_for(&runtimes, relative);
    let mut unparseable_tests: Vec<(String, String)> = Vec::new();

    let mut by_directory: BTreeMap<String, Vec<&String>> = BTreeMap::new();
    for relative in &project.files.tests {
        by_directory
            .entry(directory_of(relative))
            .or_default()
            .push(relative);
    }

    let mut packages = Vec::new();
    for (directory, tests) in by_directory {
        let evidence = evidence_directory.join(evidence_name(&directory));
        let evidence_literal = evidence.to_string_lossy().replace('\\', "\\\\");
        let mut declared_in = BTreeMap::new();
        let mut unattributed = BTreeSet::new();
        let mut declares_test_main = false;
        // The harness joins whichever package the tests are in. A directory
        // can hold both `foo` and `foo_test` files; the internal one wins,
        // because that is where the instrumented sources are.
        let mut harness_package: Option<String> = None;
        for relative in tests {
            let path = workspace.join(relative);
            let source = read(&path)?;
            let Some(name) = package_name(&source) else {
                continue;
            };
            // The package a harness joins is read from the text, so it is
            // known whether or not the file parses -- and a package whose only
            // test file is unreadable still needs one.
            let internal = source_packages.get(&directory) == Some(&name);
            if internal || harness_package.is_none() {
                harness_package = Some(name);
            }
            let file =
                match instrument_test_file(&source, RUNTIME_ALIAS, &evidence_literal, serialised) {
                    Ok(file) => file,
                    Err(error) => {
                        // Taking the run down with it is the reflex a source file
                        // was corrected for, and it is worse here: a test file
                        // need not even be in the build for Supercov to refuse the
                        // suite the toolchain just passed. What it costs is the
                        // attribution of the tests it declares, because there is
                        // nowhere to put the announcement. What they reach still
                        // counts run-wide, like any execution no test could claim.
                        project.manifest.limitations.push(
                            crate::go_project::unparseable_limitation(relative, &error.to_string()),
                        );
                        unparseable_tests.push((relative.clone(), error.to_string()));
                        // A TestMain nobody could read is still a TestMain, and Go
                        // permits one per package. Synthesising a second does not
                        // fail to measure, it fails to build -- so this is scanned
                        // for rather than parsed for, which is the one question
                        // that cannot go unanswered.
                        declares_test_main |= declares_test_main_textually(&source);
                        continue;
                    }
                };
            declares_test_main |= file.declares_test_main;
            for test in &file.tests {
                declared_in.insert(test.clone(), relative.clone());
            }
            unattributed.extend(file.unattributed.iter().cloned());
            // Written also when nothing in it changed, for its name.
            write(
                &path,
                &named_for_the_project(
                    project_root,
                    relative,
                    &rewrite(&source, &file.edits).replace(RUNTIME_IMPORT, import_for(relative)),
                ),
            )?;
        }
        let Some(package) = harness_package else {
            continue;
        };
        write(
            &workspace.join(&directory).join(HARNESS_FILE),
            &synthesized_harness(
                &package,
                RUNTIME_ALIAS,
                import_for(&format!("{directory}/x.go")),
                probe_count,
                &project.decision_widths,
                &evidence_literal,
                declares_test_main,
            ),
        )?;
        packages.push(GoTestPackage {
            directory,
            evidence,
            declared_in,
            unattributed,
        });
    }
    Ok(InstrumentedWorkspace {
        project,
        packages,
        unparseable_tests,
    })
}

/// Whether a file declares `TestMain`, read from the text rather than a tree.
///
/// Only ever asked of a file that did not parse, which is exactly when there
/// is no tree to ask. It errs towards seeing one: believing in a `TestMain`
/// that is not there costs the package Supercov's own harness, while missing
/// one that is there declares it twice and the package stops compiling.
fn declares_test_main_textually(source: &str) -> bool {
    source.split("func TestMain").skip(1).any(|rest| {
        rest.trim_start()
            .strip_prefix('(')
            .is_some_and(|arguments| arguments.contains("testing.M"))
    })
}

/// What the run adds to Go's flags, given the command and the `GOFLAGS` it
/// would have run with: `-count=1`, since `go test` caches a package that
/// passed and a cached package does not run, so it records nothing; and, for
/// `--exact-attribution`, `-parallel=1`, so tests calling `t.Parallel()`
/// resume one at a time and each is the only test running when its probes
/// fire. A flag the author already set, on the command line or in
/// `GOFLAGS`, stays theirs. Through `GOFLAGS` they reach every `go test` the
/// command starts -- from a Makefile or a script as much as directly -- Go
/// leaves them out of commands that do not take them, and a flag the
/// command names itself still wins. Returns the new `GOFLAGS`, if any, and
/// which of the two it added.
fn go_flags(
    command: &[String],
    ambient: Option<&str>,
    exact_attribution: bool,
) -> (Option<String>, bool, bool) {
    let named = |flag: &str| {
        command
            .iter()
            .map(String::as_str)
            .chain(ambient.unwrap_or_default().split_whitespace())
            .any(|argument| {
                argument
                    .strip_prefix('-')
                    .map(|rest| rest.strip_prefix('-').unwrap_or(rest))
                    .is_some_and(|rest| {
                        rest == flag
                            || rest
                                .strip_prefix(flag)
                                .is_some_and(|value| value.starts_with('='))
                    })
            })
    };
    let fresh = !named("count");
    let serial = exact_attribution && !named("parallel");
    let added = [fresh.then_some("-count=1"), serial.then_some("-parallel=1")]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    let flags = (!added.is_empty()).then(|| {
        ambient
            .into_iter()
            .filter(|flags| !flags.trim().is_empty())
            .chain(added)
            .collect::<Vec<_>>()
            .join(" ")
    });
    (flags, fresh, serial)
}

/// The fingerprint a later query compares against the stored run.
pub fn current_go_integrity(
    root: &Path,
    command: &[String],
    source_roots: Option<&[String]>,
) -> Result<crate::run_store::RunIntegrity, String> {
    let root = canonicalize_simplified(root).map_err(|error| error.to_string())?;
    let files = crate::go_project::discover_go_files(&root)?;
    let mut inputs = go_integrity_inputs(&files, command);
    crate::source_discovery::fold_roots_into_configuration(
        &mut inputs.execution_configuration,
        source_roots,
    );
    create_explicit_run_integrity(&root, &inputs, &FrontendIntegrityInputs::embedded_go())
        .map_err(|error| error.to_string())
}

pub fn run_direct_go(
    request: &DirectGoRunRequest,
    diagnostics: &mut dyn Write,
) -> Result<DirectGoRunResult, String> {
    if request.command.is_empty() {
        return Err("test command must not be empty".into());
    }
    let total_started = Instant::now();
    let initialization_started = Instant::now();
    let root = canonicalize_simplified(&request.root)
        .map_err(|error| format!("{}: {error}", request.root.display()))?;
    let mut lock = ProjectLock::acquire(&root, &request.run_id, &request.started_at)
        .map_err(|error| error.to_string())?;
    let initialization_ms = elapsed_ms(initialization_started);
    let work_directory = root.join(".supercov/work").join(&request.run_id);
    // Whether the tests were measured. A run that failed before that has no
    // evidence worth keeping; one that failed after has evidence that cost the
    // suite its wall clock.
    let measured = std::cell::Cell::new(false);
    let result = (|| {
        let recovered_runs = recover_abandoned_runs(&root, &request.started_at)
            .map_err(|error| error.to_string())?;
        if !recovered_runs.is_empty() {
            writeln!(
                diagnostics,
                "[supercov] recovered abandoned run(s): {}",
                recovered_runs.join(", ")
            )
            .map_err(|error| error.to_string())?;
        }

        let adapter_started = Instant::now();
        let files = crate::go_project::discover_go_files(&root)?;
        let source_roots = crate::source_discovery::configured_source_roots(
            &std::env::vars().collect::<std::collections::BTreeMap<_, _>>(),
        );
        let mut integrity_inputs = go_integrity_inputs(&files, &request.command);
        crate::source_discovery::fold_roots_into_configuration(
            &mut integrity_inputs.execution_configuration,
            source_roots.as_deref(),
        );
        let source_inputs =
            crate::source_capture::capture(&root, "go", integrity_inputs.assertion_paths())?;
        let integrity = create_explicit_run_integrity(
            &root,
            &integrity_inputs,
            &FrontendIntegrityInputs::embedded_go(),
        )
        .map_err(|error| error.to_string())?;

        let workspace_started = Instant::now();
        recover_cached_workspace(&root, &lock).map_err(|error| error.to_string())?;
        let workspace =
            prepare_cached_workspace(&root, &lock, &[]).map_err(|error| error.to_string())?;
        let evidence_directory = work_directory.join("go/evidence");
        fs::create_dir_all(&evidence_directory).map_err(|error| error.to_string())?;
        // Decided before instrumenting, because whether a parallel test can be
        // announced depends on whether anything will be running beside it.
        let command = request.command.clone();
        let (go_flags, forced_fresh, serialised) = go_flags(
            &command,
            std::env::var("GOFLAGS").ok().as_deref(),
            request.exact_attribution,
        );
        let ambient = std::env::vars().collect::<std::collections::BTreeMap<_, _>>();
        let roots = crate::source_discovery::ExplicitSourceRoots::from_environment_in(
            &root, &workspace, &ambient,
        )
        .map_err(|error| error.to_string())?;
        let instrumented = instrument_workspace(
            &root,
            &workspace,
            &evidence_directory,
            serialised,
            roots.as_ref(),
        )?;
        let workspace_preparation_ms = elapsed_ms(workspace_started);
        let adapter_setup_ms = (elapsed_ms(adapter_started) - workspace_preparation_ms).max(0.0);
        writeln!(
            diagnostics,
            "[supercov] detected Go; instrumenting {} source file(s) across {} test package(s) in isolated workspace {}",
            instrumented.project.instrumented.len(),
            instrumented.packages.len(),
            workspace.display()
        )
        .map_err(|error| error.to_string())?;
        for (file, reason) in &instrumented.project.unparseable {
            writeln!(
                diagnostics,
                "[supercov] could not parse {file}: {reason}; it carries no obligations"
            )
            .map_err(|error| error.to_string())?;
        }
        for (file, reason) in &instrumented.unparseable_tests {
            writeln!(
                diagnostics,
                "[supercov] could not parse {file}: {reason}; the tests it declares run unattributed and what they reach counts run-wide"
            )
            .map_err(|error| error.to_string())?;
        }
        if instrumented.packages.is_empty() {
            return Err(
                "no Go test packages were found, so a run would measure nothing: Supercov needs at least one _test.go file"
                    .into(),
            );
        }
        // A file that does not parse is a hole in the denominator, and one
        // hole is reported and measured around. Every file being a hole is not
        // a hole: there is no denominator left, and a run of nothing satisfies
        // every floor there is. Reporting that as success with exit 0 is the
        // failure mode most likely to go unnoticed, so it is refused here the
        // same way a project with no test package is.
        if instrumented.project.instrumented.is_empty()
            && !instrumented.project.files.sources.is_empty()
        {
            return Err(format!(
                "none of the {} Go source file(s) could be parsed, so the run would measure a denominator of nothing: {}",
                instrumented.project.files.sources.len(),
                instrumented
                    .project
                    .unparseable
                    .iter()
                    .map(|(file, reason)| format!("{file}: {reason}"))
                    .collect::<Vec<_>>()
                    .join("; ")
            ));
        }

        if serialised {
            writeln!(
                diagnostics,
                "[supercov] added -parallel=1 for --exact-attribution: a test that calls t.Parallel() can only be credited with what it reached when nothing else is running. Your suite runs in order, which is slower."
            )
            .map_err(|error| error.to_string())?;
        }
        if forced_fresh {
            writeln!(
                diagnostics,
                "[supercov] added -count=1: `go test` caches passing packages, and a cached package does not run, so it would record no coverage"
            )
            .map_err(|error| error.to_string())?;
        }
        let test_started = Instant::now();
        let replaced = go_flags.is_some();
        let plan =
            ExecutionPlan {
                preparation: Vec::new(),
                test: ExecutionPhase {
                    name: "test".into(),
                    kind: PhaseKind::Test,
                    command: CommandSpec {
                        program: command[0].clone().into(),
                        arguments: command[1..].iter().map(OsString::from).collect(),
                        cwd: workspace.clone(),
                        // The run's id, for a test that wants to know it is
                        // measured; and Go's flags where Supercov added any.
                        environment: Some(
                            std::env::vars_os()
                                .filter(|(name, _)| {
                                    name != crate::rust_runtime::RUN_ID_ENV
                                        && (!replaced || name != "GOFLAGS")
                                })
                                .chain(go_flags.map(|flags| {
                                    (OsString::from("GOFLAGS"), OsString::from(flags))
                                }))
                                .chain([(
                                    OsString::from(crate::rust_runtime::RUN_ID_ENV),
                                    OsString::from(&request.run_id),
                                )])
                                .collect(),
                        ),
                        captured_output: None,
                        console: Some(crate::workspace::console_names(&root, &workspace)),
                    },
                },
            };
        let options = SupervisionOptions::from_environment().map_err(|error| error.to_string())?;
        // What is in the copy now is Supercov's; what the command adds to
        // it or changes is the command's, and belongs in the project.
        let before = crate::workspace::workspace_output_baseline(&workspace)
            .map_err(|error| error.to_string())?;
        let execution = execute_plan(&plan, options, diagnostics, |_, _| Ok(()))
            .map_err(|error| error.to_string())?;
        let test_command_ms = elapsed_ms(test_started);
        // `go test -coverprofile=cover.out` left no `cover.out` in the
        // project, a golden file a test rewrote stayed in the copy, and a
        // step after the tests that reads either found the old one or none.
        // Every Go file in the copy is rewritten and stays there.
        let rewritten = instrumented
            .project
            .files
            .sources
            .iter()
            .chain(&instrumented.project.files.tests)
            .map(PathBuf::from)
            .collect::<BTreeSet<_>>();
        let outputs = crate::workspace::sync_outputs_of(
            crate::workspace::CommandOutputs::Go,
            &root,
            &workspace,
            &before,
            &rewritten,
        )
        .map_err(|error| error.to_string())?;
        for line in crate::workspace::describe_command_outputs(&outputs) {
            writeln!(diagnostics, "{line}").map_err(|error| error.to_string())?;
        }
        if let Some(signal) = execution.interrupted_signal {
            return Err(format!(
                "the test command was interrupted by {}; no run was published",
                signal.name()
            ));
        }
        let exit_code = execution.exit_code;

        let publication_started = Instant::now();
        let mut parts = Vec::new();
        let mut outcomes = Vec::new();
        let mut silent = Vec::new();
        for package in &instrumented.packages {
            let Ok(bytes) = fs::read(&package.evidence) else {
                silent.push(package.directory.clone());
                continue;
            };
            let evidence = read_evidence(&bytes)
                .map_err(|error| format!("{}: {error}", package.evidence.display()))?;
            for test in &evidence.tests {
                outcomes.push(OwnedTestOutcome {
                    name: test.name.clone(),
                    runner: test.runner.clone(),
                    package: package.directory.clone(),
                    file: package.declared_in.get(&test.name).cloned(),
                    status: test.status.clone(),
                    attributed: !test.unattributed,
                });
            }
            parts.push(evidence);
        }
        if !silent.is_empty() {
            writeln!(
                diagnostics,
                "[supercov] {} test package(s) wrote no evidence and are absent from this run: {}",
                silent.len(),
                silent.join(", ")
            )
            .map_err(|error| error.to_string())?;
        }
        let evidence = merge_evidence(parts);
        let unattributed = instrumented
            .packages
            .iter()
            .map(|package| package.unattributed.len())
            .sum::<usize>();
        // What the run reached, whether or not a test can be named for it. A
        // package whose every test calls `t.Parallel()` announces nothing by
        // design -- the coverage is real and simply has no owner -- and
        // discarding the run there threw away the lines, branches and MC/DC of
        // a fully measured suite. `t.Parallel()` is idiomatic Go: one reported
        // codebase calls it in 935 of its 1144 test files.
        let reached_something = evidence.global.iter().any(|total| *total != 0);
        if outcomes.is_empty() && !reached_something {
            let because = if unattributed > 0 {
                format!(
                    "; {unattributed} test(s) ran without being able to announce themselves (t.Parallel, Example or Fuzz) and reached nothing measured either"
                )
            } else {
                String::new()
            };
            return Err(format!(
                "no Go test recorded evidence (the command exited {exit_code}); a run that measured nothing is not published{because}"
            ));
        }
        measured.set(true);
        let run = build_frontend_run(OwnedRunInputs {
            declaration: go_declaration(),
            environment: "go",
            manifest: &instrumented.project.manifest,
            probes: &instrumented.project.probes,
            evidence: &evidence,
            outcomes: &outcomes,
            run_id: &request.run_id,
            generated_at: &request.started_at,
            test_exit_code: exit_code,
            coverage_model: go_coverage_model(),
        })
        .map_err(|error| error.to_string())?;
        // Before anything is written: a run that cannot be read back is not a
        // run, and finding that out at publication is far better than finding
        // it out when someone asks for the report.
        validate_frontend_report_request(&run.declaration, &run.request)
            .map_err(|error| error.to_string())?;
        let archive_path = work_directory.join("evidence.raw.gz");
        let raw = write_archive(
            crate::source_capture::append(
                run.archive_entries().map_err(|error| error.to_string())?,
                &source_inputs,
            )?,
            &archive_path,
        )
        .map_err(|error| error.to_string())?;
        let evidence_publication_ms = elapsed_ms(publication_started);

        let timings = RunTimings {
            initialization_ms,
            workspace_preparation_ms,
            adapter_setup_ms,
            instrumented_build_ms: 0.0,
            test_command_ms,
            evidence_publication_ms,
        };
        let metadata = RunMetadata {
            id: request.run_id.clone(),
            started_at: request.started_at.clone(),
            duration_ms: elapsed_ms(total_started),
            command: request.command.clone(),
            test_exit_code: Some(exit_code),
            integrity,
            raw_evidence: RawEvidenceMetadata {
                schema_version: raw.schema_version,
                format: raw.format.into(),
                file: raw.file.into(),
                files: raw.files,
                uncompressed_bytes: raw.uncompressed_bytes,
                compressed_bytes: raw.compressed_bytes,
            },
            isolated_build: Some(true),
            instrumented_build_cache: None,
            timings: Some(timings),
            merged: None,
            parents: None,
            source_roots: source_roots.clone(),
            copy: Some(lock.copy()),
            merged_across: None,
            recorded_by: None,
            companions: None,
        };
        let run_directory =
            publish_run(&root, &metadata, &archive_path).map_err(|error| error.to_string())?;
        finalize_published_run(&root, &request.run_id).map_err(|error| error.to_string())?;
        Ok(DirectGoRunResult {
            run_id: request.run_id.clone(),
            run_directory,
            exit_code,
            tests: outcomes
                .iter()
                .map(|outcome| outcome.name.as_str())
                .collect::<BTreeSet<_>>()
                .len(),
            unattributed,
            source_files: instrumented.project.instrumented.len(),
            unparseable_sources: instrumented.project.unparseable.len(),
            unparseable_tests: instrumented.unparseable_tests.len(),
            packages: instrumented.packages.len(),
            recovered_runs,
            metadata,
        })
    })();
    let result = result.map_err(|error| {
        if !measured.get() {
            return error;
        }
        note_kept_evidence(
            &root,
            &work_directory.join("go/evidence"),
            &request.run_id,
            error,
        )
    });
    if result.is_err() {
        let _ = remove_stored_tree_deferred(&root, &work_directory);
    }
    let release = lock.release().map_err(|error| error.to_string());
    match (result, release) {
        (Ok(result), Ok(())) => Ok(result),
        (Err(error), _) => Err(error),
        (Ok(_), Err(error)) => Err(error),
    }
}

/// The file a server's main package gains: it arms the runtime and starts
/// the clock its windows are harvested on.
const SERVER_FILE: &str = "supercov_server.go";

/// Go measured inside a browser suite's run: a server its tests drive, built
/// from the copy the suite runs in by `go run` or `go build` in the command.
/// Its sources there are rewritten with probes, and each main package arms
/// the runtime in server mode: windows of what ran, harvested on a clock and
/// given to the browser test running throughout each, by time. A binary
/// built before the run is not this one and is not measured.
pub struct GoServer {
    project: PreparedGoProject,
    integrity: crate::run_store::RunIntegrity,
    source_inputs: crate::source_manifest::Inputs,
    source_roots: Option<Vec<String>>,
    directory: PathBuf,
}

impl GoServer {
    /// Instrument the copy's Go sources for a server, or None when the
    /// project has no Go module or no main package.
    pub fn prepare(
        root: &Path,
        workspace: &Path,
        run_id: &str,
        command: &[String],
    ) -> Result<Option<Self>, String> {
        if !workspace.join("go.mod").is_file()
            && crate::go_project::workspace_modules(workspace).is_empty()
        {
            return Ok(None);
        }
        let files = crate::go_project::discover_go_files(root)?;
        if files.sources.is_empty() {
            return Ok(None);
        }
        let ambient = std::env::vars().collect::<BTreeMap<_, _>>();
        // Roots named for the JavaScript source may hold no Go at all.
        let Ok(roots) = crate::source_discovery::ExplicitSourceRoots::from_environment_in(
            root, workspace, &ambient,
        ) else {
            return Ok(None);
        };
        let source_roots = crate::source_discovery::configured_source_roots(&ambient);
        let mut integrity_inputs = go_integrity_inputs(&files, command);
        crate::source_discovery::fold_roots_into_configuration(
            &mut integrity_inputs.execution_configuration,
            source_roots.as_deref(),
        );
        let source_inputs =
            crate::source_capture::capture(root, "go", integrity_inputs.assertion_paths())?;
        let integrity = create_explicit_run_integrity(
            root,
            &integrity_inputs,
            &FrontendIntegrityInputs::embedded_go(),
        )
        .map_err(|error| error.to_string())?;
        let sources = instrument_sources(root, workspace, roots.as_ref())?;
        let widths = sources
            .project
            .decision_widths
            .iter()
            .map(u8::to_string)
            .collect::<Vec<_>>()
            .join(", ");
        let mut mains = 0;
        for (directory, package) in &sources.source_packages {
            if package != "main" {
                continue;
            }
            mains += 1;
            write(
                &workspace.join(directory).join(SERVER_FILE),
                &format!(
                    "// Code generated by Supercov. DO NOT EDIT.\n\npackage main\n\nimport {RUNTIME_ALIAS} \"{}\"\n\nfunc init() {{\n\t{RUNTIME_ALIAS}.Serve({}, []uint8{{{widths}}})\n}}\n",
                    sources.import_for(&format!("{directory}/x.go")),
                    sources.probe_count,
                ),
            )?;
        }
        // A directory has a package only for a file instrumented in it.
        if mains == 0 {
            return Ok(None);
        }
        let directory = root.join(".supercov/work").join(run_id).join("go");
        fs::create_dir_all(directory.join("evidence")).map_err(|error| error.to_string())?;
        Ok(Some(Self {
            project: sources.project,
            integrity,
            source_inputs,
            source_roots,
            directory,
        }))
    }

    /// The source this server was instrumented from.
    pub fn source_fingerprint(&self) -> String {
        self.integrity.fingerprint.source.clone()
    }

    pub fn source_files(&self) -> usize {
        self.project.instrumented.len()
    }

    /// What the suite's environment needs for a server it builds to write
    /// its windows.
    pub fn environment(&self) -> BTreeMap<String, String> {
        BTreeMap::from([(
            "SUPERCOV_GO_SERVER_EVIDENCE_DIR".into(),
            self.directory.join("evidence").display().to_string(),
        )])
    }

    /// Publish what the server ran as a Go run of its own: each window
    /// credited, by time, to the one browser test whose span it falls in;
    /// what no single test's does is no test's. None when no instrumented
    /// server ran, or no window fell in a test.
    pub fn publish(
        self,
        root: &Path,
        _evidence_run_id: &str,
        metadata: RunMetadata,
        served: &[crate::served_tests::ServedTest],
        diagnostics: &mut dyn Write,
    ) -> Result<Option<(PathBuf, RunMetadata)>, String> {
        crate::owned_server::publish(
            root,
            crate::owned_server::OwnedServer {
                language: "Go",
                declaration: go_declaration(),
                environment: "go",
                coverage_model: go_coverage_model(),
                manifest: &self.project.manifest,
                probes: &self.project.probes,
                widths: &self.project.decision_widths,
                integrity: &self.integrity,
                source_inputs: &self.source_inputs,
                source_roots: self.source_roots.clone(),
                directory: &self.directory,
                source_files: self.project.instrumented.len(),
            },
            metadata,
            served,
            diagnostics,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_package_clause_is_found_past_whatever_precedes_it() {
        // A build tag, a licence header, a block comment: all of them sit
        // above the clause in real Go, and a reader that took the first line
        // would find none of them.
        assert_eq!(package_name("package main\n").as_deref(), Some("main"));
        assert_eq!(
            package_name("//go:build linux\n\n// Copyright.\npackage worker\n").as_deref(),
            Some("worker")
        );
        assert_eq!(
            package_name("/*\nA package comment\nspanning lines.\n*/\npackage doc\n").as_deref(),
            Some("doc")
        );
        assert_eq!(
            package_name("/* one line */ package inline\n").as_deref(),
            Some("inline")
        );
        // A trailing comment is not part of the name.
        assert_eq!(
            package_name("package api // the public surface\n").as_deref(),
            Some("api")
        );
        assert_eq!(package_name("import \"fmt\"\n"), None);
    }

    /// `go test` skips a package whose result it has cached, and a package
    /// that does not run records nothing; `--exact-attribution` needs tests
    /// to run one at a time. Both go in through GOFLAGS, after whatever
    /// GOFLAGS held, unless the author already set them anywhere.
    #[test]
    fn go_flags_add_only_what_the_author_did_not_set() {
        let words = |text: &str| text.split(' ').map(str::to_owned).collect::<Vec<_>>();
        for (command, ambient, exact, expected) in [
            (
                "go test ./...",
                None,
                false,
                (Some("-count=1"), true, false),
            ),
            ("make test", None, false, (Some("-count=1"), true, false)),
            (
                "go test ./...",
                None,
                true,
                (Some("-count=1 -parallel=1"), true, true),
            ),
            (
                "go test ./...",
                Some("-mod=vendor"),
                false,
                (Some("-mod=vendor -count=1"), true, false),
            ),
            (
                "go test ./...",
                Some("  "),
                false,
                (Some("-count=1"), true, false),
            ),
            ("go test -count=3", None, false, (None, false, false)),
            ("go test -count 3", None, false, (None, false, false)),
            ("go test --count=2", None, false, (None, false, false)),
            ("make test", Some("-count=2"), false, (None, false, false)),
            (
                "go test -parallel 4 ./...",
                None,
                true,
                (Some("-count=1"), true, false),
            ),
            (
                "make test",
                Some("-parallel=2 -count=1"),
                true,
                (None, false, false),
            ),
            (
                "go test -countx",
                None,
                false,
                (Some("-count=1"), true, false),
            ),
        ] {
            let (flags, fresh, serial) = go_flags(&words(command), ambient, exact);
            assert_eq!(
                (flags.as_deref(), fresh, serial),
                expected,
                "{command} {ambient:?} {exact}"
            );
        }
    }

    #[test]
    fn each_package_gets_an_evidence_file_of_its_own() {
        // One test binary per package, each its own process: sharing a path
        // would mean the last one to finish overwrote everyone else.
        assert_ne!(evidence_name("internal/auth"), evidence_name("internal/db"));
        assert_eq!(evidence_name("."), "_.bin");
        assert!(!evidence_name("internal/auth").contains('/'));
    }

    #[test]
    fn a_test_main_is_seen_in_a_file_nothing_could_parse() {
        // Only ever asked of a file that did not parse, which is exactly when
        // there is no tree to ask. Missing a real one declares a second
        // TestMain and the package stops compiling, so this errs towards
        // seeing one -- but not so far that any mention of the word counts.
        assert!(declares_test_main_textually(
            "package p\n\nfunc TestMain(m *testing.M) {\n\tos.Exit(m.Run())\n}\n"
        ));
        // Spacing is the author's business.
        assert!(declares_test_main_textually(
            "package p\n\nfunc TestMain( m  *testing.M ) {}\n"
        ));
        assert!(declares_test_main_textually(
            "package p\n\nfunc TestMain(m *testing.M) { broken( }\n"
        ));

        // `TestMainHelper` is an ordinary function whose name starts with the
        // word, exactly as `Testify` is not a test.
        assert!(!declares_test_main_textually(
            "package p\n\nfunc TestMainHelper(t *testing.T) {}\n"
        ));
        // A test that merely takes a *testing.T is not the package's harness.
        assert!(!declares_test_main_textually(
            "package p\n\nfunc TestMain(t *testing.T) {}\n"
        ));
        assert!(!declares_test_main_textually(
            "package p\n\n// func TestMain\n"
        ));
        assert!(!declares_test_main_textually("package p\n"));
    }

    /// The package clause past comments, the runtime of the module a file
    /// is in, and the flags Supercov adds only where the command has none.
    #[test]
    fn go_package_runtime_and_command_flags_read_the_way_go_does() {
        assert_eq!(
            package_name("// c\n/* b */ package main\n").as_deref(),
            Some("main")
        );
        assert_eq!(package_name("/* never closed").as_deref(), None);
        assert_eq!(package_name("").as_deref(), None);
        let runtimes = vec![
            ("svc/api".to_owned(), "example.com/api/rt".to_owned()),
            (".".to_owned(), "example.com/rt".to_owned()),
        ];
        assert_eq!(runtime_for(&runtimes, "svc/api/h.go"), "example.com/api/rt");
        assert_eq!(runtime_for(&runtimes, "svc/apix/h.go"), "example.com/rt");
        assert_eq!(
            runtime_for(&[("svc".to_owned(), "r".to_owned())], "other/x.go"),
            ""
        );
    }

    fn temporary_root(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "supercov-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        write(&root.join("go.mod"), "module example.com/m\n\ngo 1.22\n").unwrap();
        root
    }

    fn go_request(root: &Path, command: &[&str]) -> DirectGoRunRequest {
        DirectGoRunRequest {
            root: root.to_owned(),
            command: command.iter().map(|word| (*word).to_owned()).collect(),
            run_id: "2026-01-02T00-00-00-000Z".into(),
            started_at: "2026-01-02T00:00:00Z".into(),
            exact_attribution: false,
        }
    }

    const SOURCE: &str = "package m\n\nfunc One() int {\n\treturn 1\n}\n";

    #[test]
    fn a_file_already_named_by_a_line_directive_is_left_as_it_is() {
        let text = "/*line elsewhere.go:1:1*/package m\n";
        assert_eq!(named_for_the_project(Path::new("/p"), "m.go", text), text);
    }

    #[test]
    fn a_package_s_files_vote_its_probe_file_s_package_and_its_tests_its_harness() {
        let root = temporary_root("go-votes");
        // A test behind a constraint names another package; two sources
        // outvote it.
        write(
            &root.join("lib/a.go"),
            "package lib\n\nfunc A() int {\n\treturn 1\n}\n",
        )
        .unwrap();
        write(
            &root.join("lib/b.go"),
            "package lib\n\nfunc B() int {\n\treturn 2\n}\n",
        )
        .unwrap();
        write(
            &root.join("lib/tool_test.go"),
            "//go:build tools\n\npackage main\n",
        )
        .unwrap();
        // A file the grammar reads without a package clause votes for none.
        write(&root.join("lib/c.go"), "func C() int {\n\treturn 3\n}\n").unwrap();
        // Only external tests: the harness joins the first.
        write(&root.join("ext.go"), SOURCE).unwrap();
        for name in ["a_test.go", "b_test.go"] {
            write(
                &root.join(name),
                "package m_test\n\nimport \"testing\"\n\nfunc TestX(t *testing.T) {}\n",
            )
            .unwrap();
        }
        // A test file with no package clause votes for nothing and joins no
        // harness.
        write(&root.join("none/n.go"), "package none\n\nfunc N() {}\n").unwrap();
        write(&root.join("none/n_test.go"), "// nothing here\n").unwrap();
        let instrumented =
            instrument_workspace(&root, &root, &root.join("evidence"), false, None).unwrap();
        let probes = fs::read_to_string(root.join("lib").join(PROBE_FILE)).unwrap();
        assert!(probes.contains("package lib\n"), "{probes}");
        let harness = fs::read_to_string(root.join(HARNESS_FILE)).unwrap();
        assert!(harness.contains("package m_test\n"), "{harness}");
        assert!(!root.join("none").join(HARNESS_FILE).exists());
        let packages = instrumented
            .packages
            .iter()
            .map(|package| package.directory.as_str())
            .collect::<Vec<_>>();
        assert_eq!(packages, [".", "lib"]);
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_go_run_refuses_an_empty_command() {
        let root = std::env::temp_dir().join("supercov-go-empty-command-never-created");
        let error = run_direct_go(&go_request(&root, &[]), &mut Vec::new()).unwrap_err();
        assert_eq!(error, "test command must not be empty");
        assert!(!root.exists());
    }

    #[test]
    fn a_project_without_test_packages_is_refused_before_its_command_runs() {
        let root = temporary_root("go-no-tests");
        write(&root.join("m.go"), SOURCE).unwrap();
        // A run whose process died before it cleaned up.
        let abandoned = "2026-01-01T00-00-00-000Z";
        fs::create_dir_all(root.join(".supercov/work").join(abandoned)).unwrap();
        crate::lifecycle::write_run_state(
            &root,
            &crate::lifecycle::RunState {
                id: abandoned.into(),
                pid: u32::MAX,
                root: root.display().to_string(),
                workspace: root.display().to_string(),
                started_at: "start".into(),
                updated_at: "update".into(),
                status: crate::lifecycle::RunStateStatus::Testing,
                signal: None,
                error: None,
            },
        )
        .unwrap();
        let marker = root.join("ran");
        let command = format!("touch {}", marker.display());
        let mut diagnostics = Vec::new();
        let error = run_direct_go(
            &go_request(&root, &["sh", "-c", &command]),
            &mut diagnostics,
        )
        .unwrap_err();
        assert!(
            error.starts_with("no Go test packages were found"),
            "{error}"
        );
        assert!(!marker.exists());
        let diagnostics = String::from_utf8(diagnostics).unwrap();
        assert!(
            diagnostics.contains(&format!("recovered abandoned run(s): {abandoned}")),
            "{diagnostics}"
        );
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn parallel_tests_that_reached_nothing_are_named_in_the_refusal() {
        let root = temporary_root("go-parallel");
        write(&root.join("m.go"), SOURCE).unwrap();
        write(
            &root.join("m_test.go"),
            "package m\n\nimport \"testing\"\n\nfunc TestOne(t *testing.T) {\n\tt.Parallel()\n\tOne()\n}\n",
        )
        .unwrap();
        // Told not to cache, the run adds no -count=1 of its own.
        let mut diagnostics = Vec::new();
        let error =
            run_direct_go(&go_request(&root, &["true", "-count=1"]), &mut diagnostics).unwrap_err();
        assert!(
            error.contains("1 test(s) ran without being able to announce themselves"),
            "{error}"
        );
        let diagnostics = String::from_utf8(diagnostics).unwrap();
        assert!(!diagnostics.contains("added -count=1"), "{diagnostics}");
        assert!(
            diagnostics.contains("1 test package(s) wrote no evidence"),
            "{diagnostics}"
        );
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_project_of_tests_alone_runs_its_command() {
        let root = temporary_root("go-tests-only");
        write(
            &root.join("m_test.go"),
            "package m\n\nimport \"testing\"\n\nfunc TestNothing(t *testing.T) {}\n",
        )
        .unwrap();
        let error = run_direct_go(&go_request(&root, &["true"]), &mut Vec::new()).unwrap_err();
        assert!(error.starts_with("no Go test recorded evidence"), "{error}");
        fs::remove_dir_all(&root).ok();
    }

    /// Evidence of `probes` probes, the first reached, and no test.
    fn evidence_reaching_the_first_of(probes: usize) -> Vec<u8> {
        let mut bytes = (probes as u64).to_le_bytes().to_vec();
        for index in 0..probes {
            bytes.extend(u64::from(index == 0).to_le_bytes());
        }
        bytes.extend(0_u64.to_le_bytes());
        bytes.extend(0_u64.to_le_bytes());
        bytes
    }

    // Written to Unix paths and filesystem behaviour.
    #[cfg_attr(windows, ignore = "Unix paths and filesystem behaviour")]
    #[test]
    fn code_reached_with_no_test_to_name_is_still_a_run() {
        // Every test called t.Parallel(): nothing could be credited, and what
        // they reached is the run's.
        let root = temporary_root("go-unnamed");
        write(&root.join("m.go"), SOURCE).unwrap();
        write(
            &root.join("m_test.go"),
            "package m\n\nimport \"testing\"\n\nfunc TestOne(t *testing.T) {\n\tt.Parallel()\n}\n",
        )
        .unwrap();
        let probes = crate::go_project::prepare_go_project(&root, None)
            .unwrap()
            .probes
            .len();
        let prepared = root.with_extension("bin");
        fs::write(&prepared, evidence_reaching_the_first_of(probes)).unwrap();
        let request = go_request(&root, &["cp", "", ""]);
        let evidence = root
            .join(".supercov/work")
            .join(&request.run_id)
            .join("go/evidence/_.bin");
        let command = [
            "cp".to_owned(),
            prepared.display().to_string(),
            evidence.display().to_string(),
        ];
        let request = DirectGoRunRequest {
            command: command.to_vec(),
            ..request
        };
        let result = run_direct_go(&request, &mut Vec::new()).unwrap();
        assert_eq!(result.tests, 0);
        assert_eq!(result.unattributed, 1);
        assert!(result.run_directory.exists());

        // Measured and then not published: what was measured is kept.
        let run_id = "2026-01-03T00-00-00-000Z";
        let evidence = root
            .join(".supercov/work")
            .join(run_id)
            .join("go/evidence/_.bin");
        let runs = root.join(".supercov/runs");
        let command = format!(
            "cp {} {} && rm -rf {runs} && touch {runs}",
            prepared.display(),
            evidence.display(),
            runs = runs.display()
        );
        let request = DirectGoRunRequest {
            command: vec!["sh".into(), "-c".into(), command],
            run_id: run_id.into(),
            ..request
        };
        let error = run_direct_go(&request, &mut Vec::new()).unwrap_err();
        assert!(
            error.contains("the tests were measured and the run could not be published"),
            "{error}"
        );
        fs::remove_file(&prepared).ok();
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_server_needs_a_module_sources_and_a_main_package() {
        let root = temporary_root("go-server");
        let command = ["npm".to_owned(), "test".to_owned()];
        let prepare = |root: &Path| {
            GoServer::prepare(root, root, "2026-01-02T00-00-00-000Z", &command)
                .unwrap()
                .is_none()
        };
        // A module with nothing in it.
        assert!(prepare(&root));
        // A library is no server.
        write(&root.join("m.go"), SOURCE).unwrap();
        assert!(prepare(&root));
        // No module at all, then only a workspace's.
        fs::remove_file(root.join("go.mod")).unwrap();
        assert!(prepare(&root));
        write(&root.join("go.work"), "go 1.22\n\nuse ./svc\n").unwrap();
        write(
            &root.join("svc/go.mod"),
            "module example.com/svc\n\ngo 1.22\n",
        )
        .unwrap();
        assert!(prepare(&root));
        fs::remove_dir_all(&root).ok();
    }
}
