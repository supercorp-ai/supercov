//! Supercov's single coverage engine. Target-language runtime adapters remain
//! thin generated shims; instrumentation, orchestration, analysis and queries
//! are owned here.

pub mod agent_json;
pub mod build_cache;
pub mod coverage_analysis;
pub mod coverage_index;
pub mod coverage_query;
pub mod coverage_report;
pub mod coverage_waivers;
pub mod evidence_archive;
pub mod frontend_detection;
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
pub mod python_instrumenter;
pub mod query_index;
pub mod run_merge;
pub mod run_query;
pub mod run_store;
pub mod rust_build_cache;
pub mod rust_compiler_evidence;
pub mod rust_compiler_manifest;
pub mod rust_compiler_selection;
pub mod rust_instrumenter;
pub mod rust_phase_projection;
pub mod rust_probe_transport;
pub mod rust_project;
pub mod rust_run;
pub mod rust_runtime;
pub mod rust_test_context;
pub mod rust_test_runner;
pub mod source_discovery;
pub mod workspace;

pub use supercov_contracts::CONTRACT_VERSION;

pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}
