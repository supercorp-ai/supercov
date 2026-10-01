//! The end-to-end gates, run against the binary Cargo built for this test.
//!
//! Each gate in `scripts/` drives `supercov` the way a user does -- a real
//! project, its own test runner, the published run read back -- and is the
//! single source of truth for that scenario. Run from here, the binary it
//! drives is the one this test build produced, so a coverage run of the
//! workspace credits each gate with the code its scenario exercised instead
//! of seeing the gates as invisible processes outside `cargo test`.
//!
//! They need the toolchains the scenarios use and take minutes, so they are
//! ignored by default: `cargo test -p supercov --test e2e_gates --
//! --include-ignored`. Several share fixture directories, and a coverage run
//! gives each test its own process, so they take turns through a file lock
//! rather than a mutex.

use std::fs::File;
use std::path::PathBuf;
use std::process::Command;

fn repository() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("the repository root")
}

fn gate(script: &str) {
    let turn = File::create(std::env::temp_dir().join("supercov-e2e-gates.lock"))
        .expect("the gate lock file");
    turn.lock().expect("a turn at the gates");
    let output = Command::new("node")
        .arg(repository().join("scripts").join(script))
        .current_dir(repository())
        .env("SUPERCOV_BINARY", env!("CARGO_BIN_EXE_supercov"))
        .output()
        .expect("node runs");
    assert!(
        output.status.success(),
        "{script} failed ({})\n--- stdout ---\n{}\n--- stderr ---\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

macro_rules! gates {
    ($($name:ident => $script:literal),* $(,)?) => {
        $(
            #[test]
            #[ignore = "an end-to-end gate: needs its toolchains and takes minutes"]
            fn $name() {
                gate($script);
            }
        )*
    };
}

gates! {
    assertions => "assertions-integration.mjs",
    python => "python-integration.mjs",
    ruby_coverage => "ruby-coverage-integration.mjs",
    rust_public_cargo => "rust-public-cargo-integration.mjs",
    rust_fixture_matrix => "rust-fixture-matrix.mjs",
    rust_syntax_matrix => "rust-syntax-matrix.mjs",
    rust_child_attribution => "rust-child-attribution-integration.mjs",
    rust_host_loader => "rust-host-loader-integration.mjs",
    rust_process_supervision => "rust-process-supervision.mjs",
    isolation => "isolation-integration.mjs",
    watchdog => "watchdog-integration.mjs",
    html_report => "html-report-integration.mjs",
    engine_contract => "engine-contract.mjs",
    agent_commands => "check-agent-commands.mjs",
    workspace_crash => "workspace-crash-integration.mjs",
    rust_direct_node => "rust-direct-node-integration.mjs",
    rust_public_run => "rust-public-run-integration.mjs",
    rust_embedded_runtime => "rust-embedded-runtime-integration.mjs",
    rust_direct_vitest => "rust-direct-vitest-integration.mjs",
    rust_direct_jest => "rust-direct-jest-integration.mjs",
    rust_direct_playwright => "rust-direct-playwright-integration.mjs",
    rust_custom_browser_playwright => "rust-custom-browser-playwright-integration.mjs",
    rust_classic_browser => "rust-classic-browser-integration.mjs",
    rust_top_packages => "rust-top-packages-integration.mjs",
    rust_generic_esbuild => "rust-generic-esbuild-integration.mjs",
    rust_generic_tsc => "rust-generic-tsc-integration.mjs",
    rust_generic_build_matrix => "rust-generic-build-matrix.mjs",
    rust_vite_playwright => "rust-vite-playwright-integration.mjs",
    rust_vitest_projects => "rust-vitest-projects-integration.mjs",
}
