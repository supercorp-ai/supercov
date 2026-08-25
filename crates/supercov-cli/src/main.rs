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
    indexed_query::{IndexedQueryRequest, NewerQuery, execute_indexed_query_with_waivers},
    js_instrumenter::instrument_candidate,
    query_index::{QueryIndex, QueryIndexIdentity, write_query_index},
    run_store::{StoredRun, discover_runs, open_or_rebuild_query_index, select_run},
};

const HELP: &str = "Rust candidate for the frozen Supercov engine contract v1.\n\
This binary is a contract shell, not yet a coverage engine.\n\
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
        Some("__sweep-trash") => sweep_trash(),
        Some("__benchmark-js-transform") => benchmark_js_transform(),
        Some("__pack-evidence") => pack_evidence(),
        Some(command) => {
            eprintln!(
                "[supercov] Rust engine candidate is not ready for `{command}`; use the currently shipped engine while the Rust contract gates are incomplete"
            );
            ExitCode::from(2)
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
        let container = open_or_rebuild_query_index(run).map_err(|error| error.to_string())?;
        let index = CoverageIndex::new(&container).map_err(|error| error.to_string())?;
        let waiver_source = supercov_engine::coverage_waivers::read_coverage_waivers(&request.root)
            .map_err(|error| error.to_string())?;
        let waiver_evaluation = if let Some(source) = waiver_source.as_ref() {
            let decisions = supercov_engine::coverage_query::filtered_decisions(
                &index,
                request.query.view()?,
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
    fn shell_is_explicitly_not_a_false_coverage_implementation() {
        assert!(HELP.contains("not yet a coverage engine"));
        assert_eq!(
            supercov_engine::READINESS,
            supercov_engine::EngineReadiness::ContractShell
        );
    }
}
