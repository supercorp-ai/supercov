import assert from 'node:assert/strict';
import {
  mkdirSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  symlinkSync,
  writeFileSync,
} from 'node:fs';
import { tmpdir } from 'node:os';
import { resolve } from 'node:path';
import { spawnSync } from 'node:child_process';

const repository = resolve(import.meta.dirname, '..');
const binary = (process.env.SUPERCOV_BINARY ?? resolve(repository, `target/debug/supercov${process.platform === 'win32' ? '.exe' : ''}`));
const temporary = mkdtempSync(resolve(tmpdir(), 'supercov-rust-playwright-'));
const project = resolve(temporary, 'project');

function rust(command, request, environment = {}) {
  const result = spawnSync(binary, [command], {
    cwd: repository,
    encoding: 'utf8',
    input: JSON.stringify(request),
    env: { ...process.env, ...environment },
  });
  assert.equal(result.status, 0, result.stderr || result.stdout);
  const lines = result.stdout.trim().split('\n');
  return {
    ...JSON.parse(lines.at(-1)),
    diagnosticOutput: [...lines.slice(0, -1), result.stderr].filter(Boolean).join('\n'),
  };
}

try {
  mkdirSync(resolve(project, 'src'), { recursive: true });
  mkdirSync(resolve(project, 'tests'), { recursive: true });
  mkdirSync(resolve(project, 'node_modules/.bin'), { recursive: true });
  mkdirSync(resolve(project, 'node_modules/@acme/browser-fixtures'), { recursive: true });
  mkdirSync(resolve(project, 'node_modules/@playwright'), { recursive: true });
  symlinkSync(
    resolve(repository, 'node_modules/@playwright/test'),
    resolve(project, 'node_modules/@playwright/test'),
  );
  symlinkSync(
    resolve(repository, 'node_modules/.bin/playwright'),
    resolve(project, 'node_modules/.bin/playwright'),
  );
  writeFileSync(
    resolve(project, 'node_modules/@acme/browser-fixtures/package.json'),
    JSON.stringify({
      name: '@acme/browser-fixtures',
      private: true,
      type: 'module',
      exports: './index.js',
    }) + '\n',
  );
  writeFileSync(
    resolve(project, 'node_modules/@acme/browser-fixtures/index.js'),
    "export { test as browserTest, expect } from '@playwright/test';\n",
  );
  writeFileSync(
    resolve(project, 'package.json'),
    JSON.stringify({
      name: 'supercov-rust-playwright-fixture',
      private: true,
      type: 'module',
      scripts: { test: 'playwright test' },
    }) + '\n',
  );
  writeFileSync(
    resolve(project, 'playwright.config.mjs'),
    [
      "export default { testDir: './tests', fullyParallel: true, workers: 2, reporter: 'line' };",
      '',
    ].join('\n'),
  );
  const application = [
    'export function permission(admin, owner) {',
    '  if (admin || owner) return "allowed";',
    '  return "denied";',
    '}',
    '',
  ].join('\n');
  writeFileSync(resolve(project, 'src/permission.js'), application);
  writeFileSync(
    resolve(project, 'tests/permission.spec.js'),
    [
      "import { browserTest as test, expect } from '@acme/browser-fixtures';",
      "import { permission } from '../src/permission.js';",
      "test('admin', () => expect(permission(true, false)).toBe('allowed'));",
      "test('owner', () => expect(permission(false, true)).toBe('allowed'));",
      "test('both', () => expect(permission(true, true)).toBe('allowed'));",
      "test('neither', () => expect(permission(false, false)).toBe('denied'));",
      '',
    ].join('\n'),
  );

  const run = rust('__run-js-direct', {
    root: project,
    command: ['npm', 'test'],
    runId: 'rust-direct-playwright',
    startedAt: '2026-08-25T00:00:03.000Z',
  });
  assert.equal(run.exitCode, 0, run.diagnosticOutput);
  assert.equal(run.assertionCalls, 4);
  assert.equal(readFileSync(resolve(project, 'src/permission.js'), 'utf8'), application);
  const summary = rust('__query-stored-run', {
    root: project,
    query: {
      runId: run.runId,
      filter: 'passed',
      command: 'summary',
    },
  });
  assert.equal(summary.data.valid, true);
  assert.equal(summary.data.complete, true);
  assert.equal(summary.data.tests, 4);
  assert.equal(summary.data.coverage.conditionCoveragePct, 100);
  assert.equal(summary.data.coverage.lines.percentage, 100);
  assert.equal(summary.data.coverage.branches.percentage, 100);
  assert.deepEqual(
    summary.data.coverageByRunner.map(entry => entry.runner),
    ['playwright'],
  );
  const test = rust('__query-stored-run', {
    root: project,
    query: {
      runId: run.runId,
      filter: 'passed',
      command: 'test',
      selector: 'admin',
    },
  });
  assert.equal(test.data.tests.length, 1);
  assert.ok(
    test.data.tests[0].phases.some(
      phase => phase.operation === 'expect.toBe' && phase.lines > 0,
    ),
  );
  // Playwright's default export, which the wrapper passes on as well: a spec
  // importing it failed with `(0 , _test.default) is not a function`. Once
  // as an ES module, and once as a spec Playwright compiles to CommonJS
  // (`.cts`), which reads the module's default when it says it is an ES
  // module, on its own: CommonJS and ES module specs in one worker are not
  // what this checks.
  const defaultRun = (file, lines, runId, tests) => {
    writeFileSync(resolve(project, 'tests', file), [...lines, ''].join('\n'));
    const run = rust('__run-js-direct', {
      root: project,
      command: ['npm', 'test'],
      runId,
      startedAt: '2026-08-25T00:00:04.000Z',
    });
    assert.equal(run.exitCode, 0, run.diagnosticOutput);
    const summary = rust('__query-stored-run', {
      root: project,
      query: { runId: run.runId, filter: 'passed', command: 'summary' },
    });
    assert.equal(summary.data.tests, tests);
  };
  defaultRun(
    'default-import.spec.js',
    [
      "import test, { expect } from '@playwright/test';",
      "import { permission } from '../src/permission.js';",
      "test('default import', () => expect(permission(true, false)).toBe('allowed'));",
    ],
    'rust-direct-playwright-default',
    5,
  );
  for (const file of ['permission.spec.js', 'default-import.spec.js'])
    rmSync(resolve(project, 'tests', file));
  defaultRun(
    'default-require.spec.cts',
    [
      "import test, { expect } from '@playwright/test';",
      "test.describe('commonjs', () => {",
      "  test('default require', () => expect(typeof test.extend).toBe('function'));",
      '});',
    ],
    'rust-direct-playwright-default-require',
    1,
  );
  // An ES module spec and a CommonJS spec of one project, importing
  // @playwright/test itself, in one worker: each must get the same wrapper,
  // or the worker's fixtures differ from those the runner listed the tests
  // with (`Playwright detected inconsistent test.use() options`).
  const mixed = resolve(temporary, 'mixed');
  mkdirSync(resolve(mixed, 'tests'), { recursive: true });
  mkdirSync(resolve(mixed, 'src'), { recursive: true });
  writeFileSync(resolve(mixed, 'src/value.js'), 'module.exports = (n) => (n > 1 ? n : 1);\n');
  mkdirSync(resolve(mixed, 'node_modules/.bin'), { recursive: true });
  mkdirSync(resolve(mixed, 'node_modules/@playwright'), { recursive: true });
  symlinkSync(
    resolve(repository, 'node_modules/@playwright/test'),
    resolve(mixed, 'node_modules/@playwright/test'),
  );
  symlinkSync(
    resolve(repository, 'node_modules/.bin/playwright'),
    resolve(mixed, 'node_modules/.bin/playwright'),
  );
  writeFileSync(
    resolve(mixed, 'package.json'),
    JSON.stringify({ name: 'supercov-mixed-specs', private: true, scripts: { test: 'playwright test' } }) + '\n',
  );
  writeFileSync(
    resolve(mixed, 'playwright.config.mjs'),
    "export default { testDir: './tests', workers: 1, reporter: 'line' };\n",
  );
  writeFileSync(
    resolve(mixed, 'tests/a-module.spec.mjs'),
    "import { test, expect } from '@playwright/test';\nimport value from '../src/value.js';\ntest('module', () => expect(value(2)).toBe(2));\n",
  );
  writeFileSync(
    resolve(mixed, 'tests/b-commonjs.spec.cjs'),
    "const { test, expect } = require('@playwright/test');\nconst value = require('../src/value.js');\ntest('commonjs', () => expect(value(0)).toBe(1));\n",
  );
  // And the facade project's ES module spec, through @acme/browser-fixtures,
  // beside its CommonJS spec importing @playwright/test itself, on one worker.
  writeFileSync(
    resolve(project, 'playwright.config.mjs'),
    "export default { testDir: './tests', workers: 1, reporter: 'line' };\n",
  );
  writeFileSync(
    resolve(project, 'tests/permission.spec.js'),
    [
      "import { browserTest as test, expect } from '@acme/browser-fixtures';",
      "import { permission } from '../src/permission.js';",
      "test('admin', () => expect(permission(true, false)).toBe('allowed'));",
      '',
    ].join('\n'),
  );
  // Diagnostic, temporary: the facade case once more with the hooks off the
  // loading thread, as before 4.25.
  const outcomes = [
    [mixed, 'rust-direct-playwright-mixed', 2],
    [project, 'rust-direct-playwright-mixed-facade', 2],
    [project, 'rust-direct-playwright-mixed-facade-off-thread', 2, { SUPERCOV_DIAGNOSE_OFF_THREAD_HOOKS: '1' }],
  ].map(([root, runId, tests, environment]) => {
    const run = rust(
      '__run-js-direct',
      { root, command: ['npm', 'test'], runId, startedAt: '2026-08-25T00:00:05.000Z' },
      environment,
    );
    const passed =
      run.exitCode === 0 &&
      rust('__query-stored-run', { root, query: { runId: run.runId, filter: 'passed', command: 'summary' } })
        .data.tests === tests;
    console.log(`[rust-direct-playwright] ${runId}: ${passed ? 'passed' : `failed\n${run.diagnosticOutput}`}`);
    return passed;
  });
  assert.deepEqual(outcomes, [true, true, true]);
  console.log(
    '[rust-direct-playwright] Rust-owned npm test run preserves multi-worker Playwright attribution',
  );
} finally {
  if (process.env.SUPERCOV_KEEP_FIXTURE === '1')
    console.error(`[rust-direct-playwright] retained ${project}`);
  else rmSync(temporary, { recursive: true, force: true });
}
