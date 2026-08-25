//! First private end-to-end Rust-owned JavaScript execution path.
//!
//! This intentionally supports only direct source-executing suites while the
//! Rust frontend gates are incomplete. It is not a public engine selector.

use std::{
    collections::BTreeMap,
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    time::{Instant, SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};

use crate::{
    evidence_archive::{EvidenceArchiveSource, collect_sources, write_archive},
    integrity::{FrontendIntegrityInputs, create_run_integrity},
    javascript_frontend::{javascript_runtime_files, prepare_javascript_frontend},
    lifecycle::{
        ProjectLock, RunState, RunStateStatus, finalize_published_run, publish_run,
        remove_stored_tree_deferred, update_run_state, write_run_state,
    },
    orchestration::{ExecutionPhase, ExecutionPlan, PhaseKind, execute_plan},
    process_supervision::{CommandSpec, SupervisionOptions},
    project_discovery::{BuildAdapter, command_uses_tool, discover_coverage_project},
    run_store::{RawEvidenceMetadata, RunMetadata, RunTimings},
    workspace::{prepare_cached_workspace, prune_cached_workspace_sources},
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DirectJavascriptRunRequest {
    pub root: PathBuf,
    pub runtime_root: PathBuf,
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
    pub metadata: RunMetadata,
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
        Some(format!("--import={}", preload.display())),
    ]
    .into_iter()
    .flatten()
    .filter(|value| !value.is_empty())
    .collect::<Vec<_>>()
    .join(" ")
}

/// Execute one direct JavaScript suite with every language-neutral stage owned
/// by Rust. Errors are intentionally rendered at this private boundary; public
/// activation will expose stable structured error codes.
pub fn run_direct_javascript(
    request: &DirectJavascriptRunRequest,
    diagnostics: &mut dyn std::io::Write,
) -> Result<DirectJavascriptRunResult, String> {
    if request.command.is_empty() {
        return Err("test command must not be empty".into());
    }
    let total_started = Instant::now();
    let initialization_started = Instant::now();
    let root = fs::canonicalize(&request.root)
        .map_err(|error| format!("{}: {error}", request.root.display()))?;
    let runtime_root = fs::canonicalize(&request.runtime_root)
        .map_err(|error| format!("{}: {error}", request.runtime_root.display()))?;
    let nonce = now_nonce();
    let run_id = request
        .run_id
        .clone()
        .unwrap_or_else(|| format!("rust-{nonce}"));
    let started_at = request
        .started_at
        .clone()
        .unwrap_or_else(|| format!("unix-ms-{nonce}"));
    let mut lock =
        ProjectLock::acquire(&root, &run_id, &started_at).map_err(|error| error.to_string())?;
    let environment = std::env::vars().collect::<BTreeMap<_, _>>();
    let project = discover_coverage_project(&root, &environment, &request.command)
        .map_err(|error| error.to_string())?;
    if project.build_adapter == BuildAdapter::Generic {
        return Err(format!(
            "private Rust vertical slice does not yet support generic builds, discovered {:?}",
            project.build_adapter
        ));
    }
    let runtime_files = javascript_runtime_files(&runtime_root);
    let integrity = create_run_integrity(
        &root,
        &project,
        &FrontendIntegrityInputs::javascript(runtime_root.clone(), runtime_files),
    )
    .map_err(|error| error.to_string())?;
    let initialization_ms = elapsed_ms(initialization_started);

    let workspace_started = Instant::now();
    let workspace =
        prepare_cached_workspace(&root, &lock, &[]).map_err(|error| error.to_string())?;
    let workspace_preparation_ms = elapsed_ms(workspace_started);
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
    let frontend = prepare_javascript_frontend(&workspace, &project, &runtime_root, &collector_id)
        .map_err(|error| error.to_string())?;
    let adapter_setup_ms = elapsed_ms(adapter_started);
    update_run_state(&root, &run_id, RunStateStatus::Testing, &started_at, None)
        .map_err(|error| error.to_string())?;

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
    if project.vitest_config.is_some() || command_uses_tool(&root, &request.command, "vitest") {
        overrides.insert(
            "SUPERCOV_GENERATED_VITEST_CONFIG".into(),
            frontend.vitest_config_path.display().to_string(),
        );
    }
    if project.playwright_config.is_some()
        || command_uses_tool(&root, &request.command, "playwright")
    {
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
    }
    overrides.extend(project.build_environment.clone());
    let preparation = if project.build_adapter == BuildAdapter::Vite {
        let mut arguments = project.build_command[1..]
            .iter()
            .map(OsString::from)
            .collect::<Vec<_>>();
        arguments.extend([
            OsString::from("--"),
            OsString::from("--config"),
            OsString::from(".supercov/vite.config.mjs"),
        ]);
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
    let execution = match execute_plan(
        &plan,
        SupervisionOptions::default(),
        diagnostics,
        |_| Ok(()),
    ) {
        Ok(execution) => execution,
        Err(error) => {
            let message = error.to_string();
            let _ = update_run_state(
                &root,
                &run_id,
                RunStateStatus::Failed,
                &started_at,
                Some(message.clone()),
            );
            return Err(message);
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
        initialization_ms,
        workspace_preparation_ms,
        adapter_setup_ms,
        instrumented_build_ms,
        test_command_ms,
        evidence_publication_ms,
    };
    let metadata = RunMetadata {
        id: run_id.clone(),
        started_at: started_at.clone(),
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
        isolated_build: Some(true),
        instrumented_build_cache: None,
        timings: Some(timings),
        merged: None,
        parents: None,
    };
    let run_directory =
        publish_run(&root, &metadata, &archive_path).map_err(|error| error.to_string())?;
    update_run_state(&root, &run_id, RunStateStatus::Complete, &started_at, None)
        .map_err(|error| error.to_string())?;
    prune_cached_workspace_sources(&root, &lock).map_err(|error| error.to_string())?;
    finalize_published_run(&root, &run_id).map_err(|error| error.to_string())?;
    lock.release().map_err(|error| error.to_string())?;
    Ok(DirectJavascriptRunResult {
        run_id,
        run_directory,
        workspace,
        exit_code: execution.exit_code,
        assertion_calls: frontend.assertion_calls,
        metadata,
    })
}
