//! Supercov's single coverage engine. Target-language runtime adapters remain
//! thin generated shims; instrumentation, orchestration, analysis and queries
//! are owned here.

// Stored indexes and evidence address lengths and offsets in 64 bits, read
// back as `usize` without a check that could only fail on a narrower target.
// Every target Supercov ships is 64-bit.
#[cfg(not(target_pointer_width = "64"))]
compile_error!("Supercov builds for 64-bit targets only");

// Standard error that cannot be written is not Supercov's failure:
// `supercov -- npm test 2>&1 | head` closes it early, and std's `eprintln!`
// panics then. This writes where it does and passes over a write that fails.
macro_rules! eprintln {
    ($($argument:tt)*) => {{
        use std::io::Write as _;
        let _ = writeln!(std::io::stderr(), $($argument)*);
    }};
}

pub mod agent_json;
pub mod assertion_coverage;
pub mod child_signal_guard;
pub mod contained;
pub mod coverage_analysis;
pub mod coverage_export;
pub mod coverage_html;
pub mod coverage_index;
pub mod coverage_query;
pub mod coverage_report;
pub mod decision_shape;
#[cfg(unix)]
pub mod docker_proxy;
pub mod evidence_archive;
pub mod fault;
pub mod fault_fs;
pub mod frontend_detection;
pub mod frontend_protocol;
pub mod go_instrumenter;
pub mod go_project;
pub mod go_run;
pub mod go_test_harness;
pub mod indexed_query;
pub mod integrity;
pub mod interned;
pub mod javascript_frontend;
pub mod javascript_run;
pub mod js_instrumenter;
pub mod jvm_instrumenter;
pub mod jvm_project;
pub mod jvm_run;
mod kotlin_grammar;
pub mod lifecycle;
pub mod orchestration;
pub mod outside_evidence;
pub mod owned_evidence;
pub mod owned_server;
pub mod patch_view;
pub mod probe_v2;
pub mod process_supervision;
pub mod progress;
pub mod project_discovery;
pub mod python_evidence;
#[cfg(any(test, feature = "oracle-harnesses"))]
pub mod python_frontend;
pub mod python_instrumenter;
pub mod python_project;
pub mod python_run;
pub mod query_index;
pub mod ruby_evidence;
pub mod ruby_instrumenter;
pub mod ruby_project;
pub mod ruby_run;
pub mod run_currency;
pub mod run_merge;
pub mod run_query;
pub mod run_store;
pub mod run_view;
pub mod rust_build_cache;
mod rust_cargo_config_model;
pub mod rust_cargo_configuration;
pub mod rust_instrumenter;
mod rust_libtest_report;
pub mod rust_owned_doctests;
pub mod rust_owned_nextest;
pub mod rust_project;
pub mod rust_run;
pub mod rust_runner_attempt;
pub mod rust_runtime;
pub mod rust_test_context;
pub mod rust_test_runner;
pub mod rust_wasm;
pub mod rust_wasm_browser;
pub mod rust_wasm_evidence;
pub mod rust_wasm_pack;
pub mod rust_worker;
pub mod security_candidates;
pub mod served_tests;
pub mod source_capture;
pub mod source_discovery;
pub mod source_manifest;
pub mod source_tools;
pub mod source_units;
mod stable_ids;
pub mod support_reach;
#[cfg(test)]
mod test_directory;
pub mod test_kind;
pub mod test_loads;
mod threads;
mod typescript_imports;
pub mod workspace;

pub use supercov_contracts::CONTRACT_VERSION;

pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}
