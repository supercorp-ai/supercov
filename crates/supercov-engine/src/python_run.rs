//! Public Python coverage run lifecycle.
//!
//! The project runs in place with its own interpreter, environment and test
//! command. Supercov prepares the complete obligation manifest and probe plan
//! from source, materialises its stdlib-only runtime under `.supercov/`,
//! points the interpreter at it through environment variables, supervises the
//! user's command unchanged, and publishes the joined evidence.

use std::{
    ffi::OsString,
    fs,
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
        ProjectLock, finalize_published_run, publish_run, recover_abandoned_runs,
        remove_stored_tree_deferred,
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

fn embedded_runtime_files() -> [(&'static str, &'static [u8]); 4] {
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
    ]
}

fn write_runtime(directory: &Path) -> Result<(), String> {
    fs::create_dir_all(directory).map_err(|error| format!("{}: {error}", directory.display()))?;
    for (name, contents) in embedded_runtime_files() {
        let path = directory.join(name);
        fs::write(&path, contents).map_err(|error| format!("{}: {error}", path.display()))?;
    }
    Ok(())
}

fn copy_tree(source: &Path, destination: &Path) -> Result<(), String> {
    for entry in fs::read_dir(source).map_err(|error| format!("{}: {error}", source.display()))? {
        let entry = entry.map_err(|error| error.to_string())?;
        let target = destination.join(entry.file_name());
        if entry
            .file_type()
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

fn prepend_path_list(existing: Option<OsString>, entry: &Path) -> OsString {
    let mut value = entry.as_os_str().to_owned();
    if let Some(existing) = existing.filter(|existing| !existing.is_empty()) {
        value.push(if cfg!(windows) { ";" } else { ":" });
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
    for key in [
        "SUPERCOV_PYTHON_PLAN",
        "SUPERCOV_PYTHON_EVIDENCE_DIR",
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
) -> Result<crate::run_store::RunIntegrity, String> {
    let root = canonicalize_simplified(root).map_err(|error| error.to_string())?;
    let files = crate::python_project::discover_python_files(&root)?;
    create_explicit_run_integrity(
        &root,
        &python_integrity_inputs(&files, command),
        &FrontendIntegrityInputs::embedded_python(),
    )
    .map_err(|error| error.to_string())
}

pub fn run_direct_python(
    request: &DirectPythonRunRequest,
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
        let project: PreparedPythonProject = prepare_python_project(&root)?;
        let integrity = create_explicit_run_integrity(
            &root,
            &python_integrity_inputs(&project.files, &request.command),
            &FrontendIntegrityInputs::embedded_python(),
        )
        .map_err(|error| error.to_string())?;
        let python_directory = work_directory.join("python");
        let runtime_directory = python_directory.join("runtime");
        let evidence_directory = python_directory.join("evidence");
        let plan_path = python_directory.join("plan.json");
        write_runtime(&runtime_directory)?;
        fs::create_dir_all(&evidence_directory).map_err(|error| error.to_string())?;
        fs::write(
            &plan_path,
            serde_json::to_vec(&project.plan).map_err(|error| error.to_string())?,
        )
        .map_err(|error| format!("{}: {error}", plan_path.display()))?;
        writeln!(
            diagnostics,
            "[supercov] detected Python; measuring {} source file(s) in place through CPython monitoring",
            project.plan.files.len()
        )
        .map_err(|error| error.to_string())?;
        for (file, reason) in &project.unparseable {
            writeln!(
                diagnostics,
                "[supercov] could not parse {file}: {reason}; it carries no obligations"
            )
            .map_err(|error| error.to_string())?;
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
                },
            },
        };
        let options = SupervisionOptions::from_environment().map_err(|error| error.to_string())?;
        let execution = execute_plan(&plan, options, diagnostics, |_, _| Ok(()))
            .map_err(|error| error.to_string())?;
        let test_command_ms = elapsed_ms(test_started);
        if let Some(signal) = execution.interrupted_signal {
            return Err(format!(
                "the test command was interrupted by {signal:?}; no run was published"
            ));
        }

        let publication_started = Instant::now();
        let verbose = std::env::var("SUPERCOV_VERBOSE")
            .or_else(|_| std::env::var("SUPERCOV_DEBUG"))
            .is_ok_and(|value| matches!(value.as_str(), "1" | "true" | "yes"));
        let run: PythonFrontendRun = build_python_frontend_run(
            &project.manifest,
            &evidence_directory,
            &request.run_id,
            &request.started_at,
            execution.exit_code,
        )
        .map_err(|error| error.to_string())?;
        validate_frontend_report_request(&run.declaration, &run.request)
            .map_err(|error| error.to_string())?;
        let joined_ms = elapsed_ms(publication_started);
        let archive_path = work_directory.join("evidence.raw.gz");
        let entries = run.archive_entries().map_err(|error| error.to_string())?;
        let serialized_ms = elapsed_ms(publication_started) - joined_ms;
        let raw = write_archive(entries, &archive_path).map_err(|error| error.to_string())?;
        if verbose {
            writeln!(
                diagnostics,
                "[supercov] python evidence: join={joined_ms}ms serialize={serialized_ms}ms archive={}ms",
                elapsed_ms(publication_started) - joined_ms - serialized_ms
            )
            .map_err(|error| error.to_string())?;
        }
        if std::env::var("SUPERCOV_KEEP_WORK").is_ok_and(|value| !value.is_empty()) {
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
        };
        let run_directory =
            publish_run(&root, &metadata, &archive_path).map_err(|error| error.to_string())?;
        finalize_published_run(&root, &request.run_id).map_err(|error| error.to_string())?;
        Ok(DirectPythonRunResult {
            run_id: request.run_id.clone(),
            run_directory,
            exit_code: execution.exit_code,
            tests: run.tests,
            source_files: project.plan.files.len(),
            interpreters: run.interpreters,
            python_versions: run.python_versions,
            recovered_runs,
            metadata,
        })
    })();
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
