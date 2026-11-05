// How much longer a real Next.js application's tests take under Supercov
// than without it: Bulletproof React's Next.js app, its Vitest suite and its
// Playwright flow against a production build and the app's own mock API,
// all offline. Each round runs the plain suites, then every binary named in
// SUPERCOV_BINARIES (`name=path,...`) in an order that turns each round, so
// no binary always runs on a warmer machine. A Supercov run starts with no
// `.supercov`, as on a fresh CI checkout, and ends with a merge of its two
// runs, as a pipeline that measures suites apart does.
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import {
  appendFileSync,
  copyFileSync,
  existsSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';

const binaries = (process.env.SUPERCOV_BINARIES ?? '')
  .split(',')
  .filter(Boolean)
  .map((entry) => {
    const [name, path] = entry.split('=');
    return { name, path: resolve(path) };
  });
assert.ok(binaries.length > 0, 'SUPERCOV_BINARIES names no binary');
const rounds = Number(process.env.SPEED_ROUNDS ?? 3);
const repeat = Number(process.env.SPEED_REPEAT ?? 10);
const output =
  process.env.SPEED_OUTPUT ?? mkdtempSync(join(tmpdir(), 'supercov-speed-'));
mkdirSync(output, { recursive: true });
const logs = join(output, 'logs');
mkdirSync(logs, { recursive: true });

function run(cwd, command, args, label, extra = {}) {
  const started = process.hrtime.bigint();
  const result = spawnSync(command, args, {
    cwd,
    env: { ...process.env, CI: '1', ...extra },
    encoding: 'utf8',
    timeout: 1_800_000,
    maxBuffer: 256 * 1024 * 1024,
  });
  const seconds = Number(process.hrtime.bigint() - started) / 1e9;
  writeFileSync(join(logs, `${label}.log`), result.stdout + result.stderr);
  assert.ifError(result.error);
  assert.equal(result.status, 0, `${label} failed: see ${label}.log`);
  return { seconds, stdout: result.stdout };
}

const app = join(output, 'bulletproof-react');
if (!existsSync(app)) {
  mkdirSync(app);
  run(app, 'git', ['init', '-q'], 'git-init');
  run(
    app,
    'git',
    [
      'fetch',
      '-q',
      '--depth',
      '1',
      'https://github.com/alan2207/bulletproof-react.git',
      '9506629ed003a561c6627735480cce4994244bb4',
    ],
    'git-fetch',
  );
  run(app, 'git', ['checkout', '-q', '--detach', 'FETCH_HEAD'], 'git-checkout');
}
const project = join(app, 'apps/nextjs-app');
run(
  project,
  'npx',
  ['-y', 'yarn@1.22.22', 'install', '--frozen-lockfile', '--non-interactive'],
  'install',
);
copyFileSync(join(project, '.env.example'), join(project, '.env'));
// One worker: the flow's user shares one team, whose discussion list each
// repetition empties again. A production build, as CI serves it, without
// the lint \`next build\` runs first: the same for every variant, and a
// release before 4.25 failed it on the test files it rewrote.
writeFileSync(
  join(project, 'playwright.speed.config.ts'),
  `import { defineConfig, devices } from '@playwright/test';

export default defineConfig({
  testDir: './e2e',
  workers: 1,
  retries: 0,
  reporter: [['list'], ['json']],
  use: { baseURL: 'http://localhost:3000', trace: 'off' },
  projects: [
    { name: 'setup', testMatch: /.*\\.setup\\.ts/ },
    {
      name: 'chromium',
      testMatch: /.*\\.spec\\.ts/,
      use: { ...devices['Desktop Chrome'], storageState: 'e2e/.auth/user.json' },
      dependencies: ['setup'],
    },
  ],
  webServer: [
    { command: 'yarn run-mock-server', port: 8080, timeout: 120_000 },
    { command: 'npx next build --no-lint && yarn start --port 3000', port: 3000, timeout: 600_000 },
  ],
});
`,
);
run(project, 'npx', ['playwright', 'install', 'chromium'], 'browser');

const suites = [
  { name: 'unit', command: ['npx', 'vitest', 'run'] },
  {
    name: 'e2e',
    command: [
      'npx',
      'playwright',
      'test',
      '--config',
      'playwright.speed.config.ts',
      '--repeat-each',
      String(repeat),
    ],
  },
];

// The tests' own time from Playwright's report: the server's build and
// start are left out, so this is what each action and page load cost.
function testSeconds(report) {
  const tests = [];
  const walk = (suite) => {
    for (const spec of suite.specs ?? [])
      for (const test of spec.tests ?? [])
        for (const result of test.results ?? []) tests.push(result);
    for (const child of suite.suites ?? []) walk(child);
  };
  for (const suite of JSON.parse(readFileSync(report, 'utf8')).suites) walk(suite);
  assert.ok(tests.length > 0, `${report} holds no test`);
  for (const result of tests) assert.equal(result.status, 'passed', report);
  return tests.reduce((sum, result) => sum + result.duration, 0) / 1000;
}

const samples = [];
function sample(round, variant, suite, seconds, extra = {}) {
  const entry = { round, variant, suite, seconds, ...extra };
  samples.push(entry);
  appendFileSync(join(output, 'samples.jsonl'), `${JSON.stringify(entry)}\n`);
  console.log(
    `[speed] round ${round} ${variant} ${suite} ${seconds.toFixed(2)} s` +
      (extra.testSeconds ? ` (tests ${extra.testSeconds.toFixed(2)} s)` : ''),
  );
}

function suiteRun(round, variant, suite, binary) {
  const report = join(output, `report-${round}-${variant}-${suite.name}.json`);
  const env = { PLAYWRIGHT_JSON_OUTPUT_NAME: report };
  const [command, ...args] = binary
    ? [binary, '--', ...suite.command]
    : suite.command;
  const { seconds } = run(
    project,
    command,
    args,
    `${round}-${variant}-${suite.name}`,
    env,
  );
  const extra = existsSync(report) ? { testSeconds: testSeconds(report) } : {};
  sample(round, variant, suite.name, seconds, extra);
  if (!binary) return;
  const latest = JSON.parse(
    run(project, binary, ['runs', 'latest', '--json'], `${round}-${variant}-${suite.name}-latest`)
      .stdout,
  ).data;
  assert.equal(latest.valid, true, `${variant} ${suite.name} run is not valid`);
  assert.equal(latest.testOutcomes.failed, 0, `${variant} ${suite.name} failed tests`);
  return latest.id ?? latest.runId;
}

for (let round = 1; round <= rounds; round += 1) {
  // A run leaves its copy of the repository in .supercov, and the app's
  // Vitest includes collect the copy's tests: plain starts from none.
  rmSync(join(project, '.supercov'), { recursive: true, force: true });
  for (const suite of suites) suiteRun(round, 'plain', suite);
  const order = binaries.map((_, index) => binaries[(index + round) % binaries.length]);
  for (const { name, path } of order) {
    rmSync(join(project, '.supercov'), { recursive: true, force: true });
    for (const suite of suites) suiteRun(round, name, suite, path);
    const { seconds } = run(project, path, ['merge'], `${round}-${name}-merge`);
    sample(round, name, 'merge', seconds);
  }
}

const median = (values) => {
  const sorted = [...values].sort((a, b) => a - b);
  const middle = Math.floor(sorted.length / 2);
  return sorted.length % 2 ? sorted[middle] : (sorted[middle - 1] + sorted[middle]) / 2;
};
const of = (variant, suite, field = 'seconds') =>
  median(
    samples
      .filter((entry) => entry.variant === variant && entry.suite === suite)
      .map((entry) => entry[field])
      .filter((value) => value !== undefined),
  );
const rows = [];
const plainTotal = of('plain', 'unit') + of('plain', 'e2e');
rows.push('| Variant | Unit s | E2e s | E2e tests s | Merge s | Total s | Total × plain | E2e tests × plain |');
rows.push('|---|---|---|---|---|---|---|---|');
rows.push(
  `| plain | ${of('plain', 'unit').toFixed(2)} | ${of('plain', 'e2e').toFixed(2)} | ${of('plain', 'e2e', 'testSeconds').toFixed(2)} | | ${plainTotal.toFixed(2)} | 1.000 | 1.000 |`,
);
for (const { name } of binaries) {
  const total = of(name, 'unit') + of(name, 'e2e') + of(name, 'merge');
  rows.push(
    `| ${name} | ${of(name, 'unit').toFixed(2)} | ${of(name, 'e2e').toFixed(2)} | ${of(name, 'e2e', 'testSeconds').toFixed(2)} | ${of(name, 'merge').toFixed(2)} | ${total.toFixed(2)} | ${(total / plainTotal).toFixed(3)} | ${(of(name, 'e2e', 'testSeconds') / of('plain', 'e2e', 'testSeconds')).toFixed(3)} |`,
  );
}
const table = `Medians of ${rounds} rounds, e2e flow repeated ${repeat} times.\n\n${rows.join('\n')}\n`;
writeFileSync(join(output, 'summary.md'), table);
if (process.env.GITHUB_STEP_SUMMARY) appendFileSync(process.env.GITHUB_STEP_SUMMARY, table);
console.log(table);
