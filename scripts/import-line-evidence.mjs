// Line evidence recorded where Supercov could not run is added to a run.
//
//   node scripts/import-line-evidence.mjs
//
// `supercov import` takes an LCOV file, or an LLVM profile with the binary
// it was recorded from, and publishes a new run: the evidence's lines are
// credited, by line only, to a test of their own or to the run's background,
// and a project file the run did not measure joins it with the lines the
// evidence recorded. What the run measured itself is left as it was, the new
// run is as current as the old one, and evidence of other source -- a line
// past a file's end -- is refused rather than mixed in.
// The LLVM half needs `llvm-tools`; a job that installed it sets
// SUPERCOV_REQUIRE_LLVM_TOOLS=1, and then it may not be skipped.
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { existsSync, mkdirSync, mkdtempSync, readdirSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';

const repository = resolve(import.meta.dirname, '..');
const binary = process.env.SUPERCOV_BINARY ?? resolve(repository, `target/debug/supercov${process.platform === 'win32' ? '.exe' : ''}`);
const root = mkdtempSync(join(tmpdir(), 'supercov-import-'));
const run = (cwd, command, args, env = {}) => spawnSync(command, args, { cwd, encoding: 'utf8', env: { ...process.env, NO_COLOR: '1', ...env } });
const write = (cwd, files) => {
  for (const [file, text] of Object.entries(files)) {
    mkdirSync(dirname(join(cwd, file)), { recursive: true });
    writeFileSync(join(cwd, file), text);
  }
};
const supercov = (cwd, ...args) => run(cwd, binary, args);
const lines = (cwd, id, test) => [...new Set(supercov(cwd, 'runs', id, 'test', test).stdout.match(/[\w/]+\.\w+:\d+/gu) ?? [])].sort();
const runOf = (output) => output.match(/as run (run_\w+)/u)?.[1];

try {
  // A node:test suite reaches `local`; something Supercov never saw -- a
  // deployed edge function, recorded in a container that mounted the
  // project at /app -- ran `edge` and a C helper the suite does not load.
  const js = join(root, 'javascript');
  write(js, {
    'package.json': '{ "name": "imported", "private": true, "type": "module", "scripts": { "test": "node --test" } }\n',
    'src/app.js': 'export function local(value) {\n  return value + 1;\n}\n\nexport function edge(value) {\n  if (value > 10) {\n    return "big";\n  }\n  return "small";\n}\n',
    'test/app.test.js': 'import test from "node:test";\nimport assert from "node:assert/strict";\nimport { local } from "../src/app.js";\n\ntest("local adds one", () => {\n  assert.equal(local(1), 2);\n});\n',
    'lib/native.c': 'int twice(int x) {\n  return x * 2;\n}\n\nint unused(void) {\n  return 0;\n}\n',
    'lcov.info': 'TN:\nSF:/app/src/app.js\nFN:5,edge\nFNDA:1,edge\nDA:5,1\nDA:6,1\nDA:7,0\nDA:9,1\nend_of_record\nSF:/app/lib/native.c\nFN:1,twice\nFNDA:3,twice\nDA:1,3\nDA:2,3\nDA:5,0\nDA:6,0\nend_of_record\nSF:/usr/include/stdio.h\nDA:1,1\nend_of_record\n',
  });
  assert.equal(run(js, 'git', ['init', '-q']).status, 0);
  const measured = supercov(js, '--', 'node', '--test');
  assert.equal(measured.status, 0, measured.stdout + measured.stderr);
  const parent = JSON.parse(supercov(js, 'runs', '--json').stdout).data.runs[0].id;

  const imported = supercov(js, 'import', 'lcov.info', '--test', 'edge in production', '--run', parent);
  assert.equal(imported.status, 0, imported.stdout + imported.stderr);
  assert.match(imported.stdout, /imported 5 line\(s\) that ran/u, imported.stdout);
  assert.match(imported.stdout, /measured only by this evidence: lib\/native\.c/u, imported.stdout);
  assert.match(imported.stderr, /\/usr\/include\/stdio\.h \(outside the project\)/u, imported.stderr);
  const id = runOf(imported.stdout);
  assert.ok(id, imported.stdout);
  // The imported test holds what the evidence says ran, by line.
  assert.deepEqual(lines(js, id, 'edge in production'), ['lib/native.c:1', 'lib/native.c:2', 'src/app.js:5', 'src/app.js:6', 'src/app.js:9']);
  // The suite's own test is as it was.
  assert.deepEqual(lines(js, id, 'local adds one'), lines(js, parent, 'local adds one'));
  const line = supercov(js, 'runs', id, 'line', 'src/app.js:9').stdout;
  assert.match(line, /edge in production \[imported:edge in production\] — imported\/lcov/u, line);
  // Line evidence says nothing of branches: the `if` stays unproven.
  const summary = supercov(js, 'runs', id);
  assert.match(summary.stdout, /Branches +0\.00% \(0\/2\)/u, summary.stdout);
  assert.match(summary.stdout, /imported +1 test\(s\)/u, summary.stdout);
  assert.doesNotMatch(summary.stdout + summary.stderr, /stale/iu, summary.stderr);
  // The file the run did not measure is measured by the evidence alone,
  // and says so.
  const native = supercov(js, 'runs', id, 'file', 'lib/native.c').stdout;
  assert.match(native, /Measured only by the lines lcov\.info recorded/u, native);
  assert.match(native, /5 +NOT COVERED +int unused\(void\) \{/u, native);

  // To the background instead.
  const background = supercov(js, 'import', 'lcov.info', '--background', '--run', parent);
  assert.equal(background.status, 0, background.stdout + background.stderr);
  const backgroundLine = supercov(js, 'runs', runOf(background.stdout), 'line', 'src/app.js:6').stdout;
  assert.match(backgroundLine, /Background \/ unattributed .* — background\/background/u, backgroundLine);

  // Evidence of other source is refused.
  write(js, { 'stale.info': 'SF:src/app.js\nDA:40,1\nend_of_record\n' });
  const stale = supercov(js, 'import', 'stale.info', '--test', 'old', '--run', parent);
  assert.equal(stale.status, 2, stale.stdout + stale.stderr);
  assert.match(stale.stderr, /records line 40 of src\/app\.js, which has 10 lines/u, stale.stderr);
  assert.equal(supercov(js, 'import', 'lcov.info', '--run', parent).status, 2);

  // An LLVM profile of a binary built outside Supercov, imported with the
  // binary, into the run `cargo test` made of the same crate.
  const llvm = run(tmpdir(), 'rustc', ['--print', 'sysroot']).stdout.trim();
  const host = run(tmpdir(), 'rustc', ['-vV']).stdout.match(/^host: (.+)$/mu)?.[1];
  const tools = host && existsSync(join(llvm, 'lib', 'rustlib', host, 'bin', `llvm-profdata${process.platform === 'win32' ? '.exe' : ''}`));
  if (!tools) {
    assert.ok(process.env.SUPERCOV_REQUIRE_LLVM_TOOLS !== '1', 'llvm-tools is missing where the job installed it');
    console.log('[import-line-evidence] LLVM profile: skipped, needs `rustup component add llvm-tools`');
  } else {
    const rust = join(root, 'rust');
    write(rust, {
      'Cargo.toml': '[package]\nname = "grades"\nversion = "0.1.0"\nedition = "2021"\n\n[[bin]]\nname = "edge"\npath = "src/main.rs"\n',
      'src/lib.rs': 'pub fn local(value: u32) -> u32 {\n    value + 1\n}\n\npub fn edge(value: u32) -> &\'static str {\n    if value > 10 {\n        "big"\n    } else {\n        "small"\n    }\n}\n',
      'src/main.rs': 'fn main() {\n    println!("{}", grades::edge(42));\n}\n',
      'tests/local.rs': '#[test]\nfn local_adds_one() {\n    assert_eq!(grades::local(1), 2);\n}\n',
    });
    assert.equal(run(rust, 'git', ['init', '-q']).status, 0);
    assert.equal(run(rust, 'cargo', ['generate-lockfile', '-q']).status, 0);
    const cargo = supercov(rust, '--', 'cargo', 'test', '-q');
    assert.equal(cargo.status, 0, cargo.stdout + cargo.stderr);
    const target = join(root, 'coverage-target');
    const built = run(rust, 'cargo', ['build', '-q', '--bin', 'edge'], { RUSTFLAGS: '-C instrument-coverage', CARGO_TARGET_DIR: target });
    assert.equal(built.status, 0, built.stderr);
    const edge = join(target, 'debug', `edge${process.platform === 'win32' ? '.exe' : ''}`);
    // Where LLVM writes it by default: in the project, as `default.profraw`.
    const ran = run(rust, edge, [], { LLVM_PROFILE_FILE: join(rust, 'default.profraw') });
    assert.equal(ran.stdout.trim(), 'big', ran.stderr);
    const profile = supercov(rust, 'import', 'default.profraw', '--object', edge, '--test', 'edge binary', '--status', 'passed');
    assert.equal(profile.status, 0, profile.stdout + profile.stderr);
    const rustRun = runOf(profile.stdout);
    const credited = lines(rust, rustRun, 'edge binary');
    for (const expected of ['src/lib.rs:5', 'src/lib.rs:6', 'src/lib.rs:7'])
      assert.ok(credited.includes(expected), `${expected} not in ${credited}`);
    // `"small"` did not run there.
    assert.ok(!credited.includes('src/lib.rs:9'), credited.join(' '));
    // The profile in the project is no input of the build: nothing is stale.
    const rustSummary = supercov(rust, 'runs', rustRun);
    assert.doesNotMatch(rustSummary.stdout + rustSummary.stderr, /stale/iu, rustSummary.stdout + rustSummary.stderr);
    assert.match(rustSummary.stdout, /Passed +2/u, rustSummary.stdout);
    // The import's work leaves nothing behind.
    assert.deepEqual(readdirSync(join(rust, '.supercov', 'work')).filter((entry) => !entry.startsWith('.')), []);
  }
  console.log('[import-line-evidence] LCOV and LLVM profiles add line-only evidence to a run, credited to a named test or the background');
} finally {
  if (!process.env.KEEP) rmSync(root, { recursive: true, force: true });
}
