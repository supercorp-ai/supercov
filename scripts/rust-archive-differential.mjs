import assert from 'node:assert/strict';
import { existsSync, readdirSync } from 'node:fs';
import { resolve } from 'node:path';
import { spawnSync } from 'node:child_process';
import { isDeepStrictEqual } from 'node:util';

import { analyzeCoverageArchive } from '../dist/runAnalysis.js';
import { fileGaps } from '../dist/query.js';

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

const indexRequests = [];
const indexExpected = [];
function indexedFiles(view) {
  return fileGaps(view).map((gap) => ({
    file: gap.file,
    uncoveredLines: gap.uncoveredLines,
    uncoveredStatements: gap.uncoveredStatements,
    uncoveredFunctions: gap.uncoveredFunctions,
    missingBranches: gap.missingBranches,
    missingMcdcConditions: gap.missingMcdcConditions,
    measurementLimitations: gap.measurementLimitations,
    limitationKinds: gap.limitationKinds,
    coveredByOtherTests: gap.coveredByOtherTests,
    uncoveredEverywhere: gap.uncoveredEverywhere,
    score: gap.score,
  }));
}

for (const [fixtureIndex, fixture] of fixtures.entries()) {
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
  indexRequests.push({ archivePath, runId, generatedAt });
  indexExpected.push({
    allSummary: expected.summary,
    passedSummary: expected.filters.passed.summary,
    failedSummary: expected.filters.failed.summary,
    allFiles: indexedFiles(expected),
    passedFiles: indexedFiles(expected.filters.passed),
    failedFiles: indexedFiles(expected.filters.failed),
  });

  const command = fixtureIndex % 2 === 0 ? 'files' : 'gaps';
  const filter = ['all', 'passed', 'failed'][fixtureIndex % 3];
  const metric = ['all', 'lines', 'branches', 'mcdc', 'functions'][fixtureIndex];
  const limit = 2;
  const offset = fixtureIndex % 2;
  const referenceQuery = spawnSync(
    process.execPath,
    [
      resolve(root, 'bin/supercov.js'),
      'runs',
      runId,
      'coverage',
      command,
      '--filter',
      filter,
      '--metric',
      metric,
      '--limit',
      String(limit),
      '--offset',
      String(offset),
      '--json',
    ],
    {
      cwd: resolve(root, 'tests/fixtures', fixture),
      encoding: 'utf8',
      maxBuffer: 128 * 1024 * 1024,
    },
  );
  assert.equal(referenceQuery.status, 0, `${fixture}: ${referenceQuery.stderr || referenceQuery.stdout}`);
  const rustQuery = spawnSync(binary, ['__query-index-files'], {
    cwd: root,
    input: JSON.stringify({
      archivePath,
      runId,
      generatedAt,
      filter,
      command,
      metric,
      offset,
      limit,
    }),
    encoding: 'utf8',
    maxBuffer: 128 * 1024 * 1024,
  });
  assert.equal(rustQuery.status, 0, `${fixture}: ${rustQuery.stderr || rustQuery.stdout}`);
  assert.equal(rustQuery.stdout, referenceQuery.stdout, `${fixture}: indexed ${command} JSON differs`);

  const attributed = expected.tests.find((test) => test.role === 'test');
  assert.ok(attributed, `${fixture}: expected at least one attributed test`);
  const filteredKind = fixtureIndex % 2 === 0 ? attributed.provenance.kind : undefined;
  const filteredRunner = fixtureIndex % 2 === 1 || fixtureIndex === fixtures.length - 1
    ? attributed.provenance.runner
    : undefined;
  const filteredArguments = [
    resolve(root, 'bin/supercov.js'),
    'runs', runId, 'coverage', 'gaps',
    '--filter', 'all', '--metric', 'mcdc', '--limit', '3', '--json',
    ...(filteredKind ? ['--kind', filteredKind] : []),
    ...(filteredRunner ? ['--runner', filteredRunner] : []),
  ];
  const filteredReference = spawnSync(process.execPath, filteredArguments, {
    cwd: resolve(root, 'tests/fixtures', fixture),
    encoding: 'utf8',
    maxBuffer: 128 * 1024 * 1024,
  });
  assert.equal(filteredReference.status, 0, `${fixture}: ${filteredReference.stderr || filteredReference.stdout}`);
  const filteredRust = spawnSync(binary, ['__query-index-files'], {
    cwd: root,
    input: JSON.stringify({
      archivePath,
      runId,
      generatedAt,
      filter: 'all',
      command: 'gaps',
      metric: 'mcdc',
      kind: filteredKind,
      runner: filteredRunner,
      offset: 0,
      limit: 3,
    }),
    encoding: 'utf8',
    maxBuffer: 128 * 1024 * 1024,
  });
  assert.equal(filteredRust.status, 0, `${fixture}: ${filteredRust.stderr || filteredRust.stdout}`);
  assert.equal(
    filteredRust.stdout,
    filteredReference.stdout,
    `${fixture}: provenance-filtered indexed gaps JSON differs`,
  );

  const indexedDecision = expected.decisions[0];
  assert.ok(indexedDecision, `${fixture}: expected at least one decision`);
  const decisionFile = indexedDecision.meta.file;
  const decisionReference = spawnSync(
    process.execPath,
    [
      resolve(root, 'bin/supercov.js'),
      'runs', runId, 'coverage', 'file', decisionFile,
      '--group', 'decision', '--sort', 'missing', '--limit', '2', '--json',
      ...(filteredKind ? ['--kind', filteredKind] : []),
      ...(filteredRunner ? ['--runner', filteredRunner] : []),
    ],
    {
      cwd: resolve(root, 'tests/fixtures', fixture),
      encoding: 'utf8',
      maxBuffer: 128 * 1024 * 1024,
    },
  );
  assert.equal(decisionReference.status, 0, `${fixture}: ${decisionReference.stderr || decisionReference.stdout}`);
  const decisionRust = spawnSync(binary, ['__query-index-files'], {
    cwd: root,
    input: JSON.stringify({
      archivePath,
      runId,
      generatedAt,
      filter: 'all',
      command: 'file-decisions',
      metric: 'all',
      kind: filteredKind,
      runner: filteredRunner,
      file: decisionFile,
      sort: 'missing',
      offset: 0,
      limit: 2,
    }),
    encoding: 'utf8',
    maxBuffer: 128 * 1024 * 1024,
  });
  assert.equal(decisionRust.status, 0, `${fixture}: ${decisionRust.stderr || decisionRust.stdout}`);
  assert.equal(
    decisionRust.stdout,
    decisionReference.stdout,
    `${fixture}: indexed file decision-group JSON differs`,
  );

  const dimensionCommand = fixtureIndex % 2 === 0 ? 'kinds' : 'runners';
  const dimensionReference = spawnSync(
    process.execPath,
    [
      resolve(root, 'bin/supercov.js'),
      'runs', runId, 'coverage', dimensionCommand,
      '--filter', filter, '--limit', '1', '--json',
    ],
    {
      cwd: resolve(root, 'tests/fixtures', fixture),
      encoding: 'utf8',
      maxBuffer: 128 * 1024 * 1024,
    },
  );
  assert.equal(dimensionReference.status, 0, `${fixture}: ${dimensionReference.stderr || dimensionReference.stdout}`);
  const dimensionRust = spawnSync(binary, ['__query-index-files'], {
    cwd: root,
    input: JSON.stringify({
      archivePath,
      runId,
      generatedAt,
      filter,
      command: dimensionCommand,
      metric: 'all',
      offset: 0,
      limit: 1,
    }),
    encoding: 'utf8',
    maxBuffer: 128 * 1024 * 1024,
  });
  assert.equal(dimensionRust.status, 0, `${fixture}: ${dimensionRust.stderr || dimensionRust.stdout}`);
  assert.equal(
    dimensionRust.stdout,
    dimensionReference.stdout,
    `${fixture}: indexed ${dimensionCommand} JSON differs`,
  );

  const summaryFilter = fixtureIndex % 2 === 0 ? 'all' : 'passed';
  const summaryKind = summaryFilter === 'all' ? filteredKind : undefined;
  const summaryRunner = summaryFilter === 'all' ? filteredRunner : undefined;
  const summaryReference = spawnSync(
    process.execPath,
    [
      resolve(root, 'bin/supercov.js'),
      'runs', runId, 'coverage',
      '--filter', summaryFilter, '--json',
      ...(summaryKind ? ['--kind', summaryKind] : []),
      ...(summaryRunner ? ['--runner', summaryRunner] : []),
    ],
    {
      cwd: resolve(root, 'tests/fixtures', fixture),
      encoding: 'utf8',
      maxBuffer: 128 * 1024 * 1024,
    },
  );
  assert.equal(summaryReference.status, 0, `${fixture}: ${summaryReference.stderr || summaryReference.stdout}`);
  const summaryEnvelope = JSON.parse(summaryReference.stdout);
  const summaryRust = spawnSync(binary, ['__query-index-files'], {
    cwd: root,
    input: JSON.stringify({
      archivePath,
      runId,
      generatedAt: summaryEnvelope.data.generatedAt,
      filter: summaryFilter,
      command: 'summary',
      metric: 'all',
      kind: summaryKind,
      runner: summaryRunner,
      valid: summaryEnvelope.data.valid,
      stale: summaryEnvelope.data.stale,
      staleReasons: summaryEnvelope.data.staleReasons,
      offset: 0,
      limit: 20,
    }),
    encoding: 'utf8',
    maxBuffer: 128 * 1024 * 1024,
  });
  assert.equal(summaryRust.status, 0, `${fixture}: ${summaryRust.stderr || summaryRust.stdout}`);
  assert.equal(
    summaryRust.stdout,
    summaryReference.stdout,
    `${fixture}: indexed summary JSON differs`,
  );

  if (expected.scope) {
    const scopeReference = spawnSync(
      process.execPath,
      [
        resolve(root, 'bin/supercov.js'),
        'runs', runId, 'coverage', 'scope',
        '--filter', 'all', '--limit', '2', '--offset', '1', '--json',
      ],
      {
        cwd: resolve(root, 'tests/fixtures', fixture),
        encoding: 'utf8',
        maxBuffer: 128 * 1024 * 1024,
      },
    );
    assert.equal(scopeReference.status, 0, `${fixture}: ${scopeReference.stderr || scopeReference.stdout}`);
    const scopeRust = spawnSync(binary, ['__query-index-files'], {
      cwd: root,
      input: JSON.stringify({
        archivePath,
        runId,
        generatedAt,
        filter: 'all',
        command: 'scope',
        metric: 'all',
        offset: 1,
        limit: 2,
      }),
      encoding: 'utf8',
      maxBuffer: 128 * 1024 * 1024,
    });
    assert.equal(scopeRust.status, 0, `${fixture}: ${scopeRust.stderr || scopeRust.stdout}`);
    assert.equal(scopeRust.stdout, scopeReference.stdout, `${fixture}: indexed scope JSON differs`);
  }
}

const indexed = spawnSync(binary, ['__roundtrip-query-index'], {
  cwd: root,
  input: JSON.stringify(indexRequests),
  encoding: 'utf8',
  maxBuffer: 128 * 1024 * 1024,
});
assert.equal(indexed.status, 0, indexed.stderr || indexed.stdout);
const indexedActual = JSON.parse(indexed.stdout);
const indexDifference = firstDifference(indexedActual, indexExpected);
assert.equal(indexDifference, undefined, `typed index: ${JSON.stringify(indexDifference)}`);

console.log(
  `[rust-archive-differential] ${fixtures.length} real archives have exact report plus typed mmap summary, scope, file-gap, provenance, dimension, and decision-group query parity`,
);
