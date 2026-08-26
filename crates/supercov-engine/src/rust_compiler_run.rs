//! Private transactional lifecycle for the compiler-owned Rust frontend.

use std::{fs, io::Write, path::PathBuf, time::Instant};

use serde::{Deserialize, Serialize};

use crate::{
    coverage_report::{ArchiveReportRequest, ExitCodeInput, analyze_coverage_archive},
    evidence_archive::write_archive,
    lifecycle::{
        ProjectLock, finalize_published_run, publish_run, recover_abandoned_runs,
        remove_stored_tree_deferred,
    },
    run_store::{RawEvidenceMetadata, RunMetadata, RunTimings},
    rust_compiler_test_runner::{RustCompilerRunRequest, run_rust_compiler_frontend},
    rust_run::current_rust_integrity,
    workspace::{cached_workspace_path, prepare_cached_workspace, recover_cached_workspace},
};

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DirectRustCompilerRunRequest {
    pub root: PathBuf,
    pub command: Vec<String>,
    pub run_id: String,
    pub started_at: String,
    pub wrapper_path: PathBuf,
    pub companion_candidates: Vec<PathBuf>,
    pub require_public_capabilities: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DirectRustCompilerRunResult {
    pub run_id: String,
    pub run_directory: PathBuf,
    pub exit_code: i32,
    pub tests: usize,
    pub setup_results: usize,
    pub background_results: usize,
    pub artifacts: usize,
    pub selection: crate::rust_compiler_selection::SelectedRustCompilerCompanion,
    pub denominator: RustCompilerDenominatorCounts,
    pub attempt_health: Vec<crate::rust_compiler_test_runner::RustCompilerAttemptHealth>,
    pub summary: crate::coverage_analysis::CoverageSummary,
    pub recovered_runs: Vec<String>,
    pub metadata: RunMetadata,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RustCompilerDenominatorCounts {
    pub points: usize,
    pub branches: usize,
    pub decisions: usize,
    pub limitations: usize,
}

fn elapsed_ms(started: Instant) -> f64 {
    (started.elapsed().as_secs_f64() * 10_000.0).round() / 10.0
}

pub fn run_direct_rust_compiler(
    request: &DirectRustCompilerRunRequest,
    diagnostics: &mut dyn Write,
) -> Result<DirectRustCompilerRunResult, String> {
    if request.command.is_empty() || request.companion_candidates.is_empty() {
        return Err("test command and exact compiler companion candidates are required".into());
    }
    let total_started = Instant::now();
    let initialization_started = Instant::now();
    let root = fs::canonicalize(&request.root)
        .map_err(|error| format!("{}: {error}", request.root.display()))?;
    let mut lock = ProjectLock::acquire(&root, &request.run_id, &request.started_at)
        .map_err(|error| error.to_string())?;
    let initialization_ms = elapsed_ms(initialization_started);
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
        let integrity = current_rust_integrity(&root, &request.command)?;
        let workspace_started = Instant::now();
        recover_cached_workspace(&root, &lock).map_err(|error| error.to_string())?;
        let workspace =
            prepare_cached_workspace(&root, &lock, &[]).map_err(|error| error.to_string())?;
        let workspace_preparation_ms = elapsed_ms(workspace_started);
        let adapter_setup_ms = (elapsed_ms(adapter_started) - workspace_preparation_ms).max(0.0);

        let run = run_rust_compiler_frontend(
            &RustCompilerRunRequest {
                project_root: workspace.clone(),
                command: request.command.clone(),
                run_id: request.run_id.clone(),
                generated_at: request.started_at.clone(),
                wrapper_path: request.wrapper_path.clone(),
                companion_candidates: request.companion_candidates.clone(),
                require_public_capabilities: request.require_public_capabilities,
            },
            diagnostics,
        )
        .map_err(|error| error.to_string())?;

        let publication_started = Instant::now();
        let archive_path = root
            .join(".supercov/work")
            .join(&request.run_id)
            .join("evidence.raw.gz");
        let raw = write_archive(
            run.archive_entries().map_err(|error| error.to_string())?,
            &archive_path,
        )
        .map_err(|error| error.to_string())?;
        let report = analyze_coverage_archive(&ArchiveReportRequest {
            archive_path: archive_path.clone(),
            run_id: request.run_id.clone(),
            generated_at: request.started_at.clone(),
            integrity: None,
            test_exit_code: ExitCodeInput::Present(Some(run.exit_code)),
        })
        .map_err(|error| format!("archived Rust compiler evidence is invalid: {error:?}"))?;
        let evidence_publication_ms = elapsed_ms(publication_started);
        let timings = RunTimings {
            initialization_ms,
            workspace_preparation_ms,
            adapter_setup_ms,
            instrumented_build_ms: (run.build_ms * 10.0).round() / 10.0,
            test_command_ms: (run.execution_ms * 10.0).round() / 10.0,
            evidence_publication_ms,
        };
        let metadata = RunMetadata {
            id: request.run_id.clone(),
            started_at: request.started_at.clone(),
            duration_ms: elapsed_ms(total_started),
            command: request.command.clone(),
            test_exit_code: Some(run.exit_code),
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
        let tests = run
            .request
            .raw_results
            .iter()
            .filter(|result| result.role == "test")
            .count();
        let background_results = run
            .request
            .raw_results
            .iter()
            .filter(|result| result.role == "background")
            .count();
        let setup_results = run
            .request
            .raw_results
            .iter()
            .filter(|result| result.role == "setup")
            .count();
        let denominator = RustCompilerDenominatorCounts {
            points: run.request.manifest.points.len(),
            branches: run.request.manifest.branches.len(),
            decisions: run.request.manifest.decisions.len(),
            limitations: run.request.manifest.limitations.len(),
        };
        let run_directory =
            publish_run(&root, &metadata, &archive_path).map_err(|error| error.to_string())?;
        finalize_published_run(&root, &request.run_id).map_err(|error| error.to_string())?;
        remove_stored_tree_deferred(
            &root,
            &workspace.join(".supercov/work").join(&request.run_id),
        )
        .map_err(|error| error.to_string())?;
        Ok(DirectRustCompilerRunResult {
            run_id: request.run_id.clone(),
            run_directory,
            exit_code: run.exit_code,
            tests,
            setup_results,
            background_results,
            artifacts: run.artifacts,
            selection: run.selection,
            denominator,
            attempt_health: run.attempt_health,
            summary: report.view.summary,
            recovered_runs,
            metadata,
        })
    })();
    if result.is_err() {
        let _ =
            remove_stored_tree_deferred(&root, &root.join(".supercov/work").join(&request.run_id));
        if let Ok(workspace) = cached_workspace_path(&root) {
            let _ = remove_stored_tree_deferred(
                &root,
                &workspace.join(".supercov/work").join(&request.run_id),
            );
        }
    }
    let release = lock.release().map_err(|error| error.to_string());
    match (result, release) {
        (Ok(result), Ok(())) => Ok(result),
        (Err(error), _) => Err(error),
        (Ok(_), Err(error)) => Err(error),
    }
}
