// Browser tests of a Rust Worker that `wrangler dev` serves measure the Worker.
//
//   node scripts/rust-worker-playwright.mjs
//
// A Playwright suite whose `webServer` is `wrangler dev`, for a Worker written
// in Rust and built by worker-build. Supercov instruments the crate in the
// copy the suite runs in, points the copy's wrangler configuration at its own
// entry and an inspector port, and takes the Worker's probe region through
// the inspector while the suite runs. Each take is credited, by time, to the
// browser test it fell in. Without worker-build or the wasm target there is
// nothing to build, and the gate says so.
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { mkdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { createServer } from 'node:net';
import { dirname, resolve } from 'node:path';

const repository = resolve(import.meta.dirname, '..');
const binary = process.env.SUPERCOV_BINARY ?? resolve(repository, `target/debug/supercov${process.platform === 'win32' ? '.exe' : ''}`);
const root = resolve(repository, 'target', `rust-worker-playwright-${process.pid}-${Date.now()}`);
const WRANGLER = '4.151.1';
// As a project has it: the suite's preload reaches `wrangler dev` too.
const PLAYWRIGHT = JSON.parse(readFileSync(resolve(repository, 'package.json'), 'utf8')).devDependencies['@playwright/test'];

const freePort = () => new Promise((done, fail) => {
  const server = createServer();
  server.once('error', fail);
  server.listen(0, '127.0.0.1', () => {
    const { port } = server.address();
    server.close(() => done(port));
  });
});

const run = (cwd, command, args, env = {}) => spawnSync(command, args, { cwd, encoding: 'utf8', env: { ...process.env, NO_COLOR: '1', CI: '1', ...env } });
const targets = run(repository, 'rustup', ['target', 'list', '--installed']).stdout ?? '';
if (process.platform === 'win32' || run(repository, 'worker-build', ['--version']).status !== 0 || !targets.includes('wasm32-unknown-unknown')) {
  // A job that installed them and still lands here has lost the gate.
  if (process.env.SUPERCOV_REQUIRE_RUST_WORKER === '1') {
    console.error('[rust-worker-playwright] worker-build or the wasm32-unknown-unknown target is missing where the job installed them');
    process.exit(1);
  }
  console.log('[rust-worker-playwright] skipped: needs worker-build and the wasm32-unknown-unknown target');
  process.exit(0);
}

const port = await freePort();
const files = {
  'package.json': `{ "name": "grades-worker", "private": true, "type": "module", "devDependencies": { "@playwright/test": "${PLAYWRIGHT}", "wrangler": "${WRANGLER}" } }\n`,
  // Its own workspace: the fixture sits inside Supercov's checkout.
  'Cargo.toml': '[workspace]\n\n[package]\nname = "grades-worker"\nversion = "0.1.0"\nedition = "2021"\n\n[lib]\ncrate-type = ["cdylib"]\n\n[dependencies]\nworker = "0.8"\n\n[profile.release]\nopt-level = "s"\n',
  'wrangler.toml': 'name = "grades-worker"\nmain = "build/worker/shim.mjs"\ncompatibility_date = "2026-10-01"\n\n[build]\ncommand = "worker-build --release"\n',
  'src/lib.rs': `use worker::*;

pub fn grade(score: u32, curved: bool) -> &'static str {
    if score >= 90 || (curved && score >= 85) {
        "A"
    } else if score >= 50 {
        "pass"
    } else {
        "fail"
    }
}

#[event(fetch)]
async fn fetch(request: Request, _env: Env, _ctx: Context) -> Result<Response> {
    let url = request.url()?;
    if url.path() == "/ready" {
        return Response::ok("ready");
    }
    let score = url
        .query_pairs()
        .find(|(key, _)| key == "score")
        .and_then(|(_, value)| value.parse().ok())
        .unwrap_or(0);
    let curved = url.query_pairs().any(|(key, _)| key == "curved");
    Response::ok(grade(score, curved))
}
`,
  'playwright.config.js': `export default {
  testDir: 'tests',
  workers: 1,
  reporter: 'line',
  use: { baseURL: 'http://127.0.0.1:${port}' },
  webServer: { command: 'npx wrangler dev --port ${port} --ip 127.0.0.1', url: 'http://127.0.0.1:${port}/ready', reuseExistingServer: false, timeout: 600000 },
};
`,
  'tests/grades.spec.js': `import { expect, test } from '@playwright/test';

// Worker work is credited by the window it falls in; a pause on each side
// keeps it from sharing one with the test before or after. A window runs from
// one read of the Worker to the next, and a read on a busy CI runner can
// stall: at 60 ms one window spanned two tests (4.19.0's release run).
const pause = () => new Promise((resolve) => setTimeout(resolve, 300));

test('a high score is an A', async ({ request }) => {
  await pause();
  expect(await (await request.get('/?score=95')).text()).toBe('A');
  await pause();
});

test('a curved score is an A', async ({ request }) => {
  await pause();
  expect(await (await request.get('/?score=87&curved')).text()).toBe('A');
  await pause();
});

test('a low score fails', async ({ request }) => {
  await pause();
  expect(await (await request.get('/?score=10')).text()).toBe('fail');
  await pause();
});

test('a test that sends nothing', async () => {
  expect(1).toBe(1);
});
`,
};

try {
  for (const [file, text] of Object.entries(files)) {
    mkdirSync(dirname(resolve(root, file)), { recursive: true });
    writeFileSync(resolve(root, file), text);
  }
  const installed = run(root, 'npm', ['install', '--no-audit', '--no-fund', '--loglevel', 'error']);
  assert.equal(installed.status, 0, installed.stderr);
  // As a project ships it, with its lockfile.
  const locked = run(root, 'cargo', ['generate-lockfile', '-q']);
  assert.equal(locked.status, 0, locked.stderr);
  assert.equal(run(root, 'git', ['init', '-q']).status, 0);

  const measured = run(root, binary, ['--', 'npx', 'playwright', 'test']);
  assert.equal(measured.status, 0, `${measured.stdout}${measured.stderr}`);
  assert.match(measured.stderr, /Rust Worker coverage: 4 test\(s\), 3 credited/u, measured.stderr);

  const listed = run(root, binary, ['runs', '--json']);
  const id = JSON.parse(listed.stdout).data.runs[0].id;
  // Nothing changed after the run: it is current.
  const summary = run(root, binary, ['runs', id]);
  assert.doesNotMatch(summary.stdout + summary.stderr, /stale/iu, summary.stdout + summary.stderr);
  const lines = (name) => [...new Set(run(root, binary, ['runs', id, 'test', name]).stdout.match(/src\/lib\.rs:\d+/gu) ?? [])].sort();
  const high = lines('a high score is an A');
  const low = lines('a low score fails');
  // Each test is credited its own path through `grade` and nothing else's.
  assert.ok(high.includes('src/lib.rs:4') && high.includes('src/lib.rs:5'), high.join(' '));
  assert.ok(!high.includes('src/lib.rs:9'), high.join(' '));
  assert.ok(low.includes('src/lib.rs:9'), low.join(' '));
  assert.ok(!low.includes('src/lib.rs:5'), low.join(' '));
  assert.deepEqual(lines('a test that sends nothing'), []);

  // No test asked for a plain pass, so that line is the one never run, and
  // the curved test shows the decision's second condition deciding it.
  const pass = run(root, binary, ['runs', id, 'line', 'src/lib.rs:7']).stdout;
  assert.match(pass, /NOT COVERED/u, pass);
  const decision = run(root, binary, ['runs', id, 'decision', 'src/lib.rs:4']).stdout;
  assert.match(decision, /score >= 90/u, decision);
  const coverage = JSON.parse(run(root, binary, ['runs', id, '--json']).stdout).data.coverage;
  assert.ok(coverage.lines.covered > 0 && coverage.lines.covered < coverage.lines.total, JSON.stringify(coverage.lines));

  // A run whose Worker answered only `wrangler dev`'s readiness check, as
  // no test was selected, has no test to credit and publishes no Worker run.
  const unselected = run(root, binary, ['--', 'npx', 'playwright', 'test', '-g', 'no test has this name']);
  assert.notEqual(unselected.status, 0, unselected.stderr);
  assert.doesNotMatch(unselected.stderr, /Rust Worker coverage|panicked/u, unselected.stderr);

  // The Worker run that cannot be published -- its work directory not
  // made -- ends the run, saying where, and keeps its evidence.
  const unpublished = run(root, binary, ['--', 'npx', 'playwright', 'test'], { SUPERCOV_TEST_FAULT: 'fs.create_dir_all#rust_wasm_evidence.rs:375' });
  assert.notEqual(unpublished.status, 0, unpublished.stderr);
  assert.ok(unpublished.stderr.includes('test fault at fs (rust_wasm_evidence.rs:375'), unpublished.stderr);
  assert.doesNotMatch(unpublished.stderr, /panicked/u, unpublished.stderr);

  // The project's own configuration is untouched: only the copy was pointed
  // at Supercov's entry.
  const config = run(root, 'git', ['status', '--porcelain', '--untracked-files=no']).stdout;
  assert.equal(config.trim(), '', config);
  console.log('[rust-worker-playwright] a Rust Worker under wrangler dev is measured, each test credited its own work by time');
} finally {
  if (!process.env.KEEP && root.startsWith(resolve(repository, 'target', 'rust-worker-playwright-')))
    rmSync(root, { recursive: true, force: true });
}
