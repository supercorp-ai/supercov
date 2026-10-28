//! Frozen, implementation-neutral Supercov engine contracts.
//!
//! This crate does not contain coverage behavior. It makes contract drift a
//! compile/test failure while the shipped implementation and Rust candidate
//! coexist. Independent specifications and conformance oracles—not either
//! implementation—decide whether a contract is correct.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

pub const CONTRACT_VERSION: u32 = 1;
pub const EVIDENCE_ARCHIVE_SCHEMA_VERSION: u32 = 3;
pub const EVIDENCE_ARCHIVE_MAGIC: &str = "SUPERCOV-EVIDENCE-3\n";
pub const COVERAGE_MODEL_SCHEMA_VERSION: u32 = 1;
pub const COVERAGE_MODEL_MAX_IDENTIFIER_BYTES: usize = 64;
pub const COVERAGE_MODEL_MAX_DESCRIPTION_BYTES: usize = 4_096;
pub const COVERAGE_MODEL_MAX_SURFACES_PER_LIST: usize = 256;
pub const AGENT_JSON_SCHEMA_VERSION: u32 = 1;
pub const AGENT_JSON_MAX_BYTES: usize = 65_536;
pub const DEFAULT_PAGE_SIZE: usize = 20;
pub const PROCESS_SUPERVISION_SCHEMA_VERSION: u32 = 1;
pub const DEFAULT_DIAGNOSTIC_INTERVAL_MS: u64 = 60_000;
pub const COMMAND_TIMEOUT_EXIT_CODE: i32 = 124;
pub const COMMAND_TERMINATION_GRACE_MS: u64 = 5_000;
pub const PROBE_V2_VERSION: u32 = 2;
pub const PROBE_V2_RADIX: u32 = 3;
pub const PROBE_V2_JS_MAX_CONDITIONS: usize = 32;
pub const LANGUAGE_FRONTEND_PROTOCOL_VERSION: u32 = 2;
pub const RUST_PROBE_TRANSPORT_PROTOCOL_VERSION: u32 = 1;
pub const RUST_PROBE_TRANSPORT_MAGIC: &str = "SCVRUST1";
pub const RUST_PROBE_TRANSPORT_V3_PROTOCOL_VERSION: u32 = 3;
pub const RUST_PROBE_TRANSPORT_V3_MAGIC: &str = "SCVRUST3";
pub const RUST_PROBE_TRANSPORT_HEADER_SIZE: usize = 128;
pub const RUST_PROBE_TRANSPORT_DESCRIPTOR_SIZE: usize = 40;
pub const RUST_PROBE_TRANSPORT_TOKEN_SIZE: usize = 16;
pub const RUST_LIBTEST_EVENT_PROTOCOL_VERSION: u32 = 1;
pub const RUST_LIBTEST_EVENT_MAGIC: &str = "SCVLTST1";
pub const RUST_LIBTEST_EVENT_HEADER_SIZE: usize = 64;
pub const RUST_LIBTEST_EVENT_RECORD_HEADER_SIZE: usize = 48;
pub const RUST_LIBTEST_EVENT_TOKEN_SIZE: usize = 16;
pub const RUST_LIBTEST_EVENT_MAX_NAME_BYTES: usize = 1_048_576;
pub const RUST_LIBTEST_COMPANION_BUNDLE_SCHEMA_VERSION: u32 = 3;

pub const ERROR_CODES: &[&str] = &[
    "AMBIGUOUS_SELECTOR",
    "DECISION_NOT_FOUND",
    "FILTER_UNAVAILABLE",
    "INTERNAL_ERROR",
    "INVALID_ARGUMENT",
    "MINIMIZATION_COMPLEXITY_LIMIT",
    "NO_RUNS",
    "RESPONSE_TOO_LARGE",
    "RUN_NOT_FOUND",
    "SCOPE_UNAVAILABLE",
    "SOURCE_NOT_FOUND",
    "TARGET_UNREACHABLE",
    "TEST_FILTER_EMPTY",
    "TEST_NOT_FOUND",
    "UNATTRIBUTED_EVIDENCE",
    "UNKNOWN_COMMAND",
];

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContractRegistry {
    pub contract_version: u32,
    pub status: String,
    pub resident_process: bool,
    pub evidence_archive: EvidenceArchiveContract,
    pub run_store: RunStoreContract,
    pub agent_json: AgentJsonContract,
    pub process_supervision: ProcessSupervisionContract,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EvidenceArchiveContract {
    pub schema_version: u32,
    pub file: String,
    pub format: String,
    pub magic: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunStoreContract {
    pub schema_version: u32,
    pub store: String,
    pub workspace_store: String,
    pub published_run_files: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentJsonContract {
    pub schema_version: u32,
    pub max_bytes: usize,
    pub default_page_size: usize,
    pub error_codes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProcessSupervisionContract {
    pub schema_version: u32,
    pub diagnostic_interval_ms: u64,
    pub timeout_exit_code: i32,
    pub termination_grace_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentPagination {
    pub offset: usize,
    pub limit: usize,
    pub returned: usize,
    pub total: usize,
    pub has_more: bool,
    pub next_offset: Option<usize>,
}

pub fn registry() -> Result<ContractRegistry, serde_json::Error> {
    serde_json::from_str(include_str!("../assets/v1/contract.json"))
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProbeV2Contract {
    pub probe_version: u32,
    pub semantics: String,
    pub implementation: String,
    pub decision_encoding: ProbeV2DecisionEncoding,
    pub published_evidence: String,
    pub attribution_epoch: Vec<String>,
    pub fallback: String,
    pub promotion: ProbeV2Promotion,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProbeV2DecisionEncoding {
    pub radix: u32,
    pub digits: ProbeV2Digits,
    pub outcome_stored_separately: bool,
    pub javascript_maximum_encoded_conditions: usize,
    pub wider_decision_behavior: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ProbeV2Digits {
    pub unreached: u32,
    pub r#false: u32,
    pub r#true: u32,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProbeV2Promotion {
    pub realistic_median_runtime_ratio_max: f64,
    pub semantic_equivalence_required: bool,
    pub manifest_parity_required: bool,
    pub evidence_parity_required: bool,
}

pub fn probe_v2_contract() -> Result<ProbeV2Contract, serde_json::Error> {
    serde_json::from_str(include_str!("../assets/probe-v2/contract.json"))
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LanguageFrontendProtocolContract {
    pub frontend_protocol_version: u32,
    pub status: String,
    pub manifest_model: String,
    pub observation_model: String,
    pub probe_model: String,
    pub identity_axes: Vec<String>,
    pub transition_kinds: Vec<String>,
    pub structural_sources: Vec<String>,
    pub execution_models: Vec<String>,
    pub attribution_precisions: Vec<String>,
    pub limitation_scopes: Vec<String>,
    pub requirements: LanguageFrontendRequirements,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LanguageFrontendRequirements {
    pub complete_manifest_before_execution: bool,
    pub unknown_obligation_fatal: bool,
    pub identity_downgrade_requires_limitation: bool,
    pub unknown_phase_reference_fatal: bool,
    pub phase_causality_acyclic: bool,
    pub selected_unstarted_has_test_identity_only: bool,
    pub multiple_runners_per_frontend: bool,
    pub structural_limitations_reference_manifest_ids: bool,
    pub attribution_limitations_runner_scoped: bool,
    pub timestamp_attribution_may_claim_causality: bool,
    pub frontend_may_compute_coverage_verdicts: bool,
    pub engine_owns_manifest_merge: bool,
    pub engine_owns_evidence_validation: bool,
    pub engine_owns_attribution_merge: bool,
    pub engine_owns_coverage_analysis: bool,
    pub engine_owns_persistence_and_queries: bool,
}

pub fn language_frontend_protocol_contract()
-> Result<LanguageFrontendProtocolContract, serde_json::Error> {
    serde_json::from_str(include_str!("../assets/frontend-v2/contract.json"))
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PythonCoverageImportContract {
    pub schema_version: u32,
    pub status: String,
    pub producer: String,
    pub supported_collector_cores_for_exact_contexts: Vec<String>,
    pub requires_branch_measurement: bool,
    pub database_access: String,
    pub frontend_computes_verdicts: bool,
    pub unknown_fields_fatal: bool,
    pub preserve_unrecognized_contexts_as_background: bool,
    pub mcdc_availability: String,
    pub column_locations: String,
}

pub fn python_coverage_import_contract() -> Result<PythonCoverageImportContract, serde_json::Error>
{
    serde_json::from_str(include_str!("../assets/python-coverage-v1/contract.json"))
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EvidenceV3Contract {
    pub schema_version: u32,
    pub status: String,
    pub magic: String,
    pub framing: String,
    pub required_entries: Vec<String>,
    pub frontend_protocol_version: u32,
    pub coverage_model_schema_version: u32,
    pub unknown_frontend_fields_fatal: bool,
    pub unknown_coverage_model_fields_fatal: bool,
    pub frontend_language_must_match_coverage_model: bool,
    pub malformed_recognized_jsonl_fatal: bool,
    pub recognized_jsonl_requires_final_newline: bool,
}

pub fn evidence_v3_contract() -> Result<EvidenceV3Contract, serde_json::Error> {
    serde_json::from_str(include_str!("../assets/evidence-v3/contract.json"))
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CoverageModelV1Contract {
    pub schema_version: u32,
    pub status: String,
    pub persisted_entry: String,
    pub required_fields: Vec<String>,
    pub unknown_fields_fatal: bool,
    pub frontend_language_must_match: bool,
    pub measured_must_be_nonempty: bool,
    pub surface_lists_must_be_unique: bool,
    pub surface_lists_must_be_disjoint: bool,
    pub strings_must_be_trimmed_single_line: bool,
    pub max_identifier_bytes: usize,
    pub max_description_bytes: usize,
    pub max_surfaces_per_list: usize,
}

pub fn coverage_model_v1_contract() -> Result<CoverageModelV1Contract, serde_json::Error> {
    serde_json::from_str(include_str!("../assets/coverage-model-v1/contract.json"))
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RustCoverageV1Contract {
    pub model_version: u32,
    pub status: String,
    pub language: String,
    pub variant: String,
    pub decision_semantics: String,
    pub condition_order: String,
    pub probe_model: String,
    pub generic_aggregation: String,
    pub source_identity: RustSourceIdentityContract,
    pub test_context_identity: RustTestContextIdentityContract,
    pub runner_attempt_identity: RustRunnerAttemptIdentityContract,
    pub libtest_event_transport: RustLibtestEventTransportContract,
    pub point_kinds: Vec<String>,
    pub control_decision_kinds: Vec<String>,
    pub branch_kinds: Vec<String>,
    pub required_owned_surfaces: Vec<String>,
    pub required_identity_axes: Vec<String>,
    pub completeness_requires: Vec<String>,
    pub external_coverage_in_product: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RustLibtestEventTransportContract {
    pub protocol_version: u32,
    pub status: String,
    pub magic: String,
    pub byte_order: String,
    pub header_size: usize,
    pub record_header_size: usize,
    pub token_size: usize,
    pub maximum_name_bytes: usize,
    pub writer: String,
    pub event_kinds: Vec<String>,
    pub terminal_results: Vec<String>,
    pub publication: String,
    pub sequence: String,
    pub integrity: String,
    pub unknown_event: String,
    pub truncated_record: String,
    pub invalid_semantics: String,
    pub missing_transport: String,
    pub process_model: String,
    pub output_authority: String,
    pub artifact_binding: RustLibtestArtifactBindingContract,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RustLibtestArtifactBindingContract {
    pub schema_version: u32,
    pub manifest_suffix: String,
    pub digest: String,
    pub required_bindings: Vec<String>,
    pub artifact_location: String,
    pub unknown_fields_fatal: bool,
    pub mismatch: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RustRunnerAttemptIdentityContract {
    pub version: u32,
    pub cargo_test: RustSingleAttemptRunnerContract,
    pub rustdoc: RustSingleAttemptRunnerContract,
    pub nextest: RustNextestAttemptContract,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RustSingleAttemptRunnerContract {
    pub retry: usize,
    pub total_attempts: usize,
    pub identity_authority: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RustNextestAttemptContract {
    pub minimum_version: String,
    pub maximum_version: String,
    pub verified_released_versions: Vec<String>,
    pub execution_mode: String,
    pub identity_environment: Vec<String>,
    pub attempt_numbering: String,
    pub retry_derivation: String,
    pub attempt_id_semantics: String,
    pub list_phase: String,
    pub selection_projection: String,
    pub selected_but_unstarted: String,
    pub concurrent_attempts: String,
    pub target_runner_death: String,
    pub partial_identity: String,
    pub stress_iteration: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RustTestContextIdentityContract {
    pub version: u32,
    pub algorithm: String,
    pub domain: String,
    pub input: String,
    pub reserved_values: Vec<String>,
    pub reserved_remap_xor: String,
    pub collision_policy: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RustSourceIdentityContract {
    pub version: u32,
    pub digest: String,
    pub id_digest_bytes: usize,
    pub separator: String,
    pub authored_canonical_fields: Vec<String>,
    pub synthetic_expansion_canonical_fields: Vec<String>,
    pub generated_source_key_fields: Vec<String>,
    pub repeated_authored_expansions_aggregate: bool,
    pub distinct_synthetic_invocations_remain_distinct: bool,
    pub ephemeral_paths_forbidden: bool,
    pub collision_policy: String,
}

pub fn rust_coverage_v1_contract() -> Result<RustCoverageV1Contract, serde_json::Error> {
    serde_json::from_str(include_str!("../assets/rust-coverage-v1/contract.json"))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum StructuralSource {
    OwnedProbes,
    NativeImport,
    Mixed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ExecutionModel {
    ProcessPerTest,
    SerialInProcess,
    ParallelContextPropagated,
    ParallelUnattributed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AttributionPrecision {
    Exact,
    Aggregate,
    Unavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FrontendTransitionKind {
    Setup,
    Test,
    Action,
    Assertion,
    Teardown,
    Background,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FrontendLimitationScope {
    Worker,
    Test,
    Retry,
    Phase,
    Action,
    Assertion,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FrontendAttribution {
    pub run: AttributionPrecision,
    pub worker: AttributionPrecision,
    pub test: AttributionPrecision,
    pub retry: AttributionPrecision,
    pub phase: AttributionPrecision,
    pub action: AttributionPrecision,
    pub assertion: AttributionPrecision,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FrontendLimitation {
    pub id: String,
    pub scopes: Vec<FrontendLimitationScope>,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FrontendRunDeclaration {
    pub protocol_version: u32,
    pub frontend_id: String,
    pub frontend_version: String,
    pub language: String,
    pub structural_source: StructuralSource,
    pub runners: Vec<FrontendRunnerDeclaration>,
    pub structural_limitations: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FrontendRunnerDeclaration {
    pub runner: String,
    pub execution_model: ExecutionModel,
    pub attribution: FrontendAttribution,
    pub limitations: Vec<FrontendLimitation>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FrontendDeclarationError {
    UnsupportedProtocolVersion(u32),
    InvalidToken(&'static str),
    NoRunners,
    DuplicateRunner(String),
    RunIdentityNotExact,
    DuplicateLimitation(String),
    DuplicateLimitationScope(String),
    EmptyLimitationScopes(String),
    InvalidLimitationReason(String),
    DuplicateStructuralLimitation(String),
    MissingDowngradeLimitation(FrontendLimitationScope),
    ExactRetryRequiresExactTest,
    ExactPhaseRequiresExactTest,
    ExactAssertionRequiresExactTestAndPhase,
    ExactActionRequiresExactTestAndPhase,
    ParallelUnattributedCannotClaimExactCausality,
}

impl std::fmt::Display for FrontendDeclarationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnsupportedProtocolVersion(version) => {
                write!(
                    formatter,
                    "unsupported language-frontend protocol version: {version}"
                )
            }
            Self::InvalidToken(field) => write!(formatter, "invalid frontend {field}"),
            Self::NoRunners => write!(formatter, "frontend declaration has no runners"),
            Self::DuplicateRunner(runner) => {
                write!(formatter, "duplicate frontend runner: {runner}")
            }
            Self::RunIdentityNotExact => write!(formatter, "frontend run identity must be exact"),
            Self::DuplicateLimitation(id) => {
                write!(formatter, "duplicate frontend limitation: {id}")
            }
            Self::EmptyLimitationScopes(id) => {
                write!(formatter, "frontend limitation has no scopes: {id}")
            }
            Self::DuplicateLimitationScope(id) => {
                write!(formatter, "frontend limitation has duplicate scopes: {id}")
            }
            Self::InvalidLimitationReason(id) => {
                write!(formatter, "frontend limitation has an invalid reason: {id}")
            }
            Self::DuplicateStructuralLimitation(id) => {
                write!(formatter, "duplicate structural limitation reference: {id}")
            }
            Self::MissingDowngradeLimitation(scope) => write!(
                formatter,
                "non-exact {scope:?} attribution has no matching limitation"
            ),
            Self::ExactRetryRequiresExactTest => {
                write!(
                    formatter,
                    "exact retry attribution requires exact test identity"
                )
            }
            Self::ExactPhaseRequiresExactTest => {
                write!(
                    formatter,
                    "exact phase attribution requires exact test identity"
                )
            }
            Self::ExactAssertionRequiresExactTestAndPhase => write!(
                formatter,
                "exact assertion attribution requires exact test and phase identity"
            ),
            Self::ExactActionRequiresExactTestAndPhase => write!(
                formatter,
                "exact action attribution requires exact test and phase identity"
            ),
            Self::ParallelUnattributedCannotClaimExactCausality => write!(
                formatter,
                "parallel-unattributed execution cannot claim exact test, retry, phase, action, or assertion causality"
            ),
        }
    }
}

fn validate_frontend_limitations(
    limitations: &[FrontendLimitation],
    limitation_ids: &mut BTreeSet<String>,
) -> Result<BTreeSet<FrontendLimitationScope>, FrontendDeclarationError> {
    let mut limited_scopes = BTreeSet::new();
    for limitation in limitations {
        if !valid_frontend_token(&limitation.id) {
            return Err(FrontendDeclarationError::InvalidToken("limitation ID"));
        }
        if !limitation_ids.insert(limitation.id.clone()) {
            return Err(FrontendDeclarationError::DuplicateLimitation(
                limitation.id.clone(),
            ));
        }
        if limitation.scopes.is_empty() {
            return Err(FrontendDeclarationError::EmptyLimitationScopes(
                limitation.id.clone(),
            ));
        }
        let unique_scopes = limitation.scopes.iter().copied().collect::<BTreeSet<_>>();
        if unique_scopes.len() != limitation.scopes.len() {
            return Err(FrontendDeclarationError::DuplicateLimitationScope(
                limitation.id.clone(),
            ));
        }
        if limitation.reason.trim().is_empty()
            || limitation.reason.trim().len() != limitation.reason.len()
            || limitation.reason.contains(['\n', '\r', '\0'])
        {
            return Err(FrontendDeclarationError::InvalidLimitationReason(
                limitation.id.clone(),
            ));
        }
        limited_scopes.extend(unique_scopes);
    }
    Ok(limited_scopes)
}

fn validate_frontend_runner(
    runner: &FrontendRunnerDeclaration,
    limitation_ids: &mut BTreeSet<String>,
) -> Result<(), FrontendDeclarationError> {
    if !valid_frontend_runner_token(&runner.runner) {
        return Err(FrontendDeclarationError::InvalidToken("runner"));
    }
    if runner.attribution.run != AttributionPrecision::Exact {
        return Err(FrontendDeclarationError::RunIdentityNotExact);
    }
    let limited_scopes = validate_frontend_limitations(&runner.limitations, limitation_ids)?;
    if runner.attribution.retry == AttributionPrecision::Exact
        && runner.attribution.test != AttributionPrecision::Exact
    {
        return Err(FrontendDeclarationError::ExactRetryRequiresExactTest);
    }
    if runner.attribution.phase == AttributionPrecision::Exact
        && runner.attribution.test != AttributionPrecision::Exact
    {
        return Err(FrontendDeclarationError::ExactPhaseRequiresExactTest);
    }
    for (precision, scope) in [
        (runner.attribution.worker, FrontendLimitationScope::Worker),
        (runner.attribution.test, FrontendLimitationScope::Test),
        (runner.attribution.retry, FrontendLimitationScope::Retry),
        (runner.attribution.phase, FrontendLimitationScope::Phase),
        (runner.attribution.action, FrontendLimitationScope::Action),
        (
            runner.attribution.assertion,
            FrontendLimitationScope::Assertion,
        ),
    ] {
        if precision != AttributionPrecision::Exact && !limited_scopes.contains(&scope) {
            return Err(FrontendDeclarationError::MissingDowngradeLimitation(scope));
        }
    }
    if runner.attribution.assertion == AttributionPrecision::Exact
        && (runner.attribution.test != AttributionPrecision::Exact
            || runner.attribution.phase != AttributionPrecision::Exact)
    {
        return Err(FrontendDeclarationError::ExactAssertionRequiresExactTestAndPhase);
    }
    if runner.attribution.action == AttributionPrecision::Exact
        && (runner.attribution.test != AttributionPrecision::Exact
            || runner.attribution.phase != AttributionPrecision::Exact)
    {
        return Err(FrontendDeclarationError::ExactActionRequiresExactTestAndPhase);
    }
    if runner.execution_model == ExecutionModel::ParallelUnattributed
        && [
            runner.attribution.test,
            runner.attribution.retry,
            runner.attribution.phase,
            runner.attribution.action,
            runner.attribution.assertion,
        ]
        .contains(&AttributionPrecision::Exact)
    {
        return Err(FrontendDeclarationError::ParallelUnattributedCannotClaimExactCausality);
    }
    Ok(())
}

impl std::error::Error for FrontendDeclarationError {}

fn valid_frontend_token(value: &str) -> bool {
    (1..=64).contains(&value.len())
        && value.as_bytes()[0].is_ascii_lowercase()
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'+' | b'-')
        })
}

fn valid_frontend_runner_token(value: &str) -> bool {
    (1..=64).contains(&value.len())
        && value.as_bytes()[0].is_ascii_lowercase()
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase()
                || byte.is_ascii_digit()
                || matches!(byte, b'.' | b'+' | b'-' | b':')
        })
}

fn valid_structural_limitation_reference(value: &str) -> bool {
    (1..=512).contains(&value.len())
        && value.trim().len() == value.len()
        && !value.chars().any(char::is_control)
}

pub fn validate_frontend_run_declaration(
    declaration: &FrontendRunDeclaration,
) -> Result<(), FrontendDeclarationError> {
    if declaration.protocol_version != LANGUAGE_FRONTEND_PROTOCOL_VERSION {
        return Err(FrontendDeclarationError::UnsupportedProtocolVersion(
            declaration.protocol_version,
        ));
    }
    for (field, value) in [
        ("ID", declaration.frontend_id.as_str()),
        ("version", declaration.frontend_version.as_str()),
        ("language", declaration.language.as_str()),
    ] {
        if !valid_frontend_token(value) {
            return Err(FrontendDeclarationError::InvalidToken(field));
        }
    }
    if declaration.runners.is_empty() {
        return Err(FrontendDeclarationError::NoRunners);
    }
    let mut limitation_ids = BTreeSet::new();
    for limitation in &declaration.structural_limitations {
        if !valid_structural_limitation_reference(limitation) {
            return Err(FrontendDeclarationError::InvalidToken(
                "structural limitation ID",
            ));
        }
        if !limitation_ids.insert(limitation.clone()) {
            return Err(FrontendDeclarationError::DuplicateStructuralLimitation(
                limitation.clone(),
            ));
        }
    }
    let mut runners = BTreeSet::new();
    // There is a runner at least: none was refused above.
    declaration.runners.iter().try_for_each(|runner| {
        if !runners.insert(runner.runner.clone()) {
            return Err(FrontendDeclarationError::DuplicateRunner(
                runner.runner.clone(),
            ));
        }
        validate_frontend_runner(runner, &mut limitation_ids)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checked_in_registry_matches_rust_constants() {
        let contract = registry().expect("contract registry must be valid JSON");
        assert_eq!(contract.contract_version, CONTRACT_VERSION);
        assert_eq!(contract.status, "frozen");
        assert!(!contract.resident_process);
        assert_eq!(
            contract.evidence_archive.schema_version,
            EVIDENCE_ARCHIVE_SCHEMA_VERSION
        );
        assert_eq!(contract.evidence_archive.magic, EVIDENCE_ARCHIVE_MAGIC);
        assert_eq!(
            contract.agent_json.schema_version,
            AGENT_JSON_SCHEMA_VERSION
        );
        assert_eq!(contract.agent_json.max_bytes, AGENT_JSON_MAX_BYTES);
        assert_eq!(contract.agent_json.default_page_size, DEFAULT_PAGE_SIZE);
        assert_eq!(contract.agent_json.error_codes, ERROR_CODES);
        assert_eq!(
            contract.process_supervision.schema_version,
            PROCESS_SUPERVISION_SCHEMA_VERSION
        );
        assert_eq!(
            contract.process_supervision.diagnostic_interval_ms,
            DEFAULT_DIAGNOSTIC_INTERVAL_MS
        );
        assert_eq!(
            contract.process_supervision.timeout_exit_code,
            COMMAND_TIMEOUT_EXIT_CODE
        );
        assert_eq!(
            contract.process_supervision.termination_grace_ms,
            COMMAND_TERMINATION_GRACE_MS
        );
    }

    #[test]
    fn checked_in_probe_v2_contract_matches_rust_constants() {
        let contract = probe_v2_contract().expect("probe v2 contract must be valid JSON");
        assert_eq!(contract.probe_version, PROBE_V2_VERSION);
        assert_eq!(contract.semantics, "frozen");
        assert_eq!(contract.implementation, "experimental");
        assert_eq!(contract.decision_encoding.radix, PROBE_V2_RADIX);
        assert_eq!(contract.decision_encoding.digits.unreached, 0);
        assert_eq!(contract.decision_encoding.digits.r#false, 1);
        assert_eq!(contract.decision_encoding.digits.r#true, 2);
        assert!(contract.decision_encoding.outcome_stored_separately);
        assert_eq!(
            contract
                .decision_encoding
                .javascript_maximum_encoded_conditions,
            PROBE_V2_JS_MAX_CONDITIONS
        );
        assert_eq!(contract.promotion.realistic_median_runtime_ratio_max, 1.10);
    }

    fn exact_frontend_declaration() -> FrontendRunDeclaration {
        FrontendRunDeclaration {
            protocol_version: LANGUAGE_FRONTEND_PROTOCOL_VERSION,
            frontend_id: "javascript".into(),
            frontend_version: "javascript-v1".into(),
            language: "javascript".into(),
            structural_source: StructuralSource::OwnedProbes,
            runners: vec![FrontendRunnerDeclaration {
                runner: "playwright".into(),
                execution_model: ExecutionModel::ParallelContextPropagated,
                attribution: FrontendAttribution {
                    run: AttributionPrecision::Exact,
                    worker: AttributionPrecision::Exact,
                    test: AttributionPrecision::Exact,
                    retry: AttributionPrecision::Exact,
                    phase: AttributionPrecision::Exact,
                    action: AttributionPrecision::Exact,
                    assertion: AttributionPrecision::Exact,
                },
                limitations: Vec::new(),
            }],
            structural_limitations: Vec::new(),
        }
    }

    #[test]
    fn checked_in_language_frontend_contract_matches_rust_types() {
        let contract = language_frontend_protocol_contract()
            .expect("language frontend protocol must be valid JSON");
        assert_eq!(
            contract.frontend_protocol_version,
            LANGUAGE_FRONTEND_PROTOCOL_VERSION
        );
        assert_eq!(contract.status, "frozen");
        assert_eq!(contract.manifest_model, "coverage-manifest-v1");
        assert_eq!(contract.observation_model, "evidence-archive-v3");
        assert_eq!(contract.probe_model, "ternary-decision-v2");
        assert!(
            contract
                .requirements
                .selected_unstarted_has_test_identity_only
        );
        assert_eq!(
            contract.identity_axes,
            ["run", "worker", "test", "retry", "phase"]
        );
        assert_eq!(
            contract.structural_sources,
            [
                StructuralSource::OwnedProbes,
                StructuralSource::NativeImport,
                StructuralSource::Mixed,
            ]
            .map(|value| serde_json::to_value(value)
                .unwrap()
                .as_str()
                .unwrap()
                .to_owned())
        );
        assert_eq!(
            contract.execution_models,
            [
                ExecutionModel::ProcessPerTest,
                ExecutionModel::SerialInProcess,
                ExecutionModel::ParallelContextPropagated,
                ExecutionModel::ParallelUnattributed,
            ]
            .map(|value| serde_json::to_value(value)
                .unwrap()
                .as_str()
                .unwrap()
                .to_owned())
        );
        assert_eq!(
            contract.attribution_precisions,
            [
                AttributionPrecision::Exact,
                AttributionPrecision::Aggregate,
                AttributionPrecision::Unavailable,
            ]
            .map(|value| serde_json::to_value(value)
                .unwrap()
                .as_str()
                .unwrap()
                .to_owned())
        );
        assert_eq!(
            contract.transition_kinds,
            [
                FrontendTransitionKind::Setup,
                FrontendTransitionKind::Test,
                FrontendTransitionKind::Action,
                FrontendTransitionKind::Assertion,
                FrontendTransitionKind::Teardown,
                FrontendTransitionKind::Background,
            ]
            .map(|value| serde_json::to_value(value)
                .unwrap()
                .as_str()
                .unwrap()
                .to_owned())
        );
        assert_eq!(
            contract.limitation_scopes,
            [
                FrontendLimitationScope::Worker,
                FrontendLimitationScope::Test,
                FrontendLimitationScope::Retry,
                FrontendLimitationScope::Phase,
                FrontendLimitationScope::Action,
                FrontendLimitationScope::Assertion,
            ]
            .map(|value| serde_json::to_value(value)
                .unwrap()
                .as_str()
                .unwrap()
                .to_owned())
        );
        assert!(
            !contract
                .requirements
                .timestamp_attribution_may_claim_causality
        );
        assert!(!contract.requirements.frontend_may_compute_coverage_verdicts);
        assert!(contract.requirements.multiple_runners_per_frontend);
        assert!(
            contract
                .requirements
                .structural_limitations_reference_manifest_ids
        );
        assert!(contract.requirements.attribution_limitations_runner_scoped);
        assert!(contract.requirements.complete_manifest_before_execution);
        assert!(contract.requirements.unknown_obligation_fatal);
        assert!(contract.requirements.identity_downgrade_requires_limitation);
        assert!(contract.requirements.unknown_phase_reference_fatal);
        assert!(contract.requirements.phase_causality_acyclic);
        assert!(contract.requirements.engine_owns_manifest_merge);
        assert!(contract.requirements.engine_owns_evidence_validation);
        assert!(contract.requirements.engine_owns_attribution_merge);
        assert!(contract.requirements.engine_owns_coverage_analysis);
        assert!(contract.requirements.engine_owns_persistence_and_queries);
    }

    #[test]
    fn validates_exact_and_explicitly_degraded_frontends() {
        validate_frontend_run_declaration(&exact_frontend_declaration()).unwrap();
        let mut degraded = exact_frontend_declaration();
        degraded.runners[0].runner = "opaque-runner".into();
        degraded.runners[0].attribution.action = AttributionPrecision::Unavailable;
        degraded.runners[0].limitations.push(FrontendLimitation {
            id: "no-action-lifecycle".into(),
            scopes: vec![FrontendLimitationScope::Action],
            reason: "The runner exposes assertions but no action lifecycle".into(),
        });
        validate_frontend_run_declaration(&degraded).unwrap();
    }

    #[test]
    fn accepts_the_canonical_node_test_runner_name() {
        let mut declaration = exact_frontend_declaration();
        declaration.runners[0].runner = "node:test".into();
        validate_frontend_run_declaration(&declaration).unwrap();
        declaration.runners[0].runner = "vitest2".into();
        validate_frontend_run_declaration(&declaration).unwrap();
    }

    #[test]
    fn rejects_unexplained_or_internally_impossible_attribution_claims() {
        let mut unexplained = exact_frontend_declaration();
        unexplained.runners[0].attribution.assertion = AttributionPrecision::Unavailable;
        assert_eq!(
            validate_frontend_run_declaration(&unexplained),
            Err(FrontendDeclarationError::MissingDowngradeLimitation(
                FrontendLimitationScope::Assertion
            ))
        );

        let mut impossible = exact_frontend_declaration();
        impossible.runners[0].execution_model = ExecutionModel::ParallelUnattributed;
        assert_eq!(
            validate_frontend_run_declaration(&impossible),
            Err(FrontendDeclarationError::ParallelUnattributedCannotClaimExactCausality)
        );

        let mut assertion_without_test = exact_frontend_declaration();
        assertion_without_test.runners[0].attribution.test = AttributionPrecision::Aggregate;
        assertion_without_test.runners[0].attribution.retry = AttributionPrecision::Aggregate;
        assertion_without_test.runners[0].attribution.phase = AttributionPrecision::Aggregate;
        assertion_without_test.runners[0]
            .limitations
            .push(FrontendLimitation {
                id: "aggregate-tests".into(),
                scopes: vec![
                    FrontendLimitationScope::Test,
                    FrontendLimitationScope::Retry,
                    FrontendLimitationScope::Phase,
                ],
                reason: "The runner pools concurrent test observations".into(),
            });
        assert_eq!(
            validate_frontend_run_declaration(&assertion_without_test),
            Err(FrontendDeclarationError::ExactAssertionRequiresExactTestAndPhase)
        );
    }

    #[test]
    fn supports_multiple_runners_but_rejects_duplicate_runner_claims() {
        let mut declaration = exact_frontend_declaration();
        let mut vitest = declaration.runners[0].clone();
        vitest.runner = "vitest".into();
        declaration.runners.push(vitest.clone());
        validate_frontend_run_declaration(&declaration).unwrap();
        declaration.runners.push(vitest);
        assert_eq!(
            validate_frontend_run_declaration(&declaration),
            Err(FrontendDeclarationError::DuplicateRunner("vitest".into()))
        );
    }

    #[test]
    fn keeps_structural_limitation_references_unique() {
        let mut declaration = exact_frontend_declaration();
        declaration
            .structural_limitations
            .push("dynamic-python".into());
        validate_frontend_run_declaration(&declaration).unwrap();
        declaration
            .structural_limitations
            .push("dynamic-python".into());
        assert_eq!(
            validate_frontend_run_declaration(&declaration),
            Err(FrontendDeclarationError::DuplicateStructuralLimitation(
                "dynamic-python".into()
            ))
        );
    }

    #[test]
    fn declaration_json_rejects_unknown_fields() {
        let mut value = serde_json::to_value(exact_frontend_declaration()).unwrap();
        value["verdict"] = serde_json::json!({ "mcdc": 100 });
        assert!(serde_json::from_value::<FrontendRunDeclaration>(value).is_err());
    }

    #[test]
    fn checked_in_frontend_examples_are_strict_and_valid() {
        for source in [
            include_str!("../assets/frontend-v2/examples/javascript-mixed-runners.json"),
            include_str!("../assets/frontend-v2/examples/python-pytest-xdist.json"),
        ] {
            let declaration: FrontendRunDeclaration = serde_json::from_str(source).unwrap();
            validate_frontend_run_declaration(&declaration).unwrap();
            assert_eq!(
                serde_json::from_str::<serde_json::Value>(source).unwrap(),
                serde_json::to_value(declaration).unwrap()
            );
        }
    }

    #[test]
    fn python_coverage_import_contract_keeps_the_oracle_at_the_fact_boundary() {
        let contract = python_coverage_import_contract().unwrap();
        assert_eq!(contract.schema_version, 1);
        assert_eq!(contract.status, "private-spike");
        assert_eq!(contract.producer, "coverage.py");
        assert_eq!(
            contract.supported_collector_cores_for_exact_contexts,
            ["ctrace", "pytrace"]
        );
        assert!(contract.requires_branch_measurement);
        assert_eq!(contract.database_access, "forbidden");
        assert!(!contract.frontend_computes_verdicts);
        assert!(contract.unknown_fields_fatal);
        assert!(contract.preserve_unrecognized_contexts_as_background);
        assert_eq!(
            contract.mcdc_availability,
            "unavailable-with-blocking-limitation"
        );
        assert_eq!(
            contract.column_locations,
            "unavailable-with-blocking-limitation"
        );
    }

    #[test]
    fn evidence_v3_is_the_frozen_language_bound_archive() {
        let contract = evidence_v3_contract().unwrap();
        assert_eq!(contract.schema_version, EVIDENCE_ARCHIVE_SCHEMA_VERSION);
        assert_eq!(contract.status, "frozen");
        assert_eq!(contract.magic, EVIDENCE_ARCHIVE_MAGIC);
        assert_eq!(contract.framing, "canonical-sorted-length-framed-gzip");
        assert_eq!(
            contract.required_entries,
            ["coverage-model.json", "frontend.json", "manifest.json"]
        );
        assert_eq!(
            contract.frontend_protocol_version,
            LANGUAGE_FRONTEND_PROTOCOL_VERSION
        );
        assert_eq!(
            contract.coverage_model_schema_version,
            COVERAGE_MODEL_SCHEMA_VERSION
        );
        assert!(contract.unknown_frontend_fields_fatal);
        assert!(contract.unknown_coverage_model_fields_fatal);
        assert!(contract.frontend_language_must_match_coverage_model);
        assert!(contract.malformed_recognized_jsonl_fatal);
        assert!(contract.recognized_jsonl_requires_final_newline);
        assert_eq!(EVIDENCE_ARCHIVE_SCHEMA_VERSION, 3);
        assert_eq!(EVIDENCE_ARCHIVE_MAGIC, "SUPERCOV-EVIDENCE-3\n");
    }

    #[test]
    fn coverage_model_v1_contract_is_frozen_and_bounded() {
        let contract = coverage_model_v1_contract().unwrap();
        assert_eq!(contract.schema_version, COVERAGE_MODEL_SCHEMA_VERSION);
        assert_eq!(contract.status, "frozen");
        assert_eq!(contract.persisted_entry, "coverage-model.json");
        assert_eq!(
            contract.required_fields,
            [
                "schemaVersion",
                "language",
                "variant",
                "name",
                "completenessMeaning",
                "measured",
                "notMeasured",
            ]
        );
        assert!(contract.unknown_fields_fatal);
        assert!(contract.frontend_language_must_match);
        assert!(contract.measured_must_be_nonempty);
        assert!(contract.surface_lists_must_be_unique);
        assert!(contract.surface_lists_must_be_disjoint);
        assert!(contract.strings_must_be_trimmed_single_line);
        assert_eq!(contract.max_identifier_bytes, 64);
        assert_eq!(contract.max_description_bytes, 4096);
        assert_eq!(contract.max_surfaces_per_list, 256);
    }

    #[test]
    fn rust_coverage_v1_contract_fixes_the_complete_target_model() {
        let contract = rust_coverage_v1_contract().unwrap();
        assert_eq!(contract.model_version, 1);
        assert_eq!(contract.status, "frozen-private-frontend");
        assert_eq!(contract.language, "rust");
        assert_eq!(contract.variant, "rust-source-v1");
        assert_eq!(contract.decision_semantics, "masking-mcdc");
        assert_eq!(contract.condition_order, "source-evaluation-order");
        assert_eq!(contract.probe_model, "ternary-decision-v2");
        assert_eq!(contract.source_identity.version, 1);
        assert_eq!(contract.source_identity.digest, "sha256");
        assert_eq!(contract.source_identity.id_digest_bytes, 12);
        assert_eq!(contract.source_identity.separator, "nul");
        assert!(
            contract
                .source_identity
                .repeated_authored_expansions_aggregate
        );
        assert!(
            contract
                .source_identity
                .distinct_synthetic_invocations_remain_distinct
        );
        assert!(contract.source_identity.ephemeral_paths_forbidden);
        assert_eq!(contract.source_identity.collision_policy, "fatal");
        assert_eq!(contract.test_context_identity.version, 1);
        assert_eq!(contract.test_context_identity.algorithm, "fnv1a-64");
        assert_eq!(
            contract.test_context_identity.domain,
            "supercov-rust-test-v1\0"
        );
        assert_eq!(
            contract.test_context_identity.collision_policy,
            "fatal-before-launch"
        );
        assert_eq!(contract.runner_attempt_identity.version, 1);
        assert_eq!(contract.runner_attempt_identity.cargo_test.retry, 0);
        assert_eq!(
            contract.runner_attempt_identity.cargo_test.total_attempts,
            1
        );
        assert_eq!(contract.runner_attempt_identity.rustdoc.retry, 0);
        assert_eq!(contract.runner_attempt_identity.rustdoc.total_attempts, 1);
        assert_eq!(
            contract.libtest_event_transport.protocol_version,
            RUST_LIBTEST_EVENT_PROTOCOL_VERSION
        );
        assert_eq!(
            contract.libtest_event_transport.magic,
            RUST_LIBTEST_EVENT_MAGIC
        );
        assert_eq!(
            contract.libtest_event_transport.header_size,
            RUST_LIBTEST_EVENT_HEADER_SIZE
        );
        assert_eq!(
            contract.libtest_event_transport.record_header_size,
            RUST_LIBTEST_EVENT_RECORD_HEADER_SIZE
        );
        assert_eq!(
            contract.libtest_event_transport.token_size,
            RUST_LIBTEST_EVENT_TOKEN_SIZE
        );
        assert_eq!(
            contract.libtest_event_transport.maximum_name_bytes,
            RUST_LIBTEST_EVENT_MAX_NAME_BYTES
        );
        assert_eq!(
            contract.libtest_event_transport.event_kinds,
            ["filtered-out", "filtered", "started", "timeout", "finished"]
        );
        assert_eq!(
            contract.libtest_event_transport.terminal_results,
            ["passed", "failed", "ignored", "benchmarked"]
        );
        assert_eq!(
            contract.libtest_event_transport.process_model,
            "one-stock-libtest-process-per-artifact"
        );
        assert_eq!(
            contract.libtest_event_transport.output_authority,
            "unmodified-selected-toolchain-libtest"
        );
        assert_eq!(
            contract
                .libtest_event_transport
                .artifact_binding
                .schema_version,
            RUST_LIBTEST_COMPANION_BUNDLE_SCHEMA_VERSION
        );
        assert_eq!(
            contract
                .libtest_event_transport
                .artifact_binding
                .required_bindings,
            [
                "compilerCompanionBuildId",
                "rustcCommitHash",
                "hostTriple",
                "eventProtocolVersion",
                "originalSourceSha256",
                "eventRuntimeSha256",
                "patchedSourceSha256",
                "artifactSha256",
            ]
        );
        assert!(
            contract
                .libtest_event_transport
                .artifact_binding
                .unknown_fields_fatal
        );
        assert_eq!(
            contract.runner_attempt_identity.nextest.minimum_version,
            "0.9.138"
        );
        assert_eq!(
            contract.runner_attempt_identity.nextest.maximum_version,
            "0.9.140"
        );
        assert_eq!(
            contract
                .runner_attempt_identity
                .nextest
                .verified_released_versions,
            ["0.9.138", "0.9.140"]
        );
        assert_eq!(
            contract.runner_attempt_identity.nextest.execution_mode,
            "process-per-test"
        );
        assert_eq!(
            contract
                .runner_attempt_identity
                .nextest
                .identity_environment,
            [
                "NEXTEST",
                "NEXTEST_RUN_ID",
                "NEXTEST_VERSION",
                "NEXTEST_EXECUTION_MODE",
                "NEXTEST_BINARY_ID",
                "NEXTEST_TEST_NAME",
                "NEXTEST_ATTEMPT",
                "NEXTEST_TOTAL_ATTEMPTS",
                "NEXTEST_ATTEMPT_ID",
                "NEXTEST_STRESS_CURRENT",
                "NEXTEST_STRESS_TOTAL",
            ]
        );
        assert_eq!(
            contract.runner_attempt_identity.nextest.stress_iteration,
            "distinct-axis-unsupported-fail-closed"
        );
        assert_eq!(
            contract
                .runner_attempt_identity
                .nextest
                .selected_but_unstarted,
            "logical-test-identity-only"
        );
        assert_eq!(
            contract
                .runner_attempt_identity
                .nextest
                .selection_projection,
            "exact-pre-and-post-separator"
        );
        assert_eq!(
            contract.runner_attempt_identity.nextest.concurrent_attempts,
            "distinct-durable-ordinal-and-attempt-id"
        );
        assert_eq!(
            contract.runner_attempt_identity.nextest.target_runner_death,
            "unmatched-durable-reservation-fatal"
        );
        assert!(
            contract
                .source_identity
                .authored_canonical_fields
                .iter()
                .any(|field| field == "semantic-discriminator")
        );
        assert!(
            contract
                .source_identity
                .synthetic_expansion_canonical_fields
                .iter()
                .any(|field| field == "owner-local-ordinal")
        );
        assert_eq!(
            contract.required_identity_axes,
            ["run", "worker", "test", "retry", "phase"]
        );
        for surface in [
            "authored-source",
            "declarative-macro-expansion",
            "procedural-macro-expansion",
            "derive-expansion",
            "build-script-generated-source",
            "included-source",
            "const-evaluation",
            "doctest-source",
        ] {
            assert!(
                contract
                    .required_owned_surfaces
                    .iter()
                    .any(|item| item == surface)
            );
        }
        assert!(!contract.external_coverage_in_product);
    }
    /// Each rule of a frontend declaration is enforced on its own: a
    /// declaration that breaks exactly one is refused for that one.
    #[test]
    fn a_declaration_that_breaks_one_rule_is_refused_for_it() {
        use AttributionPrecision::{Aggregate, Unavailable};
        use FrontendLimitationScope as Scope;
        validate_frontend_run_declaration(&exact_frontend_declaration()).unwrap();
        let limitation = |id: &str, scopes: &[Scope]| FrontendLimitation {
            id: id.into(),
            scopes: scopes.to_vec(),
            reason: "The runner cannot say".into(),
        };
        type Edit = Box<dyn Fn(&mut FrontendRunDeclaration)>;
        let runner = |edit: fn(&mut FrontendRunnerDeclaration)| -> Edit {
            Box::new(move |declaration: &mut FrontendRunDeclaration| {
                edit(&mut declaration.runners[0])
            })
        };
        let cases: Vec<(FrontendDeclarationError, Edit)> = vec![
            (
                FrontendDeclarationError::UnsupportedProtocolVersion(99),
                Box::new(|d| d.protocol_version = 99),
            ),
            (
                FrontendDeclarationError::InvalidToken("ID"),
                Box::new(|d| d.frontend_id = "Java Script".into()),
            ),
            // A token starts with a lowercase letter and holds no space; the
            // runner's colon is not allowed in a frontend's ID either.
            (
                FrontendDeclarationError::InvalidToken("ID"),
                Box::new(|d| d.frontend_id = "java script".into()),
            ),
            (
                FrontendDeclarationError::InvalidToken("ID"),
                Box::new(|d| d.frontend_id = "node:test".into()),
            ),
            (
                FrontendDeclarationError::InvalidToken("version"),
                Box::new(|d| d.frontend_version = String::new()),
            ),
            (
                FrontendDeclarationError::InvalidToken("language"),
                Box::new(|d| d.language = "x".repeat(65)),
            ),
            (
                FrontendDeclarationError::NoRunners,
                Box::new(|d| d.runners.clear()),
            ),
            (
                FrontendDeclarationError::InvalidToken("structural limitation ID"),
                Box::new(|d| d.structural_limitations = vec![String::new()]),
            ),
            (
                FrontendDeclarationError::InvalidToken("structural limitation ID"),
                Box::new(|d| d.structural_limitations = vec![" dynamic".into()]),
            ),
            (
                FrontendDeclarationError::InvalidToken("structural limitation ID"),
                Box::new(|d| d.structural_limitations = vec!["dyn\tamic".into()]),
            ),
            (
                FrontendDeclarationError::DuplicateStructuralLimitation("dynamic".into()),
                Box::new(|d| d.structural_limitations = vec!["dynamic".into(), "dynamic".into()]),
            ),
            (
                FrontendDeclarationError::DuplicateRunner("playwright".into()),
                Box::new(|d| {
                    let copy = d.runners[0].clone();
                    d.runners.push(copy);
                }),
            ),
            (
                FrontendDeclarationError::InvalidToken("runner"),
                runner(|r| r.runner = "Play wright".into()),
            ),
            (
                FrontendDeclarationError::InvalidToken("runner"),
                runner(|r| r.runner = "play wright".into()),
            ),
            (
                FrontendDeclarationError::InvalidToken("runner"),
                runner(|r| r.runner = String::new()),
            ),
            (
                FrontendDeclarationError::RunIdentityNotExact,
                runner(|r| r.attribution.run = Aggregate),
            ),
            (
                FrontendDeclarationError::InvalidToken("limitation ID"),
                Box::new(move |d| {
                    d.runners[0].limitations = vec![limitation("", &[Scope::Action])]
                }),
            ),
            (
                FrontendDeclarationError::DuplicateLimitation("one".into()),
                Box::new(move |d| {
                    d.runners[0].limitations = vec![
                        limitation("one", &[Scope::Action]),
                        limitation("one", &[Scope::Test]),
                    ]
                }),
            ),
            (
                FrontendDeclarationError::EmptyLimitationScopes("one".into()),
                Box::new(move |d| d.runners[0].limitations = vec![limitation("one", &[])]),
            ),
            (
                FrontendDeclarationError::DuplicateLimitationScope("one".into()),
                Box::new(move |d| {
                    d.runners[0].limitations =
                        vec![limitation("one", &[Scope::Action, Scope::Action])]
                }),
            ),
            (
                FrontendDeclarationError::InvalidLimitationReason("one".into()),
                Box::new(move |d| {
                    let mut bad = limitation("one", &[Scope::Action]);
                    bad.reason = " padded".into();
                    d.runners[0].limitations = vec![bad];
                }),
            ),
            (
                FrontendDeclarationError::InvalidLimitationReason("one".into()),
                Box::new(move |d| {
                    let mut bad = limitation("one", &[Scope::Action]);
                    bad.reason = String::new();
                    d.runners[0].limitations = vec![bad];
                }),
            ),
            (
                FrontendDeclarationError::InvalidLimitationReason("one".into()),
                Box::new(move |d| {
                    let mut bad = limitation("one", &[Scope::Action]);
                    bad.reason = "two\nlines".into();
                    d.runners[0].limitations = vec![bad];
                }),
            ),
            (
                FrontendDeclarationError::ExactRetryRequiresExactTest,
                Box::new(move |d| {
                    d.runners[0].attribution.test = Aggregate;
                    d.runners[0].limitations = vec![limitation("t", &[Scope::Test])];
                }),
            ),
            (
                FrontendDeclarationError::ExactPhaseRequiresExactTest,
                Box::new(move |d| {
                    d.runners[0].attribution.test = Aggregate;
                    d.runners[0].attribution.retry = Unavailable;
                    d.runners[0].limitations = vec![limitation("t", &[Scope::Test, Scope::Retry])];
                }),
            ),
            (
                FrontendDeclarationError::MissingDowngradeLimitation(Scope::Worker),
                runner(|r| r.attribution.worker = Aggregate),
            ),
            (
                FrontendDeclarationError::ExactAssertionRequiresExactTestAndPhase,
                Box::new(move |d| {
                    d.runners[0].attribution.phase = Aggregate;
                    d.runners[0].limitations = vec![limitation("p", &[Scope::Phase])];
                }),
            ),
            (
                FrontendDeclarationError::ExactActionRequiresExactTestAndPhase,
                Box::new(move |d| {
                    d.runners[0].attribution.phase = Aggregate;
                    d.runners[0].attribution.assertion = Unavailable;
                    d.runners[0].limitations =
                        vec![limitation("p", &[Scope::Phase, Scope::Assertion])];
                }),
            ),
            // An exact action needs an exact test as well, whatever the phase.
            (
                FrontendDeclarationError::ExactActionRequiresExactTestAndPhase,
                Box::new(move |d| {
                    d.runners[0].attribution.test = Aggregate;
                    d.runners[0].attribution.retry = Aggregate;
                    d.runners[0].attribution.phase = Aggregate;
                    d.runners[0].attribution.assertion = Unavailable;
                    d.runners[0].limitations = vec![limitation(
                        "t",
                        &[Scope::Test, Scope::Retry, Scope::Phase, Scope::Assertion],
                    )];
                }),
            ),
            (
                FrontendDeclarationError::ParallelUnattributedCannotClaimExactCausality,
                runner(|r| r.execution_model = ExecutionModel::ParallelUnattributed),
            ),
        ];
        for (expected, edit) in &cases {
            let mut declaration = exact_frontend_declaration();
            edit(&mut declaration);
            assert_eq!(
                validate_frontend_run_declaration(&declaration),
                Err(expected.clone()),
                "{expected:?}"
            );
        }
    }

    #[test]
    fn each_declaration_error_says_what_failed() {
        use super::{FrontendDeclarationError as E, FrontendLimitationScope};
        let messages = [
            E::UnsupportedProtocolVersion(9),
            E::InvalidToken("name"),
            E::NoRunners,
            E::DuplicateRunner("jest".into()),
            E::RunIdentityNotExact,
            E::DuplicateLimitation("l".into()),
            E::DuplicateLimitationScope("l".into()),
            E::EmptyLimitationScopes("l".into()),
            E::InvalidLimitationReason("l".into()),
            E::DuplicateStructuralLimitation("l".into()),
            E::MissingDowngradeLimitation(FrontendLimitationScope::Test),
            E::ExactRetryRequiresExactTest,
            E::ExactPhaseRequiresExactTest,
            E::ExactAssertionRequiresExactTestAndPhase,
            E::ExactActionRequiresExactTestAndPhase,
            E::ParallelUnattributedCannotClaimExactCausality,
        ]
        .map(|error| error.to_string());
        assert_eq!(
            messages,
            [
                "unsupported language-frontend protocol version: 9",
                "invalid frontend name",
                "frontend declaration has no runners",
                "duplicate frontend runner: jest",
                "frontend run identity must be exact",
                "duplicate frontend limitation: l",
                "frontend limitation has duplicate scopes: l",
                "frontend limitation has no scopes: l",
                "frontend limitation has an invalid reason: l",
                "duplicate structural limitation reference: l",
                "non-exact Test attribution has no matching limitation",
                "exact retry attribution requires exact test identity",
                "exact phase attribution requires exact test identity",
                "exact assertion attribution requires exact test and phase identity",
                "exact action attribution requires exact test and phase identity",
                "parallel-unattributed execution cannot claim exact test, retry, phase, action, or assertion causality",
            ]
        );
    }
}
