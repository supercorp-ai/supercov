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
//!
//! Each gate names its script by its path from the repository's root, in its
//! own entry: `tests affected` reads what a Rust test names to say which
//! tests a change to a script or a fixture reaches, and a directory every
//! gate named would have been every script for every gate.

use std::fs::File;
use std::path::PathBuf;
use std::process::Command;

fn repository() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("the repository root")
}

fn gate(name: &str, script: &str, arguments: &[&str]) {
    let turn = File::create(std::env::temp_dir().join("supercov-e2e-gates.lock"))
        .expect("the gate lock file");
    turn.lock().expect("a turn at the gates");
    let started = std::time::Instant::now();
    let output = Command::new("node")
        .arg(repository().join(script))
        .args(arguments)
        .current_dir(repository())
        .env("SUPERCOV_BINARY", env!("CARGO_BIN_EXE_supercov"))
        .output()
        .expect("node runs");
    // How long each took, where the caller keeps it: what slices of the
    // gates are balanced by.
    if let Some(path) = std::env::var_os("SUPERCOV_GATE_TIMINGS") {
        use std::io::Write;
        let mut timings = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .expect("the gate timings file");
        writeln!(timings, "{name}\t{}", started.elapsed().as_secs()).expect("a gate timing");
    }
    assert!(
        output.status.success(),
        "{script} {arguments:?} failed ({})\n--- stdout ---\n{}\n--- stderr ---\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

macro_rules! gates {
    ($($name:ident => $script:literal $([$($argument:literal),*])?),* $(,)?) => {
        $(
            #[test]
            #[ignore = "an end-to-end gate: needs its toolchains and takes minutes"]
            fn $name() {
                gate(stringify!($name), $script, &[$($($argument),*)?]);
            }
        )*
    };
}

gates! {
    assertions => "scripts/assertions-integration.mjs",
    python => "scripts/python-integration.mjs",
    ruby_coverage => "scripts/ruby-coverage-integration.mjs",
    rust_public_cargo => "scripts/rust-public-cargo-integration.mjs",
    rust_fixture_matrix => "scripts/rust-fixture-matrix.mjs",
    rust_syntax_matrix => "scripts/rust-syntax-matrix.mjs",
    rust_child_attribution => "scripts/rust-child-attribution-integration.mjs",
    rust_host_loader => "scripts/rust-host-loader-integration.mjs",
    rust_process_supervision => "scripts/rust-process-supervision.mjs",
    isolation => "scripts/isolation-integration.mjs",
    watchdog => "scripts/watchdog-integration.mjs",
    html_report => "scripts/html-report-integration.mjs",
    mutation_report_formats => "scripts/mutation-report-formats.mjs",
    engine_contract => "scripts/engine-contract.mjs",
    agent_commands => "scripts/check-agent-commands.mjs",
    workspace_crash => "scripts/workspace-crash-integration.mjs",
    rust_direct_node => "scripts/rust-direct-node-integration.mjs",
    rust_public_run => "scripts/rust-public-run-integration.mjs",
    rust_embedded_runtime => "scripts/rust-embedded-runtime-integration.mjs",
    rust_direct_vitest => "scripts/rust-direct-vitest-integration.mjs",
    rust_direct_jest => "scripts/rust-direct-jest-integration.mjs",
    rust_direct_playwright => "scripts/rust-direct-playwright-integration.mjs",
    rust_custom_browser_playwright => "scripts/rust-custom-browser-playwright-integration.mjs",
    rust_classic_browser => "scripts/rust-classic-browser-integration.mjs",
    rust_top_packages => "scripts/rust-top-packages-integration.mjs",
    rust_generic_esbuild => "scripts/rust-generic-esbuild-integration.mjs",
    rust_generic_tsc => "scripts/rust-generic-tsc-integration.mjs",
    rust_generic_build_matrix => "scripts/rust-generic-build-matrix.mjs",
    rust_vite_playwright => "scripts/rust-vite-playwright-integration.mjs",
    rust_vitest_projects => "scripts/rust-vitest-projects-integration.mjs",
    opaque_runner => "scripts/opaque-runner-integration.mjs",
    opaque_esm => "scripts/opaque-esm-integration.mjs",
    distributed_merge => "scripts/distributed-merge-integration.mjs",
    generic_build => "scripts/generic-build-integration.mjs",
    next => "scripts/next-integration.mjs",
    node_test => "scripts/node-test-integration.mjs",
    workspaces_attribution => "scripts/workspaces-attribution-integration.mjs",
    agent_query_eval => "scripts/agent-query-eval.mjs",
    background_by_time => "scripts/background-by-time.mjs",
    cross_origin_requests => "scripts/cross-origin-requests.mjs",
    leaving_page => "scripts/leaving-page.mjs",
    js_syntax_zoo => "scripts/js-syntax-zoo.mjs",
    ruby_syntax_zoo => "scripts/ruby-syntax-zoo.mjs",
    rust_syntax_zoo => "scripts/rust-syntax-zoo.mjs",
    python_syntax_zoo => "scripts/python-syntax-zoo.mjs",
    go_syntax_zoo => "scripts/go-syntax-zoo.mjs",
    java_syntax_zoo => "scripts/java-syntax-zoo.mjs",
    shared_page => "scripts/shared-page.mjs",
    context_headers => "scripts/context-headers.mjs",
    context_cookies => "scripts/context-cookies.mjs",
    hidden_request_emit => "scripts/hidden-request-emit.mjs",
    merge_across => "scripts/merge-across.mjs",
    mock_reach => "scripts/mock-reach.mjs",
    generated_inputs => "scripts/generated-inputs.mjs",
    baseline_continuation => "scripts/baseline-continuation.mjs",
    next_playwright => "scripts/next-playwright-integration.mjs",
    react_hydration => "scripts/react-hydration-compatibility.mjs",
    // A sample of every sixteenth test, across the whole corpus and so across
    // the language: the binary here is the instrumented debug build, and the
    // whole corpus runs in the conformance workflow.
    test262_equivalence => "scripts/test262-equivalence.mjs" ["--shard", "1/16"],
    clang_mcdc_oracle => "scripts/clang-mcdc-oracle.mjs",
    mocha_failure_output => "scripts/mocha-failure-output.mjs",
    python_servers => "scripts/python-server-playwright.mjs",
    ruby_servers => "scripts/ruby-server-playwright.mjs",
    go_servers => "scripts/go-server-playwright.mjs",
    jvm_servers => "scripts/jvm-server-playwright.mjs",
    docker_servers => "scripts/docker-server-playwright.mjs",
    docker_clients => "scripts/docker-clients-playwright.mjs",
    docker_exec => "scripts/docker-exec-playwright.mjs",
    rust_region_container => "scripts/rust-region-container.mjs",
    rust_worker_playwright => "scripts/rust-worker-playwright.mjs",
    rust_worker_cargo_test => "scripts/rust-worker-cargo-test.mjs",
    rust_worker_durable_objects => "scripts/rust-worker-durable-objects.mjs",
    rust_wasm_hosts => "scripts/rust-wasm-hosts.mjs",
    rust_wasm_js => "scripts/rust-wasm-js.mjs",
    react_native => "scripts/react-native-compatibility.mjs",
    exec_handover => "scripts/exec-handover.mjs",
    import_line_evidence => "scripts/import-line-evidence.mjs",
    failure_output_node => "scripts/failure-output.mjs" ["node", "ava", "tap"],
    failure_output_python => "scripts/failure-output.mjs" ["python"],
    failure_output_ruby => "scripts/failure-output.mjs" ["ruby"],
    failure_output_go => "scripts/failure-output.mjs" ["go"],
    failure_output_rust => "scripts/failure-output.mjs" ["rust"],
    side_by_side_node => "scripts/side-by-side.mjs" ["node"],
    side_by_side_python => "scripts/side-by-side.mjs" ["python"],
    side_by_side_ruby => "scripts/side-by-side.mjs" ["ruby"],
    side_by_side_go => "scripts/side-by-side.mjs" ["go"],
    side_by_side_rust => "scripts/side-by-side.mjs" ["rust"],
    side_by_side_jvm => "scripts/side-by-side.mjs" ["jvm"],
}
