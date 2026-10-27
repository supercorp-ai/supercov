//! Public Ruby coverage run lifecycle.
//!
//! The project runs in place with its own interpreter, bundle and test
//! command. Supercov prepares the complete obligation manifest and probe plan
//! from source, materialises its stdlib-only runtime under `.supercov/`, loads
//! it through `RUBYOPT`, supervises the user's command unchanged, and
//! publishes the joined evidence.

use crate::fault_fs as fs;
use std::{
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
        ProjectLock, finalize_published_run, note_kept_evidence, publish_run,
        recover_abandoned_runs, remove_stored_tree_deferred,
    },
    orchestration::{ExecutionPhase, ExecutionPlan, PhaseKind, execute_plan},
    process_supervision::{CommandSpec, SupervisionOptions},
    ruby_evidence::{RubyFrontendRun, build_ruby_frontend_run},
    ruby_project::{PreparedRubyProject, prepare_ruby_project, ruby_integrity_inputs},
    run_store::{RawEvidenceMetadata, RunMetadata, RunTimings},
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DirectRubyRunRequest {
    pub root: PathBuf,
    pub command: Vec<String>,
    pub run_id: String,
    pub started_at: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DirectRubyRunResult {
    pub run_id: String,
    pub run_directory: PathBuf,
    pub exit_code: i32,
    pub tests: usize,
    pub source_files: usize,
    pub interpreters: usize,
    pub ruby_versions: Vec<String>,
    pub recovered_runs: Vec<String>,
    pub metadata: RunMetadata,
}

fn elapsed_ms(started: Instant) -> f64 {
    (started.elapsed().as_secs_f64() * 10_000.0).round() / 10.0
}

fn embedded_runtime_files() -> [(&'static str, &'static [u8]); 5] {
    [
        (
            "supercov_runtime.rb",
            include_bytes!("../runtime-assets/ruby/supercov_runtime.rb"),
        ),
        (
            "supercov_rspec.rb",
            include_bytes!("../runtime-assets/ruby/supercov_rspec.rb"),
        ),
        (
            "supercov_minitest.rb",
            include_bytes!("../runtime-assets/ruby/supercov_minitest.rb"),
        ),
        (
            "supercov_testunit.rb",
            include_bytes!("../runtime-assets/ruby/supercov_testunit.rb"),
        ),
        (
            "supercov_cucumber.rb",
            include_bytes!("../runtime-assets/ruby/supercov_cucumber.rb"),
        ),
    ]
}

fn write_runtime(directory: &Path) -> Result<(), String> {
    fs::create_dir_all(directory).map_err(|error| format!("{}: {error}", directory.display()))?;
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

/// The test command's environment: `inherited`, the one Supercov was
/// started with, plus what loads the runtime.
fn environment(
    mut variables: Vec<(OsString, OsString)>,
    root: &Path,
    run_id: &str,
    runtime_directory: &Path,
    plan_path: &Path,
    evidence_directory: &Path,
) -> Vec<(OsString, OsString)> {
    let mut take = |key: &str| {
        let position = variables.iter().position(|(name, _)| name == key);
        position.map(|index| variables.remove(index).1)
    };
    // `-r` with an absolute path loads the runtime before the main script in
    // every Ruby the command starts, including bundler and forked workers.
    let mut rubyopt = OsString::from("-r");
    rubyopt.push(runtime_directory.join("supercov_runtime.rb"));
    if let Some(existing) = take("RUBYOPT").filter(|existing| !existing.is_empty()) {
        rubyopt.push(" ");
        rubyopt.push(existing);
    }
    for key in [
        "SUPERCOV_RUBY_PLAN",
        "SUPERCOV_RUBY_EVIDENCE_DIR",
        "SUPERCOV_RUBY_SOURCE_ROOT",
        "SUPERCOV_RUN_ID",
        "SUPERCOV_PROJECT_ROOT",
        "SUPERCOV_CONTEXT",
        "SUPERCOV_RUBY_WORKER",
    ] {
        take(key);
    }
    variables.extend([
        ("RUBYOPT".into(), rubyopt),
        (
            "SUPERCOV_RUBY_PLAN".into(),
            plan_path.as_os_str().to_owned(),
        ),
        (
            "SUPERCOV_RUBY_EVIDENCE_DIR".into(),
            evidence_directory.as_os_str().to_owned(),
        ),
        ("SUPERCOV_RUN_ID".into(), run_id.into()),
        ("SUPERCOV_PROJECT_ROOT".into(), root.as_os_str().to_owned()),
        (
            BOOTSNAP_COMPILE_CACHE.0.into(),
            BOOTSNAP_COMPILE_CACHE.1.into(),
        ),
    ]);
    variables
}

/// Bootsnap, which every Rails app loads from `config/boot.rb`, compiles each
/// file once and loads the cached instructions after, past the loader that
/// puts Supercov's probes in: lines still came from Ruby's own Coverage, and
/// branches and conditions from nowhere -- 0.08% of a Rails server's branches
/// against 1.9% with the cache off. Supercov compiles every measured file
/// itself, so the cache saves nothing here.
const BOOTSNAP_COMPILE_CACHE: (&str, &str) = ("DISABLE_BOOTSNAP_COMPILE_CACHE", "1");

/// The fingerprint a later query compares against the stored run: the same
/// discovery and inputs the run used, without preparing a plan.
pub fn current_ruby_integrity(
    root: &Path,
    command: &[String],
    source_roots: Option<&[String]>,
) -> Result<crate::run_store::RunIntegrity, String> {
    let root = canonicalize_simplified(root).map_err(|error| error.to_string())?;
    let mut files = crate::ruby_project::discover_ruby_files(&root)?;
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
    let mut inputs = ruby_integrity_inputs(&files, command);
    crate::source_discovery::fold_roots_into_configuration(
        &mut inputs.execution_configuration,
        source_roots,
    );
    create_explicit_run_integrity(&root, &inputs, &FrontendIntegrityInputs::embedded_ruby())
        .map_err(|error| error.to_string())
}

pub fn run_direct_ruby(
    request: &DirectRubyRunRequest,
    diagnostics: &mut dyn Write,
) -> Result<DirectRubyRunResult, String> {
    run_direct_ruby_in(request, &std::env::vars().collect(), diagnostics)
}

/// A run under `ambient`, the environment Supercov was started in.
fn run_direct_ruby_in(
    request: &DirectRubyRunRequest,
    ambient: &std::collections::BTreeMap<String, String>,
    diagnostics: &mut dyn Write,
) -> Result<DirectRubyRunResult, String> {
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
        let project: PreparedRubyProject = prepare_ruby_project(&root, roots.as_ref())?;
        let source_roots = crate::source_discovery::configured_source_roots(ambient);
        let mut integrity_inputs = ruby_integrity_inputs(&project.files, &request.command);
        crate::source_discovery::fold_roots_into_configuration(
            &mut integrity_inputs.execution_configuration,
            source_roots.as_deref(),
        );
        let integrity = create_explicit_run_integrity(
            &root,
            &integrity_inputs,
            &FrontendIntegrityInputs::embedded_ruby(),
        )
        .map_err(|error| error.to_string())?;
        let source_inputs = crate::source_capture::capture(
            &root,
            "ruby",
            ruby_integrity_inputs(&project.files, &request.command).assertion_paths(),
        )?;
        let ruby_directory = work_directory.join("ruby");
        let runtime_directory = ruby_directory.join("runtime");
        let evidence_directory = ruby_directory.join("evidence");
        // The plan is a Ruby literal rather than JSON: the runtime is
        // required through `RUBYOPT` before Bundler runs, and loading the
        // `json` default gem there would clash with the version an
        // application's Gemfile pins.
        let plan_path = ruby_directory.join("plan.rb");
        write_runtime(&runtime_directory)?;
        fs::create_dir_all(&evidence_directory).map_err(|error| error.to_string())?;
        fs::write(
            &plan_path,
            ruby_literal(&serde_json::to_value(&project.plan).expect("plain data serializes"))
                .into_bytes(),
        )
        .map_err(|error| format!("{}: {error}", plan_path.display()))?;
        let _ = writeln!(
            diagnostics,
            "[supercov] detected Ruby; measuring {} source file(s) in place through Ruby's Coverage module and load-time probes",
            project.plan.files.len()
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
                        std::env::vars_os().collect(),
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

        if ambient
            .get("SUPERCOV_KEEP_WORK")
            .is_some_and(|value| !value.is_empty())
        {
            // Kept before the join so a rejected evidence file stays inspectable.
            let debug_directory = root.join(".supercov/ruby-debug").join(&request.run_id);
            fs::create_dir_all(&debug_directory).map_err(|error| error.to_string())?;
            copy_tree(&ruby_directory, &debug_directory)?;
        }
        let publication_started = Instant::now();
        measured.set(true);
        let run: RubyFrontendRun = match build_ruby_frontend_run(
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
                crate::ruby_evidence::RubyEvidenceError::NoTests
                | crate::ruby_evidence::RubyEvidenceError::NoInterpreter,
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
                error @ (crate::ruby_evidence::RubyEvidenceError::UnsupportedRuby(_)
                | crate::ruby_evidence::RubyEvidenceError::NoTests
                | crate::ruby_evidence::RubyEvidenceError::NoInterpreter),
            ) => {
                measured.set(false);
                return Err(error.to_string());
            }
            result => result.map_err(|error| error.to_string())?,
        };
        validate_frontend_report_request(&run.declaration, &run.request)
            .map_err(|error| error.to_string())?;
        let archive_path = work_directory.join("evidence.raw.gz");
        let entries = run.archive_entries().map_err(|error| error.to_string())?;
        let entries = crate::source_capture::append(entries, &source_inputs)?;
        let raw = write_archive(entries, &archive_path).map_err(|error| error.to_string())?;
        remove_stored_tree_deferred(&root, &ruby_directory).map_err(|error| error.to_string())?;
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
        let run_directory =
            publish_run(&root, &metadata, &archive_path).map_err(|error| error.to_string())?;
        finalize_published_run(&root, &request.run_id).map_err(|error| error.to_string())?;
        Ok(DirectRubyRunResult {
            run_id: request.run_id.clone(),
            run_directory,
            exit_code: execution.exit_code,
            tests: run.tests,
            source_files: project.plan.files.len(),
            interpreters: run.interpreters,
            ruby_versions: run.ruby_versions,
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
            &work_directory.join("ruby/evidence"),
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

/// Ruby measured inside a browser suite's run: the server its tests drive,
/// which Playwright's `webServer` or a fixture starts. Prepared whenever the
/// project has Ruby source; a run whose suite starts no measured Ruby
/// publishes nothing of it.
pub struct RubyServer {
    project: PreparedRubyProject,
    integrity: crate::run_store::RunIntegrity,
    source_inputs: crate::source_manifest::Inputs,
    source_roots: Option<Vec<String>>,
    directory: PathBuf,
}

impl RubyServer {
    /// The plan and runtime for the project's Ruby source, or None when it
    /// has none. `workspace` is where the suite runs: the probes find the
    /// server's files there.
    pub fn prepare(
        root: &Path,
        workspace: &Path,
        run_id: &str,
        command: &[String],
    ) -> Result<Option<Self>, String> {
        let ambient = std::env::vars().collect::<std::collections::BTreeMap<_, _>>();
        // Roots named for the JavaScript source may hold no Ruby at all.
        let Ok(roots) =
            crate::source_discovery::ExplicitSourceRoots::from_environment(root, &ambient)
        else {
            return Ok(None);
        };
        let Some(project) = crate::ruby_project::prepare_ruby_server_project(root, roots.as_ref())?
        else {
            return Ok(None);
        };
        if project.plan.files.is_empty() {
            return Ok(None);
        }
        let source_roots = crate::source_discovery::configured_source_roots(&ambient);
        let mut integrity_inputs = ruby_integrity_inputs(&project.files, command);
        crate::source_discovery::fold_roots_into_configuration(
            &mut integrity_inputs.execution_configuration,
            source_roots.as_deref(),
        );
        let integrity = create_explicit_run_integrity(
            root,
            &integrity_inputs,
            &FrontendIntegrityInputs::embedded_ruby(),
        )
        .map_err(|error| error.to_string())?;
        let source_inputs = crate::source_capture::capture(
            root,
            "ruby",
            ruby_integrity_inputs(&project.files, command).assertion_paths(),
        )?;
        let directory = root.join(".supercov/work").join(run_id).join("ruby");
        write_runtime(&directory.join("runtime"))?;
        fs::create_dir_all(directory.join("evidence")).map_err(|error| error.to_string())?;
        let mut plan = serde_json::to_value(&project.plan).expect("plain data serializes");
        plan["root"] = workspace.display().to_string().into();
        let plan_path = directory.join("plan.rb");
        fs::write(&plan_path, ruby_literal(&plan).into_bytes())
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

    /// What the suite's environment needs for a Ruby process it starts to be
    /// measured; the run id and project root are the browser run's own.
    pub fn environment(&self) -> std::collections::BTreeMap<String, String> {
        self.environment_after(std::env::var("RUBYOPT").ok())
    }

    /// The environment, with `existing`, the suite's own `RUBYOPT`, kept
    /// after the runtime.
    fn environment_after(
        &self,
        existing: Option<String>,
    ) -> std::collections::BTreeMap<String, String> {
        let mut rubyopt = format!(
            "-r{}",
            self.directory.join("runtime/supercov_runtime.rb").display()
        );
        if let Some(existing) = existing.filter(|value| !value.is_empty()) {
            rubyopt.push(' ');
            rubyopt.push_str(&existing);
        }
        std::collections::BTreeMap::from([
            ("RUBYOPT".into(), rubyopt),
            (
                "SUPERCOV_RUBY_PLAN".into(),
                self.directory.join("plan.rb").display().to_string(),
            ),
            (
                "SUPERCOV_RUBY_EVIDENCE_DIR".into(),
                self.directory.join("evidence").display().to_string(),
            ),
            (
                "SUPERCOV_RUBY_SOURCE_ROOT".into(),
                self.project.root.display().to_string(),
            ),
            (
                BOOTSNAP_COMPILE_CACHE.0.into(),
                BOOTSNAP_COMPILE_CACHE.1.into(),
            ),
        ])
    }

    /// Publish what the server ran as a Ruby run of its own, credited to the
    /// browser tests that sent each request. None when no measured Ruby
    /// process ran, or none answered a test's request.
    pub fn publish(
        self,
        root: &Path,
        evidence_run_id: &str,
        metadata: RunMetadata,
        served: &[crate::served_tests::ServedTest],
        diagnostics: &mut dyn Write,
    ) -> Result<Option<(PathBuf, RunMetadata)>, String> {
        let evidence_directory = self.directory.join("evidence");
        let result = (|| {
            if served.is_empty() {
                return Ok(None);
            }
            let mut run = match crate::ruby_evidence::build_ruby_server_run(
                &self.project.manifest,
                &evidence_directory,
                evidence_run_id,
                &metadata.started_at,
                metadata.test_exit_code.unwrap_or_default(),
                &self.project.root,
                served,
            ) {
                Err(
                    crate::ruby_evidence::RubyEvidenceError::NoInterpreter
                    | crate::ruby_evidence::RubyEvidenceError::NoTests,
                ) => return Ok(None),
                result => result.map_err(|error| error.to_string())?,
            };
            let answered = run.request.raw_results.iter().any(|raw| {
                raw.role == "test" && raw.runtime.iter().any(|snapshot| !snapshot.hits.is_empty())
            });
            if !answered {
                let _ = writeln!(
                    diagnostics,
                    "[supercov] a Ruby process ran, and no request a test sent reached measured Ruby code; no Ruby run was published"
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
            let work = root.join(".supercov/work").join(&metadata.id);
            fs::create_dir_all(&work).map_err(|error| error.to_string())?;
            let archive_path = work.join("ruby-evidence.raw.gz");
            let entries = run.archive_entries().map_err(|error| error.to_string())?;
            let entries = crate::source_capture::append(entries, &self.source_inputs)?;
            let raw = write_archive(entries, &archive_path).map_err(|error| error.to_string())?;
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
            let run_directory =
                publish_run(root, &metadata, &archive_path).map_err(|error| error.to_string())?;
            let _ = writeln!(
                diagnostics,
                "[supercov] Ruby server coverage: {tests} test(s) across {} source file(s)",
                self.project.plan.files.len()
            );
            Ok(Some((run_directory, metadata)))
        })();
        let result = result.map_err(|error| {
            note_kept_evidence(
                root,
                &evidence_directory,
                &format!("{evidence_run_id}-ruby"),
                error,
            )
        });
        let _ = remove_stored_tree_deferred(root, &self.directory);
        result
    }
}

/// Render a JSON value as a Ruby literal: `nil`, booleans and numbers as
/// themselves, strings double-quoted with JSON escapes (which Ruby shares)
/// plus `#` escaped so nothing interpolates, arrays as `[..]` and objects as
/// `{"key" => value, ..}` so keys stay strings rather than becoming symbols.
pub(crate) fn ruby_literal(value: &serde_json::Value) -> String {
    fn write(value: &serde_json::Value, out: &mut String) {
        match value {
            serde_json::Value::Null => out.push_str("nil"),
            serde_json::Value::Bool(flag) => out.push_str(if *flag { "true" } else { "false" }),
            serde_json::Value::Number(number) => out.push_str(&number.to_string()),
            serde_json::Value::String(text) => write_string(text, out),
            serde_json::Value::Array(items) => {
                out.push('[');
                for (index, item) in items.iter().enumerate() {
                    if index > 0 {
                        out.push(',');
                    }
                    write(item, out);
                }
                out.push(']');
            }
            serde_json::Value::Object(entries) => {
                out.push('{');
                for (index, (key, item)) in entries.iter().enumerate() {
                    if index > 0 {
                        out.push(',');
                    }
                    write_string(key, out);
                    out.push_str("=>");
                    write(item, out);
                }
                out.push('}');
            }
        }
    }

    fn write_string(text: &str, out: &mut String) {
        let json = serde_json::to_string(text).unwrap_or_else(|_| "\"\"".into());
        out.push_str(&json.replace('#', "\\#"));
    }

    let mut out = String::new();
    write(value, &mut out);
    out.push('\n');
    out
}

#[cfg(test)]
mod literal_tests {
    use super::ruby_literal;

    #[test]
    fn ruby_literal_keeps_strings_inert_and_keys_as_strings() {
        let value = serde_json::json!({
            "a": [1, 2.5, -3, true, false, null],
            "text": "quote \" backslash \\ interpolation #{x} tab \t unicode \u{e9}",
            "nested": {"k": []}
        });
        let literal = ruby_literal(&value);
        assert_eq!(
            literal,
            "{\"a\"=>[1,2.5,-3,true,false,nil],\"text\"=>\"quote \\\" backslash \\\\ interpolation \\#{x} tab \\t unicode \u{e9}\",\"nested\"=>{\"k\"=>[]}}\n"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project(name: &str) -> PathBuf {
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let root = std::env::temp_dir().join(format!(
            "supercov-ruby-run-{}-{}-{}-{name}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(root.join("lib")).unwrap();
        fs::write(root.join("lib/m.rb"), "def f(a)\n  a ? 1 : 2\nend\n").unwrap();
        root
    }

    fn ruby_request(root: &Path, command: &[&str]) -> DirectRubyRunRequest {
        DirectRubyRunRequest {
            root: root.to_owned(),
            command: command.iter().map(|word| (*word).to_owned()).collect(),
            run_id: "2026-01-02T00-00-00-000Z".into(),
            started_at: "2026-01-02T00:00:00Z".into(),
        }
    }

    #[test]
    fn a_ruby_run_refuses_an_empty_command() {
        let root = std::env::temp_dir().join("supercov-ruby-empty-command-never-created");
        let error = run_direct_ruby(&ruby_request(&root, &[]), &mut Vec::new()).unwrap_err();
        assert_eq!(error, "test command must not be empty");
        assert!(!root.exists());
    }

    #[test]
    fn a_command_that_failed_before_any_test_is_its_own_failure() {
        let root = project("failed-command");
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
        let mut diagnostics = Vec::new();
        let error =
            run_direct_ruby(&ruby_request(&root, &["false"]), &mut diagnostics).unwrap_err();
        assert!(
            error.starts_with("the test command exited 1 before any test reported an outcome"),
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
        let root = project("unreadable-evidence");
        let request = ruby_request(
            &root,
            &[
                "sh",
                "-c",
                "echo x > \"$SUPERCOV_RUBY_EVIDENCE_DIR/notes.txt\"",
            ],
        );
        let error = run_direct_ruby(&request, &mut Vec::new()).unwrap_err();
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

    #[test]
    fn a_tree_is_copied_whole() {
        let root = project("copied");
        let copy = root.with_extension("copy");
        fs::create_dir_all(&copy).unwrap();
        copy_tree(&root, &copy).unwrap();
        assert_eq!(
            fs::read_to_string(copy.join("lib/m.rb")).unwrap(),
            "def f(a)\n  a ? 1 : 2\nend\n"
        );
        fs::remove_dir_all(&root).ok();
        fs::remove_dir_all(&copy).ok();
    }

    #[test]
    fn copying_and_preparing_a_server_survive_any_file_operation_failing() {
        let command = ["npx".to_owned(), "playwright".to_owned(), "test".to_owned()];
        let steps = crate::fault::fail_each_call(
            "fs",
            || project("faults"),
            |root| {
                let copy = root.with_extension("copy");
                fs::create_dir_all(&copy).map_err(|error| error.to_string())?;
                copy_tree(root, &copy)?;
                RubyServer::prepare(root, root, "2026-01-02T00-00-00-000Z", &command).map(drop)
            },
            |root| {
                let _ = fs::remove_dir_all(root.with_extension("copy"));
                let _ = fs::remove_dir_all(root);
            },
        );
        assert!(steps > 5, "{steps}");
        let empty = project("empty-tree");
        let nothing = empty.join("nothing");
        fs::create_dir_all(&nothing).unwrap();
        let copy = empty.join("copy");
        fs::create_dir_all(&copy).unwrap();
        copy_tree(&nothing, &copy).unwrap();
        assert_eq!(fs::read_dir(&copy).unwrap().count(), 0);
        fs::remove_dir_all(&empty).ok();
    }

    fn server_metadata(server: &RubyServer) -> RunMetadata {
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
        let root = project("server");
        let command = ["npx".to_owned(), "playwright".to_owned(), "test".to_owned()];
        let run_id = "2026-01-02T00-00-00-000Z";
        let prepare = || {
            RubyServer::prepare(&root, &root, run_id, &command)
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
        // A Ruby process ran, and only as it loaded: no request reached it.
        let server = prepare();
        let statement = server.project.manifest.points[0].id.clone();
        fs::write(
            server.directory.join("evidence/main.7.a.mmap"),
            crate::ruby_evidence::tests::transport(&[
                serde_json::json!({"t":"process","v":1,"run":run_id,"pid":7,"worker":"main","ruby":"3.4.1","executable":"ruby","argv":["puma"],"lines":"exact"}),
                serde_json::json!({"t":"hit","ctx":0,"id":statement}),
                serde_json::json!({"t":"exit","at":300}),
            ]),
        )
        .unwrap();
        let served = crate::served_tests::ServedTest {
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
        };
        let metadata = server_metadata(&server);
        let mut diagnostics = Vec::new();
        assert!(
            server
                .publish(&root, run_id, metadata, &[served], &mut diagnostics)
                .unwrap()
                .is_none()
        );
        assert!(
            String::from_utf8(diagnostics)
                .unwrap()
                .contains("no request a test sent reached measured Ruby code")
        );
        // Ruby that does not parse makes no server.
        fs::write(root.join("lib/m.rb"), "def broken(\n").unwrap();
        assert!(
            RubyServer::prepare(&root, &root, run_id, &command)
                .unwrap()
                .is_none()
        );
        fs::remove_dir_all(&root).ok();
    }

    /// The suite's own `RUBYOPT` follows the runtime, in the test command
    /// and in a server's environment alike.
    #[test]
    fn a_suite_s_own_rubyopt_is_kept_after_the_runtime() {
        let variables = environment(
            vec![
                ("RUBYOPT".into(), "-w".into()),
                ("HOME".into(), "/h".into()),
            ],
            Path::new("/p"),
            "run",
            Path::new("/r"),
            Path::new("/r/plan.rb"),
            Path::new("/r/evidence"),
        );
        let rubyopt = variables
            .iter()
            .filter(|(name, _)| name == "RUBYOPT")
            .map(|(_, value)| value.clone())
            .collect::<Vec<_>>();
        let runtime = Path::new("/r").join("supercov_runtime.rb");
        assert_eq!(
            rubyopt,
            [OsString::from(format!("-r{} -w", runtime.display()))]
        );
        assert!(variables.contains(&("HOME".into(), "/h".into())));
        let root = project("rubyopt");
        let command = ["npx".to_owned(), "playwright".to_owned(), "test".to_owned()];
        let server = RubyServer::prepare(&root, &root, "2026-01-02T00-00-00-000Z", &command)
            .unwrap()
            .unwrap();
        let runtime = server.directory.join("runtime/supercov_runtime.rb");
        assert_eq!(
            server.environment_after(Some("-w".into()))["RUBYOPT"],
            format!("-r{} -w", runtime.display())
        );
        assert_eq!(
            server.environment_after(Some(String::new()))["RUBYOPT"],
            format!("-r{}", runtime.display())
        );
        fs::remove_dir_all(&root).ok();
    }

    /// `SUPERCOV_KEEP_WORK` keeps the work directory before the evidence is
    /// read, so evidence the run rejects can still be looked at.
    #[test]
    fn a_kept_run_keeps_its_work_even_when_its_evidence_is_rejected() {
        let root = project("kept");
        let request = ruby_request(
            &root,
            &[
                "sh",
                "-c",
                "echo x > \"$SUPERCOV_RUBY_EVIDENCE_DIR/notes.txt\"",
            ],
        );
        let ambient = [("SUPERCOV_KEEP_WORK".to_owned(), "1".to_owned())].into();
        run_direct_ruby_in(&request, &ambient, &mut Vec::new()).unwrap_err();
        assert!(
            root.join(".supercov/ruby-debug")
                .join(&request.run_id)
                .join("evidence/notes.txt")
                .exists()
        );
        fs::remove_dir_all(&root).ok();
    }
}
