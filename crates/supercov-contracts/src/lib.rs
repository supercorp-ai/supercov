//! Frozen, implementation-neutral Supercov engine contracts.
//!
//! This crate does not contain coverage behavior. It makes contract drift a
//! compile/test failure while the shipped implementation and Rust candidate
//! coexist. Independent specifications and conformance oracles—not either
//! implementation—decide whether a contract is correct.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

pub const CONTRACT_VERSION: u32 = 1;
pub const EVIDENCE_ARCHIVE_SCHEMA_VERSION: u32 = 2;
pub const EVIDENCE_ARCHIVE_MAGIC: &str = "SUPERCOV-EVIDENCE-2\n";
pub const EVIDENCE_ARCHIVE_V3_SCHEMA_VERSION: u32 = 3;
pub const EVIDENCE_ARCHIVE_V3_MAGIC: &str = "SUPERCOV-EVIDENCE-3\n";
pub const COVERAGE_MODEL_SCHEMA_VERSION: u32 = 1;
pub const AGENT_JSON_SCHEMA_VERSION: u32 = 1;
pub const AGENT_JSON_MAX_BYTES: usize = 65_536;
pub const DEFAULT_PAGE_SIZE: usize = 20;
pub const WAIVERS_SCHEMA_VERSION: u32 = 1;
pub const PROCESS_SUPERVISION_SCHEMA_VERSION: u32 = 1;
pub const DEFAULT_DIAGNOSTIC_INTERVAL_MS: u64 = 60_000;
pub const COMMAND_TIMEOUT_EXIT_CODE: i32 = 124;
pub const COMMAND_TERMINATION_GRACE_MS: u64 = 5_000;
pub const PROBE_V2_VERSION: u32 = 2;
pub const PROBE_V2_RADIX: u32 = 3;
pub const PROBE_V2_JS_MAX_CONDITIONS: usize = 32;
pub const LANGUAGE_FRONTEND_PROTOCOL_VERSION: u32 = 2;

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
    pub waivers: WaiverContract,
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
pub struct WaiverContract {
    pub schema_version: u32,
    pub file: String,
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
    serde_json::from_str(include_str!("../../../contracts/v1/contract.json"))
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
    serde_json::from_str(include_str!("../../../contracts/probe-v2/contract.json"))
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
    serde_json::from_str(include_str!("../../../contracts/frontend-v2/contract.json"))
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
    serde_json::from_str(include_str!(
        "../../../contracts/python-coverage-v1/contract.json"
    ))
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
    pub v2_reader_required: bool,
    pub unknown_frontend_fields_fatal: bool,
    pub unknown_coverage_model_fields_fatal: bool,
}

pub fn evidence_v3_contract() -> Result<EvidenceV3Contract, serde_json::Error> {
    serde_json::from_str(include_str!("../../../contracts/evidence-v3/contract.json"))
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
    if !valid_frontend_token(&runner.runner) {
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
    for runner in &declaration.runners {
        if !runners.insert(runner.runner.clone()) {
            return Err(FrontendDeclarationError::DuplicateRunner(
                runner.runner.clone(),
            ));
        }
        validate_frontend_runner(runner, &mut limitation_ids)?;
    }
    Ok(())
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
        assert_eq!(contract.waivers.schema_version, WAIVERS_SCHEMA_VERSION);
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
        assert_eq!(contract.observation_model, "evidence-schema-v2");
        assert_eq!(contract.probe_model, "ternary-decision-v2");
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
            include_str!("../../../contracts/frontend-v2/examples/javascript-mixed-runners.json"),
            include_str!("../../../contracts/frontend-v2/examples/python-pytest-xdist.json"),
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
    fn evidence_v3_requires_language_identity_without_rewriting_v2() {
        let contract = evidence_v3_contract().unwrap();
        assert_eq!(contract.schema_version, EVIDENCE_ARCHIVE_V3_SCHEMA_VERSION);
        assert_eq!(contract.status, "private-candidate");
        assert_eq!(contract.magic, EVIDENCE_ARCHIVE_V3_MAGIC);
        assert_eq!(contract.framing, "evidence-v2-compatible-after-magic");
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
        assert!(contract.v2_reader_required);
        assert!(contract.unknown_frontend_fields_fatal);
        assert!(contract.unknown_coverage_model_fields_fatal);
        assert_eq!(EVIDENCE_ARCHIVE_SCHEMA_VERSION, 2);
        assert_eq!(EVIDENCE_ARCHIVE_MAGIC, "SUPERCOV-EVIDENCE-2\n");
    }
}
