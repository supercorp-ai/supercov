// Rust compiled to wasm that JavaScript tests load is measured, per test.
//
//   node scripts/rust-wasm-js.mjs
//
// Two suites build a wasm-bindgen crate in their own test command and load
// it: node:test, through the `nodejs` glue, and Playwright, from a web page
// that loads the `web` glue. Supercov instruments the crate in the copy; the
// JavaScript runtime remembers each wasm instance, in the test process and
// in each page, and takes what the module ran at every test's end. Each test
// must be credited exactly its own lines and decision vectors, the run must
// be current afterwards, and what the copy built -- `target/`, the generated
// `pkg/` with its instrumented wasm -- must stay out of the project.
// Without wasm-bindgen or the wasm target there is nothing to build, and the
// gate says so; a job that installed them sets SUPERCOV_REQUIRE_RUST_WASM=1.
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { existsSync, mkdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { createServer } from 'node:net';
import { dirname, resolve } from 'node:path';

const repository = resolve(import.meta.dirname, '..');
const binary = process.env.SUPERCOV_BINARY ?? resolve(repository, `target/debug/supercov${process.platform === 'win32' ? '.exe' : ''}`);
const root = resolve(repository, 'target', `rust-wasm-js-${process.pid}-${Date.now()}`);
const { devDependencies } = JSON.parse(readFileSync(resolve(repository, 'package.json'), 'utf8'));
const PLAYWRIGHT = devDependencies['@playwright/test'];
const VITEST = devDependencies.vitest;
const JEST = devDependencies.jest;
const run = (cwd, command, args, env = {}) => spawnSync(command, args, { cwd, encoding: 'utf8', env: { ...process.env, NO_COLOR: '1', CI: '1', ...env } });
const required = process.env.SUPERCOV_REQUIRE_RUST_WASM === '1';
const freePort = () => new Promise((done, fail) => {
  const server = createServer();
  server.once('error', fail);
  server.listen(0, '127.0.0.1', () => {
    const { port } = server.address();
    server.close(() => done(port));
  });
});

const tools = (run(repository, 'rustup', ['target', 'list', '--installed']).stdout ?? '').includes('wasm32-unknown-unknown')
  && run(repository, 'wasm-bindgen', ['--version']).status === 0;
if (process.platform === 'win32' || !tools) {
  if (required) {
    console.error('[rust-wasm-js] wasm-bindgen or the wasm32 target is missing where the job installed them');
    process.exit(1);
  }
  console.log('[rust-wasm-js] skipped: needs wasm-bindgen and the wasm32-unknown-unknown target');
  process.exit(0);
}

// The crate sits in a subdirectory of its own Cargo workspace, as it does
// beside a JavaScript project.
const crate = {
  'rust/Cargo.toml': '[workspace]\n\n[package]\nname = "grades"\nversion = "0.1.0"\nedition = "2021"\n\n[lib]\ncrate-type = ["cdylib"]\n\n[dependencies]\nwasm-bindgen = "=0.2.129"\n',
  'rust/src/lib.rs': `use wasm_bindgen::prelude::*;

#[wasm_bindgen]
pub fn grade(score: u32, curved: bool) -> String {
    if score >= 90 || (curved && score >= 85) {
        "A".into()
    } else if score >= 50 {
        "pass".into()
    } else {
        "fail".into()
    }
}
`,
};
const build = (target, out) => `set -e\ncd rust\ncargo build -q --target wasm32-unknown-unknown --release\nwasm-bindgen --target ${target} --out-dir ${out} target/wasm32-unknown-unknown/release/grades.wasm\n`;

const port = await freePort();
const suites = [
  {
    suite: 'node:test',
    files: {
      ...crate,
      'package.json': '{ "name": "grades-node", "private": true, "scripts": { "test": "sh build.sh && node --test" } }\n',
      'build.sh': build('nodejs', '../pkg'),
      'test/grades.test.mjs': 'import test from "node:test";\nimport assert from "node:assert/strict";\nimport { createRequire } from "node:module";\nconst { grade } = createRequire(import.meta.url)("../pkg/grades.js");\n\ntest("a curved score is an A", () => {\n  assert.equal(grade(87, true), "A");\n});\n\ntest("a low score fails", () => {\n  assert.equal(grade(10, false), "fail");\n});\n',
    },
    command: ['npm', 'test'],
    generated: 'pkg',
  },
  {
    // Vitest runs each file in a worker thread of its own.
    suite: 'Vitest',
    files: {
      ...crate,
      'package.json': `{ "name": "grades-vitest", "private": true, "scripts": { "test": "sh build.sh && vitest run" }, "devDependencies": { "vitest": "${VITEST}" } }\n`,
      'build.sh': build('nodejs', '../pkg'),
      'test/grades.test.mjs': 'import { expect, test } from "vitest";\nimport { createRequire } from "node:module";\nconst { grade } = createRequire(import.meta.url)("../pkg/grades.js");\n\ntest("a curved score is an A", () => {\n  expect(grade(87, true)).toBe("A");\n});\n\ntest("a low score fails", () => {\n  expect(grade(10, false)).toBe("fail");\n});\n',
    },
    command: ['npm', 'test'],
    generated: 'pkg',
  },
  {
    suite: 'Jest',
    files: {
      ...crate,
      'package.json': `{ "name": "grades-jest", "private": true, "scripts": { "test": "sh build.sh && jest" }, "devDependencies": { "jest": "${JEST}" } }\n`,
      'build.sh': build('nodejs', '../pkg'),
      'test/grades.test.js': 'const { grade } = require("../pkg/grades.js");\n\ntest("a curved score is an A", () => {\n  expect(grade(87, true)).toBe("A");\n});\n\ntest("a low score fails", () => {\n  expect(grade(10, false)).toBe("fail");\n});\n',
    },
    command: ['npm', 'test'],
    generated: 'pkg',
  },
  {
    suite: 'Playwright',
    files: {
      ...crate,
      'package.json': `{ "name": "grades-web", "private": true, "type": "module", "devDependencies": { "@playwright/test": "${PLAYWRIGHT}" } }\n`,
      'build.sh': build('web', '../public/pkg'),
      'public/index.html': '<!doctype html><meta charset="utf-8"><title>grades</title>\n<script type="module">\nimport init, { grade } from "./pkg/grades.js";\nawait init();\nwindow.grade = grade;\ndocument.body.dataset.ready = "yes";\n</script>\n<body></body>\n',
      'server.mjs': `import { createServer } from "node:http";\nimport { readFile } from "node:fs/promises";\nimport { extname, join } from "node:path";\nconst types = { ".html": "text/html", ".js": "text/javascript", ".wasm": "application/wasm" };\ncreateServer(async (request, response) => {\n  const path = new URL(request.url, "http://x").pathname;\n  try {\n    const body = await readFile(join("public", path === "/" ? "index.html" : path));\n    response.writeHead(200, { "content-type": types[extname(path || ".html")] ?? "text/html" });\n    response.end(body);\n  } catch {\n    response.writeHead(404);\n    response.end();\n  }\n}).listen(${port}, "127.0.0.1");\n`,
      'playwright.config.js': `export default {
  testDir: 'tests',
  workers: 1,
  reporter: 'line',
  use: { baseURL: 'http://127.0.0.1:${port}' },
  webServer: { command: 'sh build.sh && node server.mjs', url: 'http://127.0.0.1:${port}/', reuseExistingServer: false, timeout: 600000 },
};
`,
      'tests/grades.spec.js': `import { expect, test } from '@playwright/test';

const graded = async (page, score, curved) => {
  await page.goto('/');
  await expect(page.locator('body')).toHaveAttribute('data-ready', 'yes');
  return page.evaluate(([score, curved]) => window.grade(score, curved), [score, curved]);
};

test('a curved score is an A', async ({ page }) => {
  expect(await graded(page, 87, true)).toBe('A');
});

test('a low score fails', async ({ page }) => {
  expect(await graded(page, 10, false)).toBe('fail');
});
`,
    },
    command: ['npx', 'playwright', 'test'],
    generated: 'public/pkg',
  },
];

try {
  for (const entry of suites) {
    if (process.env.ONLY && entry.suite !== process.env.ONLY) continue;
    const cwd = resolve(root, entry.suite.replace(/\W/g, '').toLowerCase());
    for (const [file, text] of Object.entries(entry.files)) {
      mkdirSync(dirname(resolve(cwd, file)), { recursive: true });
      writeFileSync(resolve(cwd, file), text);
    }
    if (entry.files['package.json'].includes('devDependencies')) {
      const installed = run(cwd, 'npm', ['install', '--no-audit', '--no-fund', '--loglevel', 'error']);
      assert.equal(installed.status, 0, installed.stderr);
    }
    assert.equal(run(resolve(cwd, 'rust'), 'cargo', ['generate-lockfile', '-q']).status, 0);
    assert.equal(run(cwd, 'git', ['init', '-q']).status, 0);
    const measured = run(cwd, binary, ['--', ...entry.command]);
    assert.equal(measured.status, 0, `${entry.suite}\n${measured.stdout}${measured.stderr}`);
    assert.match(measured.stderr, /Rust wasm coverage: 2 test\(s\), 2 credited/u, `${entry.suite}\n${measured.stderr}`);
    // What the copy built stays there.
    assert.ok(!existsSync(resolve(cwd, entry.generated)), `${entry.suite}: ${entry.generated} was synced back`);
    assert.ok(!existsSync(resolve(cwd, 'rust/target')), `${entry.suite}: rust/target was synced back`);
    const runs = JSON.parse(run(cwd, binary, ['runs', '--json']).stdout).data.runs;
    const rust = runs.find((candidate) => run(cwd, binary, ['runs', candidate.id, 'decision', 'rust/src/lib.rs:5']).status === 0);
    assert.ok(rust, `${entry.suite}: no Rust run among ${runs.map((candidate) => candidate.id)}`);
    const summary = run(cwd, binary, ['runs', rust.id]);
    assert.doesNotMatch(summary.stdout + summary.stderr, /stale/iu, `${entry.suite}\n${summary.stderr}`);
    const lines = (name) => [...new Set(run(cwd, binary, ['runs', rust.id, 'test', name]).stdout.match(/rust\/src\/lib\.rs:\d+/gu) ?? [])].sort();
    assert.deepEqual(lines('a curved score is an A'), ['rust/src/lib.rs:4', 'rust/src/lib.rs:5', 'rust/src/lib.rs:6'], entry.suite);
    assert.deepEqual(lines('a low score fails'), ['rust/src/lib.rs:10', 'rust/src/lib.rs:4', 'rust/src/lib.rs:5'], entry.suite);
    const decision = run(cwd, binary, ['runs', rust.id, 'decision', 'rust/src/lib.rs:5']).stdout;
    assert.match(decision, /FF- -> F +tests=1/u, `${entry.suite}\n${decision}`);
    assert.match(decision, /FTT -> T +tests=1/u, `${entry.suite}\n${decision}`);
    // Each step of preparing the crate in the copy, and of reading back
    // what the tests took from it, that fails ends the run, saying where:
    // the project's inputs listed, its sources captured, its integrity
    // recorded, a test process's record read.
    // The wasm run without a test, and a test that never loads the wasm:
    // neither has anything to credit, and with no JavaScript source there
    // is no run to publish.
    if (entry.suite === 'node:test') {
      writeFileSync(resolve(cwd, 'test/plain.test.mjs'), 'import test from "node:test";\ntest("needs no wasm", () => {});\n');
      for (const command of [
        ['sh', '-c', 'sh build.sh && node -e "require(\'./pkg/grades.js\').grade(1, false)"'],
        ['sh', '-c', 'sh build.sh && node --test test/plain.test.mjs'],
      ]) {
        const ran = run(cwd, binary, ['--', ...command]);
        assert.notEqual(ran.status, 0, `${command.at(-1)}\n${ran.stderr}`);
        assert.match(ran.stderr, /there was nothing to measure, and no run was published/u, `${command.at(-1)}\n${ran.stderr}`);
      }
      rmSync(resolve(cwd, 'test/plain.test.mjs'));
    }
    if (entry.suite === 'node:test') {
      for (const [fault, place] of [
        ['fs.read_dir#rust_run.rs:88', 'rust_run.rs:88'],
        ['fs.canonicalize#source_capture.rs:25@2', 'source_capture.rs:25'],
        ['fs.open#integrity.rs:427@3', 'integrity.rs:427'],
        ['fs.read_to_string#rust_wasm.rs:291', 'rust_wasm.rs:291'],
      ]) {
        const failed = run(cwd, binary, ['--', ...entry.command], { SUPERCOV_TEST_FAULT: fault });
        assert.notEqual(failed.status, 0, `${fault}\n${failed.stderr}`);
        assert.ok(failed.stderr.includes(`test fault at fs (${place}`), `${fault}\n${failed.stderr}`);
        assert.doesNotMatch(failed.stderr, /panicked/u, `${fault}\n${failed.stderr}`);
      }
    }
  }
  console.log('[rust-wasm-js] Rust wasm loaded by node:test, Vitest, Jest and a Playwright page is measured, each test credited exactly, and its build kept out of the project');
} finally {
  if (!process.env.KEEP && root.startsWith(resolve(repository, 'target', 'rust-wasm-js-')))
    rmSync(root, { recursive: true, force: true });
}
