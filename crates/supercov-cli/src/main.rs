use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
    process::{Command, ExitCode, Stdio},
    time::Instant,
};

use serde::{Deserialize, Serialize};
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
        RunStoreError, StoredRun, compare_run_integrity, discover_runs,
        open_or_rebuild_query_index, select_run,
    },
};
use time::{OffsetDateTime, macros::format_description};

mod human_query;
mod public_query;
use human_query::render_human;
use public_query::{PublicQueryInvocation, parse_public_query};

const HELP: &str = "Supercov coverage engine (Rust differential candidate).\n\
\n\
Reference-engine UX:\n\
  supercov -- <test command>\n\
  supercov runs <run-id> coverage [resource] [--json]\n\
  supercov diff <older-run> <newer-run> [--json]\n\
  supercov merge <run-id> <run-id> [...]\n\
  supercov prune|clean [--keep N] [--dry-run]\n";

fn main() -> ExitCode {
    let mut arguments = std::env::args().skip(1);
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
        Some("prune") => cleanup_command("prune", arguments.collect()),
        Some("clean") => cleanup_command("clean", arguments.collect()),
        Some("runs") => public_query_command("runs", arguments.collect()),
        Some("diff") => public_query_command("diff", arguments.collect()),
        Some(command) => {
            eprintln!(
                "[supercov] Rust engine candidate is not ready for `{command}`; use the currently shipped engine while the Rust contract gates are incomplete"
            );
            ExitCode::from(2)
        }
    }
}

const PUBLIC_TIMESTAMP_FORMAT: &[time::format_description::BorrowedFormatItem<'static>] =
    format_description!("[year]-[month]-[day]T[hour]:[minute]:[second].[subsecond digits:3]Z");

fn public_timestamp() -> Result<(String, String), String> {
    let started_at = OffsetDateTime::now_utc()
        .format(PUBLIC_TIMESTAMP_FORMAT)
        .map_err(|error| format!("could not generate the run timestamp: {error}"))?;
    let run_id = started_at.replace([':', '.'], "-");
    Ok((run_id, started_at))
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
    let Some(runtime_root) = resolve_runtime_root(&root) else {
        eprintln!(
            "[supercov] could not locate the JavaScript runtime; set SUPERCOV_RUNTIME_ROOT to the packaged runtime directory"
        );
        return ExitCode::from(2);
    };
    let (run_id, started_at) = match public_timestamp() {
        Ok(timestamp) => timestamp,
        Err(error) => {
            eprintln!("[supercov] {error}");
            return ExitCode::from(2);
        }
    };
    spawn_trash_sweeper(&root);
    let request = supercov_engine::javascript_run::DirectJavascriptRunRequest {
        root: root.clone(),
        runtime_root,
        command,
        run_id: Some(run_id),
        started_at: Some(started_at),
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

fn parse_cleanup_options(command: &str, arguments: &[String]) -> Result<(usize, bool), String> {
    let mut keep = 20;
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
            return Err(format!("Unknown {command} option: {argument}"));
        }
        index += 1;
    }
    Ok((keep, dry_run))
}

fn cleanup_summary(
    command: &str,
    keep: usize,
    dry_run: bool,
    result: &supercov_engine::lifecycle::CleanupResult,
) -> String {
    if command == "prune" {
        format!(
            "[supercov] {} {} stored run(s), {} terminal/orphan work director{}, and {} loose evidence director{}; keeping {} newest run(s) and preserving the shared cache",
            if dry_run { "would remove" } else { "removed" },
            result.removed_runs.len(),
            result.removed_workspaces.len(),
            if result.removed_workspaces.len() == 1 {
                "y"
            } else {
                "ies"
            },
            result.removed_evidence.len(),
            if result.removed_evidence.len() == 1 {
                "y"
            } else {
                "ies"
            },
            keep,
        )
    } else {
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
}

fn cleanup_command(command: &str, arguments: Vec<String>) -> ExitCode {
    let (keep, dry_run) = match parse_cleanup_options(command, &arguments) {
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
    let result = if command == "prune" {
        supercov_engine::lifecycle::prune_storage(&root, options, &updated_at)
    } else {
        supercov_engine::lifecycle::clean_storage(&root, options, &updated_at)
    };
    let result = match result {
        Ok(result) => result,
        Err(error) => {
            eprintln!("[supercov] {command} failed: {error}");
            return ExitCode::from(2);
        }
    };
    spawn_trash_sweeper(&root);
    println!("{}", cleanup_summary(command, keep, dry_run, &result));
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

fn resolve_runtime_root(root: &Path) -> Option<PathBuf> {
    if let Some(configured) = std::env::var_os("SUPERCOV_RUNTIME_ROOT") {
        let configured = PathBuf::from(configured);
        if configured.join("runtime.js").is_file() {
            return Some(configured);
        }
    }
    let local = root.join("dist");
    if local.join("runtime.js").is_file() {
        return Some(local);
    }
    std::env::current_exe().ok().and_then(|executable| {
        executable.ancestors().find_map(|ancestor| {
            let candidate = ancestor.join("dist");
            candidate.join("runtime.js").is_file().then_some(candidate)
        })
    })
}

fn current_javascript_integrity(root: &Path) -> Option<supercov_engine::run_store::RunIntegrity> {
    let runtime_root = resolve_runtime_root(root)?;
    supercov_engine::javascript_run::current_javascript_integrity(root, &runtime_root, &[]).ok()
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
            let inventory = discover_runs(root).map_err(run_store_agent_error)?;
            let view = match filter.as_str() {
                "all" => supercov_engine::coverage_index::CoverageViewId::All,
                "passed" => supercov_engine::coverage_index::CoverageViewId::Passed,
                "failed" => supercov_engine::coverage_index::CoverageViewId::Failed,
                _ => unreachable!("public parser validates coverage filters"),
            };
            let current = current_javascript_integrity(root);
            let (data, page) = run_list_query(&inventory, current.as_ref(), view, *offset, *limit);
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
            let inventory = discover_runs(root).map_err(run_store_agent_error)?;
            let run =
                select_run(&inventory, Some(&request.run_id)).map_err(run_store_agent_error)?;
            request.run_id.clone_from(&run.id);
            request
                .valid
                .get_or_insert(run.metadata.test_exit_code == Some(0));
            let current = current_javascript_integrity(root);
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
                let waiver_source = supercov_engine::coverage_waivers::read_coverage_waivers(root)
                    .map_err(|error| {
                        internal_agent_error(format!("Failed to read coverage waivers: {error}"))
                    })?;
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
                    if let Some(current) = current.as_ref() {
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
    let request: supercov_engine::javascript_run::DirectJavascriptRunRequest =
        match serde_json::from_str(&input) {
            Ok(request) => request,
            Err(error) => {
                eprintln!("[supercov] invalid direct JavaScript run input: {error}");
                return ExitCode::from(2);
            }
        };
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
    };
    let mut stderr = std::io::stderr().lock();
    match supercov_engine::process_supervision::supervise_command(
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
    let _ = Command::new(executable)
        .arg("__sweep-trash")
        .arg(root)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
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
        "prune" | "clean" => {
            let options = supercov_engine::lifecycle::CleanupOptions {
                keep: request.keep.unwrap_or(20),
                dry_run: request.dry_run.unwrap_or(false),
            };
            let updated_at = request.updated_at.as_deref().unwrap_or("internal");
            if request.action == "prune" {
                supercov_engine::lifecycle::prune_storage(&request.root, options, updated_at)
            } else {
                supercov_engine::lifecycle::clean_storage(&request.root, options, updated_at)
            }
            .and_then(|result| {
                serde_json::to_value(result)
                    .map_err(supercov_engine::lifecycle::LifecycleError::Metadata)
            })
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
        "file-decisions" | "kinds" | "runners" | "summary" | "scope" | "covers" | "test"
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
        if request.command == "covers" {
            let file = request
                .file
                .as_deref()
                .ok_or_else(|| "indexed covers query requires a file".to_owned())?;
            let line = request
                .line
                .ok_or_else(|| "indexed covers query requires a line".to_owned())?;
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
            return agent_json::success("coverage.covers", &data, Some(&page))
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
    let mut input = String::new();
    std::io::stdin()
        .read_to_string(&mut input)
        .map_err(|error| format!("failed to read Rust engine input: {error}"))?;
    Ok(input)
}

#[derive(Deserialize)]
struct InstrumentCase {
    file: String,
    source: String,
}

/// Private migration protocol. It intentionally accepts a whole batch so the
/// Node shim never pays one process launch per source file.
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
    fn shell_reports_its_private_differential_readiness_honestly() {
        assert!(HELP.contains("Rust differential candidate"));
        assert_eq!(
            supercov_engine::READINESS,
            supercov_engine::EngineReadiness::DifferentialCandidate
        );
    }

    #[test]
    fn cleanup_options_preserve_the_frozen_cli_contract() {
        assert_eq!(parse_cleanup_options("prune", &[]).unwrap(), (20, false));
        assert_eq!(
            parse_cleanup_options("clean", &["--keep".into(), "0".into(), "--dry-run".into()])
                .unwrap(),
            (0, true)
        );
        assert_eq!(
            parse_cleanup_options("prune", &["--keep".into()]).unwrap_err(),
            "--keep must be a non-negative integer"
        );
        assert_eq!(
            parse_cleanup_options("clean", &["--unknown".into()]).unwrap_err(),
            "Unknown clean option: --unknown"
        );
        let result = supercov_engine::lifecycle::CleanupResult {
            removed_runs: vec!["run-1".into(), "run-0".into()],
            removed_workspaces: vec!["run-1".into()],
            removed_evidence: vec![],
            removed_build_cache: false,
        };
        assert_eq!(
            cleanup_summary("prune", 20, true, &result),
            "[supercov] would remove 2 stored run(s), 1 terminal/orphan work directory, and 0 loose evidence directories; keeping 20 newest run(s) and preserving the shared cache"
        );
        assert_eq!(
            cleanup_summary("clean", 0, false, &result),
            "[supercov] removed 2 stored run(s), 1 per-run workspace(s), and no isolated build cache; keeping 0 newest run(s)"
        );
    }
}
