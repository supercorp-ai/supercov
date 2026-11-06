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
  readdirSync,
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

function run(cwd, command, args, label, extra = {}, mayFail = false) {
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
  if (!mayFail) assert.equal(result.status, 0, `${label} failed: see ${label}.log`);
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
  expect: { timeout: 15_000 },
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
  // The app's own unit tests compare a time to the minute, and fail now and
  // then as the minute turns: plain or measured alike.
  { name: 'unit', command: ['npx', 'vitest', 'run'], mayFail: true },
  {
    name: 'e2e',
    // On a shared runner the flow now and then misses a timeout, and the
    // repetition after it finds that one's discussion left behind: a
    // failure is counted, and the passing repetitions are what is timed.
    mayFail: true,
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

// The flow's own time from Playwright's report, its mean over the
// repetitions that passed: the server's build and start are left out, so
// this is what each action and page load cost.
function flowTimes(report) {
  const tests = [];
  const walk = (suite) => {
    for (const spec of suite.specs ?? [])
      for (const test of spec.tests ?? [])
        for (const result of test.results ?? []) tests.push(result);
    for (const child of suite.suites ?? []) walk(child);
  };
  for (const suite of JSON.parse(readFileSync(report, 'utf8')).suites) walk(suite);
  assert.ok(tests.length > 0, `${report} holds no test`);
  const passed = tests.filter((result) => result.status === 'passed');
  const failed = tests.length - passed.length;
  assert.ok(passed.length > failed, `${report}: most of the flow failed`);
  return {
    testSeconds: passed.reduce((sum, result) => sum + result.duration, 0) / passed.length / 1000,
    failed,
  };
}

const samples = [];
function sample(round, variant, suite, seconds, extra = {}) {
  const entry = { round, variant, suite, seconds, ...extra };
  samples.push(entry);
  appendFileSync(join(output, 'samples.jsonl'), `${JSON.stringify(entry)}\n`);
  console.log(
    `[speed] round ${round} ${variant} ${suite} ${seconds.toFixed(2)} s` +
      (extra.testSeconds ? ` (mean passing test ${extra.testSeconds.toFixed(2)} s, ${extra.failed} failed)` : ''),
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
    suite.mayFail,
  );
  const extra = existsSync(report) ? flowTimes(report) : {};
  sample(round, variant, suite.name, seconds, extra);
  if (!binary) return;
  const latest = JSON.parse(
    run(project, binary, ['runs', 'latest', '--json'], `${round}-${variant}-${suite.name}-latest`)
      .stdout,
  ).data;
  // A run whose tests failed is published all the same, and not valid: the
  // flakes the samples count are no reason to stop measuring.
  const id = latest.run ?? latest.id ?? latest.runId;
  assert.ok(id, `${variant} ${suite.name} published no run`);
  return id;
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

// One more measured pass, counted in no median, with Supercov's timing on:
// where each run's setup went, and every browser round-trip of each test.
if (process.env.SPEED_DIAGNOSE === '1') {
  const { name, path } = binaries.at(-1);
  rmSync(join(project, '.supercov'), { recursive: true, force: true });
  for (const suite of suites)
    run(project, path, ['--', ...suite.command], `diagnose-${name}-${suite.name}`, { SUPERCOV_PHASE_TIMING: '1' }, true);
  // Then every Node process of one plain and one measured run of each suite,
  // CPU-profiled: where the time Supercov adds is spent, by whose code.
  // A pool that ends its workers with a signal leaves them no profile to
  // write, and a plain Vitest run's workers wrote none: each process here
  // exits on the signal instead, plain and measured alike.
  const exitOnSignal = join(output, 'exit-on-signal.cjs');
  writeFileSync(
    exitOnSignal,
    "for (const signal of ['SIGTERM', 'SIGINT', 'SIGHUP']) process.on(signal, () => process.exit(0));\n",
  );
  const profiled = {};
  for (const [variant, binary] of [['plain'], [name, path]]) {
    rmSync(join(project, '.supercov'), { recursive: true, force: true });
    for (const suite of suites) {
      const directory = join(output, 'profiles', `${variant}-${suite.name}`);
      mkdirSync(directory, { recursive: true });
      const options = `${process.env.NODE_OPTIONS ?? ''} --require=${exitOnSignal} --cpu-prof --cpu-prof-dir=${directory}`.trim();
      const [command, ...args] = binary ? [binary, '--', ...suite.command] : suite.command;
      run(project, command, args, `profile-${variant}-${suite.name}`, { NODE_OPTIONS: options }, true);
      const { totals, functions } = profileSelfTime(directory);
      profiled[`${variant} ${suite.name}`] = totals;
      const heaviest = Object.entries(functions)
        .sort(([, a], [, b]) => b - a)
        .slice(0, 25)
        .map(([what, seconds]) => `${seconds.toFixed(3)}s  ${what}`);
      writeFileSync(join(output, `profile-top-${variant}-${suite.name}.txt`), `${heaviest.join('\n')}\n`);
    }
  }
  const kinds = [...new Set(Object.values(profiled).flatMap((totals) => Object.keys(totals)))].sort();
  const table = [
    `| CPU self-time, s | ${Object.keys(profiled).join(' | ')} |`,
    `|---|${Object.keys(profiled).map(() => '---').join('|')}|`,
    ...kinds.map((kind) => `| ${kind} | ${Object.values(profiled).map((totals) => (totals[kind] ?? 0).toFixed(2)).join(' | ')} |`),
  ].join('\n');
  writeFileSync(join(output, 'profile.md'), `${table}\n`);
  if (process.env.GITHUB_STEP_SUMMARY) appendFileSync(process.env.GITHUB_STEP_SUMMARY, `\n${table}\n`);
  console.log(table);
}

// A directory of `.cpuprofile` files as seconds of self-time by whose code
// ran: Supercov's runtime, the app, a package, Node itself, or the engine's
// own work (garbage collection, the idle and program nodes).
function profileSelfTime(directory) {
  const totals = {};
  const functions = {};
  const kindOf = (url, functionName) => {
    if (!url) return functionName.startsWith('(') ? functionName : '(native)';
    if (url.includes('/.supercov/node_modules/') || url.includes('/supercov/runtime/')) return 'supercov runtime';
    if (url.startsWith('node:')) return /source_map|sourcemap|prepareStackTrace/i.test(url + functionName) ? 'node source maps' : 'node';
    const module = /node_modules\/((?:@[^/]+\/)?[^/]+)/.exec(url);
    if (module) return `pkg ${module[1]}`;
    return 'app';
  };
  for (const file of readdirSync(directory).filter((name) => name.endsWith('.cpuprofile'))) {
    const profile = JSON.parse(readFileSync(join(directory, file), 'utf8'));
    const nodes = new Map(profile.nodes.map((node) => [node.id, node]));
    const self = new Map();
    profile.samples.forEach((id, index) => self.set(id, (self.get(id) ?? 0) + (profile.timeDeltas[index] ?? 0)));
    for (const [id, micros] of self) {
      const { url, functionName, lineNumber } = nodes.get(id).callFrame;
      const kind = kindOf(url, functionName);
      totals[kind] = (totals[kind] ?? 0) + micros / 1e6;
      if (!functionName.startsWith('(')) {
        const what = `${kind}: ${functionName || '(anonymous)'} ${url.split('/').slice(-2).join('/')}:${lineNumber + 1}`;
        functions[what] = (functions[what] ?? 0) + micros / 1e6;
      }
    }
  }
  return { totals, functions };
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
const failures = (variant) =>
  samples
    .filter((entry) => entry.variant === variant)
    .reduce((sum, entry) => sum + (entry.failed ?? 0), 0);
rows.push('| Variant | Unit s | E2e s | E2e mean test s | E2e failed | Merge s | Total s | Total × plain | E2e test × plain |');
rows.push('|---|---|---|---|---|---|---|---|---|');
rows.push(
  `| plain | ${of('plain', 'unit').toFixed(2)} | ${of('plain', 'e2e').toFixed(2)} | ${of('plain', 'e2e', 'testSeconds').toFixed(2)} | ${failures('plain')} | | ${plainTotal.toFixed(2)} | 1.000 | 1.000 |`,
);
for (const { name } of binaries) {
  const total = of(name, 'unit') + of(name, 'e2e') + of(name, 'merge');
  rows.push(
    `| ${name} | ${of(name, 'unit').toFixed(2)} | ${of(name, 'e2e').toFixed(2)} | ${of(name, 'e2e', 'testSeconds').toFixed(2)} | ${failures(name)} | ${of(name, 'merge').toFixed(2)} | ${total.toFixed(2)} | ${(total / plainTotal).toFixed(3)} | ${(of(name, 'e2e', 'testSeconds') / of('plain', 'e2e', 'testSeconds')).toFixed(3)} |`,
  );
}
const table = `Medians of ${rounds} rounds, e2e flow repeated ${repeat} times.\n\n${rows.join('\n')}\n`;
writeFileSync(join(output, 'summary.md'), table);
if (process.env.GITHUB_STEP_SUMMARY) appendFileSync(process.env.GITHUB_STEP_SUMMARY, table);
console.log(table);
