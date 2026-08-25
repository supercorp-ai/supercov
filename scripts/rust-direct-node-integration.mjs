import assert from 'node:assert/strict';
import {
  cpSync,
  existsSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from 'node:fs';
import { tmpdir } from 'node:os';
import { resolve } from 'node:path';
import { spawnSync } from 'node:child_process';

const repository = resolve(import.meta.dirname, '..');
const binary = resolve(repository, 'target/debug/supercov');
const fixture = resolve(repository, 'tests/fixtures/generic-node');
const temporary = mkdtempSync(resolve(tmpdir(), 'supercov-rust-direct-node-'));
const project = resolve(temporary, 'project');
const commonjsProject = resolve(temporary, 'commonjs-project');

function rust(command, request) {
  const result = spawnSync(binary, [command], {
    cwd: repository,
    encoding: 'utf8',
    input: JSON.stringify(request),
  });
  assert.equal(result.status, 0, result.stderr || result.stdout);
  return JSON.parse(result.stdout.trim().split('\n').at(-1));
}

try {
  mkdirSync(project);
  cpSync(resolve(fixture, 'package.json'), resolve(project, 'package.json'));
  cpSync(resolve(fixture, 'src'), resolve(project, 'src'), { recursive: true });
  cpSync(resolve(fixture, 'tests'), resolve(project, 'tests'), { recursive: true });
  const original = readFileSync(resolve(project, 'src/permission.mjs'), 'utf8');
  const result = rust('__run-js-direct', {
    root: project,
    runtimeRoot: resolve(repository, 'dist'),
    command: [process.execPath, '--test', '--test-concurrency=2'],
    runId: 'rust-direct-node',
    startedAt: '2026-08-25T00:00:00.000Z',
  });
  assert.equal(result.exitCode, 0);
  assert.equal(readFileSync(resolve(project, 'src/permission.mjs'), 'utf8'), original);
  assert.equal(existsSync(resolve(result.workspace, '.supercov/evidence')), false);

  const summary = rust('__query-stored-run', {
    root: project,
    query: {
      runId: result.runId,
      filter: 'passed',
      command: 'summary',
    },
  });
  assert.equal(summary.ok, true);
  assert.equal(summary.data.valid, true);
  assert.equal(summary.data.complete, true);
  assert.equal(summary.data.tests, 4);
  assert.equal(summary.data.coverage.conditionCoveragePct, 100);
  assert.equal(summary.data.confidence.lines.asserted, 3);
  assert.equal(summary.data.confidence.assertionCoveredMcdcConditions, 2);
  assert.equal(summary.data.attribution.serverExplicit, 16);
  assert.equal(summary.data.attribution.serverFallback, 0);

  mkdirSync(resolve(commonjsProject, 'src'), { recursive: true });
  mkdirSync(resolve(commonjsProject, 'tests'), { recursive: true });
  writeFileSync(
    resolve(commonjsProject, 'package.json'),
    '{"name":"commonjs-node-fixture","private":true}\n',
  );
  const commonjsSource = [
    'exports.permission = function permission(admin, owner) {',
    '  if (admin || owner) return "allowed";',
    '  return "denied";',
    '};',
    '',
  ].join('\n');
  writeFileSync(resolve(commonjsProject, 'src/permission.cjs'), commonjsSource);
  writeFileSync(
    resolve(commonjsProject, 'tests/permission.test.cjs'),
    [
      "const test = require('node:test');",
      "const assert = require('node:assert/strict');",
      "const { permission } = require('../src/permission.cjs');",
      "test('admin', () => assert.equal(permission(true, false), 'allowed'));",
      "test('owner', () => assert.equal(permission(false, true), 'allowed'));",
      "test('both', () => assert.equal(permission(true, true), 'allowed'));",
      "test('neither', () => assert.equal(permission(false, false), 'denied'));",
      '',
    ].join('\n'),
  );
  const commonjs = rust('__run-js-direct', {
    root: commonjsProject,
    runtimeRoot: resolve(repository, 'dist'),
    command: [process.execPath, '--test', '--test-concurrency=2'],
    runId: 'rust-direct-commonjs-node',
    startedAt: '2026-08-25T00:00:01.000Z',
  });
  assert.equal(commonjs.exitCode, 0);
  assert.equal(readFileSync(resolve(commonjsProject, 'src/permission.cjs'), 'utf8'), commonjsSource);
  const commonjsSummary = rust('__query-stored-run', {
    root: commonjsProject,
    query: {
      runId: commonjs.runId,
      filter: 'passed',
      command: 'summary',
    },
  });
  assert.equal(commonjsSummary.data.complete, false);
  assert.equal(commonjsSummary.data.structurallyComplete, false);
  assert.equal(commonjsSummary.data.tests, 4);
  assert.equal(commonjsSummary.data.coverage.conditionCoveragePct, 100);
  assert.equal(commonjsSummary.data.coverage.statements.percentage, 75);
  assert.equal(commonjsSummary.data.confidence.lines.asserted, 3);
  assert.equal(commonjsSummary.data.confidence.assertionCoveredMcdcConditions, 2);
  assert.equal(commonjsSummary.data.attribution.serverFallback, 0);
  const commonjsAll = rust('__query-stored-run', {
    root: commonjsProject,
    query: {
      runId: commonjs.runId,
      filter: 'all',
      command: 'summary',
    },
  });
  assert.equal(commonjsAll.data.structurallyComplete, true);
  assert.equal(commonjsAll.data.coverage.statements.percentage, 100);
  assert.equal(commonjsAll.data.transport.backgroundServerRecords, 1);
  console.log(
    '[rust-direct-node] Rust owns complete ESM and CommonJS direct node:test runs through query',
  );
} finally {
  rmSync(temporary, { recursive: true, force: true });
}
