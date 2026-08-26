//! Private process-per-test execution for compiler-instrumented Rust artifacts.
//!
//! The compiler frontend freezes the complete denominator before this module
//! launches anything. Every libtest attempt receives its own authenticated,
//! bounded mmap transport and deterministic context. Context-zero records are
//! retained as background results and are never credited to the test.

use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsString,
    fs,
    io::{self, Write},
    path::{Component, Path, PathBuf},
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
    process_supervision::{CommandSpec, ProcessSupervisor, SupervisedOutput, SupervisionOptions},
    rust_compiler_ctfe::RustCompilerCtfeUnit,
    rust_compiler_evidence::{
        RustCompilerEvidenceProjection, RustCompilerTransportHealth, project_rust_compiler_evidence,
    },
    rust_compiler_manifest::NormalizedRustCompilerManifest,
    rust_compiler_orchestration::{
        RustCompilerBuild, RustCompilerBuildRequest, RustCompilerTestArtifact,
    },
    rust_doctest::{RustdocJoinedOutcomeState, RustdocOutcomeResolution, RustdocOutcomeStatus},
    rust_probe_transport::{
        DEFAULT_DESCRIPTOR_CAPACITY, DEFAULT_PAYLOAD_CAPACITY, RUST_CONTEXT_ENV,
        RUST_TRANSPORT_ENV, RUST_TRANSPORT_TOKEN_ENV, RustTransportRead, create_rust_transport,
        read_rust_transport,
    },
    rust_test_context::preflight_rust_test_contexts,
    rust_test_runner::{cargo_invocation, rust_libtest_selection},
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
    pub watchdog_program: Option<PathBuf>,
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
pub struct RustCompilerTransportHealthRecord {
    pub scope_id: String,
    pub scope_kind: String,
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
    pub transport_health: Vec<RustCompilerTransportHealthRecord>,
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
            contents: serde_json::to_vec(&self.transport_health)?,
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
    UnsupportedCommand(String),
    Interrupted { code: i32, signal: String },
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
            Self::UnsupportedCommand(reason) => formatter.write_str(reason),
            Self::Interrupted { signal, .. } => {
                write!(formatter, "Rust test run was interrupted by {signal}")
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
    run_arguments: Vec<String>,
}

#[derive(Debug)]
struct ProcessOutcome {
    task: ProcessTask,
    output: SupervisedOutput,
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

fn list_tests(
    project_root: &Path,
    artifact: &TestArtifact,
    selection_arguments: &[String],
    supervisor: &ProcessSupervisor,
    options: SupervisionOptions,
) -> Result<Vec<String>, RustCompilerTestError> {
    let mut arguments = selection_arguments
        .iter()
        .map(OsString::from)
        .collect::<Vec<_>>();
    arguments.extend(["--list".into(), "--format".into(), "terse".into()]);
    let output = supervisor
        .supervise_captured(
            &CommandSpec {
                program: artifact.executable.clone().into_os_string(),
                arguments,
                cwd: project_root.to_owned(),
                environment: None,
                captured_output: None,
            },
            options,
            &mut io::sink(),
        )
        .map_err(|error| RustCompilerTestError::List {
            artifact: artifact.executable.clone(),
            reason: error.to_string(),
        })?;
    if output.result.exit_code() != 0 {
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

fn inherited_environment(
    overrides: impl IntoIterator<Item = (OsString, OsString)>,
) -> Vec<(OsString, OsString)> {
    let mut environment = std::env::vars_os().collect::<BTreeMap<_, _>>();
    environment.extend(overrides);
    environment.into_iter().collect()
}

fn run_process(
    project_root: &Path,
    task: &ProcessTask,
    supervisor: &ProcessSupervisor,
    options: SupervisionOptions,
) -> Result<ProcessOutcome, String> {
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
    let mut arguments = task
        .run_arguments
        .iter()
        .map(OsString::from)
        .collect::<Vec<_>>();
    arguments.extend([OsString::from("--exact"), OsString::from(&task.test)]);
    let output = supervisor
        .supervise_captured(
            &CommandSpec {
                program: task.artifact.executable.clone().into_os_string(),
                arguments,
                cwd: project_root.to_owned(),
                environment: Some(inherited_environment([
                    (
                        OsString::from(RUST_TRANSPORT_ENV),
                        task.transport.clone().into_os_string(),
                    ),
                    (
                        OsString::from(RUST_TRANSPORT_TOKEN_ENV),
                        OsString::from(token_hex(&token)),
                    ),
                    (
                        OsString::from(RUST_CONTEXT_ENV),
                        OsString::from(format!("{:016x}", task.context_id)),
                    ),
                ])),
                captured_output: None,
            },
            options,
            &mut io::sink(),
        )
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

fn compiler_runner_declaration() -> FrontendRunnerDeclaration {
    FrontendRunnerDeclaration {
        runner: "rustc".into(),
        execution_model: ExecutionModel::ProcessPerTest,
        attribution: FrontendAttribution {
            run: AttributionPrecision::Exact,
            worker: AttributionPrecision::Exact,
            test: AttributionPrecision::Exact,
            retry: AttributionPrecision::Exact,
            phase: AttributionPrecision::Exact,
            action: AttributionPrecision::Unavailable,
            assertion: AttributionPrecision::Unavailable,
        },
        limitations: vec![
            FrontendLimitation {
                id: "rust-ctfe-action-linkage-unavailable".into(),
                scopes: vec![FrontendLimitationScope::Action],
                reason: "Compile-time evaluation is build execution, not an application action"
                    .into(),
            },
            FrontendLimitation {
                id: "rust-ctfe-assertion-linkage-unavailable".into(),
                scopes: vec![FrontendLimitationScope::Assertion],
                reason: "Compile-time execution has no user-test assertion lifecycle".into(),
            },
        ],
    }
}

fn rustdoc_runner_declaration() -> FrontendRunnerDeclaration {
    FrontendRunnerDeclaration {
        runner: "rustdoc".into(),
        execution_model: ExecutionModel::ParallelContextPropagated,
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
            id: "rustdoc-action-linkage-unavailable".into(),
            scopes: vec![FrontendLimitationScope::Action],
            reason: "Rust doctests expose assertions but no general application-action lifecycle"
                .into(),
        }],
    }
}

fn doctest_raw_results(
    run_id: &str,
    resolution: &RustdocOutcomeResolution,
    started_at_ms: i64,
    ended_at_ms: i64,
    normalized: &NormalizedRustCompilerManifest,
) -> Result<(Vec<RawTestResult>, Vec<RustCompilerTransportHealthRecord>), RustCompilerTestError> {
    let mut results = Vec::new();
    let mut health = Vec::new();
    for group in &resolution.groups {
        if group.transport.dropped != 0 {
            return Err(RustCompilerTestError::DroppedEvidence {
                test: format!("rustdoc:{}", group.group),
                dropped: group.transport.dropped,
            });
        }
        let worker_id = format!("rustdoc-{}", &group.invocation_id[..16]);
        let mut accounted_committed = 0_u64;
        for joined in &group.entries {
            let entry = &joined.catalog;
            let test_id = format!("rust:doctest:{}:{}:{}", group.group, entry.file, entry.line);
            let attempt_id = format!(
                "{run_id}:doctest:{}:{}",
                group.invocation_id, joined.catalog_index
            );
            let (status, error, started, completed) = match &joined.state {
                RustdocJoinedOutcomeState::Completed { outcome } => (
                    match outcome.status {
                        RustdocOutcomeStatus::Passed => "passed",
                        RustdocOutcomeStatus::Failed => "failed",
                        RustdocOutcomeStatus::Ignored => "skipped",
                    },
                    (outcome.status == RustdocOutcomeStatus::Failed).then(|| {
                        outcome
                            .message
                            .as_deref()
                            .or(outcome.reason.as_deref())
                            .unwrap_or("rustdoc reported a failed doctest")
                            .to_owned()
                    }),
                    true,
                    true,
                ),
                RustdocJoinedOutcomeState::UnfinishedStarted => (
                    "unknown",
                    Some("rustdoc fail-fast ended after this doctest started".into()),
                    true,
                    false,
                ),
                RustdocJoinedOutcomeState::Unstarted => (
                    "unknown",
                    Some("rustdoc fail-fast ended before this doctest started".into()),
                    false,
                    false,
                ),
                RustdocJoinedOutcomeState::FilteredOut => ("skipped", None, false, false),
                RustdocJoinedOutcomeState::NotRunAmbiguous => (
                    "unknown",
                    Some(
                        "rustdoc did not identify whether this doctest was filtered or left unstarted by fail-fast"
                            .into(),
                    ),
                    false,
                    false,
                ),
            };
            let mut phases = started
                .then(|| CoveragePhase {
                    id: phase_id(run_id, &attempt_id),
                    kind: "test".into(),
                    operation: format!("Rust doctest {}", entry.name),
                    source: Some(entry.file.clone()),
                    caused_by_phase_id: None,
                    // Pinned libtest reports duration but no wall-clock
                    // boundaries. The authenticated rustdoc invocation is the
                    // narrowest non-invented interval available; an unfinished
                    // test deliberately has no terminal timestamp.
                    started_at_ms,
                    ended_at_ms: completed.then_some(ended_at_ms),
                    status: Some(status.into()),
                    error: error.clone(),
                })
                .into_iter()
                .collect::<Vec<_>>();
            let (base_context, transport) =
                group.attributed_transport(joined).map_err(|error| {
                    RustCompilerTestError::Projection {
                        test: test_id.clone(),
                        reason: error.to_string(),
                    }
                })?;
            accounted_committed = accounted_committed
                .checked_add(transport.committed)
                .ok_or_else(|| RustCompilerTestError::Projection {
                    test: test_id.clone(),
                    reason: "rustdoc committed evidence count overflow".into(),
                })?;
            if !started && transport.committed != 0 {
                return Err(RustCompilerTestError::Projection {
                    test: test_id,
                    reason: "a filtered or unstarted doctest emitted runtime evidence".into(),
                });
            }
            let runtime = if let Some(base_phase) = phases.first() {
                let projection = project_rust_compiler_evidence(
                    base_context,
                    base_phase,
                    &transport,
                    normalized,
                )
                .map_err(|error| RustCompilerTestError::Projection {
                    test: test_id.clone(),
                    reason: error.to_string(),
                })?;
                if snapshot_has_evidence(&projection.background) {
                    return Err(RustCompilerTestError::Projection {
                        test: test_id.clone(),
                        reason: "doctest context partition retained background evidence".into(),
                    });
                }
                phases.extend(projection.assertion_phases);
                vec![projection.attributed]
            } else {
                Vec::new()
            };
            results.push(RawTestResult {
                test_id: Some(test_id.clone()),
                scope: Some(ExecutionScope {
                    version: 1,
                    run_id: run_id.into(),
                    worker_id: worker_id.clone(),
                    test_id: test_id.clone(),
                    test_key: test_id.clone(),
                    retry: 0,
                    attempt_id,
                }),
                test: test_id,
                test_file: Some(entry.file.clone()),
                title: Some(entry.name.clone()),
                retry: Some(0),
                status: Some(status.into()),
                expected_status: Some("passed".into()),
                flaky: false,
                provenance: TestProvenance {
                    runner: "rustdoc".into(),
                    kind: "doctest".into(),
                    project: Some(group.group.clone()),
                    source: "supercov-rustdoc-outcome".into(),
                },
                role: "test".into(),
                phases,
                runtime,
                browser: Vec::new(),
                server: Vec::new(),
            });
        }

        let background =
            group
                .background_transport()
                .map_err(|error| RustCompilerTestError::Projection {
                    test: format!("rustdoc:{}", group.group),
                    reason: error.to_string(),
                })?;
        accounted_committed = accounted_committed
            .checked_add(background.committed)
            .ok_or_else(|| RustCompilerTestError::Projection {
                test: format!("rustdoc:{}", group.group),
                reason: "rustdoc background evidence count overflow".into(),
            })?;
        if accounted_committed != group.transport.committed {
            return Err(RustCompilerTestError::Projection {
                test: format!("rustdoc:{}", group.group),
                reason: format!(
                    "rustdoc context partition accounted for {accounted_committed} of {} committed records",
                    group.transport.committed
                ),
            });
        }
        if background.committed != 0 {
            let background_id = format!("background:rustdoc:{}", group.invocation_id);
            let background_phase = CoveragePhase {
                id: phase_id(run_id, &background_id),
                kind: "setup".into(),
                operation: format!("Background while running Rust doctests for {}", group.group),
                source: None,
                caused_by_phase_id: None,
                started_at_ms,
                ended_at_ms: Some(ended_at_ms),
                status: Some("passed".into()),
                error: None,
            };
            let projection =
                project_rust_compiler_evidence(1, &background_phase, &background, normalized)
                    .map_err(|error| RustCompilerTestError::Projection {
                        test: background_id.clone(),
                        reason: error.to_string(),
                    })?;
            if snapshot_has_evidence(&projection.attributed)
                || !projection.assertion_phases.is_empty()
            {
                return Err(RustCompilerTestError::Projection {
                    test: background_id,
                    reason: "context-zero doctest evidence became test-attributed".into(),
                });
            }
            results.push(RawTestResult {
                test_id: Some(background_id.clone()),
                scope: Some(ExecutionScope {
                    version: 1,
                    run_id: run_id.into(),
                    worker_id: worker_id.clone(),
                    test_id: background_id.clone(),
                    test_key: background_id.clone(),
                    retry: 0,
                    attempt_id: format!("{run_id}:doctest:{}:background", group.invocation_id),
                }),
                test: background_id,
                test_file: None,
                title: Some(format!("Background Rust doctest work for {}", group.group)),
                retry: Some(0),
                status: Some("passed".into()),
                expected_status: Some("passed".into()),
                flaky: false,
                provenance: TestProvenance {
                    runner: "rustdoc".into(),
                    kind: "doctest".into(),
                    project: Some(group.group.clone()),
                    source: "supercov-rustdoc-context-zero".into(),
                },
                role: "background".into(),
                phases: Vec::new(),
                runtime: vec![projection.background],
                browser: Vec::new(),
                server: Vec::new(),
            });
        }
        health.push(RustCompilerTransportHealthRecord {
            scope_id: format!("rustdoc:{}", group.invocation_id),
            scope_kind: "runner-invocation".into(),
            status: if group.transport.dropped == 0 && group.transport.incomplete == 0 {
                "passed".into()
            } else {
                "unknown".into()
            },
            transport: RustCompilerTransportHealth {
                committed: group.transport.committed,
                incomplete: group.transport.incomplete,
                dropped: group.transport.dropped,
                attachments: group.transport.attachments,
            },
        });
    }
    Ok((results, health))
}

fn doctest_command_failed(resolution: &RustdocOutcomeResolution) -> bool {
    resolution.groups.iter().any(|group| {
        group.entries.iter().any(|joined| match &joined.state {
            RustdocJoinedOutcomeState::Completed { outcome } => {
                outcome.status == RustdocOutcomeStatus::Failed
            }
            RustdocJoinedOutcomeState::UnfinishedStarted | RustdocJoinedOutcomeState::Unstarted => {
                true
            }
            RustdocJoinedOutcomeState::FilteredOut => false,
            RustdocJoinedOutcomeState::NotRunAmbiguous => true,
        }) || group.ambiguous_unstarted_tests != 0
    })
}

fn ctfe_raw_results(
    run_id: &str,
    units: Vec<RustCompilerCtfeUnit>,
    started_at_ms: i64,
    ended_at_ms: i64,
) -> Vec<RawTestResult> {
    units
        .into_iter()
        .enumerate()
        .map(|(index, unit)| {
            let worker_id = format!("rustc-{index:04}");
            let test_id = format!("rust:build:ctfe:{}:{index:04}", unit.crate_name);
            let attempt_id = format!("{run_id}:ctfe:{index:04}");
            let phase_id = phase_id(run_id, &attempt_id);
            let mut snapshot = unit.snapshot;
            for event in &mut snapshot.events {
                event.phase_id = Some(phase_id.clone());
            }
            RawTestResult {
                test_id: Some(test_id.clone()),
                scope: Some(ExecutionScope {
                    version: 1,
                    run_id: run_id.into(),
                    worker_id,
                    test_id: test_id.clone(),
                    test_key: test_id.clone(),
                    retry: 0,
                    attempt_id,
                }),
                test: test_id.clone(),
                test_file: None,
                title: Some(format!(
                    "Compile-time evaluation for {} ({})",
                    unit.crate_name, unit.identity
                )),
                retry: Some(0),
                status: Some("passed".into()),
                expected_status: Some("passed".into()),
                flaky: false,
                provenance: TestProvenance {
                    runner: "rustc".into(),
                    kind: "build".into(),
                    project: Some(unit.crate_name),
                    source: "supercov-rustc-ctfe".into(),
                },
                role: "setup".into(),
                phases: vec![CoveragePhase {
                    id: phase_id,
                    kind: "setup".into(),
                    operation: "Rust constant evaluation".into(),
                    source: None,
                    caused_by_phase_id: None,
                    started_at_ms,
                    ended_at_ms: Some(ended_at_ms),
                    status: Some("passed".into()),
                    error: None,
                }],
                runtime: vec![snapshot],
                browser: Vec::new(),
                server: Vec::new(),
            }
        })
        .collect()
}

fn status(output: &SupervisedOutput) -> (&'static str, i32) {
    let exit = output.result.exit_code();
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
    RustCompilerTransportHealthRecord,
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
    let health = RustCompilerTransportHealthRecord {
        scope_id: task.test_id.clone(),
        scope_kind: "test-attempt".into(),
        status: status.into(),
        transport: projection.health,
    };
    (result, background, health)
}

pub fn run_rust_compiler_frontend(
    request: &RustCompilerRunRequest,
    diagnostics: &mut dyn Write,
) -> Result<RustCompilerFrontendRun, RustCompilerTestError> {
    let supervisor = request
        .watchdog_program
        .as_deref()
        .map_or_else(ProcessSupervisor::new, ProcessSupervisor::new_crash_safe)
        .map_err(|error| RustCompilerTestError::Build(error.to_string()))?;
    let options = SupervisionOptions::from_environment()
        .map_err(|error| RustCompilerTestError::Build(error.to_string()))?;
    let build = crate::rust_compiler_orchestration::build_with_rust_compiler_companion_supervised(
        &request.build_request(),
        &supervisor,
        options,
        diagnostics,
    )
    .map_err(|error| match error {
        crate::rust_compiler_orchestration::RustCompilerOrchestrationError::Interrupted {
            code,
            signal,
        } => RustCompilerTestError::Interrupted { code, signal },
        error => RustCompilerTestError::Build(error.to_string()),
    })?;
    execute_compiler_build(request, build, &supervisor, options, diagnostics)
}

fn execute_compiler_build(
    request: &RustCompilerRunRequest,
    build: RustCompilerBuild,
    supervisor: &ProcessSupervisor,
    options: SupervisionOptions,
    diagnostics: &mut dyn Write,
) -> Result<RustCompilerFrontendRun, RustCompilerTestError> {
    let project_root = fs::canonicalize(&request.project_root)
        .map_err(|error| io_error(&request.project_root, error))?;
    let invocation = cargo_invocation(&project_root, &request.command)
        .map_err(|error| RustCompilerTestError::UnsupportedCommand(error.to_string()))?;
    let selection = build
        .run_libtests
        .then(|| rust_libtest_selection(&invocation))
        .transpose()
        .map_err(|error| RustCompilerTestError::UnsupportedCommand(error.to_string()))?;
    let artifacts = normalize_artifacts(&project_root, &build.artifacts)?;
    let evidence_root = build.compiler_output_directory.join("attempts");
    fs::create_dir(&evidence_root).map_err(|error| io_error(&evidence_root, error))?;
    let mut tasks = Vec::new();
    let mut identities = BTreeSet::new();
    for (artifact_index, artifact) in artifacts.iter().enumerate() {
        let selection = selection.as_ref().ok_or_else(|| {
            RustCompilerTestError::UnsupportedCommand(
                "doc-only execution unexpectedly produced a libtest artifact".into(),
            )
        })?;
        let tests = list_tests(
            &project_root,
            artifact,
            &selection.list_arguments,
            supervisor,
            options,
        )?;
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
                run_arguments: selection.run_arguments.clone(),
            });
        }
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
                        .push(run_process(&project_root, task, supervisor, options));
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

    let mut raw_results = ctfe_raw_results(
        &request.run_id,
        build.ctfe_units.clone(),
        build.build_started_at_ms,
        build.build_ended_at_ms,
    );
    let (doctest_results, mut transport_health) = doctest_raw_results(
        &request.run_id,
        &build.doctest_outcomes,
        build.build_started_at_ms,
        build.build_ended_at_ms,
        &build.normalized,
    )?;
    raw_results.extend(doctest_results);
    let mut overall_exit = if build.run_doctests {
        build.doctest_exit_code.unwrap_or(1)
    } else {
        0
    };
    if build.run_doctests && overall_exit != 0 {
        diagnostics
            .write_all(&build.doctest_stdout)
            .and_then(|_| diagnostics.write_all(&build.doctest_stderr))
            .map_err(|error| RustCompilerTestError::Io {
                path: build.compiler_output_directory.clone(),
                reason: error.to_string(),
            })?;
    }
    if (overall_exit == 0) == doctest_command_failed(&build.doctest_outcomes) {
        return Err(RustCompilerTestError::Context(
            "Cargo rustdoc exit status disagrees with authenticated doctest outcomes".into(),
        ));
    }
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
        transport_health.push(health);
    }

    let mut structural_limitations = build
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
        .collect::<Vec<_>>();
    if !build.doctest_outcomes.is_fully_catalogued() {
        structural_limitations.push("rust-doctest-outcome-catalog-incomplete".into());
    }
    if build.doctest_outcomes.has_ambiguous_outcomes() {
        structural_limitations.push("rust-doctest-filter-fail-fast-identity-ambiguous".into());
    }
    structural_limitations.sort();
    structural_limitations.dedup();
    let observed_runners = raw_results
        .iter()
        .map(|result| result.provenance.runner.as_str())
        .collect::<BTreeSet<_>>();
    if observed_runners
        .iter()
        .any(|runner| !matches!(*runner, "rustc" | "rust-libtest" | "rustdoc"))
    {
        return Err(RustCompilerTestError::Context(
            "Rust compiler run produced an unknown runner identity".into(),
        ));
    }
    // The frontend contract declares capabilities actually present in this
    // run. In particular, `cargo test --doc` must not advertise an unobserved
    // libtest runner, and an explicit non-doc target must not advertise
    // rustdoc. The shared analyzer deliberately rejects such declarations.
    let runners = [
        ("rustc", compiler_runner_declaration()),
        ("rust-libtest", runner_declaration()),
        ("rustdoc", rustdoc_runner_declaration()),
    ]
    .into_iter()
    .filter_map(|(name, declaration)| observed_runners.contains(name).then_some(declaration))
    .collect();
    let declaration = FrontendRunDeclaration {
        protocol_version: LANGUAGE_FRONTEND_PROTOCOL_VERSION,
        frontend_id: "rust".into(),
        frontend_version: "rust-compiler-v1".into(),
        language: "rust".into(),
        structural_source: StructuralSource::OwnedProbes,
        runners,
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
        transport_health,
        build_ms: build.build_ms,
        execution_ms: execution_started.elapsed().as_secs_f64() * 1000.0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rust_doctest::{
        RustdocDoctestAttributes, RustdocDoctestCode, RustdocDoctestIgnore, RustdocDoctestWrapper,
        RustdocExtractedDoctest, RustdocJoinedOutcome, RustdocMergedEntry, RustdocOutcomeGroupJoin,
        RustdocTestOutcome,
    };

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
        let compiler = compiler_runner_declaration();
        assert_eq!(compiler.attribution.phase, AttributionPrecision::Exact);
        assert_eq!(
            compiler.attribution.assertion,
            AttributionPrecision::Unavailable
        );
        let rustdoc = rustdoc_runner_declaration();
        assert_eq!(
            rustdoc.execution_model,
            ExecutionModel::ParallelContextPropagated
        );
        assert_eq!(rustdoc.attribution.test, AttributionPrecision::Exact);
        assert_eq!(rustdoc.attribution.assertion, AttributionPrecision::Exact);
    }

    #[test]
    fn doctest_outcomes_project_exact_status_identity_and_fail_fast_state() {
        let entry = |module: &str, line: u64| RustdocMergedEntry {
            module: module.into(),
            display_name: format!("src/lib.rs - (line {line})"),
            path: "src/lib.rs".into(),
            line,
            ignored: false,
            no_run: false,
            should_panic: false,
        };
        let catalog = |line: u64| RustdocExtractedDoctest {
            file: "src/lib.rs".into(),
            line,
            doctest_attributes: RustdocDoctestAttributes {
                original: String::new(),
                should_panic: false,
                no_run: false,
                ignore: RustdocDoctestIgnore::None,
                rust: true,
                test_harness: false,
                compile_fail: false,
                standalone_crate: false,
                error_codes: Vec::new(),
                edition: None,
                added_css_classes: Vec::new(),
                unknown: Vec::new(),
            },
            original_code: "assert!(true);".into(),
            doctest_code: Some(RustdocDoctestCode {
                crate_level: String::new(),
                code: "assert!(true);".into(),
                wrapper: Some(RustdocDoctestWrapper {
                    before: "fn main() {".into(),
                    after: "}".into(),
                    returns_result: false,
                }),
            }),
            name: format!("src/lib.rs - (line {line})"),
        };
        let completed = |catalog_index, entry: RustdocMergedEntry, status| RustdocJoinedOutcome {
            catalog_index,
            catalog: catalog(entry.line),
            merged_entry: Some(entry.clone()),
            state: RustdocJoinedOutcomeState::Completed {
                outcome: RustdocTestOutcome {
                    display_name: entry.display_name,
                    status,
                    execution_seconds: Some(0.1),
                    stdout: None,
                    message: None,
                    reason: None,
                    timeout_warning: false,
                },
            },
        };
        let resolution = RustdocOutcomeResolution {
            groups: vec![RustdocOutcomeGroupJoin {
                invocation_id: "1".repeat(64),
                group: "fixture".into(),
                companion_build_id: "2".repeat(64),
                raw_catalog_sha256: "4".repeat(64),
                raw_events_sha256: "3".repeat(64),
                transport_sha256: "5".repeat(64),
                join: None,
                transport: RustTransportRead {
                    observations: Vec::new(),
                    ordinal_hits: Vec::new(),
                    phases: Vec::new(),
                    committed: 0,
                    incomplete: 0,
                    dropped: 0,
                    attachments: 0,
                },
                entries: vec![
                    completed(0, entry("__doctest_0", 3), RustdocOutcomeStatus::Passed),
                    RustdocJoinedOutcome {
                        catalog_index: 1,
                        catalog: catalog(10),
                        merged_entry: Some(entry("__doctest_1", 10)),
                        state: RustdocJoinedOutcomeState::Unstarted,
                    },
                ],
                ambiguous_filtered_out: 0,
                ambiguous_unstarted_tests: 0,
            }],
            unmatched_maps: Vec::new(),
        };
        let normalized = NormalizedRustCompilerManifest {
            manifest: crate::coverage_report::CoverageManifest {
                decisions: Vec::new(),
                points: Vec::new(),
                branches: Vec::new(),
                limitations: Vec::new(),
                scope: None,
            },
            hit_obligations_by_ordinal: std::collections::BTreeMap::new(),
            internal_ordinals: BTreeSet::new(),
            decision_outcome_obligations: std::collections::BTreeMap::new(),
            decision_loop_obligations: std::collections::BTreeMap::new(),
        };
        let (results, health) =
            doctest_raw_results("run", &resolution, 10, 20, &normalized).unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(health.len(), 1);
        assert_eq!(results[0].status.as_deref(), Some("passed"));
        assert_eq!(results[1].status.as_deref(), Some("unknown"));
        assert_eq!(results[0].retry, Some(0));
        assert_eq!(results[0].test_file.as_deref(), Some("src/lib.rs"));
        assert_eq!(
            results[0].test_id.as_deref(),
            Some("rust:doctest:fixture:src/lib.rs:3")
        );
        assert_eq!(results[0].scope.as_ref().unwrap().test_id, results[0].test);
        assert!(doctest_command_failed(&resolution));
        assert!(results[1].phases.is_empty());
        assert!(resolution.is_fully_catalogued());
    }
}
