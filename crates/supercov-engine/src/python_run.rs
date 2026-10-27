//! Public Python coverage run lifecycle.
//!
//! The project runs in place with its own interpreter, environment and test
//! command. Supercov prepares the complete obligation manifest and probe plan
//! from source, materialises its stdlib-only runtime under `.supercov/`,
//! points the interpreter at it through environment variables, supervises the
//! user's command unchanged, and publishes the joined evidence.

use crate::fault_fs as fs;
use std::{
    collections::BTreeMap,
    ffi::OsString,
    io::Write,
    path::{Path, PathBuf},
    time::Instant,
};

use serde::{Deserialize, Serialize};

use crate::workspace::canonicalize_simplified;
use crate::{
    evidence_archive::write_archive,
    frontend_protocol::validate_frontend_report_request,
    integrity::{FrontendIntegrityInputs, create_explicit_run_integrity},
    lifecycle::{
        Archived, ProjectLock, finalize_published_run, note_kept_evidence, publish_archived_run,
        recover_abandoned_runs, remove_stored_tree_deferred,
    },
    orchestration::{ExecutionPhase, ExecutionPlan, PhaseKind, execute_plan},
    process_supervision::{CommandSpec, SupervisionOptions},
    python_evidence::{PythonFrontendRun, build_python_frontend_run},
    python_project::{PreparedPythonProject, prepare_python_project, python_integrity_inputs},
    run_store::{RawEvidenceMetadata, RunMetadata, RunTimings},
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DirectPythonRunRequest {
    pub root: PathBuf,
    pub command: Vec<String>,
    pub run_id: String,
    pub started_at: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DirectPythonRunResult {
    pub run_id: String,
    pub run_directory: PathBuf,
    pub exit_code: i32,
    pub tests: usize,
    pub source_files: usize,
    pub interpreters: usize,
    pub python_versions: Vec<String>,
    pub recovered_runs: Vec<String>,
    pub metadata: RunMetadata,
}

fn elapsed_ms(started: Instant) -> f64 {
    (started.elapsed().as_secs_f64() * 10_000.0).round() / 10.0
}

fn embedded_runtime_files() -> [(&'static str, &'static [u8]); 6] {
    [
        (
            "sitecustomize.py",
            include_bytes!("../runtime-assets/python/sitecustomize.py"),
        ),
        (
            "supercov_runtime.py",
            include_bytes!("../runtime-assets/python/supercov_runtime.py"),
        ),
        (
            "supercov_pytest.py",
            include_bytes!("../runtime-assets/python/supercov_pytest.py"),
        ),
        (
            "supercov_unittest.py",
            include_bytes!("../runtime-assets/python/supercov_unittest.py"),
        ),
        (
            "supercov_probes.py",
            include_bytes!("../runtime-assets/python/supercov_probes.py"),
        ),
        (
            "supercov_instrument.py",
            include_bytes!("../runtime-assets/python/supercov_instrument.py"),
        ),
    ]
}

fn write_runtime(directory: &Path) -> Result<(), String> {
    fs::create_dir_all(directory).map_err(|error| format!("{}: {error}", directory.display()))?;
    // The runtime has its files.
    embedded_runtime_files()
        .into_iter()
        .try_for_each(|(name, contents)| {
            let path = directory.join(name);
            fs::write(&path, contents).map_err(|error| format!("{}: {error}", path.display()))
        })
}

fn copy_tree(source: &Path, destination: &Path) -> Result<(), String> {
    for entry in fs::read_dir(source).map_err(|error| format!("{}: {error}", source.display()))? {
        let entry = entry.map_err(|error| error.to_string())?;
        let target = destination.join(entry.file_name());
        if fs::file_type(&entry)
            .map_err(|error| error.to_string())?
            .is_dir()
        {
            fs::create_dir_all(&target).map_err(|error| error.to_string())?;
            copy_tree(&entry.path(), &target)?;
        } else {
            fs::copy(entry.path(), &target).map_err(|error| error.to_string())?;
        }
    }
    Ok(())
}

/// What separates the entries of `PYTHONPATH`.
#[cfg(windows)]
const PATH_LIST_SEPARATOR: &str = ";";
#[cfg(not(windows))]
const PATH_LIST_SEPARATOR: &str = ":";

fn prepend_path_list(existing: Option<OsString>, entry: &Path) -> OsString {
    let mut value = entry.as_os_str().to_owned();
    if let Some(existing) = existing.filter(|existing| !existing.is_empty()) {
        value.push(PATH_LIST_SEPARATOR);
        value.push(existing);
    }
    value
}

fn append_list(existing: Option<OsString>, entry: &str, separator: &str) -> OsString {
    match existing.filter(|existing| !existing.is_empty()) {
        Some(existing) => {
            let mut value = existing;
            value.push(separator);
            value.push(entry);
            value
        }
        None => entry.into(),
    }
}

fn environment(
    root: &Path,
    run_id: &str,
    runtime_directory: &Path,
    plan_path: &Path,
    evidence_directory: &Path,
) -> Vec<(OsString, OsString)> {
    let mut variables = std::env::vars_os().collect::<Vec<_>>();
    let mut take = |key: &str| {
        let position = variables.iter().position(|(name, _)| name == key);
        position.map(|index| variables.remove(index).1)
    };
    let python_path = prepend_path_list(take("PYTHONPATH"), runtime_directory);
    let pytest_plugins = append_list(take("PYTEST_PLUGINS"), "supercov_pytest", ",");
    // Supercov no longer turns on pytest's assertion-pass hook. Assertion
    // sites come from line events on the lines the plan names, and the hook
    // made pytest build a failure explanation for every passing assertion --
    // 30 microseconds each on two integers, 140 on a nested dict, more than
    // everything else the runtime does put together. A user's own
    // `PYTEST_ADDOPTS` passes through untouched; if it enables the hook, the
    // plugin still records what the hook reports.
    let pytest_addopts = take("PYTEST_ADDOPTS").unwrap_or_default();
    for key in [
        "SUPERCOV_PYTHON_PLAN",
        "SUPERCOV_PYTHON_EVIDENCE_DIR",
        "SUPERCOV_PYTHON_SOURCE_ROOT",
        "SUPERCOV_RUN_ID",
        "SUPERCOV_PROJECT_ROOT",
        "SUPERCOV_CONTEXT",
        "SUPERCOV_PYTHON_WORKER",
    ] {
        take(key);
    }
    variables.extend([
        ("PYTHONPATH".into(), python_path),
        ("PYTEST_PLUGINS".into(), pytest_plugins),
        ("PYTEST_ADDOPTS".into(), pytest_addopts),
        (
            "SUPERCOV_PYTHON_PLAN".into(),
            plan_path.as_os_str().to_owned(),
        ),
        (
            "SUPERCOV_PYTHON_EVIDENCE_DIR".into(),
            evidence_directory.as_os_str().to_owned(),
        ),
        ("SUPERCOV_RUN_ID".into(), run_id.into()),
        ("SUPERCOV_PROJECT_ROOT".into(), root.as_os_str().to_owned()),
    ]);
    variables
}

/// The fingerprint a later query compares against the stored run: the same
/// discovery and inputs the run used, without preparing a plan.
pub fn current_python_integrity(
    root: &Path,
    command: &[String],
    source_roots: Option<&[String]>,
) -> Result<crate::run_store::RunIntegrity, String> {
    let root = canonicalize_simplified(root).map_err(|error| error.to_string())?;
    let mut files = crate::python_project::discover_python_files(&root)?;
    // The roots the run was measured under, not this shell's: the run
    // digested the files they kept, and nothing else would match it.
    match source_roots.filter(|roots| !roots.is_empty()) {
        Some(configured) => {
            crate::source_discovery::ExplicitSourceRoots::resolve(&root, configured)
                .map_err(|error| error.to_string())?
                .narrow(&mut files.sources, &mut files.excluded, String::as_str);
        }
        None => crate::source_discovery::leave_out_what_lies_beside(
            &mut files.sources,
            &mut files.excluded,
        ),
    }
    let mut inputs = python_integrity_inputs(&files, command);
    crate::source_discovery::fold_roots_into_configuration(
        &mut inputs.execution_configuration,
        source_roots,
    );
    create_explicit_run_integrity(&root, &inputs, &FrontendIntegrityInputs::embedded_python())
        .map_err(|error| error.to_string())
}

/// The lines of each captured Python file that hold an assertion -- an
/// `assert` statement or an `assert*` method call -- for the plan. The probe on
/// such a line marks where its test's assertion phase begins. Sorted and
/// deduplicated: a line with two assertions is one line to arm.
fn assertion_sites(files: &crate::source_manifest::Files) -> BTreeMap<String, Vec<usize>> {
    let mut sites = BTreeMap::<String, Vec<usize>>::new();
    for (file, text) in files.iter().filter(|(file, _)| file.ends_with(".py")) {
        let Ok(starts) = assertion_starts(text) else {
            continue;
        };
        let lines = sites.entry(file.clone()).or_default();
        lines.extend(
            starts
                .into_iter()
                .map(|start| text[..start].bytes().filter(|b| *b == b'\n').count() + 1),
        );
        lines.sort_unstable();
        lines.dedup();
    }
    sites.retain(|_, lines| !lines.is_empty());
    sites
}

/// The byte offset of every `assert` statement and `assert*` method call.
fn assertion_starts(source: &str) -> Result<Vec<usize>, String> {
    use ruff_python_ast::{
        Expr, Stmt,
        visitor::{Visitor, walk_expr, walk_stmt},
    };
    use ruff_text_size::Ranged;
    struct Collector(Vec<usize>);
    impl<'a> Visitor<'a> for Collector {
        fn visit_stmt(&mut self, stmt: &'a Stmt) {
            if let Stmt::Assert(_) = stmt {
                self.0.push(stmt.range().start().to_usize());
            }
            walk_stmt(self, stmt);
        }
        fn visit_expr(&mut self, expr: &'a Expr) {
            if let Expr::Call(call) = expr
                && let Expr::Attribute(attr) = call.func.as_ref()
                && attr.attr.as_str().starts_with("assert")
            {
                self.0.push(expr.range().start().to_usize());
            }
            walk_expr(self, expr);
        }
    }
    let parsed = ruff_python_parser::parse_module(source).map_err(|e| e.to_string())?;
    let mut collector = Collector(vec![]);
    for stmt in &parsed.syntax().body {
        collector.visit_stmt(stmt);
    }
    Ok(collector.0)
}

struct Archive {
    raw: crate::evidence_archive::EvidenceArchiveMetadata,
    analysed: Option<crate::coverage_report::CoverageReport>,
    serialized_ms: f64,
    archive_ms: f64,
}

/// Archive a run, analysed as the archive will analyse -- the request it
/// archives, with the run's integrity record, which the archive does not
/// hold -- while it is written: neither waits on the other.
fn archive_python_run(
    run: &PythonFrontendRun,
    source_inputs: &crate::source_manifest::Inputs,
    archive_path: &Path,
    publication_started: Instant,
    joined_ms: f64,
) -> Result<Archive, String> {
    let (archived, analysed) = std::thread::scope(|scope| {
        let analysis = scope.spawn(|| {
            crate::coverage_report::analyze_frontend_request(
                &run.declaration,
                &run.request,
                crate::coverage_report::TransportStats::none(),
            )
        });
        let archived = (|| {
            let entries = run.archive_entries().map_err(|error| error.to_string())?;
            let serialized_ms = elapsed_ms(publication_started) - joined_ms;
            let entries = crate::source_capture::append(entries, source_inputs)?;
            let raw = write_archive(entries, archive_path).map_err(|error| error.to_string())?;
            let archive_ms = elapsed_ms(publication_started) - joined_ms - serialized_ms;
            Ok::<_, String>((raw, serialized_ms, archive_ms))
        })();
        let analysed = analysis.join().unwrap_or_else(crate::threads::rethrow);
        (archived, analysed)
    });
    let (raw, serialized_ms, archive_ms) = archived?;
    Ok(Archive {
        raw,
        analysed: analysed.ok(),
        serialized_ms,
        archive_ms,
    })
}

pub fn run_direct_python(
    request: &DirectPythonRunRequest,
    diagnostics: &mut dyn Write,
) -> Result<DirectPythonRunResult, String> {
    run_direct_python_in(request, &std::env::vars().collect(), diagnostics)
}

/// A run under `ambient`, the environment Supercov was started in.
fn run_direct_python_in(
    request: &DirectPythonRunRequest,
    ambient: &std::collections::BTreeMap<String, String>,
    diagnostics: &mut dyn Write,
) -> Result<DirectPythonRunResult, String> {
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
            let _ = writeln!(
                diagnostics,
                "[supercov] recovered abandoned run(s): {}",
                recovered_runs.join(", ")
            );
        }

        let adapter_started = Instant::now();
        let roots = crate::source_discovery::ExplicitSourceRoots::from_environment(&root, ambient)
            .map_err(|error| error.to_string())?;
        let project: PreparedPythonProject = prepare_python_project(&root, roots.as_ref())?;
        let source_roots = crate::source_discovery::configured_source_roots(ambient);
        let mut integrity_inputs = python_integrity_inputs(&project.files, &request.command);
        crate::source_discovery::fold_roots_into_configuration(
            &mut integrity_inputs.execution_configuration,
            source_roots.as_deref(),
        );
        let integrity = create_explicit_run_integrity(
            &root,
            &integrity_inputs,
            &FrontendIntegrityInputs::embedded_python(),
        )
        .map_err(|error| error.to_string())?;
        let source_inputs = crate::source_capture::capture(
            &root,
            "python",
            python_integrity_inputs(&project.files, &request.command).assertion_paths(),
        )?;
        let python_directory = work_directory.join("python");
        let runtime_directory = python_directory.join("runtime");
        let evidence_directory = python_directory.join("evidence");
        let plan_path = python_directory.join("plan.json");
        write_runtime(&runtime_directory)?;
        fs::create_dir_all(&evidence_directory).map_err(|error| error.to_string())?;
        let plan = crate::python_instrumenter::PythonProbePlan {
            assertion_sites: assertion_sites(&source_inputs.files),
            ..project.plan.clone()
        };
        fs::write(
            &plan_path,
            serde_json::to_vec(&plan).expect("plain data serializes"),
        )
        .map_err(|error| format!("{}: {error}", plan_path.display()))?;
        let _ = writeln!(
            diagnostics,
            "[supercov] detected Python; measuring {} source file(s) in place through probes compiled at import",
            project.plan.files.len(),
        );
        for (file, reason) in &project.unparseable {
            let _ = writeln!(
                diagnostics,
                "[supercov] could not parse {file}: {reason}; it carries no obligations"
            );
        }
        let adapter_setup_ms = elapsed_ms(adapter_started);

        let test_started = Instant::now();
        let plan = ExecutionPlan {
            preparation: Vec::new(),
            test: ExecutionPhase {
                name: "test".into(),
                kind: PhaseKind::Test,
                command: CommandSpec {
                    program: request.command[0].clone().into(),
                    arguments: request.command[1..].iter().map(OsString::from).collect(),
                    cwd: root.clone(),
                    environment: Some(environment(
                        &root,
                        &request.run_id,
                        &runtime_directory,
                        &plan_path,
                        &evidence_directory,
                    )),
                    captured_output: None,
                    console: None,
                },
            },
        };
        let options = SupervisionOptions::from_environment().map_err(|error| error.to_string())?;
        let execution = execute_plan(&plan, options, diagnostics, |_, _| Ok(()))
            .map_err(|error| error.to_string())?;
        let test_command_ms = elapsed_ms(test_started);
        if let Some(signal) = execution.interrupted_signal {
            return Err(format!(
                "the test command was interrupted by {}; no run was published",
                signal.name()
            ));
        }

        let publication_started = Instant::now();
        let verbose = ambient
            .get("SUPERCOV_VERBOSE")
            .or_else(|| ambient.get("SUPERCOV_DEBUG"))
            .is_some_and(|value| matches!(value.as_str(), "1" | "true" | "yes"));
        measured.set(true);
        let mut run: PythonFrontendRun = match build_python_frontend_run(
            &project.manifest,
            &evidence_directory,
            &request.run_id,
            &request.started_at,
            execution.exit_code,
            &root,
        ) {
            // No test reported an outcome and the command itself failed: it
            // never ran its tests (`No module named pytest`, an error while
            // collecting). That is the command's failure, said in its own
            // output, not a Supercov bug to report with evidence.
            Err(
                crate::python_evidence::PythonEvidenceError::NoTests
                | crate::python_evidence::PythonEvidenceError::NoInterpreter,
            ) if execution.exit_code != 0 => {
                measured.set(false);
                return Err(format!(
                    "the test command exited {} before any test reported an outcome; its own output above says why, and no run was published",
                    execution.exit_code
                ));
            }
            // The interpreter or the runner the command used is not one
            // Supercov measures: the message says what to change, and the
            // evidence has nothing in it to diagnose.
            Err(
                error @ (crate::python_evidence::PythonEvidenceError::UnsupportedPython(_)
                | crate::python_evidence::PythonEvidenceError::NoTests
                | crate::python_evidence::PythonEvidenceError::NoInterpreter),
            ) => {
                measured.set(false);
                return Err(error.to_string());
            }
            result => result.map_err(|error| error.to_string())?,
        };
        validate_frontend_report_request(&run.declaration, &run.request)
            .map_err(|error| error.to_string())?;
        let joined_ms = elapsed_ms(publication_started);
        let archive_path = work_directory.join("evidence.raw.gz");
        run.request.integrity = serde_json::to_value(&integrity).ok();
        let Archive {
            raw,
            analysed,
            serialized_ms,
            archive_ms,
        } = archive_python_run(
            &run,
            &source_inputs,
            &archive_path,
            publication_started,
            joined_ms,
        )?;
        let analysis_ms = elapsed_ms(publication_started) - joined_ms;
        // The joined evidence is in the archive now, and publication reads it
        // from there. Held to the end for three counts, it was the largest
        // thing alive while publication analysed the archive: 1.4 GB on a
        // 3,900-test run, on top of the analysis's own.
        let (tests, interpreters, python_versions) =
            (run.tests, run.interpreters, run.python_versions.clone());
        if verbose {
            let _ = writeln!(
                diagnostics,
                "[supercov] python evidence: join={joined_ms}ms serialize={serialized_ms}ms archive={archive_ms}ms analysis={analysis_ms}ms (beside the archive)"
            );
        }
        if ambient
            .get("SUPERCOV_KEEP_WORK")
            .is_some_and(|value| !value.is_empty())
        {
            let debug_directory = root.join(".supercov/python-debug").join(&request.run_id);
            fs::create_dir_all(&debug_directory).map_err(|error| error.to_string())?;
            copy_tree(&python_directory, &debug_directory)?;
        }
        remove_stored_tree_deferred(&root, &python_directory).map_err(|error| error.to_string())?;
        let evidence_publication_ms = elapsed_ms(publication_started);
        let timings = RunTimings {
            initialization_ms,
            workspace_preparation_ms: 0.0,
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
            test_exit_code: Some(execution.exit_code),
            integrity,
            raw_evidence: RawEvidenceMetadata {
                schema_version: raw.schema_version,
                format: raw.format.into(),
                file: raw.file.into(),
                files: raw.files,
                uncompressed_bytes: raw.uncompressed_bytes,
                compressed_bytes: raw.compressed_bytes,
            },
            isolated_build: None,
            instrumented_build_cache: None,
            timings: Some(timings),
            merged: None,
            parents: None,
            source_roots: source_roots.clone(),
            copy: None,
            merged_across: None,
            recorded_by: None,
            companions: None,
        };
        drop(run);
        let run_directory = publish_archived_run(
            &root,
            &metadata,
            &archive_path,
            Archived {
                report: analysed,
                inputs: Some(source_inputs.manifest()),
            },
        )
        .map_err(|error| error.to_string())?;
        finalize_published_run(&root, &request.run_id).map_err(|error| error.to_string())?;
        Ok(DirectPythonRunResult {
            run_id: request.run_id.clone(),
            run_directory,
            exit_code: execution.exit_code,
            tests,
            source_files: project.plan.files.len(),
            interpreters,
            python_versions,
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
            &work_directory.join("python/evidence"),
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

/// Python measured inside a browser suite's run: the server its tests drive,
/// which Playwright's `webServer` or a fixture starts. Prepared whenever the
/// project has Python source, since only the server's launch says which
/// program it is; a run whose suite starts no measured Python publishes
/// nothing of it.
pub struct PythonServer {
    project: PreparedPythonProject,
    integrity: crate::run_store::RunIntegrity,
    source_inputs: crate::source_manifest::Inputs,
    source_roots: Option<Vec<String>>,
    directory: PathBuf,
}

impl PythonServer {
    /// The plan and runtime for the project's Python source, or None when it
    /// has none. `workspace` is where the suite runs: the probes find the
    /// server's files there.
    pub fn prepare(
        root: &Path,
        workspace: &Path,
        run_id: &str,
        command: &[String],
    ) -> Result<Option<Self>, String> {
        let ambient = std::env::vars().collect::<BTreeMap<_, _>>();
        // Roots named for the JavaScript source may hold no Python at all.
        let Ok(roots) =
            crate::source_discovery::ExplicitSourceRoots::from_environment(root, &ambient)
        else {
            return Ok(None);
        };
        let Some(project) =
            crate::python_project::prepare_python_server_project(root, roots.as_ref())?
        else {
            return Ok(None);
        };
        if project.plan.files.is_empty() {
            return Ok(None);
        }
        let source_roots = crate::source_discovery::configured_source_roots(&ambient);
        let mut integrity_inputs = python_integrity_inputs(&project.files, command);
        crate::source_discovery::fold_roots_into_configuration(
            &mut integrity_inputs.execution_configuration,
            source_roots.as_deref(),
        );
        let integrity = create_explicit_run_integrity(
            root,
            &integrity_inputs,
            &FrontendIntegrityInputs::embedded_python(),
        )
        .map_err(|error| error.to_string())?;
        let source_inputs = crate::source_capture::capture(
            root,
            "python",
            python_integrity_inputs(&project.files, command).assertion_paths(),
        )?;
        let directory = root.join(".supercov/work").join(run_id).join("python");
        write_runtime(&directory.join("runtime"))?;
        fs::create_dir_all(directory.join("evidence")).map_err(|error| error.to_string())?;
        let plan = crate::python_instrumenter::PythonProbePlan {
            root: workspace.display().to_string(),
            ..project.plan.clone()
        };
        let plan_path = directory.join("plan.json");
        fs::write(
            &plan_path,
            serde_json::to_vec(&plan).expect("plain data serializes"),
        )
        .map_err(|error| format!("{}: {error}", plan_path.display()))?;
        Ok(Some(Self {
            project,
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
        self.project.plan.files.len()
    }

    /// What the suite's environment needs for a Python process it starts to
    /// be measured; the run id and project root are the browser run's own.
    pub fn environment(&self) -> BTreeMap<String, String> {
        let python_path = prepend_path_list(
            std::env::var_os("PYTHONPATH"),
            &self.directory.join("runtime"),
        );
        BTreeMap::from([
            (
                "PYTHONPATH".into(),
                python_path.to_string_lossy().into_owned(),
            ),
            (
                "SUPERCOV_PYTHON_PLAN".into(),
                self.directory.join("plan.json").display().to_string(),
            ),
            (
                "SUPERCOV_PYTHON_EVIDENCE_DIR".into(),
                self.directory.join("evidence").display().to_string(),
            ),
            (
                "SUPERCOV_PYTHON_SOURCE_ROOT".into(),
                self.project.root.display().to_string(),
            ),
        ])
    }

    /// Publish what the server ran as a Python run of its own, credited to
    /// the browser tests that sent each request. None when no measured
    /// Python process ran, or when none answered a test's request: Python a
    /// suite only ran in passing (a build tool, a seed script) is not a
    /// server's coverage.
    #[allow(clippy::too_many_arguments)]
    pub fn publish(
        self,
        root: &Path,
        evidence_run_id: &str,
        metadata: RunMetadata,
        served: &[crate::served_tests::ServedTest],
        diagnostics: &mut dyn Write,
    ) -> Result<Option<(PathBuf, RunMetadata)>, String> {
        let started = Instant::now();
        let evidence_directory = self.directory.join("evidence");
        let result = (|| {
            // No browser test reported: the suite was not Playwright's, or
            // never got to its tests.
            if served.is_empty() {
                return Ok(None);
            }
            let mut run = match crate::python_evidence::build_python_server_run(
                &self.project.manifest,
                &evidence_directory,
                evidence_run_id,
                &metadata.started_at,
                metadata.test_exit_code.unwrap_or_default(),
                Path::new(&self.project.plan.root),
                served,
            ) {
                Err(
                    crate::python_evidence::PythonEvidenceError::NoInterpreter
                    | crate::python_evidence::PythonEvidenceError::NoTests,
                ) => return Ok(None),
                result => result.map_err(|error| error.to_string())?,
            };
            let answered = run.request.raw_results.iter().any(|raw| {
                raw.role == "test" && raw.runtime.iter().any(|snapshot| !snapshot.hits.is_empty())
            });
            if !answered {
                let _ = writeln!(
                    diagnostics,
                    "[supercov] a Python process ran, and no request a test sent reached measured Python code; no Python run was published"
                );
                return Ok(None);
            }
            validate_frontend_report_request(&run.declaration, &run.request)
                .map_err(|error| error.to_string())?;
            // The evidence was recorded under the browser run's id; a run
            // published beside it has its own.
            run.request.run_id = metadata.id.clone();
            for scope in run
                .request
                .raw_results
                .iter_mut()
                .filter_map(|raw| raw.scope.as_mut())
            {
                scope.run_id = metadata.id.clone();
            }
            run.request.integrity = serde_json::to_value(&self.integrity).ok();
            let work = root.join(".supercov/work").join(&metadata.id);
            fs::create_dir_all(&work).map_err(|error| error.to_string())?;
            let archive_path = work.join("python-evidence.raw.gz");
            let Archive { raw, analysed, .. } =
                archive_python_run(&run, &self.source_inputs, &archive_path, started, 0.0)?;
            let tests = run.tests;
            drop(run);
            let metadata = RunMetadata {
                integrity: self.integrity.clone(),
                raw_evidence: RawEvidenceMetadata {
                    schema_version: raw.schema_version,
                    format: raw.format.into(),
                    file: raw.file.into(),
                    files: raw.files,
                    uncompressed_bytes: raw.uncompressed_bytes,
                    compressed_bytes: raw.compressed_bytes,
                },
                source_roots: self.source_roots.clone(),
                ..metadata
            };
            let run_directory = publish_archived_run(
                root,
                &metadata,
                &archive_path,
                Archived {
                    report: analysed,
                    inputs: Some(self.source_inputs.manifest()),
                },
            )
            .map_err(|error| error.to_string())?;
            let _ = writeln!(
                diagnostics,
                "[supercov] Python server coverage: {tests} test(s) across {} source file(s)",
                self.project.plan.files.len()
            );
            Ok(Some((run_directory, metadata)))
        })();
        let result = result.map_err(|error| {
            note_kept_evidence(
                root,
                &evidence_directory,
                &metadata_id_hint(evidence_run_id),
                error,
            )
        });
        let _ = remove_stored_tree_deferred(root, &self.directory);
        result
    }
}

fn metadata_id_hint(run_id: &str) -> String {
    format!("{run_id}-python")
}

#[cfg(test)]
mod tests {

    // The plan names each test file's assertion lines, so the runtime can
    // mark where a test's assertion phase begins from the line event that runs
    // it, and pytest's assertion-pass hook -- a failure explanation built for
    // every passing assertion -- stays off.
    #[test]
    fn the_plan_names_each_test_files_assertion_lines_once_and_sorted() {
        let files = crate::source_manifest::Files::from([
            ("tests/test_b.py".to_string(), "def test_b():\n    x = 1\n\n\n\n\n\n\n    assert x\n".to_string()),
            (
                "tests/test_a.py".to_string(),
                // Two assertions on line 12 are one line to arm.
                "import unittest\n\nassert True\n\n\n\n\n\n\nclass T(unittest.TestCase):\n    def test_a(self):\n        self.assertEqual(1, 1); assert 1\n".to_string(),
            ),
            ("src/app.py".to_string(), "def f():\n    return 1\n".to_string()),
            ("README.md".to_string(), "assert nothing\n".to_string()),
        ]);
        let sites = assertion_sites(&files);
        assert_eq!(
            sites,
            BTreeMap::from([
                ("tests/test_a.py".to_string(), vec![3, 12]),
                ("tests/test_b.py".to_string(), vec![9]),
            ])
        );

        // Serialized under the key the runtime reads, and absent -- not an
        // empty object -- when there is nothing to arm.
        let plan = crate::python_instrumenter::PythonProbePlan {
            version: 1,
            root: "/p".into(),
            files: BTreeMap::new(),
            assertion_sites: sites,
        };
        let json = serde_json::to_string(&plan).unwrap();
        assert!(
            json.contains("\"assertionSites\":{\"tests/test_a.py\":[3,12]"),
            "{json}"
        );
        let empty = crate::python_instrumenter::PythonProbePlan {
            assertion_sites: BTreeMap::new(),
            ..plan
        };
        assert!(
            !serde_json::to_string(&empty)
                .unwrap()
                .contains("assertionSites")
        );
    }
    use super::*;
    use crate::source_discovery::{ExplicitSourceRoots, fold_roots_into_configuration};

    fn fixture(name: &str) -> PathBuf {
        static UNIQUE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "supercov-python-run-{}-{}-{name}",
            std::process::id(),
            UNIQUE.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&root);
        for (file, contents) in [
            ("a.py", "def f(x):\n    return x if x else 0\n"),
            ("vendor/pkg/b.py", "def g():\n    return 1\n"),
            ("test/test_a.py", "from a import f\n"),
        ] {
            let path = root.join(file);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, contents).unwrap();
        }
        canonicalize_simplified(&root).unwrap()
    }

    #[test]
    fn a_rooted_run_is_current_whatever_shell_asks() {
        // A run recorded under roots digested only the files they kept, while
        // the query that later judged it rediscovered every file: the two could
        // never agree, and each rooted run read as stale the moment it was
        // written. The query has to replay the roots the run recorded, because
        // the shell it runs in is usually not the one the run came from.
        let root = fixture("current");
        let configured = vec!["a.py".to_owned()];
        let command = vec!["python".to_owned(), "-m".to_owned(), "unittest".to_owned()];

        let roots = ExplicitSourceRoots::resolve(&root, &configured).unwrap();
        let project = crate::python_project::prepare_python_project(&root, Some(&roots)).unwrap();
        let mut inputs = python_integrity_inputs(&project.files, &command);
        fold_roots_into_configuration(&mut inputs.execution_configuration, Some(&configured));
        let recorded = create_explicit_run_integrity(
            &root,
            &inputs,
            &FrontendIntegrityInputs::embedded_python(),
        )
        .unwrap();

        let replayed = current_python_integrity(&root, &command, Some(&configured)).unwrap();
        assert_eq!(recorded.fingerprint, replayed.fingerprint);

        // Judged without the roots, it is a different run -- which is what the
        // query used to do.
        let ignoring = current_python_integrity(&root, &command, None).unwrap();
        assert_ne!(recorded.fingerprint, ignoring.fingerprint);

        // A change the roots left out is not a change to this run; one they
        // kept is.
        fs::write(root.join("vendor/pkg/b.py"), "def g():\n    return 2\n").unwrap();
        assert_eq!(
            current_python_integrity(&root, &command, Some(&configured))
                .unwrap()
                .fingerprint,
            recorded.fingerprint,
            "a vendored file changing does not make a rooted run stale"
        );
        fs::write(root.join("a.py"), "def f(x):\n    return 1\n").unwrap();
        assert_ne!(
            current_python_integrity(&root, &command, Some(&configured))
                .unwrap()
                .fingerprint,
            recorded.fingerprint,
            "a first-party file changing does"
        );
        fs::remove_dir_all(root).ok();
    }

    fn python_request(root: &Path, command: &[&str]) -> DirectPythonRunRequest {
        DirectPythonRunRequest {
            root: root.to_owned(),
            command: command.iter().map(|word| (*word).to_owned()).collect(),
            run_id: "2026-01-02T00-00-00-000Z".into(),
            started_at: "2026-01-02T00:00:00Z".into(),
        }
    }

    /// A run whose process died before it cleaned up.
    fn abandoned_run(root: &Path) -> &'static str {
        let abandoned = "2026-01-01T00-00-00-000Z";
        fs::create_dir_all(root.join(".supercov/work").join(abandoned)).unwrap();
        crate::lifecycle::write_run_state(
            root,
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
        abandoned
    }

    #[test]
    fn a_python_run_refuses_an_empty_command() {
        let root = std::env::temp_dir().join("supercov-python-empty-command-never-created");
        let error = run_direct_python(&python_request(&root, &[]), &mut Vec::new()).unwrap_err();
        assert_eq!(error, "test command must not be empty");
        assert!(!root.exists());
    }

    #[test]
    fn a_command_that_started_no_hooked_python_publishes_nothing() {
        let root = fixture("no-interpreter");
        let abandoned = abandoned_run(&root);
        let mut diagnostics = Vec::new();
        let error =
            run_direct_python(&python_request(&root, &["true"]), &mut diagnostics).unwrap_err();
        assert!(
            error.starts_with("no Supercov-hooked Python interpreter ran"),
            "{error}"
        );
        let diagnostics = String::from_utf8(diagnostics).unwrap();
        assert!(
            diagnostics.contains(&format!("recovered abandoned run(s): {abandoned}")),
            "{diagnostics}"
        );
        assert!(!root.join(".supercov/failed-evidence").exists());
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn evidence_that_cannot_be_read_after_measuring_is_kept() {
        let root = fixture("unreadable-evidence");
        let request = python_request(
            &root,
            &[
                "sh",
                "-c",
                "echo x > \"$SUPERCOV_PYTHON_EVIDENCE_DIR/notes.txt\"",
            ],
        );
        let error = run_direct_python(&request, &mut Vec::new()).unwrap_err();
        assert!(error.contains("notes.txt"), "{error}");
        assert!(
            error.contains("the tests were measured and the run could not be published"),
            "{error}"
        );
        assert!(
            root.join(".supercov/failed-evidence")
                .join(&request.run_id)
                .join("notes.txt")
                .exists()
        );
        fs::remove_dir_all(&root).ok();
    }

    /// Each place copying a tree and preparing a server touches a file
    /// fails in turn: each stops with an error, never a panic; an empty tree
    /// copies to nothing.
    #[test]
    fn copying_and_preparing_a_server_survive_any_file_operation_failing() {
        let command = ["npx".to_owned(), "playwright".to_owned(), "test".to_owned()];
        let steps = crate::fault::fail_each_call(
            "fs",
            || fixture("faults"),
            |root| {
                let copy = root.with_extension("copy");
                fs::create_dir_all(&copy).map_err(|error| error.to_string())?;
                copy_tree(root, &copy)?;
                PythonServer::prepare(root, root, "2026-01-02T00-00-00-000Z", &command).map(drop)
            },
            |root| {
                let _ = fs::remove_dir_all(root.with_extension("copy"));
                let _ = fs::remove_dir_all(root);
            },
        );
        assert!(steps > 5, "{steps}");
        let empty = fixture("empty-tree");
        let nothing = empty.join("nothing");
        fs::create_dir_all(&nothing).unwrap();
        let copy = empty.join("copy");
        fs::create_dir_all(&copy).unwrap();
        copy_tree(&nothing, &copy).unwrap();
        assert_eq!(fs::read_dir(&copy).unwrap().count(), 0);
        fs::remove_dir_all(&empty).ok();
    }

    #[test]
    fn a_tree_is_copied_whole_and_a_path_list_keeps_what_it_held() {
        let root = fixture("copied");
        let copy = root.with_extension("copy");
        fs::create_dir_all(&copy).unwrap();
        copy_tree(&root, &copy).unwrap();
        assert_eq!(
            fs::read_to_string(copy.join("vendor/pkg/b.py")).unwrap(),
            "def g():\n    return 1\n"
        );
        assert_eq!(
            prepend_path_list(Some("/site".into()), Path::new("/runtime")),
            OsString::from(format!("/runtime{PATH_LIST_SEPARATOR}/site"))
        );
        assert_eq!(
            prepend_path_list(Some("".into()), Path::new("/runtime")),
            OsString::from("/runtime")
        );
        fs::remove_dir_all(&root).ok();
        fs::remove_dir_all(&copy).ok();
    }

    fn served_test() -> crate::served_tests::ServedTest {
        crate::served_tests::ServedTest {
            worker: "playwright:w0".into(),
            test_id: "one".into(),
            retry: 0,
            name: "one".into(),
            title: "one".into(),
            file: "tests/a.spec.js".into(),
            status: "passed".into(),
            expected_status: "passed".into(),
            provenance: Default::default(),
            window: Some((100, 200)),
            attempt_id: None,
        }
    }

    fn server_metadata(server: &PythonServer) -> RunMetadata {
        RunMetadata {
            id: "2026-01-03T00-00-00-000Z".into(),
            started_at: "2026-01-03T00:00:00Z".into(),
            duration_ms: 1.0,
            command: vec!["npx".into(), "playwright".into(), "test".into()],
            test_exit_code: Some(0),
            integrity: server.integrity.clone(),
            raw_evidence: RawEvidenceMetadata {
                schema_version: 2,
                format: "supercov-evidence-archive".into(),
                file: "evidence.raw.gz".into(),
                files: 0,
                uncompressed_bytes: 0,
                compressed_bytes: 0,
            },
            isolated_build: None,
            instrumented_build_cache: None,
            timings: None,
            merged: None,
            parents: None,
            source_roots: None,
            copy: None,
            merged_across: None,
            recorded_by: None,
            companions: None,
        }
    }

    #[test]
    fn a_server_publishes_only_what_a_browser_test_s_request_reached() {
        let root = fixture("server");
        let command = ["npx".to_owned(), "playwright".to_owned(), "test".to_owned()];
        let run_id = "2026-01-02T00-00-00-000Z";
        let prepare = || {
            PythonServer::prepare(&root, &root, run_id, &command)
                .unwrap()
                .unwrap()
        };
        // No browser test reported.
        let server = prepare();
        let metadata = server_metadata(&server);
        assert!(
            server
                .publish(&root, run_id, metadata, &[], &mut Vec::new())
                .unwrap()
                .is_none()
        );
        // A Python process ran, and only at import: no request reached it.
        let server = prepare();
        let statement = server.project.manifest.points[0].id.clone();
        crate::python_evidence::tests::write_transport(
            &server.directory.join("evidence/main.1.mmap"),
            &[
                serde_json::json!({"t":"process","v":1,"run":run_id,"pid":1,"worker":"main","python":"3.13.1","executable":"python","argv":["app.py"]}),
                serde_json::json!({"t":"hit","ctx":0,"id":statement}),
                serde_json::json!({"t":"exit","at":300}),
            ],
            0,
        );
        let metadata = server_metadata(&server);
        let mut diagnostics = Vec::new();
        assert!(
            server
                .publish(&root, run_id, metadata, &[served_test()], &mut diagnostics)
                .unwrap()
                .is_none()
        );
        assert!(
            String::from_utf8(diagnostics)
                .unwrap()
                .contains("no request a test sent reached measured Python code")
        );
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_server_of_python_that_does_not_parse_is_no_server() {
        let root = std::env::temp_dir().join(format!(
            "supercov-python-run-{}-unparseable",
            std::process::id()
        ));
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("app.py"), "def (\n").unwrap();
        let command = ["npx".to_owned(), "playwright".to_owned(), "test".to_owned()];
        assert!(
            PythonServer::prepare(&root, &root, "2026-01-02T00-00-00-000Z", &command)
                .unwrap()
                .is_none()
        );
        fs::remove_dir_all(&root).ok();
    }

    /// `SUPERCOV_VERBOSE` (or `SUPERCOV_DEBUG`) set to a yes prints the
    /// publication's timings, and `SUPERCOV_KEEP_WORK` keeps the work
    /// directory beside the project; anything else leaves both out.
    #[test]
    fn a_verbose_run_says_how_long_publication_took_and_a_kept_one_keeps_its_work() {
        let root = fixture("verbose");
        fs::write(
            root.join("test_a.py"),
            "import unittest\n\nfrom a import f\n\n\nclass T(unittest.TestCase):\n    def test_f(self):\n        self.assertEqual(f(2), 2)\n",
        )
        .unwrap();
        let command = ["python3", "-m", "unittest", "-q", "test_a"];
        let run = |run_id: &str, ambient: &[(&str, &str)]| {
            let request = DirectPythonRunRequest {
                run_id: run_id.into(),
                ..python_request(&root, &command)
            };
            let ambient = ambient
                .iter()
                .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
                .collect();
            let mut diagnostics = Vec::new();
            let result = run_direct_python_in(&request, &ambient, &mut diagnostics).unwrap();
            assert_eq!((result.exit_code, result.tests), (0, 1));
            let kept = root.join(".supercov/python-debug").join(run_id).exists();
            let timed = String::from_utf8(diagnostics)
                .unwrap()
                .contains("[supercov] python evidence: join=");
            (timed, kept)
        };
        assert_eq!(
            run(
                "2026-01-02T00-00-00-001Z",
                &[("SUPERCOV_VERBOSE", "1"), ("SUPERCOV_KEEP_WORK", "1")]
            ),
            (true, true)
        );
        assert_eq!(
            run(
                "2026-01-02T00-00-00-002Z",
                &[("SUPERCOV_DEBUG", "yes"), ("SUPERCOV_KEEP_WORK", "")]
            ),
            (true, false)
        );
        assert_eq!(
            run("2026-01-02T00-00-00-003Z", &[("SUPERCOV_VERBOSE", "0")]),
            (false, false)
        );
        fs::remove_dir_all(&root).ok();
    }
}
