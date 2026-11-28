// A strong pass over browser tests of a Rust Worker that `wrangler dev`
// serves runs each Worker mutant switched on in the Worker.
//
//   node scripts/rust-worker-playwright-mutants.mjs
//
// The Worker's crate in the copy gets the plan's Rust mutants behind
// switches; `wrangler dev` builds that copy once as it starts, and each
// request has the mutant its test attempt names switched on. A comparison
// mutant a test checks is killed; one no test checks survives, having run.
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
import { dirname, resolve } from 'node:path';
import { freePort } from './free-ports.mjs';

const repository = resolve(import.meta.dirname, '..');
const binary = process.env.SUPERCOV_BINARY ?? resolve(repository, `target/debug/supercov${process.platform === 'win32' ? '.exe' : ''}`);
const root = resolve(repository, 'target', `rust-worker-playwright-mutants-${process.pid}-${Date.now()}`);
const WRANGLER = '4.151.1';
// As a project has it: the suite's preload reaches `wrangler dev` too.
const PLAYWRIGHT = JSON.parse(readFileSync(resolve(repository, 'package.json'), 'utf8')).devDependencies['@playwright/test'];


const run = (cwd, command, args, env = {}) => spawnSync(command, args, { cwd, encoding: 'utf8', env: { ...process.env, NO_COLOR: '1', CI: '1', ...env } });
const targets = run(repository, 'rustup', ['target', 'list', '--installed']).stdout ?? '';
if (process.platform === 'win32' || run(repository, 'worker-build', ['--version']).status !== 0 || !targets.includes('wasm32-unknown-unknown')) {
  // A job that installed them and still lands here has lost the gate.
  if (process.env.SUPERCOV_REQUIRE_RUST_WORKER === '1') {
    console.error('[rust-worker-playwright-mutants] worker-build or the wasm32-unknown-unknown target is missing where the job installed them');
    process.exit(1);
  }
  console.log('[rust-worker-playwright-mutants] skipped: needs worker-build and the wasm32-unknown-unknown target');
  process.exit(0);
}

const port = await freePort();
const files = {
  'package.json': `{ "name": "grades-worker", "private": true, "type": "module", "devDependencies": { "@playwright/test": "${PLAYWRIGHT}", "wrangler": "${WRANGLER}" } }\n`,
  // Its own workspace: the fixture sits inside Supercov's checkout.
  'Cargo.toml': '[workspace]\n\n[package]\nname = "grades-worker"\nversion = "0.1.0"\nedition = "2021"\n\n[lib]\ncrate-type = ["cdylib"]\n\n[dependencies]\nworker = "0.8"\n\n[profile.release]\nopt-level = "s"\n',
  'wrangler.toml': 'name = "grades-worker"\nmain = "build/worker/shim.mjs"\ncompatibility_date = "2026-10-01"\n\n[build]\ncommand = "worker-build --dev"\n',
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

// The routing is not what the gate is about. A cfg never set: no build
// needs the mutants crate, and the attribute still says skip.
#[event(fetch)]
#[cfg_attr(gate_never_set, mutants::skip)]
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
  const locked = run(root, 'cargo', ['generate-lockfile', '-q']);
  assert.equal(locked.status, 0, locked.stderr);
  assert.equal(run(root, 'git', ['init', '-q']).status, 0);

  const started = Date.now();
  const pass = run(root, binary, ['mutants', '--json', '--', 'npx', 'playwright', 'test']);
  console.log(`[rust-worker-playwright-mutants] measured and passed in ${Math.round((Date.now() - started) / 1000)} s`);
  assert.equal(pass.status, 0, `${pass.stdout}\n${pass.stderr}`);
  const listed = JSON.parse(run(root, binary, ['runs', 'latest', 'mutants', '--json', '--limit', '0']).stdout).data.mutants;
  const grade = listed.filter((mutant) => mutant.file === 'src/lib.rs' && mutant.span.start[0] === 4);
  const said = grade.map((mutant) => `${mutant.id}: ${JSON.stringify(mutant.strong)}`).join('\n');
  console.log(said);
  // `score >= 90` made `<`: the high score is no longer an A. Killed.
  const high = grade.find((mutant) => mutant.id.startsWith('src/lib.rs:4:14-') && mutant.id.endsWith(':BinaryOperator:<'));
  assert.equal(high?.strong?.status, 'killed', `${said}\n${pass.stderr}`);
  // No grade mutant ends in an error: each ran against the Worker.
  assert.ok(grade.every((mutant) => mutant.strong && mutant.strong.status !== 'error'), `${said}\n${pass.stderr}`);
  const config = run(root, 'git', ['status', '--porcelain', '--untracked-files=no']).stdout;
  assert.equal(config.trim(), '', config);
  console.log('[rust-worker-playwright-mutants] a Rust Worker under wrangler dev runs each mutant switched on per request');
} finally {
  if (!process.env.KEEP && root.startsWith(resolve(repository, 'target', 'rust-worker-playwright-mutants-')))
    rmSync(root, { recursive: true, force: true });
}
