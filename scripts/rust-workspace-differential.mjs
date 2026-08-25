import assert from 'node:assert/strict';
import {
  lstatSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  readlinkSync,
  readdirSync,
  renameSync,
  rmSync,
  symlinkSync,
  writeFileSync,
} from 'node:fs';
import { tmpdir } from 'node:os';
import { basename, dirname, join, relative, resolve } from 'node:path';
import { spawnSync } from 'node:child_process';

import {
  cachedWorkspacePath,
  prepareCachedWorkspace,
  prepareIsolatedWorkspace,
  pruneCachedWorkspaceSources,
  recoverCachedWorkspace,
} from '../dist/workspace.js';

const repository = resolve(import.meta.dirname, '..');
const binary = resolve(repository, 'target/debug/supercov');
const temporary = [];
process.on('exit', () => {
  for (const path of temporary) rmSync(path, { recursive: true, force: true });
});

function project(label) {
  const parent = mkdtempSync(join(tmpdir(), `supercov-workspace-${label}-`));
  temporary.push(parent);
  const root = resolve(parent, 'project');
  mkdirSync(resolve(root, 'src'), { recursive: true });
  mkdirSync(resolve(root, 'dist'));
  mkdirSync(resolve(root, '.cache/tool'), { recursive: true });
  mkdirSync(resolve(root, 'packages/example/.supercov/cache'), { recursive: true });
  mkdirSync(resolve(root, 'packages/supercov/src'), { recursive: true });
  mkdirSync(resolve(root, 'node_modules/example'), { recursive: true });
  writeFileSync(resolve(root, 'src/index.js'), 'export const value = 1;\n');
  writeFileSync(resolve(root, 'dist/index.js'), 'normal build\n');
  writeFileSync(resolve(root, '.cache/tool/generated.js'), 'cache\n');
  writeFileSync(resolve(root, 'packages/example/source.js'), 'nested source\n');
  writeFileSync(resolve(root, 'packages/example/.supercov/cache/stale'), 'stale\n');
  writeFileSync(resolve(root, 'packages/supercov/src/index.js'), 'user-owned name\n');
  writeFileSync(resolve(root, 'node_modules/example/index.js'), 'dependency\n');
  writeFileSync(resolve(root, 'package.json'), '{"name":"workspace-fixture"}\n');
  if (process.platform !== 'win32')
    symlinkSync('index.js', resolve(root, 'src/internal-link.js'));
  return root;
}

function rust(root, action, options = {}, expectedStatus = 0) {
  const child = spawnSync(binary, ['__workspace'], {
    cwd: repository,
    input: JSON.stringify({ root, action, ...options }),
    encoding: 'utf8',
  });
  assert.equal(child.status, expectedStatus, child.stderr || child.stdout);
  return expectedStatus === 0 ? JSON.parse(child.stdout) : child.stderr;
}

function normalizedTarget(target, root) {
  return target.startsWith(root) ? `<root>${target.slice(root.length)}` : target;
}

function snapshot(root, projectRoot) {
  const result = [];
  function visit(directory) {
    for (const name of readdirSync(directory).sort()) {
      const path = resolve(directory, name);
      const local = relative(root, path);
      const stat = lstatSync(path);
      if (stat.isDirectory()) {
        result.push({ path: `${local}/`, type: 'directory' });
        visit(path);
      } else if (stat.isSymbolicLink()) {
        result.push({
          path: local,
          type: 'symlink',
          target: normalizedTarget(readlinkSync(path), projectRoot),
        });
      } else if (stat.isFile()) {
        result.push({
          path: local,
          type: 'file',
          mode: stat.mode & 0o777,
          contents: readFileSync(path, 'utf8'),
        });
      } else {
        result.push({ path: local, type: 'other' });
      }
    }
  }
  visit(root);
  return result;
}

function compareTrees(referenceRoot, candidateRoot, referenceProject, candidateProject, label) {
  assert.deepEqual(
    snapshot(candidateRoot, candidateProject),
    snapshot(referenceRoot, referenceProject),
    label,
  );
}

{
  const reference = project('isolated-reference');
  const candidate = project('isolated-candidate');
  const runId = '2026-08-25T00-00-00-000Z';
  const expected = prepareIsolatedWorkspace(reference, runId);
  const actual = rust(candidate, 'prepare-isolated', { runId }).workspace;
  compareTrees(expected, actual, reference, candidate, 'isolated workspace');
  assert.equal(readFileSync(resolve(candidate, 'dist/index.js'), 'utf8'), 'normal build\n');
}

{
  const reference = project('cache-reference');
  const candidate = project('cache-candidate');
  const expected = prepareCachedWorkspace(reference);
  const actual = rust(candidate, 'prepare-cached').workspace;
  compareTrees(expected, actual, reference, candidate, 'initial cache publication');

  for (const [root, workspace] of [[reference, expected], [candidate, actual]]) {
    writeFileSync(resolve(root, 'src/index.js'), 'export const value = 2;\n');
    mkdirSync(resolve(workspace, 'build'));
    mkdirSync(resolve(workspace, '.supercov'));
    writeFileSync(resolve(workspace, 'build/index.js'), 'instrumented\n');
    writeFileSync(resolve(workspace, '.supercov/manifest.json'), 'manifest\n');
    writeFileSync(resolve(workspace, 'unselected.txt'), 'stale\n');
  }
  prepareCachedWorkspace(reference, {
    reusePaths: ['build', '.supercov/manifest.json'],
  });
  rust(candidate, 'prepare-cached', {
    reusePaths: ['build', '.supercov/manifest.json'],
  });
  compareTrees(expected, actual, reference, candidate, 'explicit artifact reuse');

  for (const workspace of [expected, actual])
    writeFileSync(
      resolve(workspace, '.supercov/build-cache.json'),
      JSON.stringify({ artifactPaths: ['build', '.supercov/manifest.json'] }),
    );
  const expectedRemoved = pruneCachedWorkspaceSources(reference);
  const actualRemoved = rust(candidate, 'prune-cache').removed;
  assert.deepEqual(actualRemoved, expectedRemoved, 'pruned source inventory');
  compareTrees(expected, actual, reference, candidate, 'pruned reusable cache');

  const expectedPrevious = resolve(
    dirname(expected),
    `.${basename(reference)}.previous-interrupted`,
  );
  const actualPrevious = resolve(
    dirname(actual),
    `.${basename(candidate)}.previous-interrupted`,
  );
  renameSync(expected, expectedPrevious);
  renameSync(actual, actualPrevious);
  for (const [workspace, projectRoot] of [[expected, reference], [actual, candidate]]) {
    const staging = resolve(
      dirname(workspace),
      `.${basename(projectRoot)}.staging-interrupted`,
    );
    mkdirSync(staging);
    writeFileSync(resolve(staging, 'partial'), 'partial\n');
  }
  const expectedRecovery = recoverCachedWorkspace(reference);
  const actualRecovery = rust(candidate, 'recover-cache');
  assert.deepEqual(actualRecovery, expectedRecovery, 'cache transaction recovery');
  compareTrees(expected, actual, reference, candidate, 'recovered cache generation');
}

if (process.platform !== 'win32') {
  const reference = project('escape-reference');
  const candidate = project('escape-candidate');
  const external = mkdtempSync(join(tmpdir(), 'supercov-workspace-external-'));
  temporary.push(external);
  writeFileSync(resolve(external, 'outside.js'), 'outside\n');
  for (const root of [reference, candidate])
    symlinkSync(resolve(external, 'outside.js'), resolve(root, 'src/external.js'));
  assert.throws(
    () => prepareCachedWorkspace(reference),
    /symlink outside the isolated project/,
  );
  assert.match(
    rust(candidate, 'prepare-cached', {}, 2),
    /symlink outside the isolated project/,
  );
}

console.log(
  '[rust-workspace-differential] isolated copy, exclusions, dependency links, symlink safety, stable publication, reuse, pruning, and crash recovery parity',
);
