// A Next.js application with a Vitest suite and a Playwright suite that
// builds and serves the application itself, measured the way its developer
// would: unit, then end to end, then merged.
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
import { existsSync, rmSync, writeFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { spawnSync } from 'node:child_process';

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
  return { status: result.status, output: `${result.stdout}\n${result.stderr}` };
}

function supercov(args, env) {
  return run(binary, args, env);
}

function json(args) {
  const result = supercov([...args, '--json']);
  assert.equal(result.status, 0, result.output);
  return JSON.parse(result.output.slice(result.output.indexOf('{'))).data;
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
assert.match(e2e.output, /3 passed/);
assert.match(e2e.output, /stayed in the isolated workspace[^\n]*\.next\/ \d+/);
assert.ok(!existsSync(resolve(fixture, '.next')), 'an instrumented build reached the project');
const e2eRun = latest();
// A route under a folder named `test` is served, measured and covered.
const route = supercov(['runs', e2eRun, 'file', 'app/api/widget/test/route.ts']);
assert.equal(route.status, 0, route.output);
assert.match(route.output, /Lines not executed\s+0/, route.output);
// Server start-up ran before any test: the kind's row and the run's total
// differ by exactly what no test ran.
const summary = json(['runs', e2eRun]);
const kind = summary.coverageByKind.find((entry) => entry.kind === 'e2e');
assert.equal(kind.tests, 3);
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

// Roots named now, on the workspace the runs above left, take effect.
const named = supercov(['--', 'npm', 'run', 'test:unit'], {
  SUPERCOV_SOURCE_ROOTS: 'app,components,lib,simulators,proxy.ts,instrumentation.ts',
});
assert.equal(named.status, 0, named.output);
const namedScope = json(['runs', latest(), 'scope']);
assert.equal(namedScope.mode, 'explicit');
assert.equal(namedScope.counts.ambiguous, 0);
assert.equal(namedScope.unclassifiedSource, undefined);

console.log('[next-playwright] a Next.js application is measured through its unit and end-to-end suites, merged and read by directory');
