import assert from 'node:assert/strict';
import { mkdirSync, mkdtempSync, readFileSync, rmSync, symlinkSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { resolve } from 'node:path';
import { spawnSync } from 'node:child_process';

const repository = resolve(import.meta.dirname, '..');
const binary = process.env.SUPERCOV_BINARY ?? resolve(repository, 'target/debug/supercov');
const temporary = mkdtempSync(resolve(tmpdir(), 'supercov-nested-vitest-'));
const project = resolve(temporary, 'nested frontend (Vitest)');
const write = (file, text) => writeFileSync(resolve(project, file), text);
const output = result => `${result.stdout ?? ''}${result.stderr ?? ''}`;
const command = ['node', 'aux/front/node_modules/vitest/vitest.mjs', 'run', '--config', 'aux/front/vitest.config.ts'];

function rust(action, request) {
  const result = spawnSync(binary, [action], { cwd: repository, encoding: 'utf8', input: JSON.stringify(request), env: { ...process.env, SUPERCOV_SOURCE_ROOTS: 'src', NO_COLOR: '1', FORCE_COLOR: '0' }, timeout: 120_000 });
  assert.equal(result.status, 0, output(result));
  const value = JSON.parse(result.stdout.trim().split('\n').at(-1));
  value.output = output(result);
  return value;
}

try {
  mkdirSync(resolve(project, 'src'), { recursive: true });
  mkdirSync(resolve(project, 'aux/front/node_modules'), { recursive: true });
  // Neither root nor a fixture ancestor has these packages. Only the actual
  // runner/config package owns the dependency closure, as in a frontend package.
  for (const dependency of ['vitest']) {
    symlinkSync(resolve(repository, 'node_modules', dependency), resolve(project, 'aux/front/node_modules', dependency));
  }
  write('package.json', '{"private":true,"type":"module"}\n');
  write('aux/front/package.json', '{"private":true,"type":"module"}\n');
  write('src/pick.ts', "export function pick(flag: boolean) { if (flag) return 'yes'; return 'no'; }\n");
  write('aux/front/setup.ts', 'globalThis.__nestedConfigLoaded = true;\n');
  write('aux/front/vitest.config.ts', "import { defineConfig } from 'vitest/config';\nexport default defineConfig({ test: { include: ['aux/front/*.test.ts'], setupFiles: ['aux/front/setup.ts'] } });\n");
  write('aux/front/product.test.ts', "import { test, expect } from 'vitest';\nimport { pick } from '../../src/pick';\ntest('nested yes', () => { expect(globalThis.__nestedConfigLoaded).toBe(true); expect(pick(true)).toBe('yes'); });\ntest('nested no', () => { expect(globalThis.__nestedConfigLoaded).toBe(true); expect(pick(false)).toBe('no'); });\n");
  const plain = spawnSync(command[0], command.slice(1), { cwd: project, encoding: 'utf8', env: { ...process.env, NO_COLOR: '1', FORCE_COLOR: '0' }, timeout: 60_000 });
  assert.equal(plain.status, 0, output(plain));
  const measured = rust('__run-js-direct', { root: project, command, runId: 'nested-vitest-dependency-closure', startedAt: '2026-11-07T00:00:00.000Z' });
  assert.equal(measured.exitCode, 0, measured.output);
  const summary = rust('__query-stored-run', { root: project, query: { runId: measured.runId, filter: 'passed', command: 'summary' } });
  assert.equal(summary.data.valid, true);
  assert.equal(summary.data.tests, 2);
  assert.equal(summary.data.testOutcomes.passed, 2);
  assert.equal(summary.data.testAttribution.exact, 2);
  const executions = JSON.parse(readFileSync(resolve(project, '.supercov/runs', measured.runId, 'test-executions.json'), 'utf8')).tests;
  const cases = executions.filter(entry => /nested (yes|no)$/.test(entry.test.name));
  assert.equal(cases.length, 2);
  assert.ok(cases.every(entry => entry.test.file === 'aux/front/product.test.ts'));
  assert.equal(new Set(cases.map(entry => entry.test.file + ':' + entry.test.name)).size, 2);
  assert.equal(summary.data.coverageByTests.lines.percentage, 100);
  assert.equal(summary.data.coverageByTests.branches.percentage, 100);
  console.log('[rust-nested-vitest] actual nested runner/config closure and original setup preserved; 2 tests attributed');
} finally {
  if (process.env.SUPERCOV_KEEP_FIXTURE === '1') console.error(`[rust-nested-vitest] retained ${project}`);
  else rmSync(temporary, { recursive: true, force: true });
}
