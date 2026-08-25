import assert from 'node:assert/strict';
import {
  existsSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { spawnSync } from 'node:child_process';

import {
  cleanCoverageStorage,
  pruneCoverageStorage,
  recoverAbandonedRuns,
} from '../dist/workspace.js';

const root = resolve(import.meta.dirname, '..');
const binary = resolve(root, 'target/debug/supercov');
const temporary = [];
process.on('exit', () => {
  for (const path of temporary) rmSync(path, { recursive: true, force: true });
});

function project() {
  const path = mkdtempSync(join(tmpdir(), 'supercov-lifecycle-differential-'));
  temporary.push(path);
  mkdirSync(resolve(path, 'src'));
  writeFileSync(resolve(path, 'src/index.js'), 'user source\n');
  return path;
}

function state(projectRoot, id, status, pid = process.pid) {
  const directory = resolve(projectRoot, '.supercov/work', id);
  mkdirSync(directory, { recursive: true });
  writeFileSync(
    resolve(directory, 'state.json'),
    `${JSON.stringify({
      id,
      pid,
      root: projectRoot,
      workspace: resolve(directory, projectRoot.split('/').at(-1)),
      startedAt: '2026-01-01T00:00:00.000Z',
      updatedAt: '2026-01-01T00:00:00.000Z',
      status,
    }, null, 2)}\n`,
  );
}

function rust(projectRoot, action, options = {}) {
  const child = spawnSync(binary, ['__lifecycle'], {
    cwd: root,
    input: JSON.stringify({
      root: projectRoot,
      action,
      updatedAt: '2026-01-02T00:00:00.000Z',
      ...options,
    }),
    encoding: 'utf8',
  });
  assert.equal(child.status, 0, child.stderr || child.stdout);
  return JSON.parse(child.stdout);
}

function retentionFixture() {
  const projectRoot = project();
  const ids = [
    '2026-01-01T00-00-00-000Z',
    '2026-01-02T00-00-00-000Z',
    '2026-01-03T00-00-00-000Z',
  ];
  for (const id of ids) {
    mkdirSync(resolve(projectRoot, '.supercov/runs', id), { recursive: true });
    mkdirSync(resolve(projectRoot, '.supercov/evidence', id), { recursive: true });
    state(projectRoot, id, 'complete');
  }
  state(projectRoot, '2025-12-31T00-00-00-000Z', 'testing');
  return { projectRoot, ids };
}

for (const dryRun of [true, false]) {
  const reference = retentionFixture();
  const candidate = retentionFixture();
  const expected = pruneCoverageStorage(reference.projectRoot, { keep: 1, dryRun });
  const actual = rust(candidate.projectRoot, 'prune', { keep: 1, dryRun });
  assert.deepEqual(actual, expected, `prune dryRun=${dryRun}`);
  for (const id of reference.ids) {
    assert.equal(
      existsSync(resolve(candidate.projectRoot, '.supercov/runs', id)),
      existsSync(resolve(reference.projectRoot, '.supercov/runs', id)),
      `retained run ${id}`,
    );
  }
  assert.ok(existsSync(resolve(candidate.projectRoot, 'src/index.js')));
}

{
  const reference = project();
  const candidate = project();
  for (const projectRoot of [reference, candidate]) {
    mkdirSync(resolve(projectRoot, 'supercov/workspace/project'), { recursive: true });
    writeFileSync(resolve(projectRoot, 'supercov/.supercov-workspace-store'), 'owned\n');
    mkdirSync(resolve(projectRoot, '.supercov/cache/legacy'), { recursive: true });
  }
  const expected = cleanCoverageStorage(reference, { keep: 0, dryRun: false });
  const actual = rust(candidate, 'clean', { keep: 0, dryRun: false });
  assert.deepEqual(actual, expected);
  assert.equal(existsSync(resolve(candidate, 'supercov')), existsSync(resolve(reference, 'supercov')));
  assert.equal(
    existsSync(resolve(candidate, '.supercov/cache')),
    existsSync(resolve(reference, '.supercov/cache')),
  );
}

{
  const id = '2026-01-04T00-00-00-000Z';
  const reference = project();
  const candidate = project();
  for (const projectRoot of [reference, candidate]) {
    state(projectRoot, id, 'testing', 2_147_483_647);
    mkdirSync(resolve(projectRoot, '.supercov/evidence', id), { recursive: true });
    writeFileSync(resolve(projectRoot, '.supercov/evidence', id, 'hit.json'), '{}');
    mkdirSync(resolve(projectRoot, '.supercov/work', id, projectRoot.split('/').at(-1)), {
      recursive: true,
    });
  }
  assert.deepEqual(rust(candidate, 'recover'), recoverAbandonedRuns(reference));
  const referenceState = JSON.parse(readFileSync(resolve(reference, '.supercov/work', id, 'state.json')));
  const candidateState = JSON.parse(readFileSync(resolve(candidate, '.supercov/work', id, 'state.json')));
  assert.equal(candidateState.status, referenceState.status);
  assert.equal(candidateState.error, referenceState.error);
  assert.ok(existsSync(resolve(candidate, 'src/index.js')));
}

console.log('[rust-lifecycle-differential] prune, clean, dry-run, active-run preservation, cache policy, and dead-run recovery parity');

