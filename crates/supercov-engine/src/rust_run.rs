//! Public, isolated Rust coverage run lifecycle.

use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    time::Instant,
};

use serde::{Deserialize, Serialize};

use crate::{
    evidence_archive::write_archive_v3,
    integrity::{ExplicitIntegrityInputs, FrontendIntegrityInputs, create_explicit_run_integrity},
    lifecycle::{
        ProjectLock, finalize_published_run, publish_run, recover_abandoned_runs,
        remove_stored_tree_deferred,
    },
    run_store::{RawEvidenceMetadata, RunMetadata, RunTimings},
    rust_project::{discover_rust_source_files, prepare_rust_project},
    rust_test_runner::run_prepared_rust_tests,
    workspace::prepare_isolated_workspace,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DirectRustRunRequest {
    pub root: PathBuf,
    pub command: Vec<String>,
    pub run_id: String,
    pub started_at: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DirectRustRunResult {
    pub run_id: String,
    pub run_directory: PathBuf,
    pub exit_code: i32,
    pub tests: usize,
    pub artifacts: usize,
    pub recovered_runs: Vec<String>,
    pub metadata: RunMetadata,
}

fn elapsed_ms(started: Instant) -> f64 {
    (started.elapsed().as_secs_f64() * 10_000.0).round() / 10.0
}

fn collect_integrity_inputs(
    root: &Path,
    source_files: &[String],
    command: &[String],
) -> ExplicitIntegrityInputs {
    let source_files = source_files.iter().map(PathBuf::from).collect::<Vec<_>>();
    // Inline `#[cfg(test)]` modules make every Rust source file a possible test
    // input. Hashing the same file in both domains is intentional and prevents
    // stale reuse when only an inline test changes.
    let test_files = source_files.clone();
    let dependency_files = ["Cargo.toml", "Cargo.lock"]
        .into_iter()
        .map(PathBuf::from)
        .filter(|path| root.join(path).is_file())
        .collect();
    let configuration_files = [
        "rust-toolchain",
        "rust-toolchain.toml",
        ".cargo/config",
        ".cargo/config.toml",
    ]
    .into_iter()
    .map(PathBuf::from)
    .filter(|path| root.join(path).is_file())
    .collect();
    ExplicitIntegrityInputs {
        source_files,
        test_files,
        dependency_files,
        configuration_files,
        execution_configuration: command.join("\0").into_bytes(),
    }
}

pub fn current_rust_integrity(
    root: &Path,
    command: &[String],
) -> Result<crate::run_store::RunIntegrity, String> {
    let root = fs::canonicalize(root).map_err(|error| error.to_string())?;
    let source_files = discover_rust_source_files(&root).map_err(|error| error.to_string())?;
    create_explicit_run_integrity(
        &root,
        &collect_integrity_inputs(&root, &source_files, command),
        &FrontendIntegrityInputs::embedded_rust(),
    )
    .map_err(|error| error.to_string())
}

pub fn run_direct_rust(
    request: &DirectRustRunRequest,
    diagnostics: &mut dyn Write,
) -> Result<DirectRustRunResult, String> {
    if request.command.is_empty() {
        return Err("test command must not be empty".into());
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

        let workspace_started = Instant::now();
        let workspace = prepare_isolated_workspace(&root, &request.run_id, &lock)
            .map_err(|error| error.to_string())?;
        let workspace_preparation_ms = elapsed_ms(workspace_started);
        writeln!(
            diagnostics,
            "[supercov] detected Rust; instrumenting isolated Cargo workspace {}",
            workspace.display()
        )
        .map_err(|error| error.to_string())?;

        let adapter_started = Instant::now();
        let project = prepare_rust_project(&workspace).map_err(|error| error.to_string())?;
        let integrity_inputs =
            collect_integrity_inputs(&root, &project.source_files, &request.command);
        let integrity = create_explicit_run_integrity(
            &root,
            &integrity_inputs,
            &FrontendIntegrityInputs::embedded_rust(),
        )
        .map_err(|error| error.to_string())?;
        let adapter_setup_ms = elapsed_ms(adapter_started);

        writeln!(
            diagnostics,
            "[supercov] building once and running each libtest case in its own process"
        )
        .map_err(|error| error.to_string())?;
        let run = run_prepared_rust_tests(
            &project,
            &request.command,
            &request.run_id,
            &request.started_at,
            diagnostics,
        )
        .map_err(|error| error.to_string())?;

        let publication_started = Instant::now();
        let archive_path = root
            .join(".supercov/work")
            .join(&request.run_id)
            .join("evidence.raw.gz");
        let raw = write_archive_v3(
            run.archive_v3_entries()
                .map_err(|error| error.to_string())?,
            &archive_path,
        )
        .map_err(|error| error.to_string())?;
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
        let run_directory =
            publish_run(&root, &metadata, &archive_path).map_err(|error| error.to_string())?;
        finalize_published_run(&root, &request.run_id).map_err(|error| error.to_string())?;
        Ok(DirectRustRunResult {
            run_id: request.run_id.clone(),
            run_directory,
            exit_code: run.exit_code,
            tests: run.request.raw_results.len(),
            artifacts: run.artifacts,
            recovered_runs,
            metadata,
        })
    })();
    if result.is_err() {
        let _ =
            remove_stored_tree_deferred(&root, &root.join(".supercov/work").join(&request.run_id));
    }
    let release = lock.release().map_err(|error| error.to_string());
    match (result, release) {
        (Ok(result), Ok(())) => Ok(result),
        (Err(error), _) => Err(error),
        (Ok(_), Err(error)) => Err(error),
    }
}
