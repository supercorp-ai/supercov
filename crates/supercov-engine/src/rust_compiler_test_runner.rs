//! Private process-per-test execution for compiler-instrumented Rust artifacts.
//!
//! The compiler frontend freezes the complete denominator before this module
//! launches anything. Every libtest attempt receives its own authenticated,
//! bounded mmap transport and deterministic context. Context-zero records are
//! retained as background results and are never credited to the test.

use std::{
    collections::BTreeSet,
    fs,
    io::Write,
    path::{Component, Path, PathBuf},
    process::{Command, Output},
    sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Instant, SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use supercov_contracts::{
    AttributionPrecision, ExecutionModel, FrontendAttribution, FrontendLimitation,
    FrontendLimitationScope, FrontendRunDeclaration, FrontendRunnerDeclaration,
    LANGUAGE_FRONTEND_PROTOCOL_VERSION, StructuralSource,
};

use crate::{
    coverage_report::{
        CoverageModelDeclaration, CoveragePhase, CoverageReportRequest, ExecutionScope,
        ExitCodeInput, PersistedCoverageModel, RawTestResult, RuntimeSnapshot, TestProvenance,
    },
    evidence_archive::EvidenceArchiveEntry,
    rust_compiler_evidence::{
        RustCompilerEvidenceProjection, RustCompilerTransportHealth, project_rust_compiler_evidence,
    },
    rust_compiler_orchestration::{
        RustCompilerBuild, RustCompilerBuildRequest, RustCompilerTestArtifact,
        build_with_rust_compiler_companion,
    },
    rust_probe_transport::{
        DEFAULT_DESCRIPTOR_CAPACITY, DEFAULT_PAYLOAD_CAPACITY, RUST_CONTEXT_ENV,
        RUST_TRANSPORT_ENV, RUST_TRANSPORT_TOKEN_ENV, RustTransportRead, create_rust_transport,
        read_rust_transport,
    },
    rust_test_context::preflight_rust_test_contexts,
};

const TOKEN_BYTES: usize = supercov_contracts::RUST_PROBE_TRANSPORT_TOKEN_SIZE;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RustCompilerRunRequest {
    pub project_root: PathBuf,
    pub command: Vec<String>,
    pub run_id: String,
    pub generated_at: String,
    pub wrapper_path: PathBuf,
    pub companion_candidates: Vec<PathBuf>,
    pub require_public_capabilities: bool,
}

impl RustCompilerRunRequest {
    fn build_request(&self) -> RustCompilerBuildRequest {
        RustCompilerBuildRequest {
            project_root: self.project_root.clone(),
            command: self.command.clone(),
            run_id: self.run_id.clone(),
            wrapper_path: self.wrapper_path.clone(),
            companion_candidates: self.companion_candidates.clone(),
            require_public_capabilities: self.require_public_capabilities,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RustCompilerAttemptHealth {
    pub test_id: String,
    pub status: String,
    pub transport: RustCompilerTransportHealth,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RustCompilerFrontendRun {
    pub selection: crate::rust_compiler_selection::SelectedRustCompilerCompanion,
    pub declaration: FrontendRunDeclaration,
    pub request: CoverageReportRequest,
    pub exit_code: i32,
    pub artifacts: usize,
    pub artifact_files: Vec<PathBuf>,
    pub attempt_health: Vec<RustCompilerAttemptHealth>,
    pub build_ms: f64,
    pub execution_ms: f64,
}

impl RustCompilerFrontendRun {
    pub fn archive_entries(&self) -> Result<Vec<EvidenceArchiveEntry>, serde_json::Error> {
        let model = PersistedCoverageModel::from_declaration(
            self.request
                .coverage_model
                .as_ref()
                .expect("Rust compiler frontend always declares a coverage model"),
        )
        .expect("Rust compiler coverage model is contract-valid");
        let mut entries = vec![
            EvidenceArchiveEntry {
                path: "coverage-model.json".into(),
                contents: serde_json::to_vec(&model)?,
            },
            EvidenceArchiveEntry {
                path: "frontend.json".into(),
                contents: serde_json::to_vec(&self.declaration)?,
            },
            EvidenceArchiveEntry {
                path: "manifest.json".into(),
                contents: serde_json::to_vec(&self.request.manifest)?,
            },
        ];
        for (index, result) in self.request.raw_results.iter().enumerate() {
            entries.push(EvidenceArchiveEntry {
                path: format!("results/{index:08}/mcdc.json"),
                contents: serde_json::to_vec(result)?,
            });
        }
        entries.push(EvidenceArchiveEntry {
            path: "rust/transport-health.json".into(),
            contents: serde_json::to_vec(&self.attempt_health)?,
        });
        Ok(entries)
    }
}

#[derive(Debug)]
pub enum RustCompilerTestError {
    Build(String),
    Io { path: PathBuf, reason: String },
    UnsafeArtifact(String),
    List { artifact: PathBuf, reason: String },
    Context(String),
    DuplicateTest(String),
    Random(String),
    Launch { test: String, reason: String },
    Transport { test: String, reason: String },
    DroppedEvidence { test: String, dropped: u64 },
    Projection { test: String, reason: String },
}

impl std::fmt::Display for RustCompilerTestError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Build(reason) => write!(formatter, "Rust compiler build failed: {reason}"),
            Self::Io { path, reason } => write!(formatter, "{}: {reason}", path.display()),
            Self::UnsafeArtifact(reason) => {
                write!(formatter, "unsafe Rust test artifact: {reason}")
            }
            Self::List { artifact, reason } => write!(
                formatter,
                "could not enumerate tests in {}: {reason}",
                artifact.display()
            ),
            Self::Context(reason) => write!(formatter, "invalid Rust test context: {reason}"),
            Self::DuplicateTest(test) => write!(formatter, "duplicate Rust test identity: {test}"),
            Self::Random(reason) => {
                write!(formatter, "could not authenticate Rust evidence: {reason}")
            }
            Self::Launch { test, reason } => {
                write!(formatter, "could not launch Rust test {test}: {reason}")
            }
            Self::Transport { test, reason } => {
                write!(formatter, "invalid Rust transport for {test}: {reason}")
            }
            Self::DroppedEvidence { test, dropped } => write!(
                formatter,
                "Rust transport dropped {dropped} record(s) for {test}; refusing partial coverage"
            ),
            Self::Projection { test, reason } => {
                write!(formatter, "invalid Rust evidence for {test}: {reason}")
            }
        }
    }
}

impl std::error::Error for RustCompilerTestError {}

fn io_error(path: &Path, error: impl std::fmt::Display) -> RustCompilerTestError {
    RustCompilerTestError::Io {
        path: path.to_path_buf(),
        reason: error.to_string(),
    }
}

#[derive(Debug, Clone)]
struct TestArtifact {
    executable: PathBuf,
    target_name: String,
    kind: String,
    source: String,
}

#[derive(Debug, Clone)]
struct ProcessTask {
    ordinal: usize,
    artifact_index: usize,
    test_index: usize,
    artifact: TestArtifact,
    test: String,
    test_id: String,
    context_id: u64,
    transport: PathBuf,
}

#[derive(Debug)]
struct ProcessOutcome {
    task: ProcessTask,
    output: Output,
    read: RustTransportRead,
    started_at_ms: i64,
    ended_at_ms: i64,
}

fn epoch_ms() -> Result<i64, RustCompilerTestError> {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| RustCompilerTestError::Random(error.to_string()))?
        .as_millis();
    i64::try_from(millis).map_err(|error| RustCompilerTestError::Random(error.to_string()))
}

fn relative_source(root: &Path, source: &Path) -> Result<String, RustCompilerTestError> {
    let source = fs::canonicalize(source).map_err(|error| io_error(source, error))?;
    let relative = source
        .strip_prefix(root)
        .map_err(|_| RustCompilerTestError::UnsafeArtifact(source.display().to_string()))?;
    if relative.as_os_str().is_empty()
        || relative
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(RustCompilerTestError::UnsafeArtifact(
            source.display().to_string(),
        ));
    }
    Ok(relative.to_string_lossy().replace('\\', "/"))
}

fn normalize_artifacts(
    project_root: &Path,
    artifacts: &[RustCompilerTestArtifact],
) -> Result<Vec<TestArtifact>, RustCompilerTestError> {
    artifacts
        .iter()
        .map(|artifact| {
            Ok(TestArtifact {
                executable: artifact.executable.clone(),
                target_name: artifact.target_name.clone(),
                kind: if artifact.target_kinds.iter().any(|kind| kind == "test") {
                    "integration".into()
                } else {
                    "unit".into()
                },
                source: relative_source(project_root, &artifact.source_path)?,
            })
        })
        .collect()
}

fn list_tests(artifact: &TestArtifact) -> Result<Vec<String>, RustCompilerTestError> {
    let output = Command::new(&artifact.executable)
        .args(["--list", "--format", "terse"])
        .output()
        .map_err(|error| RustCompilerTestError::List {
            artifact: artifact.executable.clone(),
            reason: error.to_string(),
        })?;
    if !output.status.success() {
        return Err(RustCompilerTestError::List {
            artifact: artifact.executable.clone(),
            reason: format!(
                "{}{}",
                String::from_utf8_lossy(&output.stderr),
                String::from_utf8_lossy(&output.stdout)
            )
            .trim()
            .to_owned(),
        });
    }
    let mut tests = String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| line.strip_suffix(": test"))
        .map(str::to_owned)
        .collect::<Vec<_>>();
    tests.sort();
    tests.dedup();
    Ok(tests)
}

fn token_hex(token: &[u8; TOKEN_BYTES]) -> String {
    token.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn phase_id(run_id: &str, attempt_id: &str) -> String {
    let mut digest = Sha256::new();
    digest.update((run_id.len() as u64).to_be_bytes());
    digest.update(run_id.as_bytes());
    digest.update((attempt_id.len() as u64).to_be_bytes());
    digest.update(attempt_id.as_bytes());
    let hex = format!("{:x}", digest.finalize());
    format!("rust-test:{}", &hex[..40])
}

fn snapshot_has_evidence(snapshot: &RuntimeSnapshot) -> bool {
    !snapshot.hits.is_empty() || !snapshot.decisions.is_empty() || !snapshot.events.is_empty()
}

fn run_process(project_root: &Path, task: &ProcessTask) -> Result<ProcessOutcome, String> {
    let mut token = [0_u8; TOKEN_BYTES];
    getrandom::fill(&mut token).map_err(|error| error.to_string())?;
    create_rust_transport(
        &task.transport,
        token,
        DEFAULT_DESCRIPTOR_CAPACITY,
        DEFAULT_PAYLOAD_CAPACITY,
    )
    .map_err(|error| error.to_string())?;
    let started_at_ms = epoch_ms().map_err(|error| error.to_string())?;
    let output = Command::new(&task.artifact.executable)
        .args(["--exact", &task.test, "--nocapture"])
        .current_dir(project_root)
        .env(RUST_TRANSPORT_ENV, &task.transport)
        .env(RUST_TRANSPORT_TOKEN_ENV, token_hex(&token))
        .env(RUST_CONTEXT_ENV, format!("{:016x}", task.context_id))
        .output()
        .map_err(|error| error.to_string())?;
    let ended_at_ms = epoch_ms().map_err(|error| error.to_string())?;
    let read = read_rust_transport(&task.transport, &token).map_err(|error| error.to_string())?;
    fs::remove_file(&task.transport).map_err(|error| error.to_string())?;
    Ok(ProcessOutcome {
        task: task.clone(),
        output,
        read,
        started_at_ms,
        ended_at_ms,
    })
}

fn rust_compiler_coverage_model() -> CoverageModelDeclaration {
    CoverageModelDeclaration {
        language: "rust".into(),
        variant: "rustc-mir-owned-v1".into(),
        name: "supercov-rust-compiler-v1".into(),
        completeness_meaning: "Every compiler-derived obligation in the frozen owned-source denominator was observed; explicit compiler limitations identify Rust surfaces not yet measured.".into(),
        measured: vec![
            "compiler-derived owned Rust statement and function-entry obligations".into(),
            "compiler-derived control-flow alternatives and atomic decision vectors".into(),
            "macro-expanded and generated owned code with exact compiler provenance".into(),
            "exact process-per-libtest attribution".into(),
            "exact assertion-phase attribution for supported assertion macros".into(),
        ],
        not_measured: vec![
            "capabilities explicitly listed in the compiler manifest limitations".into(),
            "all input values, semantic partitions, paths, or concurrency interleavings".into(),
            "mutation score or assertion fault-detection strength".into(),
        ],
    }
}

fn runner_declaration() -> FrontendRunnerDeclaration {
    FrontendRunnerDeclaration {
        runner: "rust-libtest".into(),
        execution_model: ExecutionModel::ProcessPerTest,
        attribution: FrontendAttribution {
            run: AttributionPrecision::Exact,
            worker: AttributionPrecision::Exact,
            test: AttributionPrecision::Exact,
            retry: AttributionPrecision::Exact,
            phase: AttributionPrecision::Exact,
            action: AttributionPrecision::Unavailable,
            assertion: AttributionPrecision::Exact,
        },
        limitations: vec![FrontendLimitation {
            id: "rust-action-linkage-unavailable".into(),
            scopes: vec![FrontendLimitationScope::Action],
            reason: "Rust libtest exposes no general application-action lifecycle".into(),
        }],
    }
}

fn status(output: &Output) -> (&'static str, i32) {
    let exit = output.status.code().unwrap_or(1);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let skipped =
        exit == 0 && (stdout.contains("running 0 tests") || stdout.contains("; 1 ignored;"));
    (
        if exit != 0 {
            "failed"
        } else if skipped {
            "skipped"
        } else {
            "passed"
        },
        exit,
    )
}

fn raw_result(
    run_id: &str,
    task: &ProcessTask,
    status: &str,
    base_phase: CoveragePhase,
    projection: RustCompilerEvidenceProjection,
) -> (
    RawTestResult,
    Option<RawTestResult>,
    RustCompilerAttemptHealth,
) {
    let worker_id = format!("artifact-{:04}", task.artifact_index);
    let attempt_id = format!("{run_id}:{:04}:{:08}", task.artifact_index, task.test_index);
    let scope = ExecutionScope {
        version: 1,
        run_id: run_id.into(),
        worker_id: worker_id.clone(),
        test_id: task.test_id.clone(),
        test_key: task.test_id.clone(),
        retry: 0,
        attempt_id: attempt_id.clone(),
    };
    let mut phases = vec![base_phase];
    phases.extend(projection.assertion_phases);
    let result = RawTestResult {
        test_id: Some(task.test_id.clone()),
        scope: Some(scope),
        test: task.test_id.clone(),
        test_file: Some(task.artifact.source.clone()),
        title: Some(task.test.clone()),
        retry: Some(0),
        status: Some(status.into()),
        expected_status: Some("passed".into()),
        flaky: false,
        provenance: TestProvenance {
            runner: "rust-libtest".into(),
            kind: task.artifact.kind.clone(),
            project: Some(task.artifact.target_name.clone()),
            source: "supercov-rustc-process-per-test".into(),
        },
        role: "test".into(),
        phases,
        runtime: vec![projection.attributed],
        browser: Vec::new(),
        server: Vec::new(),
    };
    let background = snapshot_has_evidence(&projection.background).then(|| {
        let background_id = format!("background:{attempt_id}");
        RawTestResult {
            test_id: Some(background_id.clone()),
            scope: Some(ExecutionScope {
                version: 1,
                run_id: run_id.into(),
                worker_id,
                test_id: background_id.clone(),
                test_key: background_id.clone(),
                retry: 0,
                attempt_id: format!("{attempt_id}:background"),
            }),
            test: background_id,
            test_file: Some(task.artifact.source.clone()),
            title: Some(format!("Background while running {}", task.test)),
            retry: Some(0),
            status: Some(status.into()),
            expected_status: Some("passed".into()),
            flaky: false,
            provenance: TestProvenance {
                runner: "rust-libtest".into(),
                kind: task.artifact.kind.clone(),
                project: Some(task.artifact.target_name.clone()),
                source: "supercov-rustc-context-zero".into(),
            },
            role: "background".into(),
            phases: Vec::new(),
            runtime: vec![projection.background],
            browser: Vec::new(),
            server: Vec::new(),
        }
    });
    let health = RustCompilerAttemptHealth {
        test_id: task.test_id.clone(),
        status: status.into(),
        transport: projection.health,
    };
    (result, background, health)
}

pub fn run_rust_compiler_frontend(
    request: &RustCompilerRunRequest,
    diagnostics: &mut dyn Write,
) -> Result<RustCompilerFrontendRun, RustCompilerTestError> {
    let build = build_with_rust_compiler_companion(&request.build_request())
        .map_err(|error| RustCompilerTestError::Build(error.to_string()))?;
    execute_compiler_build(request, build, diagnostics)
}

fn execute_compiler_build(
    request: &RustCompilerRunRequest,
    build: RustCompilerBuild,
    diagnostics: &mut dyn Write,
) -> Result<RustCompilerFrontendRun, RustCompilerTestError> {
    let project_root = fs::canonicalize(&request.project_root)
        .map_err(|error| io_error(&request.project_root, error))?;
    let artifacts = normalize_artifacts(&project_root, &build.artifacts)?;
    let evidence_root = build.compiler_output_directory.join("attempts");
    fs::create_dir(&evidence_root).map_err(|error| io_error(&evidence_root, error))?;
    let mut tasks = Vec::new();
    let mut identities = BTreeSet::new();
    for (artifact_index, artifact) in artifacts.iter().enumerate() {
        let tests = list_tests(artifact)?;
        let contexts = preflight_rust_test_contexts(tests.clone())
            .map_err(|error| RustCompilerTestError::Context(error.to_string()))?;
        for (test_index, test) in tests.into_iter().enumerate() {
            let test_id = format!("{}::{test}", artifact.source);
            if !identities.insert(test_id.clone()) {
                return Err(RustCompilerTestError::DuplicateTest(test_id));
            }
            tasks.push(ProcessTask {
                ordinal: tasks.len(),
                artifact_index,
                test_index,
                artifact: artifact.clone(),
                test: test.clone(),
                test_id,
                context_id: contexts[&test],
                transport: evidence_root.join(format!("{artifact_index:04}-{test_index:08}.mmap")),
            });
        }
    }
    if tasks.is_empty() {
        return Err(RustCompilerTestError::Context(
            "Cargo produced no enumerable libtest tests".into(),
        ));
    }

    let execution_started = Instant::now();
    let workers = std::thread::available_parallelism()
        .map(usize::from)
        .unwrap_or(1)
        .min(tasks.len());
    let next = AtomicUsize::new(0);
    let outcomes = Mutex::new(Vec::<Result<ProcessOutcome, String>>::with_capacity(
        tasks.len(),
    ));
    std::thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| {
                loop {
                    let index = next.fetch_add(1, Ordering::Relaxed);
                    let Some(task) = tasks.get(index) else { break };
                    outcomes
                        .lock()
                        .expect("Rust compiler result lock poisoned")
                        .push(run_process(&project_root, task));
                }
            });
        }
    });
    let mut outcomes = outcomes
        .into_inner()
        .map_err(|_| RustCompilerTestError::Context("Rust compiler result lock poisoned".into()))?
        .into_iter()
        .collect::<Result<Vec<_>, _>>()
        .map_err(|reason| RustCompilerTestError::Launch {
            test: "unknown attempt".into(),
            reason,
        })?;
    outcomes.sort_by_key(|outcome| outcome.task.ordinal);

    let mut raw_results = Vec::new();
    let mut attempt_health = Vec::new();
    let mut overall_exit = 0;
    for outcome in outcomes {
        let (test_status, exit) = status(&outcome.output);
        if exit != 0 {
            overall_exit = exit;
            writeln!(
                diagnostics,
                "[supercov] Rust test failed: {}",
                outcome.task.test_id
            )
            .map_err(|error| RustCompilerTestError::Io {
                path: outcome.task.transport.clone(),
                reason: error.to_string(),
            })?;
            diagnostics
                .write_all(&outcome.output.stdout)
                .and_then(|_| diagnostics.write_all(&outcome.output.stderr))
                .map_err(|error| RustCompilerTestError::Io {
                    path: outcome.task.transport.clone(),
                    reason: error.to_string(),
                })?;
        }
        if outcome.read.dropped != 0 {
            return Err(RustCompilerTestError::DroppedEvidence {
                test: outcome.task.test_id,
                dropped: outcome.read.dropped,
            });
        }
        let attempt_id = format!(
            "{}:{:04}:{:08}",
            request.run_id, outcome.task.artifact_index, outcome.task.test_index
        );
        let base_phase = CoveragePhase {
            id: phase_id(&request.run_id, &attempt_id),
            kind: "test".into(),
            operation: format!("Rust libtest {}", outcome.task.test),
            source: Some(outcome.task.artifact.source.clone()),
            caused_by_phase_id: None,
            started_at_ms: outcome.started_at_ms,
            ended_at_ms: Some(outcome.ended_at_ms),
            status: Some(test_status.into()),
            error: (exit != 0).then(|| {
                String::from_utf8_lossy(&outcome.output.stderr)
                    .trim()
                    .to_owned()
            }),
        };
        let projection = project_rust_compiler_evidence(
            outcome.task.context_id,
            &base_phase,
            &outcome.read,
            &build.normalized,
        )
        .map_err(|error| RustCompilerTestError::Projection {
            test: outcome.task.test_id.clone(),
            reason: error.to_string(),
        })?;
        let (result, background, health) = raw_result(
            &request.run_id,
            &outcome.task,
            test_status,
            base_phase,
            projection,
        );
        raw_results.push(result);
        if let Some(background) = background {
            raw_results.push(background);
        }
        attempt_health.push(health);
    }

    let structural_limitations = build
        .normalized
        .manifest
        .limitations
        .iter()
        .filter_map(|limitation| {
            limitation
                .get("id")
                .and_then(|id| id.as_str())
                .map(str::to_owned)
        })
        .collect();
    let declaration = FrontendRunDeclaration {
        protocol_version: LANGUAGE_FRONTEND_PROTOCOL_VERSION,
        frontend_id: "rust".into(),
        frontend_version: "rust-compiler-v1".into(),
        language: "rust".into(),
        structural_source: StructuralSource::OwnedProbes,
        runners: vec![runner_declaration()],
        structural_limitations,
    };
    Ok(RustCompilerFrontendRun {
        selection: build.selection,
        declaration,
        request: CoverageReportRequest {
            run_id: request.run_id.clone(),
            manifest: build.normalized.manifest,
            raw_results,
            generated_at: request.generated_at.clone(),
            coverage_model: Some(rust_compiler_coverage_model()),
            integrity: None,
            test_exit_code: ExitCodeInput::Present(Some(overall_exit)),
        },
        exit_code: overall_exit,
        artifacts: artifacts.len(),
        artifact_files: artifacts
            .into_iter()
            .map(|artifact| artifact.executable)
            .collect(),
        attempt_health,
        build_ms: build.build_ms,
        execution_ms: execution_started.elapsed().as_secs_f64() * 1000.0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_and_phase_ids_are_fixed_width_and_domain_separated() {
        assert_eq!(token_hex(&[0xab; TOKEN_BYTES]), "ab".repeat(TOKEN_BYTES));
        let first = phase_id("run-a", "attempt");
        assert_eq!(first.len(), "rust-test:".len() + 40);
        assert_ne!(first, phase_id("run-b", "attempt"));
        assert_ne!(first, phase_id("run-a", "attempt-b"));
    }

    #[test]
    fn compiler_runner_declares_exact_assertion_but_not_action_causality() {
        let runner = runner_declaration();
        assert_eq!(runner.attribution.assertion, AttributionPrecision::Exact);
        assert_eq!(runner.attribution.action, AttributionPrecision::Unavailable);
        assert_eq!(runner.limitations.len(), 1);
    }
}
