import assert from 'node:assert/strict';
import { existsSync, readdirSync } from 'node:fs';
import { resolve } from 'node:path';
import { spawnSync } from 'node:child_process';
import { isDeepStrictEqual } from 'node:util';

import { analyzeCoverageArchive } from '../dist/runAnalysis.js';

const root = resolve(import.meta.dirname, '..');
const binary = resolve(root, 'target/debug/supercov');
const generatedAt = '2026-08-25T00:00:00.000Z';
const fixtures = [
  'generic-playwright',
  'generic-node',
  'generic-esbuild',
  'generic-webpack',
  'generic-swc',
];

function newestArchive(fixture) {
  const runs = resolve(root, 'tests/fixtures', fixture, '.supercov/runs');
  const ids = readdirSync(runs, { withFileTypes: true })
    .filter((entry) => entry.isDirectory())
    .map((entry) => entry.name)
    .sort()
    .reverse();
  for (const id of ids) {
    const archivePath = resolve(runs, id, 'evidence.raw.gz');
    if (existsSync(archivePath)) return { id, archivePath };
  }
  throw new Error(`No evidence archive for ${fixture}`);
}

function firstDifference(left, right, path = '$') {
  if (Object.is(left, right)) return undefined;
  if (typeof left !== typeof right || left === null || right === null)
    return { path, left, right };
  if (Array.isArray(left) || Array.isArray(right)) {
    if (!Array.isArray(left) || !Array.isArray(right) || left.length !== right.length)
      return { path: `${path}.length`, left: left?.length, right: right?.length };
    for (let index = 0; index < left.length; index += 1) {
      const difference = firstDifference(left[index], right[index], `${path}[${index}]`);
      if (difference) return difference;
    }
    return undefined;
  }
  if (typeof left === 'object') {
    const leftKeys = Object.keys(left).sort();
    const rightKeys = Object.keys(right).sort();
    if (!isDeepStrictEqual(leftKeys, rightKeys)) return { path, leftKeys, rightKeys };
    for (const key of leftKeys) {
      const difference = firstDifference(left[key], right[key], `${path}.${key}`);
      if (difference) return difference;
    }
    return undefined;
  }
  return { path, left, right };
}

for (const fixture of fixtures) {
  const { id: runId, archivePath } = newestArchive(fixture);
  const expected = JSON.parse(
    JSON.stringify(analyzeCoverageArchive(archivePath, { runId, generatedAt })),
  );
  const child = spawnSync(binary, ['__analyze-evidence-archive'], {
    cwd: root,
    input: JSON.stringify({ archivePath, runId, generatedAt }),
    encoding: 'utf8',
    maxBuffer: 128 * 1024 * 1024,
  });
  assert.equal(child.status, 0, `${fixture}: ${child.stderr || child.stdout}`);
  const actual = JSON.parse(child.stdout);
  const difference = firstDifference(actual, expected);
  assert.equal(difference, undefined, `${fixture}: ${JSON.stringify(difference)}`);
}

console.log(
  `[rust-archive-differential] ${fixtures.length} real immutable fixture archives have exact report, transport, attribution, outcome, and filter parity`,
);
