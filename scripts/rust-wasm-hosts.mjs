// Rust tests compiled to wasm are measured where Cargo runs them.
//
//   node scripts/rust-wasm-hosts.mjs
//
// `cargo test --target <wasm target>` runs each test module through the
// target runner Cargo is configured with, and so does Supercov, test by test:
// - wasm32-unknown-unknown under `wasm-bindgen-test-runner` (what
//   `wasm-pack test --node` does): the module runs in Node, where a hook
//   Supercov gives Node writes each crate's region, held in the module's
//   memory, to the test's evidence as the process ends;
// - wasm32-wasip1 under `wasmtime`: Supercov opens the test's evidence
//   directory to the module, and the runtime writes each first sighting to a
//   file there as it happens.
// Each test must be credited exactly its own lines and decision vectors.
// A host whose tools are missing is skipped and named; a job that installed
// them sets SUPERCOV_REQUIRE_RUST_WASM=1, and then nothing may be skipped.
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { existsSync, mkdirSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, resolve } from 'node:path';

const repository = resolve(import.meta.dirname, '..');
const binary = process.env.SUPERCOV_BINARY ?? resolve(repository, `target/debug/supercov${process.platform === 'win32' ? '.exe' : ''}`);
const root = resolve(tmpdir(), `supercov-rust-wasm-${process.pid}-${Date.now()}`);
const run = (cwd, command, args) => spawnSync(command, args, { cwd, encoding: 'utf8', env: { ...process.env, NO_COLOR: '1' } });
const required = process.env.SUPERCOV_REQUIRE_RUST_WASM === '1';
// Fixtures build with the toolchain they find, outside this repository's pin.
const targets = run(tmpdir(), 'rustup', ['target', 'list', '--installed']).stdout ?? '';
const has = (command, args) => run(tmpdir(), command, args).status === 0;

const grade = `pub fn grade(score: u32, curved: bool) -> &'static str {
    if score >= 90 || (curved && score >= 85) {
        "A"
    } else if score >= 50 {
        "pass"
    } else {
        "fail"
    }
}
`;
const hosts = [
  {
    host: 'Node (wasm-bindgen-test-runner)',
    target: 'wasm32-unknown-unknown',
    available: targets.includes('wasm32-unknown-unknown') && has('wasm-bindgen-test-runner', ['--version']),
    files: {
      'Cargo.toml': '[package]\nname = "grades"\nversion = "0.1.0"\nedition = "2021"\n\n[lib]\ncrate-type = ["cdylib", "rlib"]\n\n[dependencies]\nwasm-bindgen = "=0.2.129"\n\n[dev-dependencies]\nwasm-bindgen-test = "0.3"\n',
      '.cargo/config.toml': '[target.wasm32-unknown-unknown]\nrunner = "wasm-bindgen-test-runner"\n',
      'src/lib.rs': grade,
      'tests/grades.rs': 'use wasm_bindgen_test::*;\n\n#[wasm_bindgen_test]\nfn a_curved_score_is_an_a() {\n    assert_eq!(grades::grade(87, true), "A");\n}\n\n#[wasm_bindgen_test]\nfn a_low_score_fails() {\n    assert_eq!(grades::grade(10, false), "fail");\n}\n',
    },
    tests: { curved: 'tests/grades.rs::a_curved_score_is_an_a', low: 'tests/grades.rs::a_low_score_fails' },
  },
  {
    host: 'WASI (wasmtime)',
    target: 'wasm32-wasip1',
    available: targets.includes('wasm32-wasip1') && has('wasmtime', ['--version']),
    files: {
      'Cargo.toml': '[package]\nname = "grades"\nversion = "0.1.0"\nedition = "2021"\n',
      '.cargo/config.toml': '[target.wasm32-wasip1]\nrunner = "wasmtime"\n',
      'src/lib.rs': grade,
      'tests/grades.rs': '#[test]\nfn a_curved_score_is_an_a() {\n    assert_eq!(grades::grade(87, true), "A");\n}\n\n#[test]\nfn a_low_score_fails() {\n    assert_eq!(grades::grade(10, false), "fail");\n}\n',
    },
    tests: { curved: 'tests/grades.rs::a_curved_score_is_an_a', low: 'tests/grades.rs::a_low_score_fails' },
  },
  {
    // `wasm-pack test --headless --chrome`: wasm-pack picks the driver and
    // the runner, the runner opens Chrome through WebDriver, and Supercov
    // reads each test's page through a driver proxy before its window closes.
    host: 'Chrome (wasm-pack test --headless --chrome)',
    target: 'wasm32-unknown-unknown',
    available: targets.includes('wasm32-unknown-unknown') && has('wasm-pack', ['--version'])
      && (process.env.CHROMEWEBDRIVER !== undefined || existsSync('/Applications/Google Chrome.app')),
    command: ['wasm-pack', 'test', '--headless', '--chrome',
      // A CI runner's Chrome comes with its own matching driver.
      ...(process.env.CHROMEWEBDRIVER ? ['--chromedriver', `${process.env.CHROMEWEBDRIVER}/chromedriver`] : [])],
    files: {
      'Cargo.toml': '[package]\nname = "grades"\nversion = "0.1.0"\nedition = "2021"\n\n[lib]\ncrate-type = ["cdylib", "rlib"]\n\n[dependencies]\nwasm-bindgen = "=0.2.129"\n\n[dev-dependencies]\nwasm-bindgen-test = "0.3"\n',
      // As many projects have it: the runner named for plain `cargo test` too.
      '.cargo/config.toml': '[target.wasm32-unknown-unknown]\nrunner = "wasm-bindgen-test-runner"\n',
      'src/lib.rs': grade,
      'tests/grades.rs': 'use wasm_bindgen_test::*;\nwasm_bindgen_test_configure!(run_in_browser);\n\n#[wasm_bindgen_test]\nfn a_curved_score_is_an_a() {\n    assert_eq!(grades::grade(87, true), "A");\n}\n\n#[wasm_bindgen_test]\nfn a_low_score_fails() {\n    assert_eq!(grades::grade(10, false), "fail");\n}\n',
    },
    tests: { curved: 'tests/grades.rs::a_curved_score_is_an_a', low: 'tests/grades.rs::a_low_score_fails' },
  },
];

const measured = [];
try {
  for (const entry of hosts) {
    if (!entry.available) {
      assert.ok(!required, `${entry.host}: its tools are missing where the job installed them`);
      console.log(`[rust-wasm-hosts] ${entry.host}: skipped, its tools are not here`);
      continue;
    }
    const cwd = resolve(root, entry.host.replace(/\W+/g, '-').toLowerCase());
    for (const [file, text] of Object.entries(entry.files)) {
      mkdirSync(dirname(resolve(cwd, file)), { recursive: true });
      writeFileSync(resolve(cwd, file), text);
    }
    assert.equal(run(cwd, 'git', ['init', '-q']).status, 0);
    const result = run(cwd, binary, ['--', ...(entry.command ?? ['cargo', 'test', '--target', entry.target])]);
    assert.equal(result.status, 0, `${entry.host}\n${result.stdout}${result.stderr}`);
    assert.match(result.stderr, /Rust coverage: 2 test\(s\)/u, `${entry.host}\n${result.stderr}`);
    const lines = (test) => [...new Set(run(cwd, binary, ['runs', 'latest', 'test', test]).stdout.match(/src\/lib\.rs:\d+/gu) ?? [])].sort();
    // The curved test takes the first branch, the low one the last.
    assert.deepEqual(lines(entry.tests.curved), ['src/lib.rs:1', 'src/lib.rs:2', 'src/lib.rs:3'], entry.host);
    assert.deepEqual(lines(entry.tests.low), ['src/lib.rs:1', 'src/lib.rs:2', 'src/lib.rs:7'], entry.host);
    const decision = run(cwd, binary, ['runs', 'latest', 'decision', 'src/lib.rs:2']).stdout;
    assert.match(decision, /FF- -> F +tests=1/u, `${entry.host}\n${decision}`);
    assert.match(decision, /FTT -> T +tests=1/u, `${entry.host}\n${decision}`);
    measured.push(entry.host);
  }
  assert.ok(measured.length > 0 || !required, 'no wasm host could be checked here');
  console.log(`[rust-wasm-hosts] ${measured.join(', ') || 'nothing'}: each wasm test credited exactly its own lines and vectors`);
} finally {
  rmSync(root, { recursive: true, force: true });
}
