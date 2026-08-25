//! Rust-owned JavaScript execution for the public Supercov engine.

use std::{
    collections::BTreeMap,
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    time::{Instant, SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};

use crate::{
    build_cache::{build_cache_key, read_build_cache, reuse_paths, write_build_cache},
    evidence_archive::{EvidenceArchiveSource, collect_sources, write_archive},
    integrity::{FrontendIntegrityInputs, create_run_integrity},
    javascript_frontend::prepare_javascript_frontend,
    lifecycle::{
        ProjectLock, RunState, RunStateStatus, finalize_published_run, interrupt_run_state,
        publish_run, recover_abandoned_runs, remove_stored_tree_deferred, update_run_state,
        write_run_state,
    },
    orchestration::{ExecutionPhase, ExecutionPlan, OrchestrationError, PhaseKind, execute_plan},
    process_supervision::{
        CommandSpec, ForwardedSignal, SupervisionOptions, positive_milliseconds,
    },
    project_discovery::{BuildAdapter, discover_coverage_project},
    run_store::{
        InstrumentedBuildCache, RawEvidenceMetadata, RunIntegrity, RunMetadata, RunTimings,
    },
    workspace::{cached_workspace_path, prepare_cached_workspace, prune_cached_workspace_sources},
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DirectJavascriptRunRequest {
    pub root: PathBuf,
    pub command: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DirectJavascriptRunResult {
    pub run_id: String,
    pub run_directory: PathBuf,
    pub workspace: PathBuf,
    pub exit_code: i32,
    pub assertion_calls: usize,
    pub recovered_runs: Vec<String>,
    pub metadata: RunMetadata,
}

#[derive(Debug)]
pub enum DirectJavascriptRunError {
    Interrupted {
        signal: ForwardedSignal,
        exit_code: i32,
        timings: RunTimings,
        total_ms: f64,
    },
    Failed(String),
}

impl std::fmt::Display for DirectJavascriptRunError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Interrupted { signal, .. } => {
                write!(formatter, "interrupted by {}", signal_name(*signal))
            }
            Self::Failed(message) => formatter.write_str(message),
        }
    }
}

impl std::error::Error for DirectJavascriptRunError {}

impl From<String> for DirectJavascriptRunError {
    fn from(value: String) -> Self {
        Self::Failed(value)
    }
}

fn signal_name(signal: ForwardedSignal) -> &'static str {
    match signal {
        ForwardedSignal::Sighup => "SIGHUP",
        ForwardedSignal::Sigint => "SIGINT",
        ForwardedSignal::Sigterm => "SIGTERM",
    }
}

struct RunCleanup {
    root: PathBuf,
    run_id: String,
    started_at: String,
    lock: ProjectLock,
    workspace: Option<PathBuf>,
    state_written: bool,
    terminal_recorded: bool,
}

impl RunCleanup {
    fn lock(&self) -> &ProjectLock {
        &self.lock
    }

    fn set_workspace(&mut self, workspace: PathBuf) {
        self.workspace = Some(workspace);
    }

    fn mark_state_written(&mut self) {
        self.state_written = true;
    }

    fn mark_terminal(&mut self) {
        self.terminal_recorded = true;
    }
}

impl Drop for RunCleanup {
    fn drop(&mut self) {
        if self.state_written && !self.terminal_recorded {
            let _ = update_run_state(
                &self.root,
                &self.run_id,
                RunStateStatus::Failed,
                &self.started_at,
                Some("Rust run exited before reaching a terminal lifecycle state".into()),
            );
        }
        if let Some(workspace) = &self.workspace {
            let _ = remove_stored_tree_deferred(
                &self.root,
                &workspace.join(".supercov/evidence").join(&self.run_id),
            );
            let _ = remove_stored_tree_deferred(
                &self.root,
                &workspace
                    .join(".supercov/server-evidence")
                    .join(&self.run_id),
            );
            let keep_workspace =
                std::env::var("SUPERCOV_KEEP_WORKSPACE").is_ok_and(|value| !value.is_empty());
            if !keep_workspace {
                let _ = prune_cached_workspace_sources(&self.root, &self.lock);
            }
        }
        let _ = self.lock.release();
    }
}

fn now_nonce() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

fn elapsed_ms(started: Instant) -> f64 {
    started.elapsed().as_secs_f64() * 1000.0
}

fn rounded_millisecond(value: f64) -> f64 {
    (value * 10.0).round() / 10.0
}

fn supervision_options() -> Result<SupervisionOptions, String> {
    let defaults = SupervisionOptions::default();
    Ok(SupervisionOptions {
        diagnostic_interval: positive_milliseconds(
            std::env::var("SUPERCOV_DIAGNOSTIC_INTERVAL_MS")
                .ok()
                .as_deref(),
            "SUPERCOV_DIAGNOSTIC_INTERVAL_MS",
        )
        .map_err(|error| error.to_string())?
        .unwrap_or(defaults.diagnostic_interval),
        timeout: positive_milliseconds(
            std::env::var("SUPERCOV_COMMAND_TIMEOUT_MS").ok().as_deref(),
            "SUPERCOV_COMMAND_TIMEOUT_MS",
        )
        .map_err(|error| error.to_string())?,
        termination_grace: defaults.termination_grace,
    })
}

fn environment_with(values: BTreeMap<String, String>) -> Vec<(OsString, OsString)> {
    let mut environment = std::env::vars_os().collect::<BTreeMap<_, _>>();
    for (key, value) in values {
        environment.insert(key.into(), value.into());
    }
    environment.into_iter().collect()
}

fn node_options(preload: &Path) -> String {
    [
        std::env::var("NODE_OPTIONS").ok(),
        Some("--enable-source-maps".into()),
        Some(format!("--import={}", preload.display())),
    ]
    .into_iter()
    .flatten()
    .filter(|value| !value.is_empty())
    .collect::<Vec<_>>()
    .join(" ")
}

/// Fingerprint the current JavaScript project using the same discovery and
/// runtime-shim inputs as a Rust-owned execution. Query callers deliberately
/// treat failure as "staleness unavailable", matching the frozen CLI contract.
pub fn current_javascript_integrity(
    root: &Path,
    command: &[String],
) -> Result<RunIntegrity, String> {
    let environment = std::env::vars().collect::<BTreeMap<_, _>>();
    let project = discover_coverage_project(root, &environment, command)
        .map_err(|error| error.to_string())?;
    javascript_integrity_for_project(root, &project)
}

fn javascript_integrity_for_project(
    root: &Path,
    project: &crate::project_discovery::CoverageProject,
) -> Result<RunIntegrity, String> {
    let frontend = FrontendIntegrityInputs::embedded_javascript();
    create_run_integrity(root, project, &frontend).map_err(|error| error.to_string())
}

/// Execute one JavaScript suite with every language-neutral stage owned by
/// Rust. Target-language runtime and runner adapters remain generated shims.
pub fn run_direct_javascript(
    request: &DirectJavascriptRunRequest,
    diagnostics: &mut dyn std::io::Write,
) -> Result<DirectJavascriptRunResult, DirectJavascriptRunError> {
    if request.command.is_empty() {
        return Err(DirectJavascriptRunError::Failed(
            "test command must not be empty".into(),
        ));
    }
    let total_started = Instant::now();
    let initialization_started = Instant::now();
    let root = fs::canonicalize(&request.root)
        .map_err(|error| format!("{}: {error}", request.root.display()))?;
    let nonce = now_nonce();
    let run_id = request
        .run_id
        .clone()
        .unwrap_or_else(|| format!("rust-{nonce}"));
    let started_at = request
        .started_at
        .clone()
        .unwrap_or_else(|| format!("unix-ms-{nonce}"));
    let lock =
        ProjectLock::acquire(&root, &run_id, &started_at).map_err(|error| error.to_string())?;
    let mut cleanup = RunCleanup {
        root: root.clone(),
        run_id: run_id.clone(),
        started_at: started_at.clone(),
        lock,
        workspace: None,
        state_written: false,
        terminal_recorded: false,
    };
    let recovered_runs =
        recover_abandoned_runs(&root, &started_at).map_err(|error| error.to_string())?;
    if !recovered_runs.is_empty() {
        writeln!(
            diagnostics,
            "[supercov] recovered abandoned run(s): {}",
            recovered_runs.join(", ")
        )
        .map_err(|error| error.to_string())?;
    }
    let environment = std::env::vars().collect::<BTreeMap<_, _>>();
    let project = discover_coverage_project(&root, &environment, &request.command)
        .map_err(|error| error.to_string())?;
    let integrity = javascript_integrity_for_project(&root, &project)?;
    let build_cache_key = build_cache_key(&integrity, &project)?;
    let prior_workspace = cached_workspace_path(&root).map_err(|error| error.to_string())?;
    let reusable_build = if project.build_adapter == BuildAdapter::Direct {
        None
    } else {
        read_build_cache(&prior_workspace, &build_cache_key)
    };
    let cached_paths = reusable_build.as_ref().map(reuse_paths).unwrap_or_default();
    let initialization_ms = elapsed_ms(initialization_started);

    let workspace_started = Instant::now();
    let workspace = prepare_cached_workspace(&root, cleanup.lock(), &cached_paths)
        .map_err(|error| error.to_string())?;
    cleanup.set_workspace(workspace.clone());
    let workspace_preparation_ms = elapsed_ms(workspace_started);
    writeln!(
        diagnostics,
        "[supercov] instrumenting isolated workspace {}",
        workspace.display()
    )
    .map_err(|error| error.to_string())?;
    cleanup.mark_state_written();
    write_run_state(
        &root,
        &RunState {
            id: run_id.clone(),
            pid: std::process::id(),
            root: root.display().to_string(),
            workspace: workspace.display().to_string(),
            started_at: started_at.clone(),
            updated_at: started_at.clone(),
            status: RunStateStatus::Preparing,
            signal: None,
            error: None,
        },
    )
    .map_err(|error| error.to_string())?;

    let adapter_started = Instant::now();
    let collector_id = format!("collector-{}", integrity.fingerprint.execution);
    let frontend = prepare_javascript_frontend(&workspace, &project, &collector_id)
        .map_err(|error| error.to_string())?;
    let adapter_setup_ms = elapsed_ms(adapter_started);

    let evidence_relative = format!(".supercov/evidence/{run_id}");
    let evidence_directory = workspace.join(&evidence_relative);
    let server_evidence_root = workspace.join(".supercov/server-evidence");
    let diagnostic_owner = workspace.join(format!(".supercov/diagnostic-owner-{run_id}"));
    let mut overrides = BTreeMap::from([
        ("NODE_OPTIONS".into(), node_options(&frontend.preload_path)),
        ("SUPERCOV_CJS_INTERCEPT".into(), "1".into()),
        ("SUPERCOV_DIRECT_INSTRUMENTATION".into(), "1".into()),
        ("SUPERCOV_EVIDENCE_DIR".into(), evidence_relative.clone()),
        (
            "SUPERCOV_DIAGNOSTIC_OWNER_FILE".into(),
            diagnostic_owner.display().to_string(),
        ),
        (
            "SUPERCOV_EXECUTION_FINGERPRINT".into(),
            integrity.fingerprint.execution.clone(),
        ),
        (
            "SUPERCOV_EXECUTION_LOG".into(),
            evidence_directory
                .join("execution.jsonl")
                .display()
                .to_string(),
        ),
        (
            "SUPERCOV_MANIFEST".into(),
            frontend.manifest_path.display().to_string(),
        ),
        (
            "SUPERCOV_PROJECT_ROOT".into(),
            workspace.display().to_string(),
        ),
        ("SUPERCOV_RUN_ID".into(), run_id.clone()),
        (
            "SUPERCOV_SERVER_EVIDENCE_ROOT".into(),
            server_evidence_root.display().to_string(),
        ),
        (
            "SUPERCOV_SOURCE_PROJECT_ROOT".into(),
            root.display().to_string(),
        ),
    ]);
    // A wrapped npm/pnpm/yarn script can launch either runner several process
    // generations later. The preload is the discovery boundary, so always
    // provide both generated configs; each runner ignores the unrelated one.
    overrides.insert(
        "SUPERCOV_GENERATED_VITEST_CONFIG".into(),
        frontend.vitest_config_path.display().to_string(),
    );
    overrides.insert(
        "SUPERCOV_GENERATED_PLAYWRIGHT_CONFIG".into(),
        frontend.playwright_config_path.display().to_string(),
    );
    overrides.insert(
        "SUPERCOV_PLAYWRIGHT_MODULE".into(),
        project.playwright_module.clone(),
    );
    overrides.insert(
        "SUPERCOV_PLAYWRIGHT_TEST_EXPORT".into(),
        project.playwright_test_export.clone(),
    );
    overrides.insert(
        "SUPERCOV_PLAYWRIGHT_WRAPPER".into(),
        "./.supercov/playwright.js".into(),
    );
    if let Some(original) = project
        .playwright_config
        .as_ref()
        .and_then(|path| path.strip_prefix(&root).ok())
        .map(|path| workspace.join(path))
    {
        overrides.insert(
            "SUPERCOV_ORIGINAL_PLAYWRIGHT_CONFIG".into(),
            original.display().to_string(),
        );
    }
    overrides.extend(project.build_environment.clone());
    let preparation = if reusable_build.is_some() {
        writeln!(
            diagnostics,
            "[supercov] reusing exact-fingerprint instrumented build {}",
            &build_cache_key[..12]
        )
        .map_err(|error| error.to_string())?;
        Vec::new()
    } else if project.build_adapter != BuildAdapter::Direct {
        let mut arguments = project.build_command[1..]
            .iter()
            .map(OsString::from)
            .collect::<Vec<_>>();
        if project.build_adapter == BuildAdapter::Vite {
            arguments.extend([
                OsString::from("--"),
                OsString::from("--config"),
                OsString::from(".supercov/vite.config.mjs"),
            ]);
        }
        let mut build_overrides = overrides.clone();
        build_overrides.insert("NODE_ENV".into(), "production".into());
        let build_environment = environment_with(build_overrides);
        vec![ExecutionPhase {
            name: "build".into(),
            kind: PhaseKind::Build,
            command: CommandSpec {
                program: project.build_command[0].clone().into(),
                arguments,
                cwd: workspace.clone(),
                environment: Some(build_environment),
            },
        }]
    } else {
        Vec::new()
    };
    let plan = ExecutionPlan {
        preparation,
        test: ExecutionPhase {
            name: "test".into(),
            kind: PhaseKind::Test,
            command: CommandSpec {
                program: request.command[0].clone().into(),
                arguments: request.command[1..].iter().map(OsString::from).collect(),
                cwd: workspace.clone(),
                environment: Some(environment_with(overrides)),
            },
        },
    };
    let options = supervision_options()?;
    let execution = match execute_plan(&plan, options, diagnostics, |phase, diagnostics| {
        let status = if phase.kind == PhaseKind::Test {
            if frontend.assertion_calls > 0 {
                writeln!(
                    diagnostics,
                    "[supercov] attributed {} native node:assert call(s)",
                    frontend.assertion_calls
                )
                .map_err(|error| OrchestrationError::PhaseSetup {
                    phase: phase.name.clone(),
                    reason: error.to_string(),
                })?;
            }
            writeln!(
                diagnostics,
                "[supercov] running in isolated workspace: {}",
                request.command.join(" ")
            )
            .map_err(|error| OrchestrationError::PhaseSetup {
                phase: phase.name.clone(),
                reason: error.to_string(),
            })?;
            RunStateStatus::Testing
        } else {
            RunStateStatus::Building
        };
        update_run_state(&root, &run_id, status, &started_at, None).map_err(|error| {
            OrchestrationError::PhaseSetup {
                phase: phase.name.clone(),
                reason: error.to_string(),
            }
        })?;
        Ok(())
    }) {
        Ok(execution) => execution,
        Err(error) => {
            let message = error.to_string();
            let state_updated = update_run_state(
                &root,
                &run_id,
                RunStateStatus::Failed,
                &started_at,
                Some(message.clone()),
            )
            .is_ok();
            if state_updated {
                cleanup.mark_terminal();
            }
            return Err(message.into());
        }
    };
    let instrumented_build_ms = execution
        .phases
        .iter()
        .find(|phase| phase.kind == PhaseKind::Build)
        .map_or(0.0, |phase| phase.duration_ms as f64);
    let test_command_ms = execution
        .phases
        .iter()
        .find(|phase| phase.kind == PhaseKind::Test)
        .map_or(0.0, |phase| phase.duration_ms as f64);
    let build_succeeded = execution
        .phases
        .iter()
        .find(|phase| phase.kind == PhaseKind::Build)
        .is_some_and(|phase| phase.result.exit_code() == 0);
    if project.build_adapter != BuildAdapter::Direct && reusable_build.is_none() && build_succeeded
    {
        write_build_cache(&root, &workspace, &build_cache_key, &started_at)?;
    }
    if let Some(signal) = execution.interrupted_signal {
        interrupt_run_state(&root, &run_id, &started_at, signal_name(signal))
            .map_err(|error| error.to_string())?;
        cleanup.mark_terminal();
        return Err(DirectJavascriptRunError::Interrupted {
            signal,
            exit_code: execution.exit_code,
            timings: RunTimings {
                initialization_ms: rounded_millisecond(initialization_ms),
                workspace_preparation_ms: rounded_millisecond(workspace_preparation_ms),
                adapter_setup_ms: rounded_millisecond(adapter_setup_ms),
                instrumented_build_ms: rounded_millisecond(instrumented_build_ms),
                test_command_ms: rounded_millisecond(test_command_ms),
                evidence_publication_ms: 0.0,
            },
            total_ms: rounded_millisecond(elapsed_ms(total_started)),
        });
    }
    update_run_state(
        &root,
        &run_id,
        RunStateStatus::Publishing,
        &started_at,
        None,
    )
    .map_err(|error| error.to_string())?;

    let publication_started = Instant::now();
    let archive_path = root
        .join(".supercov/work")
        .join(&run_id)
        .join("evidence.raw.gz");
    let entries = collect_sources(&[
        EvidenceArchiveSource::File {
            file: frontend.manifest_path,
            path: "manifest.json".into(),
        },
        EvidenceArchiveSource::Directory {
            directory: evidence_directory,
            prefix: None,
        },
        EvidenceArchiveSource::Directory {
            directory: server_evidence_root.join(&run_id),
            prefix: Some("server".into()),
        },
    ])
    .map_err(|error| error.to_string())?;
    let raw = write_archive(entries, &archive_path).map_err(|error| error.to_string())?;
    remove_stored_tree_deferred(&root, &workspace.join(".supercov/evidence"))
        .map_err(|error| error.to_string())?;
    remove_stored_tree_deferred(&root, &server_evidence_root).map_err(|error| error.to_string())?;
    let evidence_publication_ms = elapsed_ms(publication_started);
    let timings = RunTimings {
        initialization_ms: rounded_millisecond(initialization_ms),
        workspace_preparation_ms: rounded_millisecond(workspace_preparation_ms),
        adapter_setup_ms: rounded_millisecond(adapter_setup_ms),
        instrumented_build_ms: rounded_millisecond(instrumented_build_ms),
        test_command_ms: rounded_millisecond(test_command_ms),
        evidence_publication_ms: rounded_millisecond(evidence_publication_ms),
    };
    let metadata = RunMetadata {
        id: run_id.clone(),
        started_at: started_at.clone(),
        duration_ms: rounded_millisecond(elapsed_ms(total_started)),
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
        isolated_build: Some(true),
        instrumented_build_cache: Some(InstrumentedBuildCache {
            key: build_cache_key,
            reused: reusable_build.is_some(),
        }),
        timings: Some(timings),
        merged: None,
        parents: None,
    };
    let run_directory =
        publish_run(&root, &metadata, &archive_path).map_err(|error| error.to_string())?;
    let terminal_status = if execution.exit_code == 0 {
        RunStateStatus::Complete
    } else {
        RunStateStatus::Failed
    };
    update_run_state(&root, &run_id, terminal_status, &started_at, None)
        .map_err(|error| error.to_string())?;
    finalize_published_run(&root, &run_id).map_err(|error| error.to_string())?;
    cleanup.mark_terminal();
    Ok(DirectJavascriptRunResult {
        run_id,
        run_directory,
        workspace,
        exit_code: execution.exit_code,
        assertion_calls: frontend.assertion_calls,
        recovered_runs,
        metadata,
    })
}
