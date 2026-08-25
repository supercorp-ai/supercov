//! Rust coverage engine.
//!
//! The implementation remains a differential candidate until its public CLI
//! and cross-platform cutover gates are complete. The TypeScript engine is a
//! temporary regression reference, not the semantic authority.

pub mod agent_json;
pub mod coverage_analysis;
pub mod coverage_index;
pub mod coverage_query;
pub mod coverage_report;
pub mod coverage_waivers;
pub mod evidence_archive;
pub mod frontend_protocol;
pub mod indexed_query;
pub mod integrity;
pub mod javascript_frontend;
pub mod javascript_run;
pub mod js_instrumenter;
pub mod lifecycle;
pub mod orchestration;
pub mod probe_v2;
pub mod process_supervision;
pub mod project_discovery;
#[cfg(any(test, feature = "oracle-harnesses"))]
pub mod python_frontend;
pub mod query_index;
pub mod run_query;
pub mod run_store;
pub mod source_discovery;
pub mod workspace;

pub use supercov_contracts::CONTRACT_VERSION;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EngineReadiness {
    ContractShell,
    DifferentialCandidate,
    Default,
}

pub const READINESS: EngineReadiness = EngineReadiness::DifferentialCandidate;

pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}
