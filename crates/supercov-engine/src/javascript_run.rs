//! Rust-owned JavaScript execution for the public Supercov engine.

use std::{
    collections::BTreeMap,
    ffi::OsString,
    path::{Path, PathBuf},
    time::{Instant, SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};
use supercov_contracts::{
    AttributionPrecision, ExecutionModel, FrontendAttribution, FrontendLimitation,
    FrontendLimitationScope, FrontendRunDeclaration, FrontendRunnerDeclaration,
    LANGUAGE_FRONTEND_PROTOCOL_VERSION, StructuralSource,
};

use crate::{
    coverage_report::{PersistedCoverageModel, RawTestResult, javascript_coverage_model},
    evidence_archive::{
        EvidenceArchiveEntry, EvidenceArchiveSource, collect_sources, write_archive,
    },
    integrity::{FrontendIntegrityInputs, create_run_integrity},
    javascript_frontend::{
        javascript_frontend_reuse_paths, load_cached_javascript_frontend,
        prepare_javascript_frontend, read_javascript_frontend_cache,
    },
    lifecycle::{
        ProjectLock, RunState, RunStateStatus, finalize_published_run, interrupt_run_state,
        publish_run, recover_abandoned_runs, remove_stored_tree_deferred, update_run_state,
        write_run_state,
    },
    orchestration::{
        ExecutionPhase, ExecutionPlan, OrchestrationError, PhaseKind, execute_plan_with_supervisor,
    },
    process_supervision::{
        CommandSpec, ForwardedSignal, ProcessSupervisor, SupervisionOptions, positive_milliseconds,
    },
    project_discovery::discover_coverage_project,
    run_store::{RawEvidenceMetadata, RunIntegrity, RunMetadata, RunTimings},
    workspace::{
        cached_workspace_path, prepare_cached_workspace, prune_cached_workspace_sources,
        sync_command_outputs, workspace_output_baseline,
    },
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
    #[serde(skip)]
    pub watchdog_program: Option<PathBuf>,
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
    /// The runs of servers the suite drove, published beside this one, by
    /// language.
    pub server_runs: Vec<(String, PathBuf)>,
    /// The project's build script, when the command did not run it. The copy
    /// starts without build output, so a suite that needs it fails; the
    /// caller says so when the failure is not a test's.
    #[serde(skip)]
    pub unrun_build: Vec<String>,
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
                write!(formatter, "interrupted by {}", signal.name())
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

fn javascript_runner_declaration(
    runner: String,
    results: &[&RawTestResult],
) -> FrontendRunnerDeclaration {
    let has_observations = |raw: &&RawTestResult| {
        !raw.phases.is_empty()
            || !raw.server.is_empty()
            || raw.runtime.iter().chain(&raw.browser).any(|snapshot| {
                !snapshot.decisions.is_empty()
                    || !snapshot.hits.is_empty()
                    || !snapshot.events.is_empty()
            })
    };
    let exact_test = results.iter().all(|raw| {
        raw.test_id
            .as_deref()
            .is_some_and(|value| !value.is_empty())
            && raw
                .scope
                .as_ref()
                .is_none_or(|scope| raw.test_id.as_deref() == Some(scope.test_id.as_str()))
    });
    let exact_worker = results.iter().all(|raw| {
        !has_observations(raw)
            || raw
                .scope
                .as_ref()
                .is_some_and(|scope| !scope.worker_id.is_empty())
    });
    let exact_retry = results.iter().all(|raw| {
        raw.retry.is_some()
            && raw
                .scope
                .as_ref()
                .is_none_or(|scope| raw.retry == Some(scope.retry))
    });
    let contextual = !results.is_empty() && exact_test && exact_worker && exact_retry;
    let precision = |exact| {
        if exact {
            AttributionPrecision::Exact
        } else {
            AttributionPrecision::Unavailable
        }
    };
    let attribution = FrontendAttribution {
        run: AttributionPrecision::Exact,
        worker: precision(contextual),
        test: precision(contextual),
        retry: precision(contextual),
        phase: precision(contextual),
        action: precision(contextual),
        assertion: precision(contextual),
    };
    let mut limitations = Vec::new();
    if !contextual {
        for (suffix, scope) in [
            ("worker", FrontendLimitationScope::Worker),
            ("test", FrontendLimitationScope::Test),
            ("retry", FrontendLimitationScope::Retry),
            ("phase", FrontendLimitationScope::Phase),
            ("action", FrontendLimitationScope::Action),
            ("assertion", FrontendLimitationScope::Assertion),
        ] {
            limitations.push(FrontendLimitation {
                id: format!("{runner}-no-{suffix}").replace(':', "-"),
                scopes: vec![scope],
                reason: format!(
                    "Runner {runner} did not expose exact {suffix} identity for every result"
                ),
            });
        }
    }
    FrontendRunnerDeclaration {
        runner,
        execution_model: if contextual {
            ExecutionModel::ParallelContextPropagated
        } else {
            ExecutionModel::ParallelUnattributed
        },
        attribution,
        limitations,
    }
}

fn javascript_archive_entries(
    mut entries: Vec<EvidenceArchiveEntry>,
    manifest: &crate::javascript_frontend::JavascriptManifest,
    run_id: &str,
    exit_code: i32,
) -> Result<Vec<EvidenceArchiveEntry>, String> {
    let mut results = Vec::new();
    for entry in &entries {
        if entry.path == "mcdc.json" || entry.path.ends_with("/mcdc.json") {
            results.push(
                serde_json::from_slice::<RawTestResult>(&entry.contents)
                    .map_err(|error| format!("invalid {}: {error}", entry.path))?,
            );
        } else if entry.path == "mcdc.jsonl" || entry.path.ends_with(".mcdc.jsonl") {
            let contents = entry
                .contents
                .strip_suffix(b"\n")
                .ok_or_else(|| format!("{} does not end with a newline", entry.path))?;
            for (index, line) in contents.split(|byte| *byte == b'\n').enumerate() {
                if line.is_empty() {
                    return Err(format!(
                        "{} contains a blank record at line {}",
                        entry.path,
                        index + 1
                    ));
                }
                results.push(
                    serde_json::from_slice::<RawTestResult>(line).map_err(|error| {
                        format!(
                            "invalid {} record at line {}: {error}",
                            entry.path,
                            index + 1
                        )
                    })?,
                );
            }
        }
    }
    if results.is_empty() {
        let status = match exit_code {
            0 => "passed",
            supercov_contracts::COMMAND_TIMEOUT_EXIT_CODE => "timedOut",
            _ => "failed",
        };
        let command_result = RawTestResult {
            span: None,
            test_id: Some(format!("command:{run_id}")),
            scope: None,
            test: "Test command".into(),
            test_file: None,
            title: Some("Test command".into()),
            retry: Some(0),
            status: Some(status.into()),
            expected_status: None,
            flaky: false,
            provenance: crate::coverage_report::TestProvenance {
                runner: "command".into(),
                kind: "setup".into(),
                project: None,
                source: "engine".into(),
                tags: Vec::new(),
            },
            role: "setup".into(),
            attribution: crate::coverage_report::ATTRIBUTION_EXACT.into(),
            phases: vec![],
            runtime: vec![],
            browser: vec![],
            server: vec![],
        };
        entries.push(EvidenceArchiveEntry {
            path: "results/command/mcdc.json".into(),
            contents: serde_json::to_vec(&command_result).map_err(|error| error.to_string())?,
        });
        results.push(command_result);
    }
    let mut by_runner = BTreeMap::<String, Vec<&RawTestResult>>::new();
    for result in &results {
        by_runner
            .entry(result.provenance.runner.clone())
            .or_default()
            .push(result);
    }
    if entries
        .iter()
        .any(|entry| entry.path.starts_with("server/background/") && entry.path.ends_with(".jsonl"))
    {
        by_runner.entry("background".into()).or_default();
    }
    let declaration = FrontendRunDeclaration {
        protocol_version: LANGUAGE_FRONTEND_PROTOCOL_VERSION,
        frontend_id: "javascript".into(),
        frontend_version: "javascript-owned-v1".into(),
        language: "javascript".into(),
        structural_source: StructuralSource::OwnedProbes,
        runners: by_runner
            .into_iter()
            .map(|(runner, results)| javascript_runner_declaration(runner, &results))
            .collect(),
        structural_limitations: manifest
            .limitations
            .iter()
            .map(|limitation| limitation.id.clone())
            .collect(),
    };
    entries.push(EvidenceArchiveEntry {
        path: "coverage-model.json".into(),
        contents: serde_json::to_vec(
            &PersistedCoverageModel::from_declaration(&javascript_coverage_model())
                .expect("JavaScript coverage model is contract-valid"),
        )
        .map_err(|error| error.to_string())?,
    });
    entries.push(EvidenceArchiveEntry {
        path: "frontend.json".into(),
        contents: serde_json::to_vec(&declaration).map_err(|error| error.to_string())?,
    });
    Ok(entries)
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
                Some("the run stopped before it finished".into()),
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

fn remove_derived_pnpm_config(environment: &mut BTreeMap<OsString, OsString>) {
    // `npx supercov` is itself launched by npm, which exports project `.npmrc`
    // keys as `npm_config_*`. pnpm-only keys then make every nested npm process
    // print a second set of "Unknown env config" warnings. They have no npm
    // semantics (npm 11 reports them as unknown), so do not leak those derived
    // environment aliases into the user's already-configured test command.
    const PNPM_ONLY_NPM_CONFIG: &[&str] = &[
        "npm_config_auto_install_peers",
        "npm_config_enable_pre_post_scripts",
        "npm_config_shamefully_hoist",
    ];
    environment.retain(|key, _| {
        let key = key.to_string_lossy().to_ascii_lowercase();
        !PNPM_ONLY_NPM_CONFIG.contains(&key.as_str())
    });
}

fn environment_with(values: BTreeMap<String, String>) -> Vec<(OsString, OsString)> {
    let mut environment = std::env::vars_os().collect::<BTreeMap<_, _>>();
    remove_derived_pnpm_config(&mut environment);
    for (key, value) in values {
        environment.retain(|existing, _| !existing.to_string_lossy().eq_ignore_ascii_case(&key));
        environment.insert(key.into(), value.into());
    }
    environment.into_iter().collect()
}

/// Where Playwright is told to keep the test files it compiles.
const PLAYWRIGHT_CACHE_VARIABLE: &str = "PWTEST_CACHE_DIR";

/// Beside the copies whose files it holds, and removed with them.
pub fn playwright_transform_cache(root: &Path) -> PathBuf {
    crate::workspace::workspace_container(root).join("caches/playwright-transform")
}

fn node_options(preload: &Path) -> String {
    // The register import must be ABSOLUTE: node resolves a relative
    // `--import` against each child process's OWN working directory, and
    // monorepo runners spawn tasks inside package directories. turbo running
    // `packages/react#build` resolved `.supercov/register.mjs` against
    // `packages/react/` and aborted every task with ERR_MODULE_NOT_FOUND.
    let preload = std::path::absolute(preload).unwrap_or_else(|_| preload.to_path_buf());
    // And it must be a URL, not a path: Node reads a bare `C:\...` as a URL
    // with scheme `c:` and refuses it, so on Windows the first Node child died
    // at startup and the suite reported only that its exit code was 1. The
    // JavaScript runtime already passes this import as a file URL; so does
    // this.
    [
        std::env::var("NODE_OPTIONS").ok(),
        Some("--enable-source-maps".into()),
        Some(format!("--import={}", crate::workspace::file_url(&preload))),
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
    source_roots: Option<&[String]>,
) -> Result<RunIntegrity, String> {
    let mut environment = std::env::vars().collect::<BTreeMap<_, _>>();
    // The roots the run was measured under, not this shell's. A run recorded
    // with the variable and queried from a shell without it discovered a
    // different scope and read as stale -- or, where automatic discovery found
    // nothing, stopped being checked at all.
    match source_roots.filter(|roots| !roots.is_empty()) {
        Some(roots) => {
            environment.insert(
                crate::source_discovery::SOURCE_ROOTS_VARIABLE.into(),
                roots.join(","),
            );
        }
        None => {
            environment.remove(crate::source_discovery::SOURCE_ROOTS_VARIABLE);
        }
    }
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
    let root = crate::workspace::canonicalize_simplified(&request.root)
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
    let source_roots = crate::source_discovery::configured_source_roots(&environment);
    let project = crate::project_discovery::discover_coverage_project_of_any_size(
        &root,
        &environment,
        &request.command,
    )
    .map_err(|error| error.to_string())?;
    // Browser tests may drive a server in another language -- Python, Ruby,
    // Go, Java/Kotlin, a Rust Worker -- whose source the probes measure in
    // the copy the suite runs in. Without one, a project
    // with no JavaScript source has nothing to measure.
    let browser_suite = project.playwright_config.is_some();
    let serves_python = browser_suite
        && crate::python_project::discover_python_files(&root)
            .is_ok_and(|files| !files.sources.is_empty());
    let serves_ruby = browser_suite
        && crate::ruby_project::discover_ruby_files(&root)
            .is_ok_and(|files| !files.sources.is_empty());
    let serves_go = browser_suite
        && (root.join("go.mod").is_file()
            || !crate::go_project::workspace_modules(&root).is_empty());
    let serves_rust_worker = browser_suite && crate::rust_worker::RustWorkerServer::present(&root);
    // A wasm library any JavaScript suite may load, not only a browser one.
    let serves_rust_wasm = crate::rust_wasm::RustWasmServer::present(&root);
    let serves_jvm = browser_suite
        && [
            "pom.xml",
            "build.gradle",
            "build.gradle.kts",
            "src/main/java",
            "src/main/kotlin",
        ]
        .iter()
        .any(|name| root.join(name).exists());
    if project.source_files.is_empty()
        && !serves_python
        && !serves_ruby
        && !serves_go
        && !serves_jvm
        && !serves_rust_worker
        && !serves_rust_wasm
    {
        return Err(
            crate::project_discovery::no_source_error(&root, &environment)
                .to_string()
                .into(),
        );
    }
    let integrity = javascript_integrity_for_project(&root, &project)?;
    let source_inputs = crate::source_capture::capture(
        &root,
        "javascript",
        crate::integrity::javascript_assertion_paths(&root, &project).map_err(|e| e.to_string())?,
    )?;
    // The root too, which a moved project must not reuse. And the files that
    // are instrumented: the fingerprint digests every first-party file
    // whatever its classification, so SUPERCOV_SOURCE_ROOTS set on a project
    // that had already run was silently ignored until the workspace was
    // deleted.
    let frontend_cache_key = format!(
        "{}:{}:{}:{}:{}",
        integrity.fingerprint.combined,
        integrity.fingerprint.execution,
        crate::source_manifest::digest(&request.command),
        crate::source_manifest::digest(&project.root.to_string_lossy()),
        crate::source_manifest::digest(&(&project.source_files, &source_roots))
    );
    let prior_workspace =
        cached_workspace_path(&root, cleanup.lock()).map_err(|error| error.to_string())?;
    let reusable_frontend = read_javascript_frontend_cache(&prior_workspace, &frontend_cache_key);
    let cached_paths = reusable_frontend
        .as_ref()
        .map(javascript_frontend_reuse_paths)
        .unwrap_or_default();
    let initialization_ms = elapsed_ms(initialization_started);
    // A server's plan is prepared while the workspace is copied and the
    // JavaScript instrumented: parsing a large Python tree took a third of a
    // second of every run, server or not.
    let prepare_servers = |workspace: PathBuf| {
        let (root, run_id, command) = (root.clone(), run_id.clone(), request.command.clone());
        move || -> Result<Vec<Server>, String> {
            let mut servers = Vec::new();
            if serves_python
                && let Some(server) =
                    crate::python_run::PythonServer::prepare(&root, &workspace, &run_id, &command)?
            {
                servers.push(Server::Python(server));
            }
            if serves_ruby
                && let Some(server) =
                    crate::ruby_run::RubyServer::prepare(&root, &workspace, &run_id, &command)?
            {
                servers.push(Server::Ruby(server));
            }
            Ok(servers)
        }
    };
    // Where the servers are prepared for: the copy as it was found, which a
    // run beside this one may take first.
    let prepared_for = crate::fault::or(
        "copy-moved",
        || prior_workspace.with_file_name("moved"),
        || prior_workspace.clone(),
    );
    let preparing_servers = std::thread::spawn(prepare_servers(prepared_for.clone()));

    let workspace_started = Instant::now();
    let workspace_progress = crate::progress::ProgressLine::start("preparing isolated workspace");
    let workspace = prepare_cached_workspace(&root, cleanup.lock(), &cached_paths)
        .map_err(|error| error.to_string())?;
    drop(workspace_progress);
    cleanup.set_workspace(workspace.clone());
    // The workspace this one replaced is in the trash now.
    crate::lifecycle::sweep_trash_in_background(&root);
    if let Err(error) = crate::workspace::mirror_git_repository(&root, &workspace)
        && std::env::var_os("SUPERCOV_DEBUG").is_some()
    {
        eprintln!("[supercov] the workspace has no git repository of its own: {error}");
    }
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
    let instrumentation_progress = crate::progress::ProgressLine::start("instrumenting sources");
    let collector_id = format!("collector-{}", integrity.fingerprint.execution);
    let frontend = if let Some(cache) = &reusable_frontend {
        load_cached_javascript_frontend(&workspace, cache)
    } else {
        prepare_javascript_frontend(
            &workspace,
            &project,
            &collector_id,
            &frontend_cache_key,
            &request.command,
        )
    }
    .map_err(|error| error.to_string())?;
    crate::workspace::hide_rewritten_files(
        &workspace,
        &crate::javascript_frontend::rewritten_files(&workspace),
    );
    drop(instrumentation_progress);
    let adapter_setup_ms = elapsed_ms(adapter_started);
    if let Some(detail) = crate::javascript_frontend::setup_timing_detail() {
        writeln!(diagnostics, "[supercov] {detail}").map_err(|error| error.to_string())?;
    }

    let mut servers = preparing_servers
        .join()
        .unwrap_or_else(|panic| std::panic::resume_unwind(panic))?;
    // Prepared for where the workspace was to be; one moved since is
    // prepared again for where it is.
    if workspace != prepared_for && !servers.is_empty() {
        servers = prepare_servers(workspace.clone())()?;
    }
    // Go, Java and Kotlin are rewritten in the copy, so they wait for it.
    if serves_go
        && let Some(server) =
            crate::go_run::GoServer::prepare(&root, &workspace, &run_id, &request.command)?
    {
        servers.push(Server::Go(server));
    }
    if serves_jvm
        && let Some(server) =
            crate::jvm_run::JvmServer::prepare(&root, &workspace, &run_id, &request.command)?
    {
        servers.push(Server::Jvm(server));
    }
    // A Rust Worker is instrumented in the copy too, and `wrangler dev`
    // builds it there when the suite starts it.
    if serves_rust_worker {
        servers.extend(
            crate::rust_worker::RustWorkerServer::prepare(
                &root,
                &workspace,
                &run_id,
                &request.command,
                diagnostics,
            )?
            .into_iter()
            .map(Server::RustWorker),
        );
    }
    if serves_rust_wasm {
        servers.extend(
            crate::rust_wasm::RustWasmServer::prepare(
                &root,
                &workspace,
                &workspace.join(".supercov/server-evidence"),
                &run_id,
                &request.command,
            )?
            .into_iter()
            .map(Server::RustWasm),
        );
    }
    if project.source_files.is_empty() && servers.is_empty() {
        return Err(
            crate::project_discovery::no_source_error(&root, &environment)
                .to_string()
                .into(),
        );
    }
    for server in &servers {
        writeln!(
            diagnostics,
            "[supercov] measuring {} {language} source file(s) in any {language} server the tests start",
            server.source_files(),
            language = server.language(),
        )
        .map_err(|error| error.to_string())?;
    }

    let evidence_relative = format!(".supercov/evidence/{run_id}");
    let evidence_directory = workspace.join(&evidence_relative);
    let server_evidence_root = workspace.join(".supercov/server-evidence");
    let diagnostic_owner = workspace.join(format!(".supercov/diagnostic-owner-{run_id}"));
    let mut overrides: BTreeMap<String, String> = BTreeMap::from([
        ("NODE_OPTIONS".into(), node_options(&frontend.preload_path)),
        ("SUPERCOV_CJS_INTERCEPT".into(), "1".into()),
        ("SUPERCOV_DIRECT_INSTRUMENTATION".into(), "1".into()),
        // tap measures its own coverage and fails a run short of 100%. Under
        // Supercov it measures the instrumented copies, whose probes are
        // branches the suite was never meant to cover: minimatch passed all
        // 6,236 tests and still failed. Its report stays; only the gate goes.
        ("TAP_ALLOW_INCOMPLETE_COVERAGE".into(), "1".into()),
        // Must be absolute: monorepo runners spawn test processes with a
        // package directory as cwd, and a relative evidence directory made
        // every per-test record land beside the package, never collected.
        (
            "SUPERCOV_EVIDENCE_DIR".into(),
            evidence_directory.display().to_string(),
        ),
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
    // Playwright keeps each test file it compiles in the system's temporary
    // directory, the code in one file and its source map in another, and
    // takes the code without looking for the map. macOS removes files there
    // that nothing has read for three days, and the two are not read alike:
    // the maps went, and every frame and test location of a suite came out
    // at the compiled code's lines (`app.spec.ts:50:1` for line 17, a code
    // frame of other tests' lines) until the code was three days unread too.
    // The copy's compiled files are kept with the copy instead. A cache
    // someone chose is theirs.
    if std::env::var_os(PLAYWRIGHT_CACHE_VARIABLE).is_none_or(|value| value.is_empty()) {
        overrides.insert(
            PLAYWRIGHT_CACHE_VARIABLE.into(),
            playwright_transform_cache(&root).display().to_string(),
        );
    }
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
        "./.supercov/node_modules/playwright.mjs".into(),
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
    for server in &servers {
        for (name, value) in server.environment() {
            // A server's preload joins the suite's own.
            let value = if name == "NODE_OPTIONS" {
                overrides
                    .get(&name)
                    .map_or_else(|| value.clone(), |options| format!("{options} {value}"))
            } else {
                value
            };
            overrides.insert(name, value);
        }
    }
    // `uv run` syncs the environment before it runs, and the copy's is the
    // project's own, linked in: a sync rewrote the project's editable install
    // to point at the copy, and regenerated a `hatch-vcs` version file that
    // then came back to the project as a change. The environment is used as
    // it is; a command that wants it synced syncs it first, in the project.
    if crate::workspace::links_a_virtual_environment(&workspace)
        && std::env::var_os("UV_NO_SYNC").is_none()
    {
        overrides.insert("UV_NO_SYNC".into(), "1".into());
    }
    // What each process a browser suite starts loads, reads and serves, so a
    // change to a mock server's files affects only the tests it served
    // (support_reach.rs).
    let support_directory = workspace.join(".supercov/support").join(&run_id);
    if project.playwright_config.is_some() {
        overrides.insert(
            crate::support_reach::DIRECTORY_VARIABLE.into(),
            support_directory.display().to_string(),
        );
    }
    // A tool that judges source as text, started without a package manager
    // or installed outside the project, is found through PATH.
    if let Some(path) = crate::source_tools::path_with_launchers(&workspace)
        && let Some(path) = crate::fault::or("launcher-path-text", || None, || path.to_str())
    {
        overrides.insert("PATH".into(), path.to_owned());
    }
    // A server built from the copy runs in a container nothing of the run
    // reaches: the suite's Docker clients reach the daemon through a relay
    // that hands it the run's settings (`start_docker_relay`).
    #[cfg(unix)]
    let docker_state = root.join(".supercov/work").join(&run_id).join("docker");
    #[cfg(unix)]
    // There is JavaScript source or a server here: a run with neither ended
    // above.
    let mut docker_proxy = start_docker_relay(
        &root,
        &workspace,
        &docker_state,
        &servers,
        &environment,
        &mut overrides,
        diagnostics,
    )?;
    // What bootstrap.cjs installs in a process that runs the workspace without
    // these settings: one started in a container or VM the workspace is
    // mounted into. It moves `root` to where it sees the workspace from.
    let settings = serde_json::json!({
        "root": workspace.display().to_string(),
        "environment": overrides
            .iter()
            .filter(|(name, _)| name.starts_with("SUPERCOV_"))
            .collect::<BTreeMap<_, _>>(),
    });
    let settings_path = frontend.preload_path.with_file_name("run.json");
    crate::fault_fs::write(
        &settings_path,
        serde_json::to_vec(&settings).map_err(|error| error.to_string())?,
    )
    .map_err(|error| format!("{}: {error}", settings_path.display()))?;
    // What the tools of a command before this one changed in the source has
    // been applied or let go; this command starts from the project as it is.
    let _ = crate::source_tools::clear_source_changes(&workspace);
    // The command, and nothing before it.
    let plan = ExecutionPlan {
        preparation: Vec::new(),
        test: ExecutionPhase {
            name: "test".into(),
            kind: PhaseKind::Test,
            command: CommandSpec {
                program: request.command[0].clone().into(),
                arguments: request.command[1..].iter().map(OsString::from).collect(),
                cwd: workspace.clone(),
                environment: Some(environment_with(overrides)),
                captured_output: None,
                console: None,
            },
        },
    };
    let options = supervision_options()?;
    let watchdog_program = request.watchdog_program.as_ref().ok_or_else(|| {
        DirectJavascriptRunError::Failed(
            "the JavaScript run is missing its crash-containment executable".into(),
        )
    })?;
    let supervisor = ProcessSupervisor::new_crash_safe(watchdog_program)
        .map_err(|error| DirectJavascriptRunError::Failed(error.to_string()))?;
    let output_baseline = std::cell::RefCell::new(None);
    let execution = match execute_plan_with_supervisor(
        &supervisor,
        &plan,
        options,
        diagnostics,
        |phase, diagnostics| {
            {
                // The snapshot boundary sits before the user's command, so
                // only the command's own effects flow back to the real
                // project afterwards.
                *output_baseline.borrow_mut() =
                    Some(workspace_output_baseline(&workspace).map_err(|error| {
                        OrchestrationError::PhaseSetup {
                            phase: phase.name.clone(),
                            reason: error.to_string(),
                        }
                    })?);
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
            }
            update_run_state(&root, &run_id, RunStateStatus::Testing, &started_at, None).map_err(
                |error| OrchestrationError::PhaseSetup {
                    phase: phase.name.clone(),
                    reason: error.to_string(),
                },
            )?;
            Ok(())
        },
    ) {
        Ok(execution) => execution,
        Err(error) => {
            let message = error.to_string();
            let state_updated = crate::fault::or(
                "failed-run-state",
                || false,
                || {
                    update_run_state(
                        &root,
                        &run_id,
                        RunStateStatus::Failed,
                        &started_at,
                        Some(message.clone()),
                    )
                    .is_ok()
                },
            );
            if state_updated {
                cleanup.mark_terminal();
            }
            return Err(message.into());
        }
    };
    // A server left running writes its last window when stopped.
    #[cfg(unix)]
    if let Some(proxy) = docker_proxy.take() {
        proxy.finish();
        for note in crate::docker_proxy::notes(&docker_state.join("containers.jsonl")) {
            writeln!(diagnostics, "[supercov] {note}").map_err(|error| error.to_string())?;
        }
    }
    let test_command_ms = execution
        .phases
        .iter()
        .find(|phase| phase.kind == PhaseKind::Test)
        .map_or(0.0, |phase| phase.duration_ms as f64);
    if let Some(signal) = execution.interrupted_signal {
        interrupt_run_state(&root, &run_id, &started_at, signal.name())
            .map_err(|error| error.to_string())?;
        cleanup.mark_terminal();
        return Err(DirectJavascriptRunError::Interrupted {
            signal,
            exit_code: execution.exit_code,
            timings: RunTimings {
                initialization_ms: rounded_millisecond(initialization_ms),
                workspace_preparation_ms: rounded_millisecond(workspace_preparation_ms),
                adapter_setup_ms: rounded_millisecond(adapter_setup_ms),
                instrumented_build_ms: 0.0,
                test_command_ms: rounded_millisecond(test_command_ms),
                evidence_publication_ms: 0.0,
            },
            total_ms: rounded_millisecond(elapsed_ms(total_started)),
        });
    }
    // Leave the real working tree as the command alone would have: updated
    // snapshots, generated fixtures, and reports belong in the repository,
    // not in the cache. Deletions and instrumented-source rewrites are
    // reported, never propagated.
    {
        // Taken as the first phase began, which a run that got here did.
        let baseline = &output_baseline
            .into_inner()
            .expect("the first phase took the output baseline");
        let protected = project
            .source_files
            .iter()
            .map(PathBuf::from)
            .collect::<std::collections::BTreeSet<_>>();
        let outputs = sync_command_outputs(&root, &workspace, baseline, &protected)
            .map_err(|error| error.to_string())?;
        if outputs.synced > 0 {
            let _ = writeln!(
                diagnostics,
                "[supercov] synced {} file(s) the command created or changed back to the project: {}",
                outputs.synced,
                crate::workspace::summarize_paths(&outputs.synced_paths)
            );
        }
        if !outputs.skipped_instrumented.is_empty() {
            let _ = writeln!(
                diagnostics,
                "[supercov] {} file(s) stayed in the isolated workspace: they are instrumented copies, or were built from them, and must not overwrite your project: {}",
                outputs.skipped_instrumented.len(),
                crate::workspace::summarize_paths(&outputs.skipped_instrumented)
            );
        }
        // A formatter or a fixer the command ran changed source files. The
        // tests ran the copy instrumented from the text before, so that is
        // what this run measured; the project gets the change, as it does
        // when the command runs alone.
        match crate::source_tools::apply_source_changes(&root, &workspace) {
            Ok(changes) => {
                if !changes.applied.is_empty() {
                    let _ = writeln!(
                        diagnostics,
                        "[supercov] the command's tools changed {} source file(s), now changed in the project too: {}; this run measured them as they were before",
                        changes.applied.len(),
                        crate::workspace::summarize_paths(&changes.applied)
                    );
                }
                if !changes.kept_back.is_empty() {
                    let _ = writeln!(
                        diagnostics,
                        "[supercov] the command's tools changed {} source file(s) that were edited in the project while it ran; those changes were not applied: {}",
                        changes.kept_back.len(),
                        crate::workspace::summarize_paths(&changes.kept_back)
                    );
                }
            }
            Err(error) => {
                let _ = writeln!(
                    diagnostics,
                    "[supercov] the source changes the command's tools made could not be applied to the project: {error}"
                );
            }
        }
        if !outputs.deleted_in_workspace.is_empty() {
            let _ = writeln!(
                diagnostics,
                "[supercov] the command deleted {} file(s) in the isolated workspace; deletions are not propagated to the project: {}",
                outputs.deleted_in_workspace.len(),
                crate::workspace::summarize_paths(&outputs.deleted_in_workspace)
            );
        }
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
    // Read before the evidence is archived and removed.
    let served = if servers.is_empty() && !support_directory.is_dir() {
        Vec::new()
    } else {
        crate::served_tests::read(&evidence_directory)
    };
    let support = crate::support_reach::read(&support_directory, &workspace, &served);
    if project.source_files.is_empty() {
        // Only the servers were measured: the first published is the run,
        // any other beside it.
        let metadata = RunMetadata {
            id: run_id.clone(),
            started_at: started_at.clone(),
            duration_ms: rounded_millisecond(elapsed_ms(total_started)),
            command: request.command.clone(),
            test_exit_code: Some(execution.exit_code),
            integrity: integrity.clone(),
            raw_evidence: RawEvidenceMetadata::default(),
            isolated_build: Some(true),
            instrumented_build_cache: None,
            timings: Some(RunTimings {
                initialization_ms: rounded_millisecond(initialization_ms),
                workspace_preparation_ms: rounded_millisecond(workspace_preparation_ms),
                adapter_setup_ms: rounded_millisecond(adapter_setup_ms),
                instrumented_build_ms: 0.0,
                test_command_ms: rounded_millisecond(test_command_ms),
                evidence_publication_ms: 0.0,
            }),
            merged: None,
            parents: None,
            source_roots: source_roots.clone(),
            copy: Some(cleanup.lock().copy()),
            merged_across: None,
            recorded_by: None,
            companions: None,
        };
        let mut published = None;
        let mut server_runs = Vec::new();
        let mut failure = None;
        for server in servers {
            let id = if published.is_none() {
                run_id.clone()
            } else {
                companion_run_id(&run_id, server_runs.len() + 1)
            };
            let language = server.language();
            let published_run = crate::fault::or(
                "server-unpublished",
                || Ok(None),
                || {
                    crate::fault::or(
                        "server-publish",
                        || Err("injected fault at server-publish".to_owned()),
                        || {
                            server.publish(
                                &root,
                                &run_id,
                                RunMetadata {
                                    id: id.clone(),
                                    ..metadata.clone()
                                },
                                &served,
                                diagnostics,
                            )
                        },
                    )
                },
            );
            match published_run {
                Ok(Some(run)) if published.is_none() => published = Some(run),
                Ok(Some((directory, _))) => {
                    finalize_published_run(&root, &id).map_err(|error| error.to_string())?;
                    server_runs.push((language.to_owned(), directory));
                }
                Ok(None) => {}
                Err(error) => {
                    let _ = writeln!(
                        diagnostics,
                        "[supercov] the {language} server's run could not be published: {error}"
                    );
                    failure.get_or_insert(error);
                }
            }
        }
        remove_stored_tree_deferred(&root, &workspace.join(".supercov/evidence"))
            .map_err(|error| error.to_string())?;
        remove_stored_tree_deferred(&root, &server_evidence_root)
            .map_err(|error| error.to_string())?;
        if published.is_none()
            && let Some(error) = failure
        {
            return Err(error.into());
        }
        let Some((run_directory, metadata)) = published else {
            let message = if execution.exit_code != 0 && served.is_empty() {
                format!(
                    "the test command exited {} before any test reported an outcome; its own output above says why, and no run was published",
                    execution.exit_code
                )
            } else {
                "the project has no JavaScript source, and no test's request reached measured server code: there was nothing to measure, and no run was published".to_owned()
            };
            let _ = update_run_state(
                &root,
                &run_id,
                RunStateStatus::Failed,
                &started_at,
                Some(message.clone()),
            );
            cleanup.mark_terminal();
            return Err(message.into());
        };
        update_run_state(
            &root,
            &run_id,
            if execution.exit_code == 0 {
                RunStateStatus::Complete
            } else {
                RunStateStatus::Failed
            },
            &started_at,
            None,
        )
        .map_err(|error| error.to_string())?;
        finalize_published_run(&root, &run_id).map_err(|error| error.to_string())?;
        cleanup.mark_terminal();
        return Ok(DirectJavascriptRunResult {
            run_id,
            run_directory,
            workspace,
            exit_code: execution.exit_code,
            assertion_calls: frontend.assertion_calls,
            recovered_runs,
            metadata,
            server_runs,
            unrun_build: project.unrun_build.clone(),
        });
    }
    let archive_path = root
        .join(".supercov/work")
        .join(&run_id)
        .join("evidence.raw.gz");
    let entries = collect_sources(&[
        EvidenceArchiveSource::File {
            file: frontend.manifest_path,
            path: "manifest.json".into(),
        },
        EvidenceArchiveSource::File {
            file: workspace.join(".supercov/statement-exclusions.json"),
            path: "statement-exclusions.json".into(),
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
    let mut entries =
        javascript_archive_entries(entries, &frontend.manifest, &run_id, execution.exit_code)?;
    if let Some(support) = &support {
        entries.push(EvidenceArchiveEntry {
            path: crate::support_reach::ARCHIVE_PATH.into(),
            contents: serde_json::to_vec(support).map_err(|error| error.to_string())?,
        });
    }
    // The files the run's servers loaded and read are recorded with the
    // rest, so a change to one -- a mock's `requests.json` as much as its
    // code -- is seen, and judged by the tests that reached it.
    let mut recorded_inputs = source_inputs.clone();
    if let Some(support) = &support
        && let Ok(reached) = crate::fault::or(
            "support-inputs",
            || Err("injected fault at support-inputs".to_owned()),
            || {
                crate::source_capture::capture(
                    &root,
                    "javascript",
                    support.files.keys().map(PathBuf::from),
                )
            },
        )
    {
        for (file, text) in reached.files {
            recorded_inputs.files.entry(file).or_insert(text);
        }
    }
    let entries = crate::source_capture::append(entries, &recorded_inputs)?;
    let raw = write_archive(entries, &archive_path).map_err(|error| error.to_string())?;
    remove_stored_tree_deferred(&root, &workspace.join(".supercov/evidence"))
        .map_err(|error| error.to_string())?;
    remove_stored_tree_deferred(&root, &server_evidence_root).map_err(|error| error.to_string())?;
    let evidence_publication_ms = elapsed_ms(publication_started);
    let timings = RunTimings {
        initialization_ms: rounded_millisecond(initialization_ms),
        workspace_preparation_ms: rounded_millisecond(workspace_preparation_ms),
        adapter_setup_ms: rounded_millisecond(adapter_setup_ms),
        instrumented_build_ms: 0.0,
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
        instrumented_build_cache: None,
        timings: Some(timings),
        merged: None,
        parents: None,
        source_roots: source_roots.clone(),
        copy: Some(cleanup.lock().copy()),
        merged_across: None,
        recorded_by: None,
        companions: None,
    };
    // Each server's run beside the browser run, under an id of its own that
    // sorts after it: `runs latest` stays the browser run. Published first,
    // so the browser run names the ones that were.
    let mut server_runs = Vec::new();
    let mut companions = Vec::new();
    for server in servers {
        let id = companion_run_id(&run_id, server_runs.len() + 1);
        let language = server.language();
        let server_metadata = RunMetadata {
            id: id.clone(),
            companions: Some(vec![crate::run_store::CompanionRun {
                id: run_id.clone(),
                language: "JavaScript".into(),
                role: "browser".into(),
            }]),
            ..metadata.clone()
        };
        match server.publish(&root, &run_id, server_metadata, &served, diagnostics) {
            Ok(Some((directory, _))) => {
                finalize_published_run(&root, &id).map_err(|error| error.to_string())?;
                server_runs.push((language.to_owned(), directory));
                companions.push(crate::run_store::CompanionRun {
                    id,
                    language: language.into(),
                    role: "server".into(),
                });
            }
            Ok(None) => {}
            // The browser run stands on its own.
            Err(error) => {
                let _ = writeln!(
                    diagnostics,
                    "[supercov] the {language} server's run could not be published: {error}"
                );
            }
        }
    }
    let metadata = RunMetadata {
        companions: (!companions.is_empty()).then_some(companions),
        ..metadata
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
        server_runs,
        unrun_build: project.unrun_build.clone(),
    })
}

/// The id of the `nth` run published beside `run_id` from the same command:
/// a public id `nth` below it when `run_id` is one, since runs that started
/// together are listed by id, highest first.
fn companion_run_id(run_id: &str, nth: usize) -> String {
    match run_id
        .strip_prefix("run_")
        .filter(|hex| hex.len() == 16)
        .and_then(|hex| u64::from_str_radix(hex, 16).ok())
    {
        Some(number) => format!("run_{:016x}", number.wrapping_sub(nth as u64)),
        None => format!("{run_id}-server-{nth}"),
    }
}

/// The relay the suite's Docker clients reach the daemon through
/// (docker_proxy.rs). It gives each container whose image holds the copy's
/// code the run's settings and Supercov's directory, mounted at its path: a
/// Go or Java server's windows and a Node server's evidence are written where
/// the run reads them. None when the daemon is not a local socket.
#[cfg(unix)]
fn start_docker_relay(
    root: &Path,
    workspace: &Path,
    docker_state: &Path,
    servers: &[Server],
    environment: &BTreeMap<String, String>,
    overrides: &mut BTreeMap<String, String>,
    diagnostics: &mut dyn std::io::Write,
) -> Result<Option<crate::docker_proxy::DockerProxy>, String> {
    let Some(upstream) = crate::docker_proxy::upstream_socket(environment) else {
        return Ok(None);
    };
    let mut names = overrides
        .keys()
        .filter(|name| name.starts_with("SUPERCOV_"))
        .cloned()
        .collect::<std::collections::BTreeSet<_>>();
    names.extend(
        servers
            .iter()
            .flat_map(|server| server.environment().into_keys()),
    );
    let settings = crate::docker_proxy::ProxySettings {
        copy: workspace.to_owned(),
        mount: root.join(".supercov"),
        environment: names
            .iter()
            .filter_map(|name| Some((name.clone(), overrides.get(name)?.clone())))
            .collect(),
        fingerprint: servers
            .iter()
            .map(Server::source_fingerprint)
            .collect::<Vec<_>>()
            .join(","),
        registry: root.join(".supercov/cache/docker-images.json"),
        state: docker_state.join("containers.jsonl"),
    };
    match crate::docker_proxy::socket_path(docker_state)
        .and_then(|socket| crate::docker_proxy::DockerProxy::start(&socket, &upstream, settings))
    {
        Ok(proxy) => {
            overrides.extend(proxy.environment(environment));
            Ok(Some(proxy))
        }
        Err(error) => {
            writeln!(
                diagnostics,
                "[supercov] containers the tests start are not measured: the Docker relay could not start ({error})"
            )
            .map_err(|error| error.to_string())?;
            Ok(None)
        }
    }
}

/// A server in another language a browser suite may start.
enum Server {
    Python(crate::python_run::PythonServer),
    Ruby(crate::ruby_run::RubyServer),
    Go(crate::go_run::GoServer),
    Jvm(crate::jvm_run::JvmServer),
    RustWorker(crate::rust_worker::RustWorkerServer),
    RustWasm(crate::rust_wasm::RustWasmServer),
}

impl Server {
    fn language(&self) -> &'static str {
        match self {
            Self::Python(_) => "Python",
            Self::Ruby(_) => "Ruby",
            Self::Go(_) => "Go",
            Self::Jvm(_) => "Java/Kotlin",
            Self::RustWorker(_) => "Rust Worker",
            Self::RustWasm(_) => "Rust wasm",
        }
    }

    fn source_files(&self) -> usize {
        match self {
            Self::Python(server) => server.source_files(),
            Self::Ruby(server) => server.source_files(),
            Self::Go(server) => server.source_files(),
            Self::Jvm(server) => server.source_files(),
            Self::RustWorker(server) => server.source_files(),
            Self::RustWasm(server) => server.source_files(),
        }
    }

    /// The source this server was instrumented from: an image built from an
    /// earlier copy holds probes placed in other source.
    #[cfg(unix)]
    fn source_fingerprint(&self) -> String {
        match self {
            Self::Python(server) => server.source_fingerprint(),
            Self::Ruby(server) => server.source_fingerprint(),
            Self::Go(server) => server.source_fingerprint(),
            Self::Jvm(server) => server.source_fingerprint(),
            Self::RustWorker(server) => server.source_fingerprint(),
            Self::RustWasm(server) => server.source_fingerprint(),
        }
    }

    fn environment(&self) -> BTreeMap<String, String> {
        match self {
            Self::Python(server) => server.environment(),
            Self::Ruby(server) => server.environment(),
            Self::Go(server) => server.environment(),
            Self::Jvm(server) => server.environment(),
            Self::RustWorker(server) => server.environment(),
            Self::RustWasm(server) => server.environment(),
        }
    }

    fn publish(
        self,
        root: &Path,
        evidence_run_id: &str,
        metadata: RunMetadata,
        served: &[crate::served_tests::ServedTest],
        diagnostics: &mut dyn std::io::Write,
    ) -> Result<Option<(PathBuf, RunMetadata)>, String> {
        match self {
            Self::Python(server) => {
                server.publish(root, evidence_run_id, metadata, served, diagnostics)
            }
            Self::Ruby(server) => {
                server.publish(root, evidence_run_id, metadata, served, diagnostics)
            }
            Self::Go(server) => {
                server.publish(root, evidence_run_id, metadata, served, diagnostics)
            }
            Self::Jvm(server) => {
                server.publish(root, evidence_run_id, metadata, served, diagnostics)
            }
            Self::RustWorker(server) => {
                server.publish(root, evidence_run_id, metadata, served, diagnostics)
            }
            Self::RustWasm(server) => {
                server.publish(root, evidence_run_id, metadata, served, diagnostics)
            }
        }
    }
}

#[cfg(test)]
mod environment_tests {
    use super::*;

    // A server's run published beside a browser run sorts after it, so
    // `runs latest` stays the browser run; and is the same each time.
    #[test]
    fn a_companion_run_id_is_public_and_sorts_after_its_run() {
        for run in [
            "run_0123456789abcdef",
            "run_ffffffffffffffff",
            "run_8000000000000000",
        ] {
            let companion = companion_run_id(run, 1);
            assert_eq!(companion.len(), 20);
            assert!(companion.starts_with("run_"));
            assert!(companion.as_str() < run, "{companion} >= {run}");
            assert_eq!(companion, companion_run_id(run, 1));
            assert!(companion_run_id(run, 2) < companion);
        }
        assert_eq!(companion_run_id("rust-1", 1), "rust-1-server-1");
    }

    fn result(runner: &str, runtime: serde_json::Value) -> RawTestResult {
        serde_json::from_value(serde_json::json!({
            "testId": "t1",
            "test": "adds",
            "retry": 0,
            "provenance": { "runner": runner, "kind": "unit", "source": "runner" },
            "runtime": runtime,
        }))
        .unwrap()
    }

    fn manifest() -> crate::javascript_frontend::JavascriptManifest {
        serde_json::from_value(serde_json::json!({
            "decisions": [], "points": [], "branches": [], "limitations": [],
            "scope": { "version": 1, "mode": "automatic", "roots": [], "entries": [] }
        }))
        .unwrap()
    }

    // A worker matters only to a result that observed something in it.
    #[test]
    fn a_result_that_observed_nothing_needs_no_worker() {
        let quiet = result("vitest", serde_json::json!([{}]));
        let declaration = javascript_runner_declaration("vitest".into(), &[&quiet]);
        assert_eq!(
            declaration.execution_model,
            ExecutionModel::ParallelContextPropagated
        );
        let event = result(
            "vitest",
            serde_json::json!([{ "events": [{
                "type": "hit", "id": "p1", "timestampMs": 1, "environment": "node"
            }] }]),
        );
        let declaration = javascript_runner_declaration("vitest".into(), &[&event]);
        assert_eq!(
            declaration.execution_model,
            ExecutionModel::ParallelUnattributed
        );
    }

    #[test]
    fn results_are_read_from_every_place_a_runner_writes_them() {
        let encoded = |runner| serde_json::to_vec(&result(runner, serde_json::json!([]))).unwrap();
        let mut lines = encoded("jest");
        lines.push(b'\n');
        let entries = vec![
            EvidenceArchiveEntry {
                path: "mcdc.json".into(),
                contents: encoded("node:test"),
            },
            EvidenceArchiveEntry {
                path: "mcdc.jsonl".into(),
                contents: lines,
            },
            // Not a record of server work.
            EvidenceArchiveEntry {
                path: "server/background/notes.txt".into(),
                contents: b"x".to_vec(),
            },
        ];
        let entries = javascript_archive_entries(entries, &manifest(), "run_1", 0).unwrap();
        let declaration: supercov_contracts::FrontendRunDeclaration = serde_json::from_slice(
            &entries
                .iter()
                .find(|entry| entry.path == "frontend.json")
                .unwrap()
                .contents,
        )
        .unwrap();
        assert_eq!(
            declaration
                .runners
                .iter()
                .map(|runner| runner.runner.as_str())
                .collect::<Vec<_>>(),
            ["jest", "node:test"]
        );
    }

    #[test]
    fn nested_npm_drops_only_pnpm_derived_environment_aliases() {
        let mut environment = BTreeMap::from([
            ("npm_config_auto_install_peers".into(), "true".into()),
            ("NPM_CONFIG_SHAMEFULLY_HOIST".into(), "true".into()),
            (
                "npm_config_registry".into(),
                "https://registry.npmjs.org".into(),
            ),
            ("USER_VALUE".into(), "kept".into()),
        ]);
        remove_derived_pnpm_config(&mut environment);
        assert_eq!(
            environment,
            BTreeMap::from([
                ("USER_VALUE".into(), "kept".into()),
                (
                    "npm_config_registry".into(),
                    "https://registry.npmjs.org".into()
                ),
            ])
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coverage_report::{ExecutionScope, RuntimeSnapshot, TestProvenance};

    /// A run that stops between writing its state and finishing is marked
    /// failed, in words about the run it was, and releases its lock.
    #[test]
    fn a_run_that_stops_early_is_marked_failed_as_itself() {
        let root = std::env::temp_dir().join(format!(
            "supercov-js-cleanup-{}-{}",
            std::process::id(),
            now_nonce()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let root = crate::test_directory::TestDirectory::adopt(root);
        let started_at = "2026-01-01T00:00:00.000Z".to_owned();
        let lock = ProjectLock::acquire(&root, "run-early", &started_at).unwrap();
        write_run_state(
            &root,
            &RunState {
                id: "run-early".into(),
                pid: std::process::id(),
                root: root.display().to_string(),
                workspace: String::new(),
                started_at: started_at.clone(),
                updated_at: started_at.clone(),
                status: RunStateStatus::Preparing,
                signal: None,
                error: None,
            },
        )
        .unwrap();
        let mut cleanup = RunCleanup {
            root: root.to_path_buf(),
            run_id: "run-early".into(),
            started_at,
            lock,
            workspace: None,
            state_written: false,
            terminal_recorded: false,
        };
        cleanup.mark_state_written();
        drop(cleanup);
        let state: serde_json::Value = serde_json::from_slice(
            &std::fs::read(root.join(".supercov/work/run-early/state.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(state["error"], "the run stopped before it finished");
        assert_ne!(state["status"], serde_json::json!("preparing"));
        // The lock went with it: another run can take the store.
        ProjectLock::acquire(&root, "run-next", "2026-01-01T00:00:01.000Z")
            .unwrap()
            .release()
            .unwrap();
    }

    fn result(
        test_id: Option<&str>,
        retry: Option<usize>,
        scope: Option<(&str, &str, usize)>,
        observed: bool,
    ) -> RawTestResult {
        RawTestResult {
            span: None,
            test_id: test_id.map(str::to_owned),
            scope: scope.map(|(worker, test, retry)| ExecutionScope {
                version: 1,
                run_id: "run".into(),
                worker_id: worker.into(),
                test_id: test.into(),
                test_key: test.into(),
                retry,
                attempt_id: format!("{test}:{retry}"),
            }),
            test: "t".into(),
            test_file: None,
            title: None,
            retry,
            status: Some("passed".into()),
            expected_status: None,
            flaky: false,
            provenance: TestProvenance {
                runner: "vitest".into(),
                kind: "unit".into(),
                project: None,
                source: "runner-default".into(),
                tags: Vec::new(),
            },
            role: "test".into(),
            attribution: crate::coverage_report::ATTRIBUTION_EXACT.into(),
            phases: vec![],
            runtime: if observed {
                vec![RuntimeSnapshot {
                    decisions: vec![],
                    hits: vec!["h".into()],
                    events: vec![],
                    logicals: vec![],
                    phase_id: None,
                }]
            } else {
                vec![]
            },
            browser: vec![],
            server: vec![],
        }
    }

    /// A runner is exact only where every result names its test, worker
    /// and retry the way its scope does; any one missing or disagreeing
    /// makes it unattributed, with a limitation for each kind.
    #[test]
    fn a_runner_is_exact_only_when_every_result_says_who_ran_it() {
        let exact = result(Some("a"), Some(0), Some(("w1", "a", 0)), true);
        let declared = |results: &[RawTestResult]| {
            javascript_runner_declaration("vitest".into(), &results.iter().collect::<Vec<_>>())
        };
        let good = declared(std::slice::from_ref(&exact));
        assert_eq!(
            good.execution_model,
            ExecutionModel::ParallelContextPropagated
        );
        assert!(good.limitations.is_empty());
        // Nothing observed needs no worker.
        assert_eq!(
            declared(&[result(Some("a"), Some(0), None, false)]).execution_model,
            ExecutionModel::ParallelContextPropagated
        );
        for (case, bad) in [
            (
                "no test id",
                result(None, Some(0), Some(("w1", "a", 0)), true),
            ),
            (
                "empty test id",
                result(Some(""), Some(0), Some(("w1", "a", 0)), true),
            ),
            (
                "test id disagrees",
                result(Some("a"), Some(0), Some(("w1", "b", 0)), true),
            ),
            (
                "no worker",
                result(Some("a"), Some(0), Some(("", "a", 0)), true),
            ),
            ("observed, no scope", result(Some("a"), Some(0), None, true)),
            (
                "no retry",
                result(Some("a"), None, Some(("w1", "a", 0)), true),
            ),
            (
                "retry disagrees",
                result(Some("a"), Some(1), Some(("w1", "a", 0)), true),
            ),
        ] {
            let declaration = declared(&[exact.clone(), bad]);
            assert_eq!(
                declaration.execution_model,
                ExecutionModel::ParallelUnattributed,
                "{case}"
            );
            assert_eq!(declaration.limitations.len(), 6, "{case}");
            assert_eq!(declaration.limitations[0].id, "vitest-no-worker");
        }
        assert_eq!(
            declared(&[]).execution_model,
            ExecutionModel::ParallelUnattributed
        );
    }

    /// A JavaScript run's archive is read result by result: malformed files
    /// are named, a run that recorded nothing gets the command's own result,
    /// background server records get a runner of their own, and the
    /// declaration and model are added.
    #[test]
    fn javascript_archive_entries_are_read_or_refused_by_name() {
        let manifest: crate::javascript_frontend::JavascriptManifest =
            serde_json::from_value(serde_json::json!({
                "decisions": [], "points": [], "branches": [], "limitations": [],
                "scope": {"version": 1, "mode": "automatic", "roots": [], "entries": []}
            }))
            .unwrap();
        let entry = |path: &str, contents: &[u8]| EvidenceArchiveEntry {
            path: path.into(),
            contents: contents.to_vec(),
        };
        let read = |entries: Vec<EvidenceArchiveEntry>, exit: i32| {
            javascript_archive_entries(entries, &manifest, "run", exit)
        };
        let good =
            serde_json::to_vec(&result(Some("a"), Some(0), Some(("w1", "a", 0)), true)).unwrap();
        for (entries, said) in [
            (
                vec![entry("results/a/mcdc.json", b"{")],
                "invalid results/a/mcdc.json",
            ),
            (
                vec![entry("w.mcdc.jsonl", &good)],
                "does not end with a newline",
            ),
            (
                vec![entry(
                    "w.mcdc.jsonl",
                    &[good.as_slice(), b"\n\n", good.as_slice(), b"\n"].concat(),
                )],
                "blank record at line 2",
            ),
            (
                vec![entry("w.mcdc.jsonl", b"{}\n")],
                "invalid w.mcdc.jsonl record at line 1",
            ),
        ] {
            let error = read(entries, 0).unwrap_err();
            assert!(error.contains(said), "{said}: {error}");
        }
        let declaration = |entries: &[EvidenceArchiveEntry]| -> serde_json::Value {
            serde_json::from_slice(
                &entries
                    .iter()
                    .find(|e| e.path == "frontend.json")
                    .unwrap()
                    .contents,
            )
            .unwrap()
        };
        // Nothing recorded: the command's own result, failed with its exit.
        let empty = read(vec![], 3).unwrap();
        let command = empty
            .iter()
            .find(|e| e.path == "results/command/mcdc.json")
            .unwrap();
        assert!(String::from_utf8_lossy(&command.contents).contains("\"status\":\"failed\""));
        assert!(empty.iter().any(|e| e.path == "coverage-model.json"));
        let timed_out = read(vec![], supercov_contracts::COMMAND_TIMEOUT_EXIT_CODE).unwrap();
        assert!(
            String::from_utf8_lossy(
                &timed_out
                    .iter()
                    .find(|e| e.path == "results/command/mcdc.json")
                    .unwrap()
                    .contents
            )
            .contains("timedOut")
        );
        let with_server = read(
            vec![
                entry("w.mcdc.jsonl", &[good.as_slice(), b"\n"].concat()),
                entry("server/background/p1.jsonl", b"{}\n"),
            ],
            0,
        )
        .unwrap();
        let runners = declaration(&with_server)["runners"]
            .as_array()
            .unwrap()
            .iter()
            .map(|runner| runner["runner"].as_str().unwrap().to_owned())
            .collect::<Vec<_>>();
        assert_eq!(runners, ["background", "vitest"]);
    }
}
