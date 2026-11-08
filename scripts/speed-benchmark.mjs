// How much longer a real Next.js application's tests take under Supercov
// than without it: the application SPEED_APP names (see `applications`),
// its Vitest suite and its Playwright suite against a production build. Each
// round runs the plain suites, then every binary named in
// SUPERCOV_BINARIES (`name=path,...`) in an order that turns each round, so
// no binary always runs on a warmer machine. A Supercov run starts with no
// `.supercov`, as on a fresh CI checkout, and ends with a merge of its two
// runs, as a pipeline that measures suites apart does.
import assert from 'node:assert/strict';
import { spawn, spawnSync } from 'node:child_process';
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
    env: { ...process.env, CI: 'true', ...extra },
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

// A command run as `run` runs it, with samply attached to its own process
// (not its children) until it exits.
async function attachedProfile(cwd, command, args, label, profile) {
  const child = spawn(command, args, {
    cwd,
    env: { ...process.env, CI: 'true', SUPERCOV_PHASE_TIMING: '1' },
    stdio: ['ignore', 'pipe', 'pipe'],
  });
  let text = '';
  child.stdout.on('data', (chunk) => { text += chunk; });
  child.stderr.on('data', (chunk) => { text += chunk; });
  const sampler = spawn(process.env.SPEED_SAMPLY, ['record', '--save-only', '--unstable-presymbolicate', '-o', profile, '-p', String(child.pid)], { stdio: 'ignore' });
  await new Promise((resolve) => child.on('close', resolve));
  writeFileSync(join(logs, `${label}.log`), text);
  const stopped = new Promise((resolve) => sampler.on('close', resolve));
  setTimeout(() => sampler.kill('SIGINT'), 5_000).unref();
  await stopped;
}

function checkout(directory, repository, commit) {
  if (existsSync(directory)) return;
  mkdirSync(directory);
  run(directory, 'git', ['init', '-q'], 'git-init');
  run(directory, 'git', ['fetch', '-q', '--depth', '1', repository, commit], 'git-fetch');
  run(directory, 'git', ['checkout', '-q', '--detach', 'FETCH_HEAD'], 'git-checkout');
}

// Each application at a fixed commit: where its suites run, what prepares
// it once, and what puts its state back before each run.
const applications = {
  // Bulletproof React's Next.js app: its Vitest suite, and one Playwright
  // flow repeated against a production build and the app's own mock API.
  bulletproof() {
    const root = join(output, 'bulletproof-react');
    checkout(root, 'https://github.com/alan2207/bulletproof-react.git', '9506629ed003a561c6627735480cce4994244bb4');
    const project = join(root, 'apps/nextjs-app');
    run(project, 'npx', ['-y', 'yarn@1.22.22', 'install', '--frozen-lockfile', '--non-interactive'], 'install');
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
    return {
      project,
      description: `the e2e flow repeated ${repeat} times`,
      suites: [
        // The app's own unit tests compare a time to the minute, and fail now
        // and then as the minute turns: plain or measured alike.
        { name: 'unit', command: ['npx', 'vitest', 'run'], mayFail: true },
        {
          name: 'e2e',
          // On a shared runner the flow now and then misses a timeout, and the
          // repetition after it finds that one's discussion left behind: a
          // failure is counted, and the passing repetitions are what is timed.
          mayFail: true,
          command: ['npx', 'playwright', 'test', '--config', 'playwright.speed.config.ts', '--repeat-each', String(repeat)],
        },
      ],
      reset() {},
    };
  },
  // Rallly, a pnpm monorepo on Next 16 and Turbopack: the web app's 1,100
  // Vitest tests, and its 170 Playwright tests against a production build,
  // Postgres on localhost:5450 and Mailpit on localhost:8025, as its .env.test
  // says.
  rallly() {
    const root = join(output, 'rallly');
    checkout(root, 'https://github.com/lukevella/rallly.git', '26111d7e993d2e7016810f68bcad139393644db0');
    const project = join(root, 'apps/web');
    const pnpm = ['-y', 'pnpm@12.14.0'];
    run(root, 'npx', [...pnpm, 'install', '--frozen-lockfile'], 'install');
    run(root, 'npx', [...pnpm, 'db:generate'], 'db-generate');
    // Its own configuration, with the build in the command (Supercov measures
    // the server it builds) and the one worker its suite is written for: the
    // tests share one database and one mailbox.
    writeFileSync(
      join(project, 'playwright.speed.config.ts'),
      `import base from './playwright.config';

export default {
  ...base,
  workers: 1,
  retries: 0,
  reporter: [['list'], ['json']],
  webServer: {
    ...base.webServer,
    command: 'NODE_ENV=test npx next build && npx next start --port 3002',
    timeout: 900_000,
    // Stopped with a signal it can exit on, so a profiled server writes its
    // profile: Playwright's default is SIGKILL.
    gracefulShutdown: { signal: 'SIGTERM', timeout: 10_000 },
  },
};
`,
    );
    run(project, 'npx', ['playwright', 'install', 'chromium'], 'browser');
    return {
      project,
      description: 'the e2e suite once',
      suites: [
        { name: 'unit', command: ['npx', 'vitest', 'run'], mayFail: true },
        // A slow runner times a test out now and then: counted, and the
        // passing tests are what is timed.
        { name: 'e2e', command: ['npx', 'playwright', 'test', '--config', 'playwright.speed.config.ts'], mayFail: true },
      ],
      // Every run starts from an empty database and mailbox, as each of
      // rallly's CI shards does.
      reset(label) {
        const database = 'postgres://postgres:postgres@localhost:5450/rallly';
        run(
          root,
          'npx',
          [...pnpm, '--filter', '@rallly/database', 'exec', 'prisma', 'migrate', 'reset', '--force'],
          `${label}-db-reset`,
          { DATABASE_URL: database, DIRECT_DATABASE_URL: database },
        );
        run(root, 'curl', ['-fsS', '-X', 'DELETE', 'http://localhost:8025/api/v1/messages'], `${label}-mail-reset`);
      },
    };
  },
};
const application = process.env.SPEED_APP ?? 'bulletproof';
assert.ok(applications[application], `SPEED_APP names no application: ${application}`);
const { project, description, suites, reset } = applications[application]();

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

// Each failed test's Playwright trace, from the project or Supercov's copy of
// it, kept under results/ for the workflow to upload.
function keepTraces(label) {
  const found = [];
  const walk = (directory, depth) => {
    if (depth > 12) return;
    for (const entry of readdirSync(directory, { withFileTypes: true })) {
      const path = join(directory, entry.name);
      if (entry.isDirectory() && entry.name !== 'node_modules') walk(path, depth + 1);
      else if (entry.isFile() && entry.name === 'trace.zip') found.push(path);
    }
  };
  for (const root of [join(project, 'test-results'), join(project, '.supercov/workspaces')])
    if (existsSync(root)) walk(root, 0);
  found.forEach((trace, index) => {
    const destination = join(output, 'results', label, `${index}-${trace.split('/').at(-2)}`);
    mkdirSync(destination, { recursive: true });
    copyFileSync(trace, join(destination, 'trace.zip'));
  });
  rmSync(join(project, 'test-results'), { recursive: true, force: true });
}

function suiteRun(round, variant, suite, binary) {
  reset(`${round}-${variant}-${suite.name}`);
  // Every run builds from nothing, as on a fresh CI checkout: Supercov's copy
  // never has the project's build, and a plain run after the first found the
  // one the run before it left and built warm.
  rmSync(join(project, '.next'), { recursive: true, force: true });
  const report = join(output, `report-${round}-${variant}-${suite.name}.json`);
  // Supercov's copy is kept, for the traces of the tests that failed in it.
  const env = { PLAYWRIGHT_JSON_OUTPUT_NAME: report, SUPERCOV_KEEP_WORKSPACE: '1' };
  const [command, ...args] = binary
    ? [binary, '--', ...suite.command]
    : suite.command;
  // CPU by kind of process (scripts/speed/cpu-sampler.py), on Linux.
  const cpuFile = join(output, `cpu-${round}-${variant}-${suite.name}.json`);
  const sampler = process.platform === 'linux'
    ? spawn('python3', [new URL('./speed/cpu-sampler.py', import.meta.url).pathname, cpuFile], { stdio: 'ignore' })
    : undefined;
  const { seconds } = run(
    project,
    command,
    args,
    `${round}-${variant}-${suite.name}`,
    env,
    suite.mayFail,
  );
  sampler?.kill();
  const extra = existsSync(report) ? flowTimes(report) : {};
  sample(round, variant, suite.name, seconds, extra);
  keepTraces(`${round}-${variant}-${suite.name}`);
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
  for (const suite of suites) {
    reset(`diagnose-${name}-${suite.name}`);
    if (suite.name === 'e2e' && process.env.SPEED_SAMPLY) {
      // The e2e run's Supercov process alone, natively profiled: its setup,
      // evidence and publication, without the browser and Node beside it.
      await attachedProfile(project, path, ['--', ...suite.command], `diagnose-${name}-${suite.name}`, join(output, 'samply-e2e.json.gz'));
    } else {
      run(project, path, ['--', ...suite.command], `diagnose-${name}-${suite.name}`, { SUPERCOV_PHASE_TIMING: '1' }, true);
    }
  }
  // Supercov's own work, natively profiled: a whole measured unit run (its
  // setup, evidence and publication beside Vitest), and the merge of the two
  // runs just made, which publishes the merged run.
  if (process.env.SPEED_SAMPLY) {
    const unit = suites.find((suite) => suite.name === 'unit');
    if (unit) {
      reset('samply-unit');
      run(
        project,
        process.env.SPEED_SAMPLY,
        ['record', '--save-only', '--unstable-presymbolicate', '-o', join(output, 'samply-unit.json.gz'), '--', path, '--', ...unit.command],
        'samply-unit',
        {},
        true,
      );
    }
    run(
      project,
      process.env.SPEED_SAMPLY,
      ['record', '--save-only', '--unstable-presymbolicate', '-o', join(output, 'samply-merge.json.gz'), '--', path, 'merge'],
      'samply-merge',
      {},
      true,
    );
  }
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
      reset(`profile-${variant}-${suite.name}`);
      const directory = join(output, 'profiles', `${variant}-${suite.name}`);
      mkdirSync(directory, { recursive: true });
      const options = `${process.env.NODE_OPTIONS ?? ''} --require=${exitOnSignal} --cpu-prof --cpu-prof-dir=${directory}`.trim();
      const [command, ...args] = binary ? [binary, '--', ...suite.command] : suite.command;
      run(project, command, args, `profile-${variant}-${suite.name}`, { NODE_OPTIONS: options, SUPERCOV_KEEP_WORKSPACE: '1' }, true);
      const { totals, functions, processes } = profileSelfTime(directory);
      // Each process that kept a CPU busy for a second or more: which one
      // the time went to, by whose code.
      writeFileSync(
        join(output, `profile-processes-${variant}-${suite.name}.txt`),
        processes
          .filter(({ busy }) => busy >= 1)
          .sort((a, b) => b.busy - a.busy)
          .map(({ file, busy, kinds, functions }) => [`== ${file} busy ${busy.toFixed(2)}s`, ...kinds, '--', ...functions].join('\n'))
          .join('\n\n') + '\n',
      );
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
  // What the browser loads: each build's client chunks, plain's in the
  // project and the measured one's in Supercov's kept copy.
  const bundles = run(
    project,
    'sh',
    ['-c', "find . -path '*/.next/static' -prune -type d -exec du -sk {} \\; ; find . -path '*/.next/static/chunks/*.js' | xargs wc -c | sort -n | tail -15"],
    'bundles',
    {},
    true,
  ).stdout;
  // And what the build was given: the project's sources against the copy's
  // instrumented ones, and how much of the copy is inline source maps.
  const sources = run(
    project,
    'python3',
    ['-c', `
import os, re, sys
def walk(root):
    total = maps = files = 0
    for base, dirs, names in os.walk(root):
        dirs[:] = [d for d in dirs if d not in ('node_modules', '.next', '.git', '.supercov')]
        for name in names:
            if not re.search(r'\\.(m?[jt]sx?|cjs)$', name):
                continue
            text = open(os.path.join(base, name), 'rb').read()
            files += 1
            total += len(text)
            at = text.rfind(b'sourceMappingURL=data:')
            if at >= 0:
                maps += len(text) - at
    return files, total, maps
copy = next((os.path.join(b, d) for b, ds, _ in os.walk('.supercov/workspaces') for d in ds if d == 'web' and b.endswith('apps')), None)
for label, root in (('project', '.'), ('copy', copy)):
    if root:
        files, total, maps = walk(root)
        print(f'{label}: {files} files, {total/1e6:.1f} MB, inline maps {maps/1e6:.1f} MB')
# Three source files as the build was given them, beside their originals:
# the largest component, and a page and a server module of middling size.
import shutil
if copy:
    picked = []
    for pattern in (r'components/.*\\.tsx$', r'app/.*page\\.tsx$', r'(server|lib)/.*\\.ts$'):
        found = []
        for base, dirs, names in os.walk('src'):
            for name in names:
                path = os.path.join(base, name)
                if re.search(pattern, path):
                    found.append((os.path.getsize(path), path))
        found.sort()
        if found:
            picked.append(found[-1][1] if pattern.startswith('components') else found[len(found) // 2][1])
    out = sys.argv[1]
    os.makedirs(out, exist_ok=True)
    for index, path in enumerate(picked):
        shutil.copy(path, os.path.join(out, f'{index}-original-{os.path.basename(path)}'))
        shutil.copy(os.path.join(copy, path), os.path.join(out, f'{index}-instrumented-{os.path.basename(path)}'))
        print('sample', path)
`, join(output, 'samples')],
    'sources',
    {},
    true,
  ).stdout;
  writeFileSync(join(output, 'bundles.txt'), `${bundles}\n${sources}`);
  if (process.env.GITHUB_STEP_SUMMARY) appendFileSync(process.env.GITHUB_STEP_SUMMARY, `\n${table}\n`);
  console.log(table);
}

// A directory of `.cpuprofile` files as seconds of self-time by whose code
// ran: Supercov's runtime, the app, a package, Node itself, or the engine's
// own work (garbage collection, the idle and program nodes).
function profileSelfTime(directory) {
  const totals = {};
  const functions = {};
  const processes = [];
  const kindOf = (url, functionName) => {
    if (!url) return functionName.startsWith('(') ? functionName : '(native)';
    if (url.includes('/.supercov/node_modules/') || url.includes('/supercov/runtime/')) return 'supercov runtime';
    if (url.startsWith('node:')) return /source_map|sourcemap|prepareStackTrace/i.test(url + functionName) ? 'node source maps' : 'node';
    // The last node_modules in the path: pnpm keeps each package under
    // node_modules/.pnpm/<name>@<version>/node_modules/<name>.
    const module = /.*node_modules\/((?:@[^/]+\/)?[^/]+)/.exec(url);
    if (module) return `pkg ${module[1]}`;
    return 'app';
  };
  for (const file of readdirSync(directory).filter((name) => name.endsWith('.cpuprofile'))) {
    const profile = JSON.parse(readFileSync(join(directory, file), 'utf8'));
    const nodes = new Map(profile.nodes.map((node) => [node.id, node]));
    const self = new Map();
    profile.samples.forEach((id, index) => self.set(id, (self.get(id) ?? 0) + (profile.timeDeltas[index] ?? 0)));
    const own = {};
    const ownFunctions = {};
    for (const [id, micros] of self) {
      const { url, functionName, lineNumber } = nodes.get(id).callFrame;
      const kind = kindOf(url, functionName);
      totals[kind] = (totals[kind] ?? 0) + micros / 1e6;
      if (kind !== '(idle)') own[kind] = (own[kind] ?? 0) + micros / 1e6;
      if (!functionName.startsWith('(')) {
        const what = `${kind}: ${functionName || '(anonymous)'}`;
        ownFunctions[what] = (ownFunctions[what] ?? 0) + micros / 1e6;
      }
      if (!functionName.startsWith('(')) {
        const what = `${kind}: ${functionName || '(anonymous)'} ${url.split('/').slice(-2).join('/')}:${lineNumber + 1}`;
        functions[what] = (functions[what] ?? 0) + micros / 1e6;
      }
    }
    const busy = Object.values(own).reduce((sum, seconds) => sum + seconds, 0);
    const top = (table, count) =>
      Object.entries(table)
        .sort(([, a], [, b]) => b - a)
        .slice(0, count)
        .map(([what, seconds]) => `${seconds.toFixed(2)}s ${what}`);
    processes.push({ file, busy, kinds: top(own, 8), functions: top(ownFunctions, 30) });
  }
  return { totals, functions, processes };
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
// CPU seconds by kind of process, for the first round's runs.
const cpu = Object.fromEntries(
  readdirSync(output)
    .filter((name) => /^cpu-1-.*\.json$/.test(name))
    .map((name) => [name.slice(6, -5), JSON.parse(readFileSync(join(output, name), 'utf8'))]),
);
const cpuKinds = [...new Set(Object.values(cpu).flatMap((totals) => Object.keys(totals)))].sort();
const cpuTable = Object.keys(cpu).length === 0 ? '' : [
  '',
  `| CPU s, round 1 | ${Object.keys(cpu).join(' | ')} |`,
  `|---|${Object.keys(cpu).map(() => '---').join('|')}|`,
  ...cpuKinds.map((kind) => `| ${kind} | ${Object.values(cpu).map((totals) => (totals[kind] ?? 0).toFixed(1)).join(' | ')} |`),
].join('\n');
const machineFile = join(process.env.RUNNER_TEMP ?? output, 'machine.txt');
const machine = existsSync(machineFile) ? readFileSync(machineFile, 'utf8').trim().replace(/\s+/g, ' ') : 'unknown machine';
const table = `${application}: medians of ${rounds} rounds, ${description}; ${machine}.\n\n${rows.join('\n')}\n${cpuTable}\n`;
writeFileSync(join(output, 'summary.md'), table);
if (process.env.GITHUB_STEP_SUMMARY) appendFileSync(process.env.GITHUB_STEP_SUMMARY, table);
console.log(table);
