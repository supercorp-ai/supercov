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

#[track_caller]
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
        let mut entries = run.archive_entries();
        if let Some(mutants) = &project.mutants {
            entries.push(crate::evidence_archive::EvidenceArchiveEntry {
                path: "mutants.json".into(),
                contents: serde_json::to_vec(mutants.get()).expect("plain data serializes"),
            });
        }
        let entries = crate::source_capture::append(entries, &source_inputs);
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
            supersedes: None,
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
        // Roots named for the JavaScript source may hold no Ruby at all. They
        // resolve: the suite's own discovery read them first and refused any
        // that did not.
        let roots = crate::source_discovery::ExplicitSourceRoots::from_environment(root, &ambient)
            .expect("roots the suite's discovery accepted resolve");
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

    /// Build what the server ran as a Ruby run of its own, credited to the
    /// browser tests that sent each request, to be published under the
    /// metadata the returned step is given. None when no measured Ruby
    /// process ran, or none answered a test's request.
    pub fn build_run<'a>(
        self,
        root: &'a Path,
        evidence_run_id: &str,
        run: &RunMetadata,
        served: &[crate::served_tests::ServedTest],
        diagnostics: &mut dyn Write,
    ) -> Result<Option<crate::served_tests::PreparedRun<'a>>, String> {
        let evidence_directory = self.directory.join("evidence");
        let hint = format!("{evidence_run_id}-ruby");
        let built = (|| {
            if served.is_empty() {
                return Ok(None);
            }
            let run = match crate::ruby_evidence::build_ruby_server_run(
                &self.project.manifest,
                &evidence_directory,
                evidence_run_id,
                &run.started_at,
                run.test_exit_code.unwrap_or_default(),
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
            Ok(Some(run))
        })();
        let mut run = match built {
            Ok(Some(run)) => run,
            other => {
                let result = other
                    .map(|_| None)
                    .map_err(|error| note_kept_evidence(root, &evidence_directory, &hint, error));
                let _ = remove_stored_tree_deferred(root, &self.directory);
                return result;
            }
        };
        Ok(Some(Box::new(
            move |metadata: RunMetadata, diagnostics: &mut dyn Write| {
                let result = (|| {
                    // The evidence was recorded under the browser run's id; a run
                    // published beside it has its own.
                    run.request.run_id = metadata.id.clone();
                    run.request
                        .raw_results
                        .iter_mut()
                        .filter_map(|raw| raw.scope.as_mut())
                        .for_each(|scope| scope.run_id = metadata.id.clone());
                    let work = root.join(".supercov/work").join(&metadata.id);
                    fs::create_dir_all(&work).map_err(|error| error.to_string())?;
                    let archive_path = work.join("ruby-evidence.raw.gz");
                    let entries = run.archive_entries();
                    let entries = crate::source_capture::append(entries, &self.source_inputs);
                    let raw =
                        write_archive(entries, &archive_path).map_err(|error| error.to_string())?;
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
                    let run_directory = publish_run(root, &metadata, &archive_path)
                        .map_err(|error| error.to_string())?;
                    let _ = writeln!(
                        diagnostics,
                        "[supercov] Ruby server coverage: {tests} test(s) across {} source file(s)",
                        self.project.plan.files.len()
                    );
                    Ok((run_directory, metadata))
                })();
                let result = result
                    .map_err(|error| note_kept_evidence(root, &evidence_directory, &hint, error));
                let _ = remove_stored_tree_deferred(root, &self.directory);
                result
            },
        )))
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

    /// The mutants the Ruby pass cannot run get their verdicts from it, and
    /// what it cannot read or write ends it with an error.
    #[cfg(unix)]
    #[test]
    fn the_ruby_pass_gives_mutants_it_cannot_run_their_verdicts() {
        use serde_json::json;
        let root = std::env::temp_dir().join(format!(
            "supercov-ruby-strong-verdicts-{}-{:?}",
            std::process::id(),
            std::time::SystemTime::now()
        ));
        fs::create_dir_all(root.join("lib")).unwrap();
        fs::create_dir_all(root.join("pass")).unwrap();
        let source = b"def m(a)\n  a + 1\nend\n";
        fs::write(root.join("lib/a.rb"), source).unwrap();
        let ids = crate::ruby_mutants::mutant_ids("lib/a.rb", source);
        let runnable =
            json!({"id": ids[1], "file": "lib/a.rb", "index": 1, "candidates": [{"test": 0}]});
        let full = json!({
            "tests": [{"id": "T#test_a", "runner": "minitest"}, {"id": "a.test.js", "runner": "vitest"}],
            "mutants": [
                {"id": ids[0], "file": "lib/a.rb", "index": 0, "decided": "survived", "candidates": [{"test": 0}]},
                {"file": "lib/a.rb"},
                {"id": "elsewhere", "file": "lib/a.rb", "index": 1, "candidates": [{"test": 1}, {"test": 9}, {"test": "z"}]},
                {"id": "gone", "file": "lib/missing.rb", "index": 0, "candidates": [{"test": 0}]},
                {"id": "stale", "file": "lib/a.rb", "index": 2, "candidates": [{"test": 0}]},
                runnable,
            ],
        });
        let pass = |plan: &serde_json::Value, results: &str, command: &[&str]| {
            let directory = root.join("pass");
            fs::write(directory.join("plan.json"), plan.to_string()).unwrap();
            let strong = crate::strong_driver::StrongPass {
                root: root.clone(),
                command: command.iter().map(|part| (*part).to_owned()).collect(),
                run_id: "run-1".into(),
                watchdog_program: None,
                strong: crate::strong_driver::StrongRun {
                    plan: directory.join("plan.json"),
                    results: directory.join(results),
                },
            };
            run_strong_ruby(&strong, &mut Vec::new())
        };
        let results =
            |name: &str| fs::read_to_string(root.join("pass").join(name)).unwrap_or_default();
        assert_eq!(pass(&full, "results.jsonl", &["sh", "-c", "exit 3"]), Ok(3));
        let written = results("results.jsonl");
        for (id, reason) in [
            ("elsewhere", "none of its candidate tests"),
            ("gone", "lib/missing.rb"),
            ("stale", "its file changed"),
        ] {
            assert!(
                written
                    .lines()
                    .any(|line| line.contains(&format!("\"{id}\"")) && line.contains(reason)),
                "{id}: {written}"
            );
        }
        assert_eq!(written.lines().count(), 3, "{written}");
        assert!(root.join("pass/ruby/strong-plan.rb").is_file());
        // A mutant with no switch is an error, and with nothing left to
        // run the command is not started.
        let alone = json!({"tests": full["tests"], "mutants": [runnable]});
        assert_eq!(
            crate::fault::during("ruby-strong-unplaceable", || pass(
                &alone,
                "alone.jsonl",
                &["false"]
            )),
            Ok(0)
        );
        assert!(results("alone.jsonl").contains("it has no switch (injected)"));
        // A plan with no mutants to run starts nothing.
        assert_eq!(pass(&json!({"tests": []}), "none.jsonl", &["false"]), Ok(0));
        // Nor does one whose every mutant's file changed.
        let stale = json!({"tests": full["tests"], "mutants": [full["mutants"][4]]});
        assert_eq!(pass(&stale, "stale.jsonl", &["false"]), Ok(0));
        // Supervision that cannot be set up.
        assert!(
            crate::fault::during("ruby-strong-supervision", || pass(
                &alone,
                "results.jsonl",
                &["true"]
            ))
            .is_err()
        );
        // What it cannot read, write or start.
        assert!(pass(&alone, "no/such/results.jsonl", &["true"]).is_err());
        assert!(pass(&alone, "results.jsonl", &["/no/such/program"]).is_err());
        fs::write(root.join("pass/plan.json"), b"{").unwrap();
        let unparsed = crate::strong_driver::StrongPass {
            root: root.clone(),
            command: vec!["true".into()],
            run_id: "run-1".into(),
            watchdog_program: None,
            strong: crate::strong_driver::StrongRun {
                plan: root.join("pass/plan.json"),
                results: root.join("pass/results.jsonl"),
            },
        };
        assert!(run_strong_ruby(&unparsed, &mut Vec::new()).is_err());
        let unread = crate::strong_driver::StrongPass {
            strong: crate::strong_driver::StrongRun {
                plan: root.join("pass/none.json"),
                results: root.join("pass/results.jsonl"),
            },
            ..unparsed.clone()
        };
        assert!(run_strong_ruby(&unread, &mut Vec::new()).is_err());
        let rootless = crate::strong_driver::StrongPass {
            root: root.join("none"),
            ..unparsed.clone()
        };
        assert!(run_strong_ruby(&rootless, &mut Vec::new()).is_err());
        // The switched copy, the plan and the runtime each in a place that
        // cannot be written.
        fs::remove_file(root.join("pass/ruby/switched-1.rb")).unwrap();
        fs::create_dir_all(root.join("pass/ruby/switched-1.rb")).unwrap();
        assert!(pass(&alone, "results.jsonl", &["true"]).is_err());
        fs::remove_dir_all(root.join("pass/ruby/switched-1.rb")).unwrap();
        fs::remove_file(root.join("pass/ruby/strong-plan.rb")).unwrap();
        fs::create_dir_all(root.join("pass/ruby/strong-plan.rb")).unwrap();
        assert!(pass(&alone, "results.jsonl", &["true"]).is_err());
        fs::remove_dir_all(root.join("pass/ruby/strong-plan.rb")).unwrap();
        fs::remove_file(root.join("pass/ruby/runtime/supercov_strong.rb")).unwrap();
        fs::create_dir_all(root.join("pass/ruby/runtime/supercov_strong.rb")).unwrap();
        assert!(pass(&alone, "results.jsonl", &["true"]).is_err());
        fs::remove_dir_all(root.join("pass/ruby")).unwrap();
        fs::write(root.join("pass/ruby"), b"").unwrap();
        assert!(pass(&alone, "results.jsonl", &["true"]).is_err());
        // A verdict that cannot be written ends the pass.
        #[cfg(target_os = "linux")]
        {
            fs::remove_file(root.join("pass/ruby")).unwrap();
            let directory = root.join("pass");
            fs::write(directory.join("plan.json"), full.to_string()).unwrap();
            // Results that cannot be written to, beside a directory that
            // can be (the pass's own files go beside its results).
            std::os::unix::fs::symlink("/dev/full", directory.join("full.jsonl")).unwrap();
            let full_disk = crate::strong_driver::StrongPass {
                strong: crate::strong_driver::StrongRun {
                    plan: directory.join("plan.json"),
                    results: directory.join("full.jsonl"),
                },
                ..unparsed.clone()
            };
            assert!(run_strong_ruby(&full_disk, &mut Vec::new()).is_err());
            // Each other verdict too: a missing file's, a changed file's and
            // a mutant's without a switch.
            for (mutant, fault) in [(3, ""), (4, ""), (5, "ruby-strong-unplaceable")] {
                let one = json!({"tests": full["tests"], "mutants": [full["mutants"][mutant]]});
                fs::write(directory.join("plan.json"), one.to_string()).unwrap();
                let ran =
                    crate::fault::during(fault, || run_strong_ruby(&full_disk, &mut Vec::new()));
                assert!(ran.is_err(), "{mutant}");
            }
        }
        fs::remove_dir_all(&root).unwrap();
    }

    /// The pass's RUBYOPT loads its runtime first, then the command's own.
    #[test]
    fn the_ruby_pass_keeps_the_command_s_rubyopt_after_its_runtime() {
        let runtime = Path::new("/r/supercov_strong.rb");
        let rubyopt = |existing: Option<&str>| {
            strong_rubyopt(runtime, existing.map(std::ffi::OsStr::new))
                .to_string_lossy()
                .into_owned()
        };
        assert_eq!(rubyopt(None), "-r/r/supercov_strong.rb");
        assert_eq!(rubyopt(Some("")), "-r/r/supercov_strong.rb");
        assert_eq!(rubyopt(Some("-w")), "-r/r/supercov_strong.rb -w");
    }
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
        // An empty directory is copied as one.
        fs::create_dir_all(root.join("empty")).unwrap();
        copy_tree(&root, &copy).unwrap();
        assert!(copy.join("empty").is_dir());
        assert_eq!(
            fs::read_to_string(copy.join("lib/m.rb")).unwrap(),
            "def f(a)\n  a ? 1 : 2\nend\n"
        );
        fs::remove_dir_all(&root).ok();
        fs::remove_dir_all(&copy).ok();
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
            supersedes: None,
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
                .build_run(&root, run_id, &metadata, &[], &mut Vec::new())
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
                .build_run(&root, run_id, &metadata, &[served], &mut diagnostics)
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

/// RUBYOPT for the strong pass: its runtime first, then what the command's
/// own RUBYOPT asks for.
fn strong_rubyopt(runtime: &Path, existing: Option<&std::ffi::OsStr>) -> OsString {
    let mut rubyopt = OsString::from("-r");
    rubyopt.push(runtime);
    if let Some(existing) = existing.filter(|existing| !existing.is_empty()) {
        rubyopt.push(" ");
        rubyopt.push(existing);
    }
    rubyopt
}

/// The test runners whose tests the Ruby strong driver runs.
pub const RUBY_STRONG_RUNNERS: [&str; 4] = ["rspec", "minitest", "test-unit", "cucumber"];

/// Ruby's strong pass (`docs/design/mutation-drivers.md`): the plan's Ruby
/// mutants written into their files behind switches, and the command run
/// once with `supercov_strong.rb` loaded in place of the coverage runtime.
/// When the runner is about to run the loaded suite, that runtime
/// runs the plan instead, one fork per mutant, and appends each verdict to
/// the results. Mutants it cannot run are given their verdict here.
pub fn run_strong_ruby(
    pass: &crate::strong_driver::StrongPass,
    diagnostics: &mut dyn Write,
) -> Result<i32, String> {
    use std::collections::{BTreeMap, BTreeSet};
    let root = canonicalize_simplified(&pass.root).map_err(|error| error.to_string())?;
    let plan: serde_json::Value = serde_json::from_slice(
        &fs::read(&pass.strong.plan)
            .map_err(|error| format!("{}: {error}", pass.strong.plan.display()))?,
    )
    .map_err(|error| format!("{}: {error}", pass.strong.plan.display()))?;
    let tests = plan["tests"].as_array().cloned().unwrap_or_default();
    let mut results = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&pass.strong.results)
        .map_err(|error| format!("{}: {error}", pass.strong.results.display()))?;
    let mut verdict = |id: &str, status: &str, reason: &str| -> Result<(), String> {
        let line = serde_json::json!({ "id": id, "status": status, "killedBy": null, "tests": 0, "durationMs": 0, "reason": reason });
        writeln!(results, "{line}").map_err(|error| error.to_string())
    };
    // The mutants to run, by file, each with its candidate tests.
    let mut by_file = BTreeMap::<String, Vec<serde_json::Value>>::new();
    for mutant in plan["mutants"].as_array().into_iter().flatten() {
        if !mutant["decided"].is_null() {
            continue;
        }
        let (Some(id), Some(file)) = (mutant["id"].as_str(), mutant["file"].as_str()) else {
            continue;
        };
        let candidates = mutant["candidates"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|candidate| tests.get(candidate["test"].as_u64()? as usize))
            .filter(|test| {
                test["runner"]
                    .as_str()
                    .is_some_and(|runner| RUBY_STRONG_RUNNERS.contains(&runner))
            })
            .filter_map(|test| test["id"].as_str().map(str::to_owned))
            .collect::<Vec<_>>();
        if candidates.is_empty() {
            verdict(
                id,
                "error",
                "none of its candidate tests ran under RSpec, Minitest, test-unit or Cucumber",
            )?;
            continue;
        }
        let mut entry = mutant.clone();
        entry["tests"] = candidates.into();
        by_file.entry(file.to_owned()).or_default().push(entry);
    }
    let directory = pass.strong.results.with_file_name("ruby");
    let runtime_directory = directory.join("runtime");
    write_runtime(&runtime_directory)?;
    fs::write(
        runtime_directory.join("supercov_strong.rb"),
        include_bytes!("../runtime-assets/ruby/supercov_strong.rb").as_slice(),
    )
    .map_err(|error| error.to_string())?;
    let mut files = serde_json::Map::new();
    let mut mutants = Vec::new();
    let mut baseline = BTreeSet::new();
    let mut candidate_sets = BTreeMap::<Vec<String>, usize>::new();
    let mut switched_count = 0;
    for (number, (file, wanted)) in by_file.into_iter().enumerate() {
        let number = number as u64 + 1;
        let source = match fs::read(root.join(&file)) {
            Ok(source) => source,
            Err(error) => {
                wanted.iter().try_for_each(|mutant| {
                    verdict(
                        mutant["id"].as_str().unwrap_or_default(),
                        "error",
                        &format!("{file}: {error}"),
                    )
                })?;
                continue;
            }
        };
        let ids = crate::ruby_mutants::mutant_ids(&file, &source);
        let mut indexes = BTreeSet::new();
        let mut runnable = Vec::new();
        wanted.into_iter().try_for_each(|mutant| {
            let index = mutant["index"].as_u64().unwrap_or(u64::MAX) as usize;
            let id = mutant["id"].as_str().unwrap_or_default();
            if ids.get(index).map(String::as_str) != Some(id) {
                return verdict(
                    id,
                    "error",
                    "its file changed since the run: measure it again",
                );
            }
            indexes.insert(index);
            runnable.push(mutant);
            Ok(())
        })?;
        let switches = crate::ruby_mutants::schemata::switches(&source, number, Some(&indexes));
        for mutant in runnable {
            let index = mutant["index"].as_u64().unwrap_or_default() as usize;
            let id = mutant["id"].as_str().unwrap_or_default();
            // (No mutant the enumeration lists lacks a switch; a test fault
            // stands in for one that would.)
            let unplaceable = switches.unplaceable.get(&index).cloned().or_else(|| {
                crate::fault::injected("ruby-strong-unplaceable").then(|| "injected".to_owned())
            });
            if let Some(reason) = unplaceable {
                verdict(id, "error", &format!("it has no switch ({reason})"))?;
                continue;
            }
            // Mutants of one method share their candidates: each list is
            // written once, and a mutant names it.
            let list: Vec<String> = mutant["tests"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|test| test.as_str().map(str::to_owned))
                .collect();
            baseline.extend(list.iter().cloned());
            let next = candidate_sets.len();
            let set = *candidate_sets.entry(list).or_insert(next);
            mutants.push(serde_json::json!({
                "id": id,
                "file": file,
                "index": index,
                "key": number * crate::ruby_mutants::schemata::FILE_KEY + index as u64,
                "set": set,
            }));
        }
        let switched = directory.join(format!("switched-{number}.rb"));
        fs::write(
            &switched,
            crate::ruby_mutants::schemata::apply(&source, &switches.edits),
        )
        .map_err(|error| format!("{}: {error}", switched.display()))?;
        files.insert(file.clone(), switched.display().to_string().into());
        switched_count += 1;
    }
    drop(results);
    if mutants.is_empty() {
        return Ok(0);
    }
    // The baseline runs the candidates in the plan's order of tests.
    let baseline = tests
        .iter()
        .filter_map(|test| test["id"].as_str())
        .filter(|id| baseline.contains(*id))
        .collect::<Vec<_>>();
    // Each candidate list as indexes into the baseline's tests.
    let position = baseline
        .iter()
        .enumerate()
        .map(|(index, id)| (*id, index))
        .collect::<std::collections::HashMap<_, _>>();
    let mut sets = candidate_sets
        .into_iter()
        .map(|(list, index)| {
            let tests = list
                .iter()
                .filter_map(|id| position.get(id.as_str()).copied())
                .collect::<Vec<_>>();
            (index, tests)
        })
        .collect::<Vec<_>>();
    sets.sort();
    let sets = sets.into_iter().map(|(_, tests)| tests).collect::<Vec<_>>();
    let jobs = std::env::var("SUPERCOV_STRONG_JOBS")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or_else(|| std::thread::available_parallelism().map_or(4, usize::from));
    let ruby_plan = serde_json::json!({
        "root": root.display().to_string(),
        "files": files,
        "mutants": mutants,
        "baseline": baseline,
        "sets": sets,
        "results": pass.strong.results.display().to_string(),
        "timeouts": pass.strong.timeouts_path().display().to_string(),
        "hitLimit": crate::strong_driver::DEFAULT_HIT_LIMIT,
        "jobs": jobs,
    });
    let plan_path = directory.join("strong-plan.rb");
    fs::write(&plan_path, ruby_literal(&ruby_plan).into_bytes())
        .map_err(|error| format!("{}: {error}", plan_path.display()))?;
    let _ = writeln!(
        diagnostics,
        "[supercov] strong pass: switches in {switched_count} Ruby source file(s), {} mutant(s), {jobs} at a time",
        mutants.len()
    );
    let mut environment = std::env::vars_os().collect::<BTreeMap<_, _>>();
    let rubyopt = strong_rubyopt(
        &runtime_directory.join("supercov_strong.rb"),
        std::env::var_os("RUBYOPT").as_deref(),
    );
    environment.insert("RUBYOPT".into(), rubyopt);
    environment.insert(
        "SUPERCOV_STRONG_RUBY_PLAN".into(),
        plan_path.into_os_string(),
    );
    environment.insert(
        BOOTSNAP_COMPILE_CACHE.0.into(),
        BOOTSNAP_COMPILE_CACHE.1.into(),
    );
    let environment = environment.into_iter().collect::<Vec<_>>();
    let execution_plan = ExecutionPlan {
        preparation: Vec::new(),
        test: ExecutionPhase {
            name: "strong".into(),
            kind: PhaseKind::Test,
            command: CommandSpec {
                program: pass.command[0].clone().into(),
                arguments: pass.command[1..].iter().map(OsString::from).collect(),
                cwd: root.clone(),
                environment: Some(environment),
                captured_output: None,
                console: None,
            },
        },
    };
    let options = crate::fault::or(
        "ruby-strong-supervision",
        || Err("injected".to_owned()),
        || SupervisionOptions::from_environment().map_err(|error| error.to_string()),
    )?;
    let execution = execute_plan(&execution_plan, options, diagnostics, |_, _| Ok(()))
        .map_err(|error| error.to_string())?;
    Ok(execution.exit_code)
}
