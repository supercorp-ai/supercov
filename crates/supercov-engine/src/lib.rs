//! Rust engine shell.
//!
//! Modules land here only after their reference behavior is represented in the
//! shared contract/differential corpus. The JavaScript instrumenter is not
//! ported until probe-v2 semantics are frozen.

pub mod agent_json;
pub mod probe_v2;

pub use supercov_contracts::CONTRACT_VERSION;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EngineReadiness {
    ContractShell,
    DifferentialCandidate,
    Default,
}

pub const READINESS: EngineReadiness = EngineReadiness::ContractShell;

pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}
