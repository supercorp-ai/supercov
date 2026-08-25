import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { resolve } from 'node:path';

const root = resolve(import.meta.dirname, '..');
const fixture = resolve(root, 'tests/fixtures/generic-webpack');
const binary = resolve(root, 'target/debug/supercov');
const reference = resolve(root, 'dist/cli.js');

function invoke(engine, args) {
  const child = engine === 'rust'
    ? spawnSync(binary, args, { cwd: fixture, encoding: 'utf8' })
    : spawnSync(process.execPath, [reference, ...args], { cwd: fixture, encoding: 'utf8' });
  assert.ok(child.stdout.endsWith('\n'), `${engine} must emit one newline-terminated JSON object`);
  assert.equal(child.stderr, '', `${engine} JSON diagnostics must not leak to stderr: ${child.stderr}`);
  return { status: child.status, stdout: child.stdout, value: JSON.parse(child.stdout) };
}

function exact(args) {
  const expected = invoke('typescript', args);
  const actual = invoke('rust', args);
  assert.equal(actual.status, expected.status, args.join(' '));
  assert.equal(actual.stdout, expected.stdout, args.join(' '));
}

function withoutEngineIdentity(value) {
  const normalized = structuredClone(value);
  if (normalized.command === 'runs') {
    for (const run of normalized.data.runs) {
      delete run.stale;
      delete run.reasons;
    }
  }
  if (normalized.command === 'coverage.summary') {
    delete normalized.data.stale;
    delete normalized.data.staleReasons;
    delete normalized.data.complete;
  }
  return normalized;
}

function exactExceptEngineIdentity(args) {
  const expected = invoke('typescript', args);
  const actual = invoke('rust', args);
  assert.equal(actual.status, expected.status, args.join(' '));
  assert.deepEqual(
    withoutEngineIdentity(actual.value),
    withoutEngineIdentity(expected.value),
    args.join(' '),
  );
}

const older = '2026-08-25T00-23-38-498Z';
const newer = '2026-08-25T00-23-39-434Z';

// Materialize the immutable Rust indexes for both fixture runs. A run-listing
// reports whether the current engine has an authenticated index, so this is a
// required precondition for comparing a store created by the former engine.
invoke('rust', ['diff', older, newer, '--json']);

exactExceptEngineIdentity(['runs', '--json']);
exactExceptEngineIdentity(['runs', 'latest', 'coverage', '--json']);

for (const args of [
  ['runs', 'latest', 'coverage', 'files', '--json'],
  ['runs', 'latest', 'coverage', 'files', '--offset', '1e0', '--limit', '2.0', '--json'],
  ['runs', 'latest', 'coverage', 'gaps', '--json'],
  ['runs', 'latest', 'coverage', 'kinds', '--json'],
  ['runs', 'latest', 'coverage', 'runners', '--json'],
  ['runs', 'latest', 'coverage', 'scope', '--json'],
  ['runs', 'latest', 'coverage', 'file', 'src/permission.js', '--json'],
  ['runs', 'latest', 'coverage', 'file', 'src/permission.js', '--group', 'decision', '--json'],
  ['runs', 'latest', 'coverage', 'decision', 'c6612c395bd5925b', '--json'],
  ['runs', 'latest', 'coverage', 'decision', 'src/permission.js:2', '--json'],
  ['runs', 'latest', 'coverage', 'covers', 'src/permission.js:2:7', '--json'],
  ['runs', 'latest', 'coverage', 'test', 'admin', '--json'],
  ['runs', 'latest', 'coverage', 'minimize', '--json'],
  ['runs', 'latest', 'coverage', 'files', '--filter', 'passed', '--json'],
  ['diff', older, newer, '--json'],
]) exact(args);

for (const args of [
  ['runs', 'run-1', 'gaps', '--json'],
  ['runs', 'latest', 'coverage', 'nope', '--json'],
  ['runs', 'missing', 'coverage', '--json'],
  ['runs', 'latest', 'coverage', 'file', '--json'],
  ['runs', 'latest', 'coverage', 'file', 'missing.ts', '--json'],
  ['runs', 'latest', 'coverage', 'decision', 'nope', '--json'],
  ['runs', 'latest', 'coverage', 'test', 'nope', '--json'],
  ['runs', 'latest', 'coverage', 'gaps', '--kind', 'e2e', '--json'],
  ['runs', 'latest', 'coverage', 'gaps', '--limit', '0', '--json'],
  ['runs', 'latest', 'coverage', 'covers', 'nope', '--json'],
  ['diff', older, '--json'],
]) exact(args);

console.log('[rust-query-differential] public hierarchy, JSON, pagination, resources, filters, selectors, minimization, diff, and structured errors');
