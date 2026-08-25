//! Public, isolated Rust coverage run lifecycle.

use std::{
    collections::BTreeSet,
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
    run_store::{InstrumentedBuildCache, RawEvidenceMetadata, RunMetadata, RunTimings},
    rust_build_cache::{
        read_rust_build_cache, rust_build_cache_key, rust_target_directory, write_rust_build_cache,
    },
    rust_project::{PreparedRustProject, prepare_rust_project},
    rust_test_runner::run_prepared_rust_tests,
    workspace::{cached_workspace_path, prepare_cached_workspace, recover_cached_workspace},
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

#[cfg(unix)]
fn os_string_bytes(value: &std::ffi::OsStr) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt as _;
    value.as_bytes().to_vec()
}

#[cfg(windows)]
fn os_string_bytes(value: &std::ffi::OsStr) -> Vec<u8> {
    use std::os::windows::ffi::OsStrExt as _;
    value
        .encode_wide()
        .flat_map(u16::to_le_bytes)
        .collect::<Vec<_>>()
}

#[cfg(not(any(unix, windows)))]
fn os_string_bytes(value: &std::ffi::OsStr) -> Vec<u8> {
    value.to_string_lossy().as_bytes().to_vec()
}

fn append_identity_field(destination: &mut Vec<u8>, value: &[u8]) {
    destination.extend_from_slice(&(value.len() as u64).to_le_bytes());
    destination.extend_from_slice(value);
}

const ROOT_INPUT_EXCLUSIONS: &[&str] = &[
    ".cache",
    ".git",
    ".supercov",
    ".mcdc-pool",
    "node_modules",
    "target",
    "build",
    "dist",
    ".next",
    ".nuxt",
    ".output",
    "coverage",
    "playwright-report",
    "test-results",
];

fn collect_project_inputs(
    root: &Path,
    directory: &Path,
    root_level: bool,
    regular: &mut Vec<PathBuf>,
    links: &mut Vec<String>,
) -> Result<(), String> {
    let mut entries = fs::read_dir(directory)
        .map_err(|error| format!("{}: {error}", directory.display()))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| error.to_string())?;
    entries.sort_by_key(fs::DirEntry::file_name);
    for entry in entries {
        let path = entry.path();
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| format!("Rust project contains a non-UTF-8 path: {}", path.display()))?;
        if (root_level && ROOT_INPUT_EXCLUSIONS.contains(&name.as_str()))
            || matches!(name.as_str(), ".supercov" | ".mcdc-pool")
        {
            continue;
        }
        let file_type = entry.file_type().map_err(|error| error.to_string())?;
        if file_type.is_dir() {
            collect_project_inputs(root, &path, false, regular, links)?;
        } else if file_type.is_file() {
            let relative = path
                .strip_prefix(root)
                .map_err(|_| format!("project input escaped root: {}", path.display()))?;
            regular.push(relative.to_owned());
        } else if file_type.is_symlink() {
            let relative = path
                .strip_prefix(root)
                .map_err(|_| format!("project link escaped root: {}", path.display()))?;
            let target = fs::read_link(&path).map_err(|error| error.to_string())?;
            links.push(format!(
                "{}=>{}",
                relative.to_string_lossy().replace('\\', "/"),
                target.to_string_lossy().replace('\\', "/")
            ));
        } else {
            return Err(format!(
                "unsupported Rust project input: {}",
                path.display()
            ));
        }
    }
    Ok(())
}

fn collect_integrity_inputs(
    root: &Path,
    command: &[String],
) -> Result<ExplicitIntegrityInputs, String> {
    let mut files = Vec::new();
    let mut links = Vec::new();
    collect_project_inputs(root, root, true, &mut files, &mut links)?;
    files.sort();
    files.dedup();
    links.sort();
    links.dedup();
    let source_files = files
        .iter()
        .filter(|path| path.extension().and_then(|value| value.to_str()) == Some("rs"))
        .cloned()
        .collect::<Vec<_>>();
    // Inline `#[cfg(test)]` modules make every Rust source file a possible test
    // input. Hashing the same file in both domains is intentional and prevents
    // stale reuse when only an inline test changes.
    let test_files = source_files.clone();
    let dependency_files = files
        .iter()
        .filter(|path| {
            path.file_name()
                .and_then(|value| value.to_str())
                .is_some_and(|name| matches!(name, "Cargo.toml" | "Cargo.lock"))
        })
        .cloned()
        .collect::<Vec<_>>();
    let source_set = source_files.iter().cloned().collect::<BTreeSet<_>>();
    let dependency_set = dependency_files.iter().cloned().collect::<BTreeSet<_>>();
    let configuration_files = files
        .into_iter()
        .filter(|path| !source_set.contains(path) && !dependency_set.contains(path))
        .collect();
    let mut execution_configuration = command.join("\0").into_bytes();
    for link in links {
        execution_configuration.push(0);
        execution_configuration.extend_from_slice(link.as_bytes());
    }
    let mut environment = std::env::vars_os()
        .map(|(key, value)| (os_string_bytes(&key), os_string_bytes(&value)))
        .collect::<Vec<_>>();
    environment.sort();
    for (key, value) in environment {
        append_identity_field(&mut execution_configuration, &key);
        append_identity_field(&mut execution_configuration, &value);
    }
    Ok(ExplicitIntegrityInputs {
        source_files,
        test_files,
        dependency_files,
        configuration_files,
        execution_configuration,
    })
}

pub fn current_rust_integrity(
    root: &Path,
    command: &[String],
) -> Result<crate::run_store::RunIntegrity, String> {
    let root = fs::canonicalize(root).map_err(|error| error.to_string())?;
    create_explicit_run_integrity(
        &root,
        &collect_integrity_inputs(&root, command)?,
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
        let legacy_workspace = root.join("supercov");
        if fs::symlink_metadata(legacy_workspace.join(".supercov-workspace-store"))
            .is_ok_and(|metadata| metadata.file_type().is_file())
        {
            remove_stored_tree_deferred(&root, &legacy_workspace)
                .map_err(|error| error.to_string())?;
        }

        let adapter_started = Instant::now();
        let integrity_inputs = collect_integrity_inputs(&root, &request.command)?;
        let integrity = create_explicit_run_integrity(
            &root,
            &integrity_inputs,
            &FrontendIntegrityInputs::embedded_rust(),
        )
        .map_err(|error| error.to_string())?;
        let build_cache_key = rust_build_cache_key(&integrity, &request.command)
            .map_err(|error| error.to_string())?;

        let workspace_started = Instant::now();
        recover_cached_workspace(&root, &lock).map_err(|error| error.to_string())?;
        let workspace = cached_workspace_path(&root).map_err(|error| error.to_string())?;
        let target_directory = rust_target_directory(&root);
        let cached = read_rust_build_cache(&workspace, &target_directory, &build_cache_key);
        let reused_build = cached.is_some();
        let mut project = if let Some(cached) = cached {
            writeln!(
                diagnostics,
                "[supercov] detected Rust; reusing authenticated instrumented workspace {}",
                workspace.display()
            )
            .map_err(|error| error.to_string())?;
            PreparedRustProject {
                workspace_root: workspace.clone(),
                target_directory: target_directory.clone(),
                source_files: cached.source_files,
                crate_roots: Vec::new(),
                runtime_module: String::new(),
                manifest: cached.manifest,
            }
        } else {
            let workspace =
                prepare_cached_workspace(&root, &lock, &[]).map_err(|error| error.to_string())?;
            writeln!(
                diagnostics,
                "[supercov] detected Rust; instrumenting isolated Cargo workspace {}",
                workspace.display()
            )
            .map_err(|error| error.to_string())?;
            prepare_rust_project(&workspace).map_err(|error| error.to_string())?
        };
        project.target_directory = target_directory;
        fs::create_dir_all(&project.target_directory).map_err(|error| error.to_string())?;
        let workspace_preparation_ms = elapsed_ms(workspace_started);
        let adapter_setup_ms = (elapsed_ms(adapter_started) - workspace_preparation_ms).max(0.0);

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
        write_rust_build_cache(
            &root,
            &workspace,
            &build_cache_key,
            &request.started_at,
            &project.source_files,
            &project.manifest,
            &run.artifact_files,
        )?;

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
        remove_stored_tree_deferred(
            &root,
            &workspace
                .join(".supercov/rust-evidence")
                .join(&request.run_id),
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
            instrumented_build_cache: Some(InstrumentedBuildCache {
                key: build_cache_key,
                reused: reused_build,
            }),
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
        if let Ok(workspace) = cached_workspace_path(&root) {
            let _ = remove_stored_tree_deferred(
                &root,
                &workspace
                    .join(".supercov/rust-evidence")
                    .join(&request.run_id),
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
