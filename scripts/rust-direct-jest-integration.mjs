// A Jest suite under the Rust frontend: exact per-test identity through the
// substituted configuration (jest.config.mjs reads the user's own and adds the
// adapter and reporter), outcomes from the reporter, assertion phases for
// Jest's global `expect`, and the user's own setup file still running. Jest
// is installed into the temporary project from the registry; the repository
// does not depend on it.
import assert from 'node:assert/strict';
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { resolve } from 'node:path';
import { spawnSync } from 'node:child_process';

const repository = resolve(import.meta.dirname, '..');
const binary = resolve(repository, `target/debug/supercov${process.platform === 'win32' ? '.exe' : ''}`);
const temporary = mkdtempSync(resolve(tmpdir(), 'supercov-rust-jest-'));
const project = resolve(temporary, 'project');
const windows = process.platform === 'win32';

function rust(command, request) {
  const result = spawnSync(binary, [command], {
    cwd: repository,
    encoding: 'utf8',
    input: JSON.stringify(request),
  });
  assert.equal(result.status, 0, result.stderr || result.stdout);
  return JSON.parse(result.stdout.trim().split('\n').at(-1));
}

function query(runId, filter, command, extra = {}) {
  return rust('__query-stored-run', { root: project, query: { runId, filter, command, ...extra } });
}

try {
  mkdirSync(resolve(project, 'src'), { recursive: true });
  mkdirSync(resolve(project, 'tests'), { recursive: true });
  writeFileSync(
    resolve(project, 'package.json'),
    JSON.stringify({
      name: 'supercov-rust-jest-fixture',
      private: true,
      scripts: { test: 'jest' },
    }) + '\n',
  );
  // The user's own configuration and setup file must keep working: the setup
  // file defines a global the tests read.
  writeFileSync(
    resolve(project, 'jest.config.js'),
    "module.exports = { testEnvironment: 'node', setupFilesAfterEnv: ['<rootDir>/tests/setup.js'] };\n",
  );
  writeFileSync(resolve(project, 'tests/setup.js'), "globalThis.__fixtureSetupRan = true;\n");
  const application = [
    'function permission(admin, owner) {',
    '  if (admin || owner) {',
    "    return 'allowed';",
    '  }',
    "  return 'denied';",
    '}',
    '',
    'function never() {',
    "  return 'unreached';",
    '}',
    '',
    'module.exports = { permission, never };',
    '',
  ].join('\n');
  writeFileSync(resolve(project, 'src/permission.js'), application);
  writeFileSync(
    resolve(project, 'tests/permission.test.js'),
    [
      "const { permission } = require('../src/permission');",
      '',
      // Also touches the application: an assertion over nothing instrumented
      // is flagged by the report as evidence that may be missing.
      "test('setup file ran', () => expect(globalThis.__fixtureSetupRan && permission(true, true)).toBe('allowed'));",
      "test('admin', () => expect(permission(true, false)).toBe('allowed'));",
      "test('owner', () => expect(permission(false, true)).toBe('allowed'));",
      "test('neither', () => expect(permission(false, false)).toBe('denied'));",
      "test.each([[true, 'allowed'], [false, 'denied']])('param %s', (admin, expected) => {",
      '  expect(permission(admin, false)).toBe(expected);',
      '});',
      "test('wrong', () => expect(permission(false, false)).toBe('allowed'));",
      '',
    ].join('\n'),
  );
  // `jest.retryTimes` re-runs the test with its hooks; Jest reports one
  // result after the last attempt, so the reporter records the earlier
  // attempts as the failures they were.
  writeFileSync(
    resolve(project, 'tests/flaky.test.js'),
    [
      "const { permission } = require('../src/permission');",
      '',
      'jest.retryTimes(1);',
      '',
      'let attempts = 0;',
      "test('flaky', () => {",
      '  attempts += 1;',
      "  if (attempts === 1) throw new Error('first attempt fails');",
      "  expect(permission(true, false)).toBe('allowed');",
      '});',
      '',
    ].join('\n'),
  );
  const install = spawnSync(windows ? 'npm.cmd' : 'npm', ['install', '--no-audit', '--no-fund', '--silent', 'jest@29'], {
    cwd: project,
    encoding: 'utf8',
    shell: windows,
  });
  assert.equal(install.status, 0, `installing jest: ${install.stdout}\n${install.stderr}`);

  const run = rust('__run-js-direct', {
    root: project,
    command: ['npm', 'test'],
    runId: 'rust-direct-jest',
    startedAt: '2026-09-08T00:00:02.000Z',
  });
  assert.equal(run.exitCode, 1, 'the failing test fails the command');
  assert.equal(readFileSync(resolve(project, 'src/permission.js'), 'utf8'), application);

  const all = query(run.runId, 'all', 'summary');
  // `valid` is the test command exiting 0; the failing test rules that out.
  assert.equal(all.data.valid, false);
  assert.deepEqual(all.data.diagnostics, [], 'every test with assertion phases also carries evidence');
  assert.equal(
    all.data.tests,
    8,
    `eight tests, two of them parameterized and one flaky: ${JSON.stringify({ outcomes: all.data.testOutcomes, diagnostics: all.data.diagnostics, measurement: all.data.measurement, stale: all.data.staleReasons })}`,
  );
  assert.equal(all.data.testOutcomes.passed, 6, JSON.stringify(all.data.testOutcomes));
  assert.equal(all.data.testOutcomes.failed, 1, JSON.stringify(all.data.testOutcomes));
  assert.equal(all.data.testOutcomes.flaky, 1, `a retried test that then passed is flaky: ${JSON.stringify(all.data.testOutcomes)}`);
  assert.deepEqual(all.data.coverageByRunner.map((entry) => entry.runner), ['jest']);
  assert.equal(all.data.measurement.complete, true, JSON.stringify(all.data.measurement));

  // The flaky test's passing attempt counts among the passed, its failing one
  // among the failed: filters recalculate from attempts.
  const passed = query(run.runId, 'passed', 'summary');
  assert.equal(passed.data.tests, 7, JSON.stringify(passed.data.testOutcomes));
  assert.ok(passed.data.confidence.lines.asserted >= 3, `global expect links the evidence before it: ${JSON.stringify(passed.data.confidence)}`);

  const allowed = query(run.runId, 'all', 'line', { file: 'src/permission.js', line: 3, offset: 0, limit: 20 });
  const owners = JSON.stringify(allowed);
  assert.match(owners, /admin/, 'exact test identity reaches the line');
  assert.match(owners, /param true/, 'parameterized tests keep their names');
  assert.doesNotMatch(owners, /wrong/, 'the failing test never reached this line');

  const failed = query(run.runId, 'failed', 'summary');
  assert.equal(failed.data.tests, 2, JSON.stringify(failed.data.testOutcomes));

  console.log('[rust-direct-jest] a Jest suite has exact per-test identity, reporter outcomes, its own setup file and assertion phases for the global expect');
} finally {
  rmSync(temporary, { recursive: true, force: true, maxRetries: 10, retryDelay: 20 });
}
