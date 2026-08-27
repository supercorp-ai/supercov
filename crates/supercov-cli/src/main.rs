use std::{
    ffi::OsString,
    fs,
    io::Read,
    path::{Path, PathBuf},
    process::{Command, ExitCode, Stdio},
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use supercov_engine::{
    agent_json,
    coverage_analysis::{CoverageCoreInput, analyze_core},
    coverage_index::{CoverageIndex, coverage_index_sections},
    coverage_query::{
        CoverageCoversQueryOptions, CoverageDecisionQueryOptions, CoverageDiffQueryOptions,
        CoverageDimensionQueryData, CoverageDimensionQueryOptions, CoverageFileDecisionsOptions,
        CoverageFileDetailOptions, CoverageFileQueryData, CoverageFileQueryOptions,
        CoverageMinimizeQueryOptions, CoverageQueryFilters, CoverageScopeQueryOptions,
        CoverageSummaryQueryOptions, CoverageTestQueryOptions, DecisionSort, MinimizeMetric,
        MinimumTestSetRequest, coverage_covers_query, coverage_decision_query, coverage_diff_query,
        coverage_dimension_query, coverage_file_decisions_query, coverage_file_detail_query,
        coverage_file_query, coverage_minimize_query, coverage_scope_query, coverage_summary_query,
        coverage_test_query, minimum_test_set_for_request,
    },
    coverage_report::{
        ArchiveReportRequest, CoverageReportRequest, analyze_coverage_archive,
        analyze_coverage_results,
    },
    evidence_archive::{
        EvidenceArchiveEntry, EvidenceArchiveSource, collect_sources, write_archive,
    },
    indexed_query::{
        IndexedQueryOutput, IndexedQueryRequest, NewerQuery, execute_indexed_query_with_waivers,
        query_indexed_with_waivers,
    },
    js_instrumenter::instrument_candidate,
    query_index::{QueryIndex, QueryIndexIdentity, write_query_index},
    run_query::{RunListData, run_list_query},
    run_store::{
        RunInventory, RunStoreError, StoredRun, compare_run_integrity, discover_runs,
        open_or_rebuild_query_index, select_run,
    },
};
use time::{OffsetDateTime, macros::format_description};

mod human_query;
mod public_query;
use human_query::render_human;
use public_query::{PublicQueryInvocation, help_for, parse_public_query};

const HELP: &str = "Supercov coverage engine.\n\
\n\
Usage:\n\
  supercov -- <test command>\n\
  supercov runs <run-id> [resource] [--json]\n\
  supercov diff <older-run> <newer-run> [--json]\n\
  supercov merge <run-id> <run-id> [...]\n\
  supercov clean [--keep N] [--dry-run]\n";

fn is_executable_wrapper_program(argument: &str) -> bool {
    if argument.starts_with(['-', '@']) {
        return false;
    }
    let path = Path::new(argument);
    let candidates = if path.is_absolute() || path.components().count() > 1 {
        vec![path.to_path_buf()]
    } else {
        std::env::var_os("PATH")
            .map(|value| {
                std::env::split_paths(&value)
                    .map(|directory| directory.join(path))
                    .collect()
            })
            .unwrap_or_default()
    };
    candidates.into_iter().any(|candidate| {
        let Ok(metadata) = fs::metadata(candidate) else {
            return false;
        };
        if !metadata.is_file() {
            return false;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            metadata.permissions().mode() & 0o111 != 0
        }
        #[cfg(not(unix))]
        {
            true
        }
    })
}

fn main() -> ExitCode {
    let os_arguments = std::env::args_os().skip(1).collect::<Vec<_>>();
    if os_arguments
        .first()
        .is_some_and(|argument| argument == "__cargo-test-runner")
    {
        return rust_cargo_test_runner(os_arguments.into_iter().skip(1).collect());
    }
    let arguments = std::env::args().skip(1).collect::<Vec<_>>();
    // The parent-death watchdog inherits arbitrary wrapper environments. It
    // must dispatch before Cargo/rustdoc wrapper detection and must never emit
    // user-visible output.
    if arguments.as_slice() == ["__watch-process-group"] {
        return match supercov_engine::process_supervision::watch_parent_process_group() {
            Ok(()) => ExitCode::SUCCESS,
            Err(_) => ExitCode::from(125),
        };
    }
    // Cargo inherits both wrapper-mode variables during a doctest build. Its
    // rustc wrapper protocol always prepends the real compiler executable,
    // whereas RUSTDOC receives rustdoc's arguments directly. Dispatch the
    // nested rustc version/build probes first or the rustdoc path would wait
    // for the selection attestation that this same process must publish.
    if std::env::var_os(
        supercov_engine::rust_compiler_orchestration::RUST_COMPILER_WRAPPER_CONFIG_ENV,
    )
    .is_some()
        && arguments
            .first()
            .is_some_and(|argument| is_executable_wrapper_program(argument))
    {
        return rust_compiler_wrapper(arguments);
    }
    if std::env::var_os(supercov_engine::rust_compiler_orchestration::RUSTDOC_WRAPPER_MODE_ENV)
        .is_some()
    {
        return rustdoc_wrapper(arguments);
    }
    if std::env::var_os(
        supercov_engine::rust_compiler_orchestration::RUST_COMPILER_WRAPPER_CONFIG_ENV,
    )
    .is_some()
    {
        return rust_compiler_wrapper(arguments);
    }
    let mut arguments = arguments.into_iter();
    match arguments.next().as_deref() {
        None | Some("help" | "--help" | "-h") => {
            print!("{HELP}");
            ExitCode::SUCCESS
        }
        Some("--version" | "-V") => {
            println!(
                "supercov {} (rust contract v{})",
                supercov_engine::version(),
                supercov_contracts::CONTRACT_VERSION
            );
            ExitCode::SUCCESS
        }
        Some("--") => public_coverage_run(arguments.collect()),
        Some("__instrument-js") => instrument_js(),
        Some("__analyze-coverage-core") => analyze_coverage_core(),
        Some("__analyze-coverage-results") => analyze_coverage_report(),
        Some("__analyze-evidence-archive") => analyze_evidence_archive(),
        Some("__minimum-test-set") => minimum_test_sets(),
        Some("__roundtrip-query-index") => roundtrip_query_indexes(),
        Some("__query-index-files") => query_index_files(),
        Some("__query-stored-run") => query_stored_run(),
        Some("__discover-source") => discover_source(),
        Some("__discover-project") => discover_project(),
        Some("__lifecycle") => lifecycle(),
        Some("__workspace") => workspace(),
        Some("__supervise") => supervise(),
        Some("__run-js-direct") => run_js_direct(),
        Some("__sweep-trash") => sweep_trash(),
        Some("__benchmark-js-transform") => benchmark_js_transform(),
        Some("__pack-evidence") => pack_evidence(),
        Some("__validate-rust-compiler-manifest") => validate_rust_compiler_manifest(),
        Some("__normalize-rust-compiler-manifest") => normalize_rust_compiler_manifest(),
        Some("__join-rustdoc-merged-manifest") => join_rustdoc_merged_manifest(),
        Some("__prepare-rustdoc-transport") => prepare_rustdoc_transport(arguments.collect()),
        Some("__publish-rustdoc-outcome") => publish_rustdoc_outcome(arguments.collect()),
        Some("__project-rust-compiler-evidence") => project_rust_compiler_evidence(),
        Some("__select-rust-compiler-companion") => select_rust_compiler_companion(),
        Some("__build-rust-compiler") => build_rust_compiler(),
        Some("__run-rust-compiler") => run_rust_compiler(),
        Some("clean") => cleanup_command(arguments.collect()),
        Some("runs") => public_query_command("runs", arguments.collect()),
        Some("diff") => public_query_command("diff", arguments.collect()),
        Some("merge") => merge_command(arguments.collect()),
        Some(command) => {
            eprintln!("[supercov] Unknown command: {command}. Try supercov help.");
            ExitCode::from(2)
        }
    }
}

fn rust_cargo_test_runner(arguments: Vec<OsString>) -> ExitCode {
    let Some(config_path) =
        std::env::var_os(supercov_engine::rust_compiler_test_runner::RUST_CARGO_RUNNER_CONFIG_ENV)
            .map(PathBuf::from)
    else {
        eprintln!("[supercov] Cargo test runner configuration is missing");
        return ExitCode::from(2);
    };
    let watchdog = std::env::current_exe()
        .ok()
        .and_then(|path| fs::canonicalize(path).ok());
    let mut stdout = std::io::stdout().lock();
    let mut stderr = std::io::stderr().lock();
    match supercov_engine::rust_compiler_test_runner::run_cargo_libtest_runner(
        &config_path,
        arguments,
        watchdog,
        &mut stdout,
        &mut stderr,
    ) {
        Ok(execution) => ExitCode::from(execution.exit_code.clamp(0, 255) as u8),
        Err(error) => {
            eprintln!("[supercov] {error}");
            ExitCode::from(2)
        }
    }
}

fn strip_injected_rustdoc_runner(
    arguments: Vec<String>,
    runner: &Path,
) -> Result<Vec<String>, String> {
    let runner = runner
        .to_str()
        .ok_or_else(|| "the injected Cargo runner path is not UTF-8".to_owned())?;
    let mut stripped = Vec::with_capacity(arguments.len());
    let mut arguments = arguments.into_iter();
    let mut removed_runner = false;
    let mut removed_marker = false;
    while let Some(argument) = arguments.next() {
        if argument == "--test-runtool" {
            let value = arguments
                .next()
                .ok_or_else(|| "rustdoc --test-runtool has no value".to_owned())?;
            if value != runner || removed_runner {
                return Err(format!(
                    "rustdoc received an unexpected test runner: {value}"
                ));
            }
            removed_runner = true;
            continue;
        }
        if let Some(value) = argument.strip_prefix("--test-runtool=") {
            if value != runner || removed_runner {
                return Err(format!(
                    "rustdoc received an unexpected test runner: {value}"
                ));
            }
            removed_runner = true;
            continue;
        }
        if argument == "--test-runtool-arg" {
            let value = arguments
                .next()
                .ok_or_else(|| "rustdoc --test-runtool-arg has no value".to_owned())?;
            if value != "__cargo-test-runner" || removed_marker {
                return Err(format!(
                    "rustdoc received an unexpected test-runner argument: {value}"
                ));
            }
            removed_marker = true;
            continue;
        }
        if let Some(value) = argument.strip_prefix("--test-runtool-arg=") {
            if value != "__cargo-test-runner" || removed_marker {
                return Err(format!(
                    "rustdoc received an unexpected test-runner argument: {value}"
                ));
            }
            removed_marker = true;
            continue;
        }
        stripped.push(argument);
    }
    if removed_runner != removed_marker {
        return Err("rustdoc received an incomplete injected Cargo runner".into());
    }
    if !removed_runner {
        return Err("rustdoc did not receive Supercov's injected Cargo runner".into());
    }
    Ok(stripped)
}

fn rustdoc_wrapper(arguments: Vec<String>) -> ExitCode {
    let Some(config_path) = std::env::var_os(
        supercov_engine::rust_compiler_orchestration::RUST_COMPILER_WRAPPER_CONFIG_ENV,
    )
    .map(PathBuf::from) else {
        eprintln!("[supercov] Cargo rustdoc wrapper configuration is missing");
        return ExitCode::from(2);
    };
    let result = (|| -> Result<ExitCode, String> {
        let metadata = fs::symlink_metadata(&config_path).map_err(|error| error.to_string())?;
        if !metadata.file_type().is_file() {
            return Err(format!(
                "unsafe Rust compiler wrapper configuration: {}",
                config_path.display()
            ));
        }
        let config: supercov_engine::rust_compiler_orchestration::RustCompilerWrapperConfig =
            serde_json::from_slice(&fs::read(&config_path).map_err(|error| error.to_string())?)
                .map_err(|error| format!("invalid Rust compiler wrapper configuration: {error}"))?;
        let engine = fs::canonicalize(std::env::current_exe().map_err(|error| error.to_string())?)
            .map_err(|error| error.to_string())?;
        // Cargo forwards the configured target runner to rustdoc as
        // --test-runtool. Ordinary libtest artifacts must pass through the
        // Cargo-authoritative runner, while rustdoc already has its own exact
        // catalog/outcome/transport supervisor below. Remove only Supercov's
        // injected pair so the same executable is not misclassified twice.
        // Any configured or malformed alternative fails closed.
        let arguments = strip_injected_rustdoc_runner(arguments, &engine)?;
        let started = Instant::now();
        let selection = loop {
            match supercov_engine::rust_compiler_orchestration::verified_compiler_selection(
                &config.selection_directory,
                &config.candidates,
                config.require_public_capabilities,
                true,
            )
            .map_err(|error| error.to_string())?
            {
                Some(selection) => break selection,
                None if started.elapsed() < Duration::from_secs(30) => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                None => {
                    return Err(
                        "timed out waiting for Cargo's authenticated rustc selection before rustdoc"
                            .into(),
                    );
                }
            }
        };
        let rustdoc =
            supercov_engine::rust_compiler_selection::resolve_matching_rustdoc(&selection)
                .map_err(|error| error.to_string())?;
        let mut command = Command::new(&selection.companion_path);
        command
            .args(&arguments)
            .env_remove(supercov_engine::rust_compiler_orchestration::RUSTDOC_WRAPPER_MODE_ENV)
            .env(
                supercov_engine::rust_compiler_orchestration::RUST_REAL_RUSTDOC_ENV,
                &rustdoc,
            )
            .env(
                supercov_engine::rust_compiler_orchestration::RUST_COMPANION_PATH_ENV,
                &selection.companion_path,
            )
            .env(
                supercov_engine::rust_compiler_orchestration::RUSTDOC_CAPTURE_OUTCOMES_ENV,
                "1",
            )
            .env(
                supercov_engine::rust_compiler_orchestration::RUSTDOC_ENGINE_PATH_ENV,
                &engine,
            )
            .env(
                supercov_engine::rust_compiler_orchestration::RUST_STATIC_RUNTIME_DIRECTORY_ENV,
                &config.shared_runtime_directory,
            );
        supercov_engine::rust_compiler_selection::configure_companion_loader_environment(
            &mut command,
            &selection.compiler_library_directory,
        )
        .map_err(|error| error.to_string())?;
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt as _;
            command.arg0("supercov-rustdoc-backend-spike");
            let error = command.exec();
            Err(format!(
                "could not execute exact Rust rustdoc companion: {error}"
            ))
        }
        #[cfg(not(unix))]
        {
            Err("the exact Rust rustdoc wrapper is not yet implemented on Windows".into())
        }
    })();
    match result {
        Ok(status) => status,
        Err(error) => {
            eprintln!("[supercov] {error}");
            ExitCode::from(2)
        }
    }
}

fn rust_compiler_wrapper(arguments: Vec<String>) -> ExitCode {
    let Some(rustc) = arguments.first() else {
        eprintln!("[supercov] Cargo compiler wrapper received no rustc path");
        return ExitCode::from(2);
    };
    let Some(config_path) = std::env::var_os(
        supercov_engine::rust_compiler_orchestration::RUST_COMPILER_WRAPPER_CONFIG_ENV,
    )
    .map(PathBuf::from) else {
        eprintln!("[supercov] Cargo compiler wrapper configuration is missing");
        return ExitCode::from(2);
    };
    let result = (|| -> Result<ExitCode, String> {
        let metadata = fs::symlink_metadata(&config_path).map_err(|error| error.to_string())?;
        if !metadata.file_type().is_file() {
            return Err(format!(
                "unsafe Rust compiler wrapper configuration: {}",
                config_path.display()
            ));
        }
        let config: supercov_engine::rust_compiler_orchestration::RustCompilerWrapperConfig =
            serde_json::from_slice(&fs::read(&config_path).map_err(|error| error.to_string())?)
                .map_err(|error| format!("invalid Rust compiler wrapper configuration: {error}"))?;
        let selection = supercov_engine::rust_compiler_selection::select_rust_compiler_companion(
            Path::new(rustc),
            &config.candidates,
            config.require_public_capabilities,
        )
        .map_err(|error| error.to_string())?;
        supercov_engine::rust_compiler_orchestration::publish_compiler_selection_attestation(
            &config.selection_directory,
            &selection,
        )
        .map_err(|error| error.to_string())?;

        supercov_engine::rust_compiler_orchestration::prepare_shared_rust_runtime(
            Path::new(rustc),
            &config.shared_runtime_directory,
        )
        .map_err(|error| error.to_string())?;

        let mut command = Command::new(&selection.companion_path);
        command
            .args(&arguments)
            .env_remove(
                supercov_engine::rust_compiler_orchestration::RUST_COMPILER_WRAPPER_CONFIG_ENV,
            )
            .env(
                supercov_engine::rust_compiler_orchestration::RUST_STATIC_RUNTIME_DIRECTORY_ENV,
                &config.shared_runtime_directory,
            );
        supercov_engine::rust_compiler_selection::configure_companion_loader_environment(
            &mut command,
            &selection.compiler_library_directory,
        )
        .map_err(|error| error.to_string())?;
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt as _;
            let error = command.exec();
            Err(format!(
                "could not execute Rust compiler companion: {error}"
            ))
        }
        #[cfg(not(unix))]
        {
            let status = command
                .status()
                .map_err(|error| format!("could not execute Rust compiler companion: {error}"))?;
            Ok(ExitCode::from(
                status.code().unwrap_or(1).clamp(0, 255) as u8
            ))
        }
    })();
    match result {
        Ok(status) => status,
        Err(error) => {
            eprintln!("[supercov] {error}");
            ExitCode::from(2)
        }
    }
}

fn build_rust_compiler() -> ExitCode {
    let input = match stdin() {
        Ok(input) => input,
        Err(error) => {
            eprintln!("[supercov] {error}");
            return ExitCode::from(2);
        }
    };
    let request: supercov_engine::rust_compiler_orchestration::RustCompilerBuildRequest =
        match serde_json::from_str(&input) {
            Ok(request) => request,
            Err(error) => {
                eprintln!("[supercov] invalid Rust compiler build request: {error}");
                return ExitCode::from(2);
            }
        };
    match supercov_engine::rust_compiler_orchestration::build_with_rust_compiler_companion(&request)
    {
        Ok(build) => match serde_json::to_string(&build) {
            Ok(output) => {
                println!("{output}");
                ExitCode::SUCCESS
            }
            Err(error) => {
                eprintln!("[supercov] could not serialize Rust compiler build: {error}");
                ExitCode::from(2)
            }
        },
        Err(error) => {
            eprintln!("[supercov] {error}");
            ExitCode::from(2)
        }
    }
}

fn run_rust_compiler() -> ExitCode {
    let input = match stdin() {
        Ok(input) => input,
        Err(error) => {
            eprintln!("[supercov] {error}");
            return ExitCode::from(2);
        }
    };
    let mut request: supercov_engine::rust_compiler_run::DirectRustCompilerRunRequest =
        match serde_json::from_str(&input) {
            Ok(request) => request,
            Err(error) => {
                eprintln!("[supercov] invalid Rust compiler run request: {error}");
                return ExitCode::from(2);
            }
        };
    request.watchdog_program = std::env::current_exe()
        .ok()
        .and_then(|path| fs::canonicalize(path).ok());
    let run = match supercov_engine::rust_compiler_run::run_direct_rust_compiler(
        &request,
        &mut std::io::stderr(),
    ) {
        Ok(run) => run,
        Err(error) => {
            eprintln!("[supercov] {error}");
            return ExitCode::from(error.exit_code.clamp(0, 255) as u8);
        }
    };
    match serde_json::to_string(&run) {
        Ok(output) => {
            println!("{output}");
            ExitCode::from(run.exit_code.clamp(0, 255) as u8)
        }
        Err(error) => {
            eprintln!("[supercov] could not serialize Rust compiler run: {error}");
            ExitCode::from(2)
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RustCompilerSelectionInput {
    rustc_path: PathBuf,
    candidates: Vec<PathBuf>,
    require_public_capabilities: bool,
}

fn select_rust_compiler_companion() -> ExitCode {
    let input = match stdin() {
        Ok(input) => input,
        Err(error) => {
            eprintln!("[supercov] {error}");
            return ExitCode::from(2);
        }
    };
    let request: RustCompilerSelectionInput = match serde_json::from_str(&input) {
        Ok(request) => request,
        Err(error) => {
            eprintln!("[supercov] invalid Rust compiler selection request: {error}");
            return ExitCode::from(2);
        }
    };
    match supercov_engine::rust_compiler_selection::select_rust_compiler_companion(
        &request.rustc_path,
        &request.candidates,
        request.require_public_capabilities,
    ) {
        Ok(selection) => match serde_json::to_string(&selection) {
            Ok(output) => {
                println!("{output}");
                ExitCode::SUCCESS
            }
            Err(error) => {
                eprintln!("[supercov] could not serialize Rust compiler selection: {error}");
                ExitCode::from(2)
            }
        },
        Err(error) => {
            eprintln!("[supercov] {error}");
            ExitCode::from(2)
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RustCompilerProjectionInput {
    normalization: supercov_engine::rust_compiler_manifest::RustCompilerNormalizationRequest,
    transport_path: PathBuf,
    token_hex: String,
    base_context_id: String,
    base_phase: supercov_engine::coverage_report::CoveragePhase,
}

fn rust_transport_token(value: &str) -> Result<[u8; 16], String> {
    if value.len() != 32 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("Rust transport token must be exactly 32 hexadecimal digits".into());
    }
    let mut token = [0_u8; 16];
    for (index, byte) in token.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&value[index * 2..index * 2 + 2], 16)
            .map_err(|error| format!("invalid Rust transport token: {error}"))?;
    }
    Ok(token)
}

fn project_rust_compiler_evidence() -> ExitCode {
    let input = match stdin() {
        Ok(input) => input,
        Err(error) => {
            eprintln!("[supercov] {error}");
            return ExitCode::from(2);
        }
    };
    let request: RustCompilerProjectionInput = match serde_json::from_str(&input) {
        Ok(request) => request,
        Err(error) => {
            eprintln!("[supercov] invalid Rust compiler evidence request: {error}");
            return ExitCode::from(2);
        }
    };
    let result = (|| {
        let normalized = request
            .normalization
            .manifest
            .normalize(&request.normalization.sources)
            .map_err(|error| error.to_string())?;
        let token = rust_transport_token(&request.token_hex)?;
        let context = request
            .base_context_id
            .parse::<u64>()
            .map_err(|error| format!("invalid Rust base context: {error}"))?;
        let transport = supercov_engine::rust_probe_transport::read_rust_transport(
            &request.transport_path,
            &token,
        )
        .map_err(|error| error.to_string())?;
        supercov_engine::rust_compiler_evidence::project_rust_compiler_evidence(
            context,
            &request.base_phase,
            &transport,
            &normalized,
        )
        .map_err(|error| error.to_string())
    })();
    match result.and_then(|projection| {
        serde_json::to_string(&projection)
            .map_err(|error| format!("could not serialize Rust compiler evidence: {error}"))
    }) {
        Ok(output) => {
            println!("{output}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("[supercov] {error}");
            ExitCode::from(2)
        }
    }
}

fn normalize_rust_compiler_manifest() -> ExitCode {
    let input = match stdin() {
        Ok(input) => input,
        Err(error) => {
            eprintln!("[supercov] {error}");
            return ExitCode::from(2);
        }
    };
    match supercov_engine::rust_compiler_manifest::RustCompilerNormalizationRequest::parse_and_normalize(
        input.as_bytes(),
    ) {
        Ok(normalized) => match serde_json::to_string(&normalized) {
            Ok(output) => {
                println!("{output}");
                ExitCode::SUCCESS
            }
            Err(error) => {
                eprintln!("[supercov] could not serialize normalized Rust manifest: {error}");
                ExitCode::from(2)
            }
        },
        Err(error) => {
            eprintln!("[supercov] {error}");
            ExitCode::from(2)
        }
    }
}

fn validate_rust_compiler_manifest() -> ExitCode {
    let input = match stdin() {
        Ok(input) => input,
        Err(error) => {
            eprintln!("[supercov] {error}");
            return ExitCode::from(2);
        }
    };
    match supercov_engine::rust_compiler_manifest::RustCompilerManifest::parse(input.as_bytes()) {
        Ok(manifest) => {
            println!("{}", manifest.crate_name);
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("[supercov] {error}");
            ExitCode::from(2)
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RustdocMergedJoinInput {
    pending_manifest: serde_json::Value,
    pending_sources: serde_json::Value,
    map: serde_json::Value,
    authored_sources: std::collections::BTreeMap<
        String,
        supercov_engine::rust_compiler_manifest::RustCompilerSource,
    >,
}

fn join_rustdoc_merged_manifest() -> ExitCode {
    let input = match stdin() {
        Ok(input) => input,
        Err(error) => {
            eprintln!("[supercov] {error}");
            return ExitCode::from(2);
        }
    };
    let request: RustdocMergedJoinInput = match serde_json::from_str(&input) {
        Ok(request) => request,
        Err(error) => {
            eprintln!("[supercov] invalid merged rustdoc join request: {error}");
            return ExitCode::from(2);
        }
    };
    let result = (|| {
        let manifest = serde_json::to_vec(&request.pending_manifest)
            .map_err(|error| format!("could not encode pending rustdoc manifest: {error}"))?;
        let sources = serde_json::to_vec(&request.pending_sources)
            .map_err(|error| format!("could not encode pending rustdoc sources: {error}"))?;
        let map = serde_json::to_vec(&request.map)
            .map_err(|error| format!("could not encode rustdoc source map: {error}"))?;
        let joined = supercov_engine::rust_doctest::join_merged_doctest(
            &manifest,
            &sources,
            &map,
            &request.authored_sources,
        )
        .map_err(|error| error.to_string())?;
        serde_json::to_string(&joined)
            .map_err(|error| format!("could not serialize merged rustdoc join: {error}"))
    })();
    match result {
        Ok(output) => {
            println!("{output}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("[supercov] {error}");
            ExitCode::from(2)
        }
    }
}

fn publish_rustdoc_outcome(arguments: Vec<String>) -> ExitCode {
    let [directory, invocation_id, group, companion_build_id] = arguments.as_slice() else {
        eprintln!(
            "[supercov] rustdoc outcome publication requires directory, invocation, group and companion build identity"
        );
        return ExitCode::from(2);
    };
    let input = match stdin_bytes() {
        Ok(input) => input,
        Err(error) => {
            eprintln!("[supercov] {error}");
            return ExitCode::from(2);
        }
    };
    let result = (|| {
        let transport_path =
            std::env::var_os(supercov_engine::rust_probe_transport::RUST_TRANSPORT_ENV)
                .ok_or_else(|| {
                    supercov_engine::rust_doctest::RustdocOutcomeError::Invalid(
                        "rustdoc outcome publisher has no reserved transport path".into(),
                    )
                })?;
        let transport_token =
            std::env::var(supercov_engine::rust_probe_transport::RUST_TRANSPORT_TOKEN_ENV)
                .map_err(|_| {
                    supercov_engine::rust_doctest::RustdocOutcomeError::Invalid(
                        "rustdoc outcome publisher has no reserved transport token".into(),
                    )
                })?;
        let transport_path = PathBuf::from(transport_path);
        let transport = supercov_engine::rust_doctest::read_reserved_rustdoc_transport(
            &transport_path,
            &transport_token,
        )?;
        let unit = supercov_engine::rust_doctest::rustdoc_outcome_unit_from_framed_input(
            invocation_id.clone(),
            group.clone(),
            companion_build_id.clone(),
            &input,
            transport,
        )?;
        let path = supercov_engine::rust_doctest::publish_rustdoc_outcome_unit(
            Path::new(directory),
            &unit,
        )?;
        fs::remove_file(&transport_path).map_err(|error| {
            supercov_engine::rust_doctest::RustdocOutcomeError::Io {
                path: transport_path,
                reason: error.to_string(),
            }
        })?;
        serde_json::to_string(&serde_json::json!({
            "path": path,
            "unit": unit,
        }))
        .map_err(|error| {
            supercov_engine::rust_doctest::RustdocOutcomeError::Json(error.to_string())
        })
    })();
    match result {
        Ok(output) => {
            println!("{output}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("[supercov] {error}");
            ExitCode::from(2)
        }
    }
}

fn prepare_rustdoc_transport(arguments: Vec<String>) -> ExitCode {
    let [directory, invocation_id] = arguments.as_slice() else {
        eprintln!(
            "[supercov] rustdoc transport preparation requires directory and invocation identity"
        );
        return ExitCode::from(2);
    };
    let result = supercov_engine::rust_doctest::reserve_rustdoc_transport(
        Path::new(directory),
        invocation_id,
    )
    .and_then(|reservation| {
        serde_json::to_string(&serde_json::json!({
            "path": reservation.path,
            "token": supercov_engine::rust_doctest::rustdoc_transport_token_hex(
                &reservation.token,
            ),
        }))
        .map_err(|error| {
            supercov_engine::rust_doctest::RustdocOutcomeError::Json(error.to_string())
        })
    });
    match result {
        Ok(output) => {
            println!("{output}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("[supercov] {error}");
            ExitCode::from(2)
        }
    }
}

fn merge_command(run_ids: Vec<String>) -> ExitCode {
    if run_ids
        .iter()
        .any(|argument| matches!(argument.as_str(), "--help" | "-h"))
    {
        println!("Usage: supercov merge <run-id> <run-id> [...]");
        return ExitCode::SUCCESS;
    }
    let root = match std::env::current_dir() {
        Ok(root) => root,
        Err(error) => {
            eprintln!("[supercov] could not resolve the current directory: {error}");
            return ExitCode::from(2);
        }
    };
    let (run_id, started_at) = match public_run_identity() {
        Ok(identity) => identity,
        Err(error) => {
            eprintln!("[supercov] {error}");
            return ExitCode::from(2);
        }
    };
    match supercov_engine::run_merge::merge_coverage_runs(&root, &run_ids, &run_id, &started_at) {
        Ok(merged) => {
            println!("[supercov] merged run {merged}");
            println!("npx supercov runs {merged}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("[supercov] {error}");
            ExitCode::from(2)
        }
    }
}

const PUBLIC_TIMESTAMP_FORMAT: &[time::format_description::BorrowedFormatItem<'static>] =
    format_description!("[year]-[month]-[day]T[hour]:[minute]:[second].[subsecond digits:3]Z");

fn public_run_identity() -> Result<(String, String), String> {
    let started_at = OffsetDateTime::now_utc()
        .format(PUBLIC_TIMESTAMP_FORMAT)
        .map_err(|error| format!("could not generate the run identity: {error}"))?;
    let identity = format!(
        "{started_at}\0{}\0{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    );
    let digest = format!("{:x}", Sha256::digest(identity.as_bytes()));
    let run_id = format!("run_{}", &digest[..16]);
    Ok((run_id, started_at))
}

fn public_run_id(value: &str) -> bool {
    value.len() == 20
        && value.starts_with("run_")
        && value[4..]
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn public_run_inventory(root: &Path) -> Result<RunInventory, RunStoreError> {
    let mut inventory = discover_runs(root)?;
    // Timestamp-named runs belonged to the pre-release local store contract.
    // They are intentionally not a public compatibility surface: the CLI has
    // one stable identity shape and `supercov clean` removes old stores.
    inventory.runs.retain(|run| public_run_id(&run.id));
    Ok(inventory)
}

fn javascript_number(value: f64) -> String {
    let rounded = (value * 10.0).round() / 10.0;
    if rounded.fract() == 0.0 {
        format!("{rounded:.0}")
    } else {
        format!("{rounded:.1}")
    }
}

fn format_run_timings(timings: &supercov_engine::run_store::RunTimings, total_ms: f64) -> String {
    format!(
        "initialization={}ms workspace={}ms setup={}ms build={}ms tests={}ms evidence={}ms total={}ms",
        javascript_number(timings.initialization_ms),
        javascript_number(timings.workspace_preparation_ms),
        javascript_number(timings.adapter_setup_ms),
        javascript_number(timings.instrumented_build_ms),
        javascript_number(timings.test_command_ms),
        javascript_number(timings.evidence_publication_ms),
        javascript_number(total_ms),
    )
}

fn process_exit_code(code: i32) -> ExitCode {
    ExitCode::from(u8::try_from(code).unwrap_or(1))
}

fn public_coverage_run(command: Vec<String>) -> ExitCode {
    if command.is_empty() {
        eprintln!("Usage: supercov -- <test command>");
        return ExitCode::from(2);
    }
    let root = match std::env::current_dir() {
        Ok(root) => root,
        Err(error) => {
            eprintln!("[supercov] could not resolve the current directory: {error}");
            return ExitCode::from(2);
        }
    };
    let (run_id, started_at) = match public_run_identity() {
        Ok(timestamp) => timestamp,
        Err(error) => {
            eprintln!("[supercov] {error}");
            return ExitCode::from(2);
        }
    };
    spawn_trash_sweeper(&root);
    let detection = supercov_engine::frontend_detection::detect_frontends(&root, &command);
    if detection.frontends.is_empty() {
        eprintln!(
            "[supercov] could not determine a supported test language from the command or project manifests"
        );
        return ExitCode::from(2);
    }
    if detection.frontends.len() > 1 {
        let languages = detection
            .frontends
            .iter()
            .map(|language| format!("{language:?}").to_ascii_lowercase())
            .collect::<Vec<_>>()
            .join(", ");
        eprintln!(
            "[supercov] the test command launches multiple language frontends ({languages}); combined polyglot evidence is not implemented yet, so Supercov refuses to publish a partial run"
        );
        return ExitCode::from(2);
    }
    if detection.frontends == [supercov_engine::frontend_detection::FrontendLanguage::Rust] {
        let request = supercov_engine::rust_run::DirectRustRunRequest {
            root: root.clone(),
            command,
            run_id,
            started_at,
        };
        let mut diagnostics = std::io::stderr().lock();
        let result = supercov_engine::rust_run::run_direct_rust(&request, &mut diagnostics);
        spawn_trash_sweeper(&root);
        return match result {
            Ok(result) => {
                println!(
                    "[coverage] evidence: {}",
                    result.run_directory.join("evidence.raw.gz").display()
                );
                eprintln!(
                    "[supercov] Rust coverage: {} test(s) across {} artifact(s)",
                    result.tests, result.artifacts
                );
                if let Some(timings) = &result.metadata.timings {
                    eprintln!(
                        "[supercov] timings {}",
                        format_run_timings(timings, result.metadata.duration_ms)
                    );
                }
                process_exit_code(result.exit_code)
            }
            Err(error) => {
                eprintln!("[supercov] {error}");
                ExitCode::from(1)
            }
        };
    }
    if detection.frontends == [supercov_engine::frontend_detection::FrontendLanguage::Python] {
        eprintln!(
            "[supercov] Python was detected, but the owned Python user-run frontend is not enabled yet"
        );
        return ExitCode::from(2);
    }
    let watchdog_program = std::env::current_exe()
        .ok()
        .and_then(|path| fs::canonicalize(path).ok());
    let request = supercov_engine::javascript_run::DirectJavascriptRunRequest {
        root: root.clone(),
        command,
        run_id: Some(run_id),
        started_at: Some(started_at),
        watchdog_program,
    };
    let mut diagnostics = std::io::stderr().lock();
    let result = supercov_engine::javascript_run::run_direct_javascript(&request, &mut diagnostics);
    spawn_trash_sweeper(&root);
    match result {
        Ok(result) => {
            println!(
                "[coverage] evidence: {}",
                result.run_directory.join("evidence.raw.gz").display()
            );
            if let Some(timings) = &result.metadata.timings {
                eprintln!(
                    "[supercov] timings {}",
                    format_run_timings(timings, result.metadata.duration_ms)
                );
            }
            process_exit_code(result.exit_code)
        }
        Err(supercov_engine::javascript_run::DirectJavascriptRunError::Interrupted {
            exit_code,
            timings,
            total_ms,
            ..
        }) => {
            eprintln!(
                "[supercov] timings {}",
                format_run_timings(&timings, total_ms)
            );
            process_exit_code(exit_code)
        }
        Err(error) => {
            eprintln!("[supercov] {error}");
            ExitCode::from(1)
        }
    }
}

fn parse_cleanup_options(arguments: &[String]) -> Result<(usize, bool), String> {
    let mut keep = 0;
    let mut dry_run = false;
    let mut index = 0;
    while index < arguments.len() {
        let argument = &arguments[index];
        if argument == "--dry-run" {
            dry_run = true;
        } else if argument == "--keep" {
            index += 1;
            let Some(value) = arguments.get(index) else {
                return Err("--keep must be a non-negative integer".into());
            };
            keep = value
                .parse::<usize>()
                .map_err(|_| "--keep must be a non-negative integer".to_owned())?;
        } else {
            return Err(format!("Unknown clean option: {argument}"));
        }
        index += 1;
    }
    Ok((keep, dry_run))
}

fn cleanup_summary(
    keep: usize,
    dry_run: bool,
    result: &supercov_engine::lifecycle::CleanupResult,
) -> String {
    format!(
        "[supercov] {} {} stored run(s), {} per-run workspace(s), and {} isolated build cache; keeping {} newest run(s)",
        if dry_run { "would remove" } else { "removed" },
        result.removed_runs.len(),
        result.removed_workspaces.len(),
        if result.removed_build_cache {
            "the"
        } else {
            "no"
        },
        keep,
    )
}

fn cleanup_command(arguments: Vec<String>) -> ExitCode {
    if arguments
        .iter()
        .any(|argument| matches!(argument.as_str(), "--help" | "-h"))
    {
        print!(
            "Usage: supercov clean [--keep N] [--dry-run]\n\nRemoves all stored runs and Supercov's isolated build cache by default.\nUse --keep N to retain the N newest runs.\n"
        );
        return ExitCode::SUCCESS;
    }
    let (keep, dry_run) = match parse_cleanup_options(&arguments) {
        Ok(options) => options,
        Err(error) => {
            eprintln!("[supercov] {error}");
            return ExitCode::from(2);
        }
    };
    let root = match std::env::current_dir() {
        Ok(root) => root,
        Err(error) => {
            eprintln!("[supercov] could not resolve the current directory: {error}");
            return ExitCode::from(2);
        }
    };
    spawn_trash_sweeper(&root);
    let options = supercov_engine::lifecycle::CleanupOptions { keep, dry_run };
    let updated_at = format!(
        "unix-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
    );
    let result = supercov_engine::lifecycle::clean_storage(&root, options, &updated_at);
    let result = match result {
        Ok(result) => result,
        Err(error) => {
            eprintln!("[supercov] clean failed: {error}");
            return ExitCode::from(2);
        }
    };
    spawn_trash_sweeper(&root);
    println!("{}", cleanup_summary(keep, dry_run, &result));
    for id in result.removed_runs {
        println!("{id}");
    }
    ExitCode::SUCCESS
}

fn internal_agent_error(message: impl Into<String>) -> agent_json::AgentError {
    agent_json::AgentError {
        code: agent_json::ErrorCode::InternalError,
        message: message.into(),
        retryable: false,
        details: None,
    }
}

fn run_store_agent_error(error: RunStoreError) -> agent_json::AgentError {
    match error {
        RunStoreError::NoRuns => agent_json::AgentError {
            code: agent_json::ErrorCode::NoRuns,
            message: "No local coverage runs. Run supercov first.".into(),
            retryable: false,
            details: None,
        },
        RunStoreError::RunNotFound(selector) => agent_json::AgentError {
            code: agent_json::ErrorCode::RunNotFound,
            message: format!("Coverage run not found: {selector}"),
            retryable: false,
            details: Some(serde_json::json!({ "selector": selector })),
        },
        error => internal_agent_error(format!("Failed to read local coverage runs: {error}")),
    }
}

fn current_javascript_integrity(
    root: &Path,
    command: &[String],
) -> Option<supercov_engine::run_store::RunIntegrity> {
    supercov_engine::javascript_run::current_javascript_integrity(root, command).ok()
}

fn current_integrity_for_run(
    root: &Path,
    run: &StoredRun,
) -> Option<supercov_engine::run_store::RunIntegrity> {
    if run
        .metadata
        .integrity
        .instrumenter_version
        .starts_with("supercov-rust-")
    {
        supercov_engine::rust_run::current_rust_integrity(root, &run.metadata.command).ok()
    } else {
        current_javascript_integrity(root, &run.metadata.command)
    }
}

fn javascript_run(run: &StoredRun) -> bool {
    !run.metadata
        .integrity
        .instrumenter_version
        .starts_with("supercov-rust-")
}

enum PublicQueryOutput {
    Runs {
        data: RunListData,
        pagination: supercov_contracts::AgentPagination,
    },
    Coverage {
        output: IndexedQueryOutput,
        warnings: Vec<String>,
    },
}

impl PublicQueryOutput {
    fn agent_json(&self) -> Result<String, agent_json::AgentError> {
        match self {
            Self::Runs { data, pagination } => agent_json::success("runs", data, Some(pagination))
                .map_err(|error| {
                    supercov_engine::indexed_query::IndexedQueryError::ResponseTooLarge(error)
                        .agent_error()
                }),
            Self::Coverage { output, .. } => {
                output.agent_json().map_err(|error| error.agent_error())
            }
        }
    }
}

struct PublicQueryExecutionError {
    error: Box<agent_json::AgentError>,
    warnings: Vec<String>,
}

impl From<agent_json::AgentError> for PublicQueryExecutionError {
    fn from(error: agent_json::AgentError) -> Self {
        Self {
            error: Box::new(error),
            warnings: Vec::new(),
        }
    }
}

fn execute_public_query(
    root: &Path,
    invocation: &PublicQueryInvocation,
) -> Result<PublicQueryOutput, PublicQueryExecutionError> {
    match invocation {
        PublicQueryInvocation::Runs {
            filter,
            offset,
            limit,
            ..
        } => {
            let inventory = public_run_inventory(root).map_err(run_store_agent_error)?;
            let view = match filter.as_str() {
                "all" => supercov_engine::coverage_index::CoverageViewId::All,
                "passed" => supercov_engine::coverage_index::CoverageViewId::Passed,
                "failed" => supercov_engine::coverage_index::CoverageViewId::Failed,
                _ => unreachable!("public parser validates coverage filters"),
            };
            let (data, page) = run_list_query(
                &inventory,
                &|run| current_integrity_for_run(root, run),
                view,
                *offset,
                *limit,
            )
            .map_err(|error| {
                internal_agent_error(format!("Failed to prepare run summaries: {error}"))
            })?;
            Ok(PublicQueryOutput::Runs {
                data,
                pagination: page,
            })
        }
        PublicQueryInvocation::Coverage {
            request,
            newer_run_id,
            ..
        } => {
            let mut request = (**request).clone();
            let inventory = public_run_inventory(root).map_err(run_store_agent_error)?;
            let run =
                select_run(&inventory, Some(&request.run_id)).map_err(run_store_agent_error)?;
            request.run_id.clone_from(&run.id);
            request
                .valid
                .get_or_insert(run.metadata.test_exit_code == Some(0));
            let current = current_integrity_for_run(root, run);
            let mut warnings = Vec::new();
            if let Some(current) = current.as_ref() {
                let comparison = compare_run_integrity(Some(&run.metadata.integrity), current);
                request.stale.get_or_insert(comparison.stale);
                request
                    .stale_reasons
                    .get_or_insert_with(|| comparison.reasons.clone());
                if comparison.stale {
                    warnings.push(format!(
                        "[supercov] stale run {}: {}",
                        run.id,
                        comparison.reasons.join(", ")
                    ));
                }
            }
            let result = (|| -> Result<IndexedQueryOutput, agent_json::AgentError> {
                let container = open_or_rebuild_query_index(run).map_err(|error| {
                    internal_agent_error(format!("Failed to open coverage index: {error}"))
                })?;
                let index = CoverageIndex::new(&container).map_err(|error| {
                    internal_agent_error(format!("Failed to read coverage index: {error}"))
                })?;
                let waiver_source = if javascript_run(run) {
                    supercov_engine::coverage_waivers::read_coverage_waivers(root).map_err(
                        |error| {
                            internal_agent_error(format!(
                                "Failed to read coverage waivers: {error}"
                            ))
                        },
                    )?
                } else {
                    None
                };
                let waiver_evaluation = if let Some(source) = waiver_source.as_ref() {
                    let decisions = supercov_engine::coverage_query::filtered_decisions(
                        &index,
                        request.view().map_err(|error| error.agent_error())?,
                        request.kind.as_deref(),
                        request.runner.as_deref(),
                    )
                    .map_err(|error| {
                        supercov_engine::indexed_query::IndexedQueryError::Query(error)
                            .agent_error()
                    })?;
                    Some(
                        supercov_engine::coverage_waivers::evaluate_coverage_waivers(
                            &decisions, source,
                        ),
                    )
                } else {
                    None
                };
                let report = if request.command == "minimize" {
                    Some(
                        analyze_stored_run(run)
                            .map_err(|error| internal_agent_error(error.to_string()))?,
                    )
                } else {
                    None
                };

                let newer_container;
                let newer_index;
                let newer_query = if request.command == "diff" {
                    let selector = newer_run_id.as_deref().ok_or_else(|| {
                        supercov_engine::indexed_query::IndexedQueryError::MissingNewerRun
                            .agent_error()
                    })?;
                    let newer_run =
                        select_run(&inventory, Some(selector)).map_err(run_store_agent_error)?;
                    if let Some(current) = current_integrity_for_run(root, newer_run).as_ref() {
                        let comparison =
                            compare_run_integrity(Some(&newer_run.metadata.integrity), current);
                        if comparison.stale {
                            warnings.push(format!(
                                "[supercov] stale run {}: {}",
                                newer_run.id,
                                comparison.reasons.join(", ")
                            ));
                        }
                    }
                    newer_container = open_or_rebuild_query_index(newer_run).map_err(|error| {
                        internal_agent_error(format!(
                            "Failed to open newer coverage index: {error}"
                        ))
                    })?;
                    newer_index = CoverageIndex::new(&newer_container).map_err(|error| {
                        internal_agent_error(format!(
                            "Failed to read newer coverage index: {error}"
                        ))
                    })?;
                    Some(NewerQuery {
                        run_id: &newer_run.id,
                        index: &newer_index,
                    })
                } else {
                    None
                };
                query_indexed_with_waivers(
                    &index,
                    report.as_ref(),
                    &request,
                    newer_query,
                    waiver_evaluation.as_ref(),
                )
                .map_err(|error| error.agent_error())
            })();
            result
                .map(|output| PublicQueryOutput::Coverage {
                    output,
                    warnings: warnings.clone(),
                })
                .map_err(|error| PublicQueryExecutionError {
                    error: Box::new(error),
                    warnings,
                })
        }
    }
}

fn public_query_command(command: &str, arguments: Vec<String>) -> ExitCode {
    if let Some(help) = help_for(command, &arguments) {
        print!("{help}");
        return ExitCode::SUCCESS;
    }
    let invocation = match parse_public_query(command, &arguments) {
        Ok(invocation) => invocation,
        Err(error) => {
            if error.json {
                print!(
                    "{}",
                    agent_json::failure(error.command.as_deref(), &error.error)
                );
            } else {
                eprintln!("[supercov] {}", error.error.message);
            }
            return ExitCode::from(2);
        }
    };
    let (json_output, agent_command) = match &invocation {
        PublicQueryInvocation::Runs { json, .. } => (*json, "runs".to_owned()),
        PublicQueryInvocation::Coverage {
            json,
            agent_command,
            ..
        } => (*json, agent_command.clone()),
    };
    let root = match std::env::current_dir() {
        Ok(root) => root,
        Err(error) => {
            let error =
                internal_agent_error(format!("Could not resolve the current directory: {error}"));
            print!("{}", agent_json::failure(Some(&agent_command), &error));
            return ExitCode::from(2);
        }
    };
    match execute_public_query(&root, &invocation) {
        Ok(output) if json_output => match output.agent_json() {
            Ok(output) => {
                print!("{output}");
                ExitCode::SUCCESS
            }
            Err(error) => {
                print!("{}", agent_json::failure(Some(&agent_command), &error));
                ExitCode::from(2)
            }
        },
        Ok(output) => {
            if let PublicQueryOutput::Coverage { warnings, .. } = &output {
                for warning in warnings {
                    eprintln!("{warning}");
                }
            }
            println!("{}", render_human(&invocation, &output));
            ExitCode::SUCCESS
        }
        Err(failure) => {
            if !json_output {
                for warning in &failure.warnings {
                    eprintln!("{warning}");
                }
            }
            if json_output {
                print!(
                    "{}",
                    agent_json::failure(Some(&agent_command), &failure.error)
                );
            } else {
                eprintln!("[supercov] {}", failure.error.message);
            }
            ExitCode::from(2)
        }
    }
}

fn run_js_direct() -> ExitCode {
    let input = match stdin() {
        Ok(input) => input,
        Err(error) => {
            eprintln!("[supercov] {error}");
            return ExitCode::from(2);
        }
    };
    let mut request: supercov_engine::javascript_run::DirectJavascriptRunRequest =
        match serde_json::from_str(&input) {
            Ok(request) => request,
            Err(error) => {
                eprintln!("[supercov] invalid direct JavaScript run input: {error}");
                return ExitCode::from(2);
            }
        };
    request.watchdog_program = std::env::current_exe()
        .ok()
        .and_then(|path| fs::canonicalize(path).ok());
    let mut diagnostics = std::io::stderr().lock();
    match supercov_engine::javascript_run::run_direct_javascript(&request, &mut diagnostics) {
        Ok(result) => match serde_json::to_string(&result) {
            Ok(output) => {
                println!("{output}");
                ExitCode::SUCCESS
            }
            Err(error) => {
                eprintln!("[supercov] failed to serialize direct JavaScript run: {error}");
                ExitCode::from(2)
            }
        },
        Err(error) => {
            eprintln!("[supercov] direct JavaScript run failed: {error}");
            ExitCode::from(2)
        }
    }
}

fn supervise() -> ExitCode {
    let mut arguments = std::env::args_os().skip(2).collect::<Vec<_>>();
    if arguments.first().is_some_and(|argument| argument == "--") {
        arguments.remove(0);
    }
    let Some(program) = arguments.first().cloned() else {
        eprintln!("[supercov] test command must not be empty");
        return ExitCode::from(2);
    };
    let diagnostic_interval = match supercov_engine::process_supervision::positive_milliseconds(
        std::env::var("SUPERCOV_DIAGNOSTIC_INTERVAL_MS")
            .ok()
            .as_deref(),
        "SUPERCOV_DIAGNOSTIC_INTERVAL_MS",
    ) {
        Ok(value) => value.unwrap_or_else(|| {
            std::time::Duration::from_millis(supercov_contracts::DEFAULT_DIAGNOSTIC_INTERVAL_MS)
        }),
        Err(error) => {
            eprintln!("[supercov] {error}");
            return ExitCode::from(2);
        }
    };
    let timeout = match supercov_engine::process_supervision::positive_milliseconds(
        std::env::var("SUPERCOV_COMMAND_TIMEOUT_MS").ok().as_deref(),
        "SUPERCOV_COMMAND_TIMEOUT_MS",
    ) {
        Ok(value) => value,
        Err(error) => {
            eprintln!("[supercov] {error}");
            return ExitCode::from(2);
        }
    };
    let cwd = match std::env::current_dir() {
        Ok(cwd) => cwd,
        Err(error) => {
            eprintln!("[supercov] could not resolve the current directory: {error}");
            return ExitCode::from(2);
        }
    };
    let spec = supercov_engine::process_supervision::CommandSpec {
        program,
        arguments: arguments.into_iter().skip(1).collect(),
        cwd,
        environment: None,
        captured_output: None,
    };
    let mut stderr = std::io::stderr().lock();
    let supervisor = match std::env::current_exe()
        .map_err(|error| error.to_string())
        .and_then(|path| {
            supercov_engine::process_supervision::ProcessSupervisor::new_crash_safe(&path)
                .map_err(|error| error.to_string())
        }) {
        Ok(supervisor) => supervisor,
        Err(error) => {
            eprintln!("[supercov] {error}");
            return ExitCode::FAILURE;
        }
    };
    match supervisor.supervise(
        &spec,
        supercov_engine::process_supervision::SupervisionOptions {
            diagnostic_interval,
            timeout,
            termination_grace: std::time::Duration::from_millis(
                supercov_contracts::COMMAND_TERMINATION_GRACE_MS,
            ),
        },
        &mut stderr,
    ) {
        Ok(result) => match u8::try_from(result.exit_code()) {
            Ok(code) => ExitCode::from(code),
            Err(_) => ExitCode::FAILURE,
        },
        Err(error) => {
            eprintln!("[supercov] {error}");
            ExitCode::FAILURE
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct WorkspaceRequest {
    root: PathBuf,
    action: String,
    run_id: Option<String>,
    reuse_paths: Option<Vec<PathBuf>>,
}

/// Internal differential surface for workspace publication and recovery.
/// The public run path is enabled only after workspace and supervision gates
/// are complete.
fn workspace() -> ExitCode {
    let input = match stdin() {
        Ok(input) => input,
        Err(error) => {
            eprintln!("[supercov] {error}");
            return ExitCode::from(2);
        }
    };
    let request: WorkspaceRequest = match serde_json::from_str(&input) {
        Ok(request) => request,
        Err(error) => {
            eprintln!("[supercov] invalid workspace input: {error}");
            return ExitCode::from(2);
        }
    };
    let result = (|| -> Result<serde_json::Value, String> {
        let run_id = request
            .run_id
            .as_deref()
            .unwrap_or("workspace-differential");
        let mut lock =
            supercov_engine::lifecycle::ProjectLock::acquire(&request.root, run_id, "internal")
                .map_err(|error| error.to_string())?;
        let value = match request.action.as_str() {
            "prepare-isolated" => serde_json::json!({
                "workspace": supercov_engine::workspace::prepare_isolated_workspace(
                    &request.root,
                    run_id,
                    &lock,
                ).map_err(|error| error.to_string())?,
            }),
            "prepare-cached" => serde_json::json!({
                "workspace": supercov_engine::workspace::prepare_cached_workspace(
                    &request.root,
                    &lock,
                    request.reuse_paths.as_deref().unwrap_or_default(),
                ).map_err(|error| error.to_string())?,
            }),
            "recover-cache" => serde_json::to_value(
                supercov_engine::workspace::recover_cached_workspace(&request.root, &lock)
                    .map_err(|error| error.to_string())?,
            )
            .map_err(|error| error.to_string())?,
            "prune-cache" => serde_json::json!({
                "removed": supercov_engine::workspace::prune_cached_workspace_sources(
                    &request.root,
                    &lock,
                ).map_err(|error| error.to_string())?,
            }),
            _ => return Err(format!("unsupported workspace action: {}", request.action)),
        };
        lock.release().map_err(|error| error.to_string())?;
        Ok(value)
    })();
    match result {
        Ok(result) => {
            spawn_trash_sweeper(&request.root);
            println!("{result}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("[supercov] workspace failed: {error}");
            ExitCode::from(2)
        }
    }
}

fn spawn_trash_sweeper(root: &Path) {
    let Ok(executable) = std::env::current_exe() else {
        return;
    };
    let mut command = Command::new(executable);
    command
        .arg("__sweep-trash")
        .arg(root)
        // Keep the best-effort child from pinning a project or isolated
        // workspace as its cwd. This is required for recursive cleanup on
        // Windows and harmless on Unix filesystems.
        .current_dir(std::env::temp_dir())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let Ok(mut child) = command.spawn() else {
        return;
    };
    // Most sweeps complete in a few milliseconds. Give that fast path a
    // bounded opportunity to finish so a command launched immediately after
    // Supercov cannot discover copied config files in deferred trash. Slow
    // filesystems remain asynchronous, and the independent process group lets
    // the best-effort child survive the parent CLI exiting on Unix.
    for _ in 0..10 {
        match child.try_wait() {
            Ok(Some(_)) | Err(_) => return,
            Ok(None) => std::thread::sleep(Duration::from_millis(10)),
        }
    }
}

fn sweep_trash() -> ExitCode {
    let Some(root) = std::env::args_os().nth(2).map(PathBuf::from) else {
        return ExitCode::from(2);
    };
    match supercov_engine::lifecycle::sweep_trash(&root) {
        Ok(_) => ExitCode::SUCCESS,
        Err(_) => ExitCode::from(2),
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct LifecycleRequest {
    root: PathBuf,
    action: String,
    keep: Option<usize>,
    dry_run: Option<bool>,
    updated_at: Option<String>,
}

fn lifecycle() -> ExitCode {
    let input = match stdin() {
        Ok(input) => input,
        Err(error) => {
            eprintln!("[supercov] {error}");
            return ExitCode::from(2);
        }
    };
    let request: LifecycleRequest = match serde_json::from_str(&input) {
        Ok(request) => request,
        Err(error) => {
            eprintln!("[supercov] invalid lifecycle input: {error}");
            return ExitCode::from(2);
        }
    };
    spawn_trash_sweeper(&request.root);
    let result = match request.action.as_str() {
        "clean" => {
            let options = supercov_engine::lifecycle::CleanupOptions {
                keep: request.keep.unwrap_or(0),
                dry_run: request.dry_run.unwrap_or(false),
            };
            let updated_at = request.updated_at.as_deref().unwrap_or("internal");
            supercov_engine::lifecycle::clean_storage(&request.root, options, updated_at).and_then(
                |result| {
                    serde_json::to_value(result)
                        .map_err(supercov_engine::lifecycle::LifecycleError::Metadata)
                },
            )
        }
        "recover" => supercov_engine::lifecycle::recover_abandoned_runs(
            &request.root,
            request.updated_at.as_deref().unwrap_or("internal"),
        )
        .and_then(|result| {
            serde_json::to_value(result)
                .map_err(supercov_engine::lifecycle::LifecycleError::Metadata)
        }),
        "sweep" => supercov_engine::lifecycle::sweep_trash(&request.root).and_then(|result| {
            serde_json::to_value(result)
                .map_err(supercov_engine::lifecycle::LifecycleError::Metadata)
        }),
        _ => {
            eprintln!("[supercov] unsupported lifecycle action");
            return ExitCode::from(2);
        }
    };
    match result {
        Ok(result) => {
            spawn_trash_sweeper(&request.root);
            println!("{result}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("[supercov] lifecycle failed: {error}");
            ExitCode::from(2)
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct StoredQueryRequest {
    root: PathBuf,
    query: IndexedQueryRequest,
    newer_run_id: Option<String>,
}

fn analyze_stored_run(
    run: &StoredRun,
) -> Result<supercov_engine::coverage_report::CoverageReport, String> {
    analyze_coverage_archive(&ArchiveReportRequest {
        archive_path: run.evidence_path.clone(),
        run_id: run.id.clone(),
        generated_at: run.metadata.started_at.clone(),
        integrity: serde_json::to_value(&run.metadata.integrity).ok(),
        test_exit_code: supercov_engine::coverage_report::ExitCodeInput::Present(
            run.metadata.test_exit_code,
        ),
    })
    .map_err(|error| format!("invalid coverage archive: {error:?}"))
}

/// Internal differential surface for real persisted-run opening and indexing.
/// Public argument parsing is enabled only after this path has exact parity.
fn query_stored_run() -> ExitCode {
    let input = match stdin() {
        Ok(input) => input,
        Err(error) => {
            eprintln!("[supercov] {error}");
            return ExitCode::from(2);
        }
    };
    let mut request: StoredQueryRequest = match serde_json::from_str(&input) {
        Ok(request) => request,
        Err(error) => {
            eprintln!("[supercov] invalid stored query input: {error}");
            return ExitCode::from(2);
        }
    };
    let result = (|| -> Result<String, String> {
        let inventory = discover_runs(&request.root).map_err(|error| error.to_string())?;
        let run = select_run(&inventory, Some(&request.query.run_id))
            .map_err(|error| error.to_string())?;
        request.query.run_id.clone_from(&run.id);
        request
            .query
            .valid
            .get_or_insert(run.metadata.test_exit_code == Some(0));
        let container = open_or_rebuild_query_index(run).map_err(|error| error.to_string())?;
        let index = CoverageIndex::new(&container).map_err(|error| error.to_string())?;
        let waiver_source = supercov_engine::coverage_waivers::read_coverage_waivers(&request.root)
            .map_err(|error| error.to_string())?;
        let waiver_evaluation = if let Some(source) = waiver_source.as_ref() {
            let decisions = supercov_engine::coverage_query::filtered_decisions(
                &index,
                request.query.view().map_err(|error| error.to_string())?,
                request.query.kind.as_deref(),
                request.query.runner.as_deref(),
            )
            .map_err(|error| format!("{error:?}"))?;
            Some(supercov_engine::coverage_waivers::evaluate_coverage_waivers(&decisions, source))
        } else {
            None
        };
        let report = if request.query.command == "minimize" {
            Some(analyze_stored_run(run)?)
        } else {
            None
        };

        let newer_container;
        let newer_index;
        let newer_query = if request.query.command == "diff" {
            let selector = request
                .newer_run_id
                .as_deref()
                .ok_or_else(|| "stored diff requires a newer run ID".to_owned())?;
            let newer_run =
                select_run(&inventory, Some(selector)).map_err(|error| error.to_string())?;
            newer_container =
                open_or_rebuild_query_index(newer_run).map_err(|error| error.to_string())?;
            newer_index =
                CoverageIndex::new(&newer_container).map_err(|error| error.to_string())?;
            Some(NewerQuery {
                run_id: &newer_run.id,
                index: &newer_index,
            })
        } else {
            None
        };
        execute_indexed_query_with_waivers(
            &index,
            report.as_ref(),
            &request.query,
            newer_query,
            waiver_evaluation.as_ref(),
        )
        .map_err(|error| error.to_string())
    })();
    match result {
        Ok(output) => {
            print!("{output}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("[supercov] stored query failed: {error}");
            ExitCode::from(2)
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct DiscoverSourceRequest {
    root: PathBuf,
    configured_roots: Option<Vec<String>>,
}

fn discover_source() -> ExitCode {
    let input = match stdin() {
        Ok(input) => input,
        Err(error) => {
            eprintln!("[supercov] {error}");
            return ExitCode::from(2);
        }
    };
    let request: DiscoverSourceRequest = match serde_json::from_str(&input) {
        Ok(request) => request,
        Err(error) => {
            eprintln!("[supercov] invalid source-discovery input: {error}");
            return ExitCode::from(2);
        }
    };
    match supercov_engine::source_discovery::discover_source_scope(
        &request.root,
        request.configured_roots.as_deref(),
    ) {
        Ok(discovered) => match serde_json::to_string(&discovered) {
            Ok(output) => {
                println!("{output}");
                ExitCode::SUCCESS
            }
            Err(error) => {
                eprintln!("[supercov] failed to serialize source discovery: {error}");
                ExitCode::from(2)
            }
        },
        Err(error) => {
            eprintln!("[supercov] source discovery failed: {error}");
            ExitCode::from(2)
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct DiscoverProjectRequest {
    root: PathBuf,
    #[serde(default)]
    environment: std::collections::BTreeMap<String, String>,
    #[serde(default)]
    command: Vec<String>,
}

fn discover_project() -> ExitCode {
    let input = match stdin() {
        Ok(input) => input,
        Err(error) => {
            eprintln!("[supercov] {error}");
            return ExitCode::from(2);
        }
    };
    let request: DiscoverProjectRequest = match serde_json::from_str(&input) {
        Ok(request) => request,
        Err(error) => {
            eprintln!("[supercov] invalid project-discovery input: {error}");
            return ExitCode::from(2);
        }
    };
    match supercov_engine::project_discovery::discover_coverage_project(
        &request.root,
        &request.environment,
        &request.command,
    ) {
        Ok(project) => match serde_json::to_string(&project) {
            Ok(output) => {
                println!("{output}");
                ExitCode::SUCCESS
            }
            Err(error) => {
                eprintln!("[supercov] failed to serialize project discovery: {error}");
                ExitCode::from(2)
            }
        },
        Err(error) => {
            eprintln!("[supercov] project discovery failed: {error}");
            ExitCode::from(2)
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct IndexedFileQueryRequest {
    archive_path: PathBuf,
    run_id: String,
    generated_at: String,
    filter: String,
    command: String,
    metric: MinimizeMetric,
    kind: Option<String>,
    runner: Option<String>,
    file: Option<String>,
    line: Option<usize>,
    selector: Option<String>,
    sort: Option<DecisionSort>,
    valid: Option<bool>,
    stale: Option<bool>,
    stale_reasons: Option<Vec<String>>,
    offset: usize,
    limit: usize,
    target: Option<f64>,
    max_states: Option<usize>,
    newer_archive_path: Option<PathBuf>,
    newer_run_id: Option<String>,
}

fn query_index_files() -> ExitCode {
    let input = match stdin() {
        Ok(input) => input,
        Err(error) => {
            eprintln!("[supercov] {error}");
            return ExitCode::from(2);
        }
    };
    let request: IndexedFileQueryRequest = match serde_json::from_str(&input) {
        Ok(request) => request,
        Err(error) => {
            eprintln!("[supercov] invalid Rust indexed query input: {error}");
            return ExitCode::from(2);
        }
    };
    let view = match request.filter.as_str() {
        "all" => supercov_engine::coverage_index::CoverageViewId::All,
        "passed" => supercov_engine::coverage_index::CoverageViewId::Passed,
        "failed" => supercov_engine::coverage_index::CoverageViewId::Failed,
        _ => {
            eprintln!("[supercov] invalid coverage filter");
            return ExitCode::from(2);
        }
    };
    let gaps_only = match request.command.as_str() {
        "files" => Some(false),
        "gaps" => Some(true),
        "file-decisions" | "kinds" | "runners" | "summary" | "scope" | "line" | "test"
        | "decision" | "file-detail" | "minimize" | "diff" => None,
        _ => {
            eprintln!("[supercov] unsupported indexed query");
            return ExitCode::from(2);
        }
    };
    let archive_request = ArchiveReportRequest {
        archive_path: request.archive_path.clone(),
        run_id: request.run_id.clone(),
        generated_at: request.generated_at.clone(),
        integrity: None,
        test_exit_code: Default::default(),
    };
    let report = match analyze_coverage_archive(&archive_request) {
        Ok(report) => report,
        Err(error) => {
            eprintln!("[supercov] invalid coverage archive: {error:?}");
            return ExitCode::from(2);
        }
    };
    let root = std::env::temp_dir().join(format!("supercov-indexed-query-{}", std::process::id()));
    if let Err(error) = fs::create_dir_all(&root) {
        eprintln!("[supercov] failed to create indexed query directory: {error}");
        return ExitCode::from(2);
    }
    let path = root.join("query-index.v1.bin");
    let identity = QueryIndexIdentity {
        evidence_sha256: [1; 32],
        evidence_bytes: fs::metadata(&request.archive_path)
            .map(|metadata| metadata.len())
            .unwrap_or(0),
        analysis_sha256: [2; 32],
        producer_sha256: [3; 32],
        archive_schema_version: 2,
    };
    let result = (|| -> Result<String, String> {
        let sections = coverage_index_sections(&report).map_err(|error| error.to_string())?;
        write_query_index(&sections, &identity, &path).map_err(|error| error.to_string())?;
        let container = QueryIndex::open(&path, &identity).map_err(|error| error.to_string())?;
        let index = CoverageIndex::new(&container).map_err(|error| error.to_string())?;
        if request.command == "diff" {
            let newer_archive_path = request
                .newer_archive_path
                .as_ref()
                .ok_or_else(|| "indexed diff requires a newer archive".to_owned())?;
            let newer_run_id = request
                .newer_run_id
                .as_deref()
                .ok_or_else(|| "indexed diff requires a newer run ID".to_owned())?;
            let newer_report = analyze_coverage_archive(&ArchiveReportRequest {
                archive_path: newer_archive_path.clone(),
                run_id: newer_run_id.into(),
                generated_at: request.generated_at.clone(),
                integrity: None,
                test_exit_code: Default::default(),
            })
            .map_err(|error| format!("invalid newer coverage archive: {error:?}"))?;
            let newer_path = root.join("newer-query-index.v1.bin");
            let newer_identity = QueryIndexIdentity {
                evidence_sha256: [4; 32],
                evidence_bytes: fs::metadata(newer_archive_path)
                    .map(|metadata| metadata.len())
                    .unwrap_or(0),
                analysis_sha256: [5; 32],
                producer_sha256: [6; 32],
                archive_schema_version: 2,
            };
            write_query_index(
                &coverage_index_sections(&newer_report).map_err(|error| error.to_string())?,
                &newer_identity,
                &newer_path,
            )
            .map_err(|error| error.to_string())?;
            let newer_container = QueryIndex::open(&newer_path, &newer_identity)
                .map_err(|error| error.to_string())?;
            let newer_index =
                CoverageIndex::new(&newer_container).map_err(|error| error.to_string())?;
            let (data, page) = coverage_diff_query(
                &index,
                &newer_index,
                CoverageDiffQueryOptions {
                    older_run: &request.run_id,
                    newer_run: newer_run_id,
                    view,
                    kind: request.kind.as_deref(),
                    runner: request.runner.as_deref(),
                    offset: request.offset,
                    limit: request.limit,
                },
            )
            .map_err(|error| format!("{error:?}"))?;
            return agent_json::success("diff", &data, Some(&page))
                .map_err(|error| format!("response exceeds {} bytes", error.max_bytes));
        }
        if request.command == "minimize" {
            let coverage_view = match view {
                supercov_engine::coverage_index::CoverageViewId::All => &report.view,
                supercov_engine::coverage_index::CoverageViewId::Passed => &report.filters.passed,
                supercov_engine::coverage_index::CoverageViewId::Failed => &report.filters.failed,
            };
            let (data, page) = coverage_minimize_query(
                coverage_view,
                CoverageMinimizeQueryOptions {
                    run: &request.run_id,
                    view_id: view,
                    kind: request.kind.as_deref(),
                    runner: request.runner.as_deref(),
                    target: request.target.unwrap_or(100.0),
                    metric: request.metric,
                    max_states: request.max_states.unwrap_or(5_000),
                    offset: request.offset,
                    limit: request.limit,
                },
            )
            .map_err(|error| format!("{error:?}"))?;
            return agent_json::success("coverage.minimize", &data, Some(&page))
                .map_err(|error| format!("response exceeds {} bytes", error.max_bytes));
        }
        if request.command == "summary" {
            let data = coverage_summary_query(
                &index,
                CoverageSummaryQueryOptions {
                    run: &request.run_id,
                    view,
                    kind: request.kind.as_deref(),
                    runner: request.runner.as_deref(),
                    valid: request.valid.unwrap_or(false),
                    stale: request.stale.unwrap_or(false),
                    stale_reasons: request.stale_reasons.clone().unwrap_or_default(),
                },
            )
            .map_err(|error| format!("{error:?}"))?;
            return agent_json::success("coverage.summary", &data, None)
                .map_err(|error| format!("response exceeds {} bytes", error.max_bytes));
        }
        if request.command == "scope" {
            let (data, page) = coverage_scope_query(
                &index,
                CoverageScopeQueryOptions {
                    run: &request.run_id,
                    view,
                    kind: request.kind.as_deref(),
                    runner: request.runner.as_deref(),
                    offset: request.offset,
                    limit: request.limit,
                },
            )
            .map_err(|error| format!("{error:?}"))?;
            return agent_json::success("coverage.scope", &data, Some(&page))
                .map_err(|error| format!("response exceeds {} bytes", error.max_bytes));
        }
        if request.command == "line" {
            let file = request
                .file
                .as_deref()
                .ok_or_else(|| "indexed line query requires a file".to_owned())?;
            let line = request
                .line
                .ok_or_else(|| "indexed line query requires a line".to_owned())?;
            let (data, page) = coverage_covers_query(
                &index,
                CoverageCoversQueryOptions {
                    run: &request.run_id,
                    view,
                    kind: request.kind.as_deref(),
                    runner: request.runner.as_deref(),
                    file,
                    line,
                    offset: request.offset,
                    limit: request.limit,
                },
            )
            .map_err(|error| format!("{error:?}"))?;
            return agent_json::success("coverage.line", &data, Some(&page))
                .map_err(|error| format!("response exceeds {} bytes", error.max_bytes));
        }
        if request.command == "test" {
            let selector = request
                .selector
                .as_deref()
                .ok_or_else(|| "indexed test query requires a selector".to_owned())?;
            let (data, page) = coverage_test_query(
                &index,
                CoverageTestQueryOptions {
                    run: &request.run_id,
                    view,
                    kind: request.kind.as_deref(),
                    runner: request.runner.as_deref(),
                    selector,
                    offset: request.offset,
                    limit: request.limit,
                },
            )
            .map_err(|error| format!("{error:?}"))?;
            return agent_json::success("coverage.test", &data, Some(&page))
                .map_err(|error| format!("response exceeds {} bytes", error.max_bytes));
        }
        if request.command == "decision" {
            let selector = request
                .selector
                .as_deref()
                .ok_or_else(|| "indexed decision query requires a selector".to_owned())?;
            let (data, page) = coverage_decision_query(
                &index,
                CoverageDecisionQueryOptions {
                    run: &request.run_id,
                    view,
                    kind: request.kind.as_deref(),
                    runner: request.runner.as_deref(),
                    selector,
                    offset: request.offset,
                    limit: request.limit,
                },
            )
            .map_err(|error| format!("{error:?}"))?;
            return agent_json::success("coverage.decision", &data, Some(&page))
                .map_err(|error| format!("response exceeds {} bytes", error.max_bytes));
        }
        if request.command == "file-detail" {
            let selector = request
                .file
                .as_deref()
                .ok_or_else(|| "indexed file query requires a file".to_owned())?;
            let (data, page) = coverage_file_detail_query(
                &index,
                CoverageFileDetailOptions {
                    run: &request.run_id,
                    view,
                    kind: request.kind.as_deref(),
                    runner: request.runner.as_deref(),
                    selector,
                    metric: request.metric,
                    offset: request.offset,
                    limit: request.limit,
                },
            )
            .map_err(|error| format!("{error:?}"))?;
            return agent_json::success("coverage.file", &data, Some(&page))
                .map_err(|error| format!("response exceeds {} bytes", error.max_bytes));
        }
        if request.command == "kinds" || request.command == "runners" {
            let dimension = if request.command == "kinds" {
                supercov_engine::coverage_index::CoverageDimension::Kind
            } else {
                supercov_engine::coverage_index::CoverageDimension::Runner
            };
            let filters = CoverageQueryFilters {
                outcome: request.filter.clone(),
                kind: request.kind.clone(),
                runner: request.runner.clone(),
            };
            let (data, page) = coverage_dimension_query(
                &index,
                CoverageDimensionQueryOptions {
                    run: &request.run_id,
                    view,
                    dimension,
                    filters,
                    offset: request.offset,
                    limit: request.limit,
                },
            )
            .map_err(|error| format!("{error:?}"))?;
            let command = if request.command == "kinds" {
                "coverage.kinds"
            } else {
                "coverage.runners"
            };
            return match data {
                CoverageDimensionQueryData::Kinds(data) => {
                    agent_json::success(command, &data, Some(&page))
                }
                CoverageDimensionQueryData::Runners(data) => {
                    agent_json::success(command, &data, Some(&page))
                }
            }
            .map_err(|error| format!("response exceeds {} bytes", error.max_bytes));
        }
        if request.command == "file-decisions" {
            let file = request
                .file
                .as_deref()
                .ok_or_else(|| "indexed file-decision query requires a file".to_owned())?;
            let (data, page) = coverage_file_decisions_query(
                &index,
                CoverageFileDecisionsOptions {
                    run: &request.run_id,
                    view,
                    kind: request.kind.as_deref(),
                    runner: request.runner.as_deref(),
                    file,
                    waived_by_decision: None,
                    sort: request.sort.unwrap_or(DecisionSort::Location),
                    offset: request.offset,
                    limit: request.limit,
                },
            )
            .map_err(|error| format!("{error:?}"))?;
            return agent_json::success("coverage.file", &data, Some(&page))
                .map_err(|error| format!("response exceeds {} bytes", error.max_bytes));
        }
        let query = coverage_file_query(
            &index,
            CoverageFileQueryOptions {
                run: &request.run_id,
                view,
                metric: request.metric,
                gaps_only: gaps_only.expect("files/gaps command"),
                kind: request.kind.as_deref(),
                runner: request.runner.as_deref(),
                offset: request.offset,
                limit: request.limit,
            },
        )
        .map_err(|error| format!("{error:?}"))?;
        let command = if gaps_only == Some(true) {
            "coverage.gaps"
        } else {
            "coverage.files"
        };
        match query.data {
            CoverageFileQueryData::Files(data) => {
                agent_json::success(command, &data, Some(&query.pagination))
            }
            CoverageFileQueryData::Gaps(data) => {
                agent_json::success(command, &data, Some(&query.pagination))
            }
        }
        .map_err(|error| format!("response exceeds {} bytes", error.max_bytes))
    })();
    let _ = fs::remove_dir_all(&root);
    match result {
        Ok(output) => {
            print!("{output}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("[supercov] indexed query failed: {error}");
            ExitCode::from(2)
        }
    }
}

fn roundtrip_query_indexes() -> ExitCode {
    let input = match stdin() {
        Ok(input) => input,
        Err(error) => {
            eprintln!("[supercov] {error}");
            return ExitCode::from(2);
        }
    };
    let requests: Vec<ArchiveReportRequest> = match serde_json::from_str(&input) {
        Ok(requests) => requests,
        Err(error) => {
            eprintln!("[supercov] invalid Rust query-index input: {error}");
            return ExitCode::from(2);
        }
    };
    let root = std::env::temp_dir().join(format!(
        "supercov-query-index-roundtrip-{}",
        std::process::id()
    ));
    if let Err(error) = fs::create_dir_all(&root) {
        eprintln!("[supercov] failed to create query-index test directory: {error}");
        return ExitCode::from(2);
    }
    let result = (|| -> Result<Vec<_>, String> {
        let mut snapshots = Vec::with_capacity(requests.len());
        for (index, request) in requests.iter().enumerate() {
            let report = analyze_coverage_archive(request)
                .map_err(|error| format!("invalid coverage archive: {error:?}"))?;
            let seed = u8::try_from(index % 251).unwrap_or(0);
            let identity = QueryIndexIdentity {
                evidence_sha256: [seed; 32],
                evidence_bytes: fs::metadata(&request.archive_path)
                    .map_err(|error| error.to_string())?
                    .len(),
                analysis_sha256: [seed.wrapping_add(1); 32],
                producer_sha256: [seed.wrapping_add(2); 32],
                archive_schema_version: 2,
            };
            let path = root.join(format!("{index}.bin"));
            let sections = coverage_index_sections(&report).map_err(|error| error.to_string())?;
            write_query_index(&sections, &identity, &path).map_err(|error| error.to_string())?;
            let container =
                QueryIndex::open(&path, &identity).map_err(|error| error.to_string())?;
            snapshots.push(
                CoverageIndex::new(&container)
                    .and_then(|index| index.snapshot())
                    .map_err(|error| error.to_string())?,
            );
        }
        Ok(snapshots)
    })();
    let _ = fs::remove_dir_all(&root);
    let snapshots = match result {
        Ok(snapshots) => snapshots,
        Err(error) => {
            eprintln!("[supercov] query-index roundtrip failed: {error}");
            return ExitCode::from(2);
        }
    };
    if let Err(error) = serde_json::to_writer(std::io::stdout(), &snapshots) {
        eprintln!("[supercov] failed to write Rust query-index output: {error}");
        return ExitCode::from(2);
    }
    ExitCode::SUCCESS
}

fn minimum_test_sets() -> ExitCode {
    let input = match stdin() {
        Ok(input) => input,
        Err(error) => {
            eprintln!("[supercov] {error}");
            return ExitCode::from(2);
        }
    };
    let requests: Vec<MinimumTestSetRequest> = match serde_json::from_str(&input) {
        Ok(requests) => requests,
        Err(error) => {
            eprintln!("[supercov] invalid Rust minimization input: {error}");
            return ExitCode::from(2);
        }
    };
    let mut results = Vec::with_capacity(requests.len());
    for request in &requests {
        match minimum_test_set_for_request(request) {
            Ok(result) => results.push(result),
            Err(error) => {
                eprintln!("[supercov] coverage minimization failed: {error:?}");
                return ExitCode::from(2);
            }
        }
    }
    if let Err(error) = serde_json::to_writer(std::io::stdout(), &results) {
        eprintln!("[supercov] failed to write Rust minimization output: {error}");
        return ExitCode::from(2);
    }
    ExitCode::SUCCESS
}

fn analyze_evidence_archive() -> ExitCode {
    let input = match stdin() {
        Ok(input) => input,
        Err(error) => {
            eprintln!("[supercov] {error}");
            return ExitCode::from(2);
        }
    };
    let request: ArchiveReportRequest = match serde_json::from_str(&input) {
        Ok(request) => request,
        Err(error) => {
            eprintln!("[supercov] invalid Rust archive analysis input: {error}");
            return ExitCode::from(2);
        }
    };
    let report = match analyze_coverage_archive(&request) {
        Ok(report) => report,
        Err(error) => {
            eprintln!("[supercov] invalid coverage archive: {error:?}");
            return ExitCode::from(2);
        }
    };
    if let Err(error) = serde_json::to_writer(std::io::stdout(), &report) {
        eprintln!("[supercov] failed to write Rust archive report output: {error}");
        return ExitCode::from(2);
    }
    ExitCode::SUCCESS
}

fn analyze_coverage_report() -> ExitCode {
    let input = match stdin() {
        Ok(input) => input,
        Err(error) => {
            eprintln!("[supercov] {error}");
            return ExitCode::from(2);
        }
    };
    let requests: Vec<CoverageReportRequest> = match serde_json::from_str(&input) {
        Ok(requests) => requests,
        Err(error) => {
            eprintln!("[supercov] invalid Rust coverage report input: {error}");
            return ExitCode::from(2);
        }
    };
    let mut reports = Vec::with_capacity(requests.len());
    for request in &requests {
        match analyze_coverage_results(request) {
            Ok(report) => reports.push(report),
            Err(error) => {
                eprintln!("[supercov] invalid coverage evidence: {error:?}");
                return ExitCode::from(2);
            }
        }
    }
    if let Err(error) = serde_json::to_writer(std::io::stdout(), &reports) {
        eprintln!("[supercov] failed to write Rust coverage report output: {error}");
        return ExitCode::from(2);
    }
    ExitCode::SUCCESS
}

fn analyze_coverage_core() -> ExitCode {
    let input = match stdin() {
        Ok(input) => input,
        Err(error) => {
            eprintln!("[supercov] {error}");
            return ExitCode::from(2);
        }
    };
    let requests: Vec<CoverageCoreInput> = match serde_json::from_str(&input) {
        Ok(requests) => requests,
        Err(error) => {
            eprintln!("[supercov] invalid Rust coverage analysis input: {error}");
            return ExitCode::from(2);
        }
    };
    let mut outputs = Vec::with_capacity(requests.len());
    for request in &requests {
        match analyze_core(request) {
            Ok(output) => outputs.push(output),
            Err(error) => {
                eprintln!("[supercov] invalid coverage vectors: {error:?}");
                return ExitCode::from(2);
            }
        }
    }
    if let Err(error) = serde_json::to_writer(std::io::stdout(), &outputs) {
        eprintln!("[supercov] failed to write Rust coverage analysis output: {error}");
        return ExitCode::from(2);
    }
    ExitCode::SUCCESS
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct TransformBenchmarkResult {
    files: usize,
    duration_ns: u128,
}

/// Development-only measurement boundary for the frozen Phase 3 transform
/// gate. Input decoding and output transport are measured separately by the
/// caller; this reports only parse -> transform -> codegen engine time.
fn benchmark_js_transform() -> ExitCode {
    let input = match stdin() {
        Ok(input) => input,
        Err(error) => {
            eprintln!("[supercov] {error}");
            return ExitCode::from(2);
        }
    };
    let cases: Vec<InstrumentCase> = match serde_json::from_str(&input) {
        Ok(cases) => cases,
        Err(error) => {
            eprintln!("[supercov] invalid Rust benchmark input: {error}");
            return ExitCode::from(2);
        }
    };
    let files = cases.len();
    let started = Instant::now();
    for case in cases {
        if let Err(error) = instrument_candidate(&case.source, &case.file) {
            eprintln!("[supercov] {}: {error:?}", case.file);
            return ExitCode::from(2);
        }
    }
    let result = TransformBenchmarkResult {
        files,
        duration_ns: started.elapsed().as_nanos(),
    };
    if let Err(error) = serde_json::to_writer(std::io::stdout(), &result) {
        eprintln!("[supercov] failed to write Rust benchmark output: {error}");
        return ExitCode::from(2);
    }
    ExitCode::SUCCESS
}

fn stdin() -> Result<String, String> {
    let input = stdin_bytes()?;
    String::from_utf8(input).map_err(|error| format!("Rust engine input is not UTF-8: {error}"))
}

fn stdin_bytes() -> Result<Vec<u8>, String> {
    if let Some(path) = std::env::var_os("SUPERCOV_INTERNAL_INPUT_FILE") {
        return fs::read(&path).map_err(|error| {
            format!(
                "failed to read Rust engine input {}: {error}",
                Path::new(&path).display()
            )
        });
    }
    let mut input = Vec::new();
    std::io::stdin()
        .read_to_end(&mut input)
        .map_err(|error| format!("failed to read Rust engine input: {error}"))?;
    Ok(input)
}

#[derive(Deserialize)]
struct InstrumentCase {
    file: String,
    source: String,
}

/// Private batch protocol used by conformance and performance harnesses.
fn instrument_js() -> ExitCode {
    let input = match stdin() {
        Ok(input) => input,
        Err(error) => {
            eprintln!("[supercov] {error}");
            return ExitCode::from(2);
        }
    };
    let cases: Vec<InstrumentCase> = match serde_json::from_str(&input) {
        Ok(cases) => cases,
        Err(error) => {
            eprintln!("[supercov] invalid Rust instrumenter input: {error}");
            return ExitCode::from(2);
        }
    };
    let mut outputs = Vec::with_capacity(cases.len());
    for case in cases {
        match instrument_candidate(&case.source, &case.file) {
            Ok(output) => outputs.push(output),
            Err(error) => {
                eprintln!("[supercov] {}: {error:?}", case.file);
                return ExitCode::from(2);
            }
        }
    }
    if let Err(error) = serde_json::to_writer(std::io::stdout(), &outputs) {
        eprintln!("[supercov] failed to write Rust instrumenter output: {error}");
        return ExitCode::from(2);
    }
    ExitCode::SUCCESS
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PackEvidenceRequest {
    destination: PathBuf,
    #[serde(default)]
    sources: Vec<PackEvidenceSource>,
    #[serde(default)]
    entries: Vec<PackEvidenceEntry>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PackEvidenceSource {
    directory: Option<PathBuf>,
    prefix: Option<String>,
    file: Option<PathBuf>,
    path: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PackEvidenceEntry {
    path: String,
    contents: String,
}

fn pack_evidence() -> ExitCode {
    let input = match stdin() {
        Ok(input) => input,
        Err(error) => {
            eprintln!("[supercov] {error}");
            return ExitCode::from(2);
        }
    };
    let request: PackEvidenceRequest = match serde_json::from_str(&input) {
        Ok(request) => request,
        Err(error) => {
            eprintln!("[supercov] invalid Rust evidence request: {error}");
            return ExitCode::from(2);
        }
    };
    if !request.sources.is_empty() && !request.entries.is_empty() {
        eprintln!("[supercov] Rust evidence request cannot mix sources and entries");
        return ExitCode::from(2);
    }
    let entries = if request.entries.is_empty() {
        let mut sources = Vec::with_capacity(request.sources.len());
        for source in request.sources {
            match (source.directory, source.file, source.path) {
                (Some(directory), None, None) => sources.push(EvidenceArchiveSource::Directory {
                    directory,
                    prefix: source.prefix,
                }),
                (None, Some(file), Some(path)) if source.prefix.is_none() => {
                    sources.push(EvidenceArchiveSource::File { file, path });
                }
                _ => {
                    eprintln!(
                        "[supercov] each evidence source must be one directory with an optional prefix or one file with an archive path"
                    );
                    return ExitCode::from(2);
                }
            }
        }
        match collect_sources(&sources) {
            Ok(entries) => entries,
            Err(error) => {
                eprintln!("[supercov] failed to collect evidence: {error}");
                return ExitCode::from(2);
            }
        }
    } else {
        request
            .entries
            .into_iter()
            .map(|entry| EvidenceArchiveEntry {
                path: entry.path,
                contents: entry.contents.into_bytes(),
            })
            .collect()
    };
    let metadata = match write_archive(entries, &request.destination) {
        Ok(metadata) => metadata,
        Err(error) => {
            eprintln!("[supercov] failed to pack evidence: {error}");
            return ExitCode::from(2);
        }
    };
    if let Err(error) = serde_json::to_writer(std::io::stdout(), &metadata) {
        eprintln!("[supercov] failed to serialize evidence metadata: {error}");
        return ExitCode::from(2);
    }
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_reports_the_public_engine() {
        assert!(HELP.contains("Supercov coverage engine"));
    }

    #[test]
    fn nested_cargo_wrapper_dispatch_requires_a_real_executable() {
        let executable = std::env::current_exe().unwrap();
        assert!(is_executable_wrapper_program(&executable.to_string_lossy()));
        assert!(!is_executable_wrapper_program("--edition=2024"));
        assert!(!is_executable_wrapper_program("@rustdoc-arguments"));
        assert!(!is_executable_wrapper_program("Cargo.toml"));
    }

    #[test]
    fn rustdoc_removes_only_the_exact_injected_cargo_runner() {
        let runner = Path::new("/opt/supercov");
        assert_eq!(
            strip_injected_rustdoc_runner(
                vec![
                    "--crate-name".into(),
                    "fixture".into(),
                    "--test-runtool".into(),
                    "/opt/supercov".into(),
                    "--test-runtool-arg".into(),
                    "__cargo-test-runner".into(),
                    "src/lib.rs".into(),
                ],
                runner,
            )
            .unwrap(),
            ["--crate-name", "fixture", "src/lib.rs"]
        );
        assert_eq!(
            strip_injected_rustdoc_runner(
                vec![
                    "--test-runtool=/opt/supercov".into(),
                    "--test-runtool-arg=__cargo-test-runner".into(),
                ],
                runner,
            )
            .unwrap(),
            Vec::<String>::new()
        );
    }

    #[test]
    fn rustdoc_rejects_missing_or_foreign_runner_composition() {
        let runner = Path::new("/opt/supercov");
        assert!(strip_injected_rustdoc_runner(vec!["--test".into()], runner).is_err());
        assert!(
            strip_injected_rustdoc_runner(
                vec![
                    "--test-runtool".into(),
                    "/opt/foreign".into(),
                    "--test-runtool-arg".into(),
                    "__cargo-test-runner".into(),
                ],
                runner,
            )
            .is_err()
        );
        assert!(
            strip_injected_rustdoc_runner(
                vec!["--test-runtool".into(), "/opt/supercov".into()],
                runner,
            )
            .is_err()
        );
    }

    #[test]
    fn cleanup_options_preserve_the_frozen_cli_contract() {
        assert_eq!(parse_cleanup_options(&[]).unwrap(), (0, false));
        assert_eq!(
            parse_cleanup_options(&["--keep".into(), "20".into(), "--dry-run".into()]).unwrap(),
            (20, true)
        );
        assert_eq!(
            parse_cleanup_options(&["--keep".into()]).unwrap_err(),
            "--keep must be a non-negative integer"
        );
        assert_eq!(
            parse_cleanup_options(&["--unknown".into()]).unwrap_err(),
            "Unknown clean option: --unknown"
        );
        let result = supercov_engine::lifecycle::CleanupResult {
            removed_runs: vec!["run-1".into(), "run-0".into()],
            removed_workspaces: vec!["run-1".into()],
            removed_evidence: vec![],
            removed_build_cache: false,
        };
        assert_eq!(
            cleanup_summary(0, false, &result),
            "[supercov] removed 2 stored run(s), 1 per-run workspace(s), and no isolated build cache; keeping 0 newest run(s)"
        );
    }

    #[test]
    fn public_run_ids_are_short_opaque_and_unique() {
        let (first, first_started_at) = public_run_identity().unwrap();
        let (second, second_started_at) = public_run_identity().unwrap();
        assert!(first.starts_with("run_"));
        assert_eq!(first.len(), 20);
        assert!(public_run_id(&first));
        assert_ne!(first, second);
        assert!(!public_run_id("2026-08-25T23-33-12-211Z"));
        assert!(!public_run_id("run_35c35ceeaf4b843Z"));
        assert!(first_started_at.contains('T'));
        assert!(second_started_at.contains('T'));
    }
}
