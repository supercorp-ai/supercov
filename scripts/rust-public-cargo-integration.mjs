// The public Rust path, end to end: `supercov -- cargo test` on a small crate,
// through the exact compiler chain and the libtest companion, then queries
// for what each test proved. Two tests reach library code indirectly -- one
// from a spawned thread, one from a child binary -- because that attribution
// rests on the runtime taking over thread and process creation, which is
// interposition on POSIX and import-table patching on Windows.
import assert from 'node:assert/strict';
import { mkdirSync, mkdtempSync, realpathSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { resolve } from 'node:path';
import { spawnSync } from 'node:child_process';

const repository = resolve(import.meta.dirname, '..');
const binary = resolve(repository, `target/debug/supercov${process.platform === 'win32' ? '.exe' : ''}`);
const launcher = resolve(repository, 'bin/supercov.js');
// The runner spells TEMP as an 8.3 short name; the product resolves paths to
// long names. Hand it long names, as real installations have.
const temporary = mkdtempSync(resolve(realpathSync.native(tmpdir()), 'supercov-rust-public-'));
const project = resolve(temporary, 'fixture');

const library = [
  'pub fn classify(value: i32) -> &\'static str {',
  '    if value < 0 {',
  '        "negative"',
  '    } else if value == 0 {',
  '        "zero"',
  '    } else {',
  '        "positive"',
  '    }',
  '}',
  '',
  'pub fn doubled(value: i32) -> i32 {',
  '    value * 2',
  '}',
  '',
  'pub fn greeting(word: &str) -> String {',
  '    format!("child:{word}")',
  '}',
  '',
];
const line = (text) => library.indexOf(text) + 1;
const zeroLine = line('        "zero"');
const doubledLine = line('    value * 2');
const greetingLine = line('    format!("child:{word}")');

function environment() {
  return { ...process.env, SUPERCOV_RUST_BINARY: binary };
}

function supercov(args) {
  return spawnSync(process.execPath, [launcher, ...args], {
    cwd: project,
    encoding: 'utf8',
    env: environment(),
    timeout: 900_000,
  });
}

function query(args) {
  const result = supercov([...args, '--json']);
  assert.equal(result.status, 0, result.stderr);
  const payload = JSON.parse(result.stdout);
  assert.equal(payload.ok, true, result.stdout);
  return payload.data;
}

try {
  mkdirSync(resolve(project, 'src/bin'), { recursive: true });
  mkdirSync(resolve(project, 'tests'), { recursive: true });
  writeFileSync(
    resolve(project, 'Cargo.toml'),
    ['[package]', 'name = "public_rust_fixture"', 'version = "0.0.0"', 'edition = "2024"', ''].join('\n'),
  );
  writeFileSync(resolve(project, 'src/lib.rs'), library.join('\n'));
  writeFileSync(
    resolve(project, 'src/bin/worker.rs'),
    [
      'fn main() {',
      '    let word = std::env::args().nth(1).unwrap_or_default();',
      '    println!("{}", public_rust_fixture::greeting(&word));',
      '}',
      '',
    ].join('\n'),
  );
  writeFileSync(
    resolve(project, 'tests/suite.rs'),
    [
      'use public_rust_fixture::{classify, doubled};',
      '',
      '#[test]',
      'fn classifies_negative() {',
      '    assert_eq!(classify(-1), "negative");',
      '}',
      '',
      '#[test]',
      'fn classifies_positive() {',
      '    assert_eq!(classify(5), "positive");',
      '}',
      '',
      '#[test]',
      'fn doubles_on_a_thread() {',
      '    let handle = std::thread::spawn(|| doubled(21));',
      '    assert_eq!(handle.join().unwrap(), 42);',
      '}',
      '',
      '#[test]',
      'fn greets_from_a_child() {',
      '    let output = std::process::Command::new(env!("CARGO_BIN_EXE_worker"))',
      '        .arg("hi")',
      '        .output()',
      '        .unwrap();',
      '    assert!(output.status.success());',
      '    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "child:hi");',
      '}',
      '',
    ].join('\n'),
  );

  const run = supercov(['--', 'cargo', 'test']);
  assert.equal(run.status, 0, `${run.stdout}\n${run.stderr}`);
  assert.match(run.stderr, /\[supercov\] detected Rust/);
  assert.match(run.stderr, /\[supercov\] Rust coverage: 4 test\(s\)/, run.stderr);

  const summary = query(['runs', 'latest', 'summary']);
  const { lines } = summary.coverage;
  assert.ok(lines.covered > 0 && lines.covered < lines.total, `the zero branch is never taken: ${JSON.stringify(lines)}`);

  // What each test proved, through the paths that depend on the platform hooks.
  const threaded = query(['runs', 'latest', 'line', `src/lib.rs:${doubledLine}`]);
  assert.match(
    JSON.stringify(threaded),
    /doubles_on_a_thread/,
    `a line reached only from a spawned thread must belong to the test that spawned it: ${JSON.stringify(threaded)}`,
  );
  const child = query(['runs', 'latest', 'line', `src/lib.rs:${greetingLine}`]);
  assert.match(
    JSON.stringify(child),
    /greets_from_a_child/,
    `a line reached only in a child process must belong to the test that started it: ${JSON.stringify(child)}`,
  );
  const untaken = query(['runs', 'latest', 'line', `src/lib.rs:${zeroLine}`]);
  assert.doesNotMatch(JSON.stringify(untaken), /classifies_/, `no test reaches the zero branch: ${JSON.stringify(untaken)}`);

  console.log(
    `[rust-public-cargo] ${process.platform} ran the public cargo test path through the exact compiler chain; a spawned thread and a child process kept their tests`,
  );
} finally {
  rmSync(temporary, { recursive: true, force: true, maxRetries: 10, retryDelay: 20 });
}
