//! Frozen, implementation-neutral Supercov engine contracts.
//!
//! This crate does not contain coverage behavior. It makes contract drift a
//! compile/test failure while the TypeScript reference and Rust candidate
//! coexist.

use serde::{Deserialize, Serialize};

pub const CONTRACT_VERSION: u32 = 1;
pub const EVIDENCE_ARCHIVE_SCHEMA_VERSION: u32 = 2;
pub const EVIDENCE_ARCHIVE_MAGIC: &str = "SUPERCOV-EVIDENCE-2\n";
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
}
