// A Next.js application with a Vitest suite and a Playwright suite that
// builds and serves the application itself, measured the way its developer
// would: unit, then end to end, then merged, and then the merged run through
// everything that reads one: its assertions assessed, `quality`, and
// `security` beside it.
//
// Supercov 3.0.4 failed such an application at two consecutive steps before
// an end-to-end test ran, and scoped it wrongly on top. Each property of the
// fixture is one of the ordinary choices that did it:
//
// - `scripts/use.ts`, outside the source roots and inside the TypeScript
//   program, calls a function whose return type is inferred through
//   narrowing: `next build` type-checked the instrumented copy and failed.
// - `package.json` names no `"type"` and `playwright.config.ts` is written
//   with `import`: the generated wrapper could not import it (Playwright 1.60
//   on Node 24, which is why the fixture pins its own dependencies).
// - The Playwright `webServer` runs `next build` itself: the instrumented
//   `.next/` was synced into the project.
// - `components/`, `proxy.ts` and `instrumentation.ts` at the root were
//   unclassified, and `app/api/widget/test/route.ts` was left out as a test.
import assert from 'node:assert/strict';
import { existsSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { resolve } from 'node:path';
import { spawn, spawnSync } from 'node:child_process';

const repository = resolve(import.meta.dirname, '..');
const binary = process.env.SUPERCOV_BINARY ?? resolve(repository, `target/debug/supercov${process.platform === 'win32' ? '.exe' : ''}`);
const fixture = resolve(repository, 'tests/fixtures/next-playwright');

function run(command, args, env = {}) {
  const result = spawnSync(command, args, {
    cwd: fixture,
    encoding: 'utf8',
    env: { ...process.env, CI: '1', NO_COLOR: '1', ...env },
    timeout: 600_000,
    maxBuffer: 64 * 1024 * 1024,
  });
  return { status: result.status, stdout: result.stdout, output: `${result.stdout}\n${result.stderr}` };
}

function supercov(args, env) {
  return run(binary, args, env);
}

// A server answering as Jev does, yes or no to everything, and the
// environment that sends a command's questions to it. `asked()` is how many
// requests it has answered.
async function standIn(answer) {
  const directory = mkdtempSync(resolve(tmpdir(), 'supercov-jev-'));
  const seen = resolve(directory, 'seen');
  const child = spawn(process.execPath, [resolve(repository, 'scripts/jev-stand-in.mjs')], {
    env: { ...process.env, ANSWER: answer, SEEN: seen },
    stdio: ['ignore', 'pipe', 'inherit'],
  });
  const port = await new Promise((settle, fail) => {
    child.stdout.once('data', (chunk) => settle(String(chunk).trim()));
    child.once('error', fail);
    child.once('exit', (code) => fail(new Error(`the stand-in exited with ${code}`)));
  });
  return {
    env: {
      TYPESAFE_BASE_URL: `http://127.0.0.1:${port}`,
      TYPESAFE_API_KEY: 'sk-stand-in',
      TYPESAFE_DEFAULT_MODEL: 'typesafe/jev-1.13',
    },
    asked: () => (existsSync(seen) ? Number(readFileSync(seen, 'utf8')) : 0),
    // The directory goes with the server: left behind, every run of the gate
    // added one.
    stop: () => {
      child.kill();
      try {
        rmSync(directory, { recursive: true, force: true });
      } catch {
        // The killed server still holding its file on Windows.
      }
    },
  };
}

function parsed(args, env) {
  const result = supercov([...args, '--json'], env);
  assert.equal(result.status, 0, result.output);
  return JSON.parse(result.stdout.slice(result.stdout.indexOf('{')));
}

function json(args, env) {
  return parsed(args, env).data;
}

function latest() {
  return json(['runs', '--limit', '1']).runs[0].id;
}

if (!existsSync(resolve(fixture, 'node_modules/next/package.json'))) {
  const installed = run('npm', ['ci', '--no-audit', '--no-fund']);
  assert.equal(installed.status, 0, installed.output);
}
for (const generated of ['.supercov', '.next', 'test-results', 'next-env.d.ts'])
  rmSync(resolve(fixture, generated), { recursive: true, force: true });
writeFileSync(resolve(fixture, '.env.test.local'), 'from-an-ignored-file\n');

// The unit suite, with nothing configured.
const unit = supercov(['--', 'npm', 'run', 'test:unit'], { SUPERCOV_TEST_KIND: 'unit' });
assert.equal(unit.status, 0, unit.output);
const unitRun = latest();
const scope = json(['runs', unitRun, 'scope', '--limit', '100']);
const status = Object.fromEntries(scope.entries.map((entry) => [entry.file, entry.status]));
for (const file of [
  'app/api/edge/route.ts',
  'app/api/widget/test/route.ts',
  'app/page.tsx',
  'components/greeting.tsx',
  'instrumentation.ts',
  'lib/client.ts',
  'proxy.ts',
])
  assert.equal(status[file], 'included', `${file}\n${JSON.stringify(status, null, 2)}`);
assert.equal(status['scripts/use.ts'], 'excluded');
assert.equal(status['tests/unit/client.test.ts'], 'excluded');
assert.ok(!scope.roots.includes('next-env.d.ts'), scope.roots.join(', '));
// What nothing classifies is named with what would settle it.
assert.deepEqual(scope.unclassifiedSource.directories, [{ directory: 'simulators', files: 1 }]);
assert.ok(scope.unclassifiedSource.roots.includes('simulators'));

// The end-to-end suite, whose own web server builds the application.
// Supercov runs the command and no build of its own: it used to run the
// project's `build` script first, twenty seconds before a suite that builds.
const e2e = supercov(['--', 'npm', 'run', 'test:e2e'], { SUPERCOV_TEST_KIND: 'e2e' });
assert.equal(e2e.status, 0, e2e.output);
assert.match(e2e.output, /4 passed/);
assert.match(e2e.output, /stayed in the isolated workspace[^\n]*\.next\/ \d+/);
assert.ok(!existsSync(resolve(fixture, '.next')), 'an instrumented build reached the project');
// Next warns about more than one lockfile above it. The copy's own, inside
// the project, is not one of them. (Lockfiles above the fixture are, and when
// this gate itself runs inside a Supercov copy their paths hold
// `.supercov/workspaces` too.)
const ownCopy = resolve(fixture, '.supercov/workspaces');
assert.ok(
  !e2e.output.split('\n').some((line) => /^\[WebServer\]\s+\* /u.test(line) && line.includes(ownCopy)),
  e2e.output,
);
const e2eRun = latest();
// A route under a folder named `test` is served, measured and covered.
const route = supercov(['runs', e2eRun, 'file', 'app/api/widget/test/route.ts']);
assert.equal(route.status, 0, route.output);
assert.match(route.output, /Lines not executed\s+0/, route.output);
// A route with `runtime = 'edge'` runs in a VM context of Next's own, where
// nothing had installed the runtime: `next build` failed collecting its page
// data, and a middleware.ts failed every request. It is measured there, and
// what a test's request runs is that test's.
const edge = supercov(['runs', e2eRun, 'line', 'app/api/edge/route.ts:5']);
assert.equal(edge.status, 0, edge.output);
assert.match(edge.output, /COVERED/, edge.output);
assert.match(edge.output, /edge route grades a value/, edge.output);
// Server start-up ran before any test: the kind's row and the run's total
// differ by exactly what no test ran.
const summary = json(['runs', e2eRun]);
const kind = summary.coverageByKind.find((entry) => entry.kind === 'e2e');
assert.equal(kind.tests, 4);
assert.equal(summary.coverageByTests.lines.covered, kind.summary.lines.covered);
assert.ok(summary.coverage.lines.covered > summary.coverageByTests.lines.covered, JSON.stringify(summary.coverage.lines));
const text = supercov(['runs', e2eRun]);
assert.match(text.output, /no test\s+lines\s+\d/);

// Both suites as one result, read by directory and by kind.
const merged = supercov(['merge', unitRun, e2eRun]);
assert.equal(merged.status, 0, merged.output);
const mergedRun = latest();
const areas = json(['runs', mergedRun, 'files', '--group', 'dir']);
assert.deepEqual([...areas.kinds].sort(), ['e2e', 'unit']);
const area = Object.fromEntries(areas.areas.map((entry) => [entry.directory, entry]));
for (const directory of ['app', 'components', 'lib']) {
  assert.ok(area[directory], `${directory}\n${JSON.stringify(areas.areas, null, 2)}`);
  assert.ok(area[directory].covered.lines <= area[directory].totals.lines);
}
const covered = (directory, name) => area[directory].byKind.find((entry) => entry.kind === name).covered.lines;
assert.equal(covered('app', 'unit'), 0);
assert.ok(covered('app', 'e2e') > 0);
assert.ok(covered('lib', 'unit') > 0);
// Per-file totals add up to the run's own.
const files = json(['runs', mergedRun, 'files', '--limit', '100']).files;
const mergedSummary = json(['runs', mergedRun]);
assert.equal(files.reduce((sum, file) => sum + file.totals.lines, 0), mergedSummary.coverage.lines.total);
assert.equal(files.reduce((sum, file) => sum + file.covered.lines, 0), mergedSummary.coverage.lines.covered);

// The merged run, read by every command that reads a run. Each was tested on
// a three-test project of one suite, and the first application to put them
// together found four defects in a row: `assess` refused a merged run, the
// run's own page named the command that refused, `security --run` took "not
// assessed" for "no test catches it", and `quality` and `security` left out
// files coverage measures. Jev is a local server here, so what is checked is
// that the commands agree with each other, not what Jev thinks.
const included = Object.keys(status).filter((file) => status[file] === 'included');
const no = await standIn('no');
const yes = await standIn('yes');
try {
  // What the run's page says to do next works when it is done.
  const unassessed = supercov(['runs', mergedRun, 'assertions']);
  assert.equal(unassessed.status, 0, unassessed.output);
  const hint = unassessed.output.match(/Assess it with `supercov ([^`]+)`/);
  assert.ok(hint, unassessed.output);
  assert.equal(hint[1], `runs ${mergedRun} assertions assess`);
  // Nobody has asked the run's tests what they catch, which is not the same
  // as their catching nothing.
  const before = supercov(['security', '--run', mergedRun], yes.env);
  assert.equal(before.status, 0, before.output);
  assert.match(before.output, /has no assertion assessment, so whether its tests would catch/, before.output);
  assert.doesNotMatch(before.output, /judged to catch/, before.output);

  // The estimate's most is what the pass sends when every answer is a no.
  const estimate = json(['runs', mergedRun, 'assertions', 'assess', '--dry-run']);
  assert.ok(estimate.firstRound.requests > 0, JSON.stringify(estimate));
  const refused = supercov(hint[1].split(' '), no.env);
  assert.equal(refused.status, 0, refused.output);
  assert.match(refused.output, /\b0% asserted/, refused.output);
  assert.equal(no.asked(), estimate.atMost.requests, refused.output);
  // And the least is the first round, when every test catches everything.
  const sentBefore = yes.asked();
  const assessed = supercov([...hint[1].split(' '), '--refresh'], yes.env);
  assert.equal(assessed.status, 0, assessed.output);
  assert.match(assessed.output, /\b100% asserted \((\d+) of \1 executed statements\)/, assessed.output);
  assert.equal(yes.asked() - sentBefore, estimate.firstRound.requests, assessed.output);
  // Both suites' tests were asked: the pages only the end-to-end suite runs
  // and the library only the unit suite runs are assessed in one result.
  const assertedFiles = json(['runs', mergedRun, 'assertions', '--all']).files.map((file) => file.file);
  for (const file of ['app/page.tsx', 'lib/client.ts'])
    assert.ok(assertedFiles.includes(file), `${file}\n${assertedFiles.join('\n')}`);
  assert.match(supercov(['runs', mergedRun]).output, /Assertions\s+100%/);

  // `quality` and `security` read every file coverage measures, the route
  // under `test/` among them, and leave unclassified what coverage does.
  for (const command of ['quality', 'security']) {
    const view = parsed([command, 'scope', '--limit', '0']);
    assert.equal(view.total_not_included, view.not_included.length, JSON.stringify(view.summary));
    const left = new Map(view.not_included.map((entry) => [entry.path, entry]));
    for (const file of included) assert.ok(!left.has(file), `${command} leaves out ${file}: ${JSON.stringify(left.get(file))}`);
    assert.deepEqual(view.summary.roots, scope.roots, command);
    for (const file of ['scripts/use.ts', 'tests/unit/client.test.ts'])
      assert.equal(left.get(file)?.status, 'excluded', `${command} ${file}`);
    const dry = supercov([command, '--dry-run'], yes.env);
    assert.equal(dry.status, 0, dry.output.slice(-2000));
    assert.match(dry.output, new RegExp(`\\[supercov\\] ${command}: \\d+ requests to 127\\.0\\.0\\.1 .* if none is cached`), dry.output.slice(-2000));
  }
  const quality = parsed(['quality'], yes.env);
  const read = quality.files.map((file) => file.path);
  for (const file of included) assert.ok(read.includes(file), `quality did not read ${file}`);

  // Beside the assessed run, security says what the tests catch.
  const beside = parsed(['security', '--run', mergedRun], yes.env);
  assert.equal(beside.run, mergedRun);
  assert.equal(beside.run_assertions, undefined, JSON.stringify(beside.run_assertions));
  assert.equal(beside.flagged_executed_unasserted, 0);
  const handler = beside.files.find((file) => file.path === 'app/api/widget/test/route.ts');
  assert.ok(handler, beside.files.map((file) => file.path).join('\n'));
  assert.equal(handler.coverage.in_run, true, JSON.stringify(handler.coverage));

  // A merge computes nothing nobody asked for: what each test ran stays with
  // the runs that recorded it, and a merge is answered from them when asked.
  const affected = supercov(['runs', mergedRun, 'tests', 'affected', '--json']);
  assert.equal(affected.status, 0, affected.output);
  const answer = JSON.parse(affected.stdout).data;
  assert.deepEqual(answer.mergedFrom, [unitRun, e2eRun], affected.output);
  const fromParts = [unitRun, e2eRun]
    .map((run) => JSON.parse(supercov(['runs', run, 'tests', 'affected', '--json']).stdout).data.summary.tests)
    .reduce((sum, tests) => sum + tests, 0);
  assert.equal(answer.summary.tests, fromParts, affected.output);
} finally {
  no.stop();
  yes.stop();
}

// Roots named now, on the workspace the runs above left, take effect.
const named = supercov(['--', 'npm', 'run', 'test:unit'], {
  SUPERCOV_SOURCE_ROOTS: 'app,components,lib,simulators,proxy.ts,instrumentation.ts',
});
assert.equal(named.status, 0, named.output);
const namedScope = json(['runs', latest(), 'scope']);
assert.equal(namedScope.mode, 'explicit');
assert.equal(namedScope.counts.ambiguous, 0);
assert.equal(namedScope.unclassifiedSource, undefined);

console.log('[next-playwright] a Next.js application is measured through its unit and end-to-end suites, merged, read by directory, and assessed for assertions, quality and security');
