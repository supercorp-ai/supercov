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

function archivesForFixture(fixture) {
  const runs = resolve(root, 'tests/fixtures', fixture, '.supercov/runs');
  return readdirSync(runs, { withFileTypes: true })
    .filter((entry) => entry.isDirectory())
    .map((entry) => entry.name)
    .sort()
    .reverse()
    .map((id) => ({ id, archivePath: resolve(runs, id, 'evidence.raw.gz') }))
    .filter(({ archivePath }) => existsSync(archivePath));
}

function newestArchive(fixture) {
  const [newest] = archivesForFixture(fixture);
  if (newest) return newest;
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

  const detailReference = spawnSync(
    process.execPath,
    [
      resolve(root, 'bin/supercov.js'),
      'runs', runId, 'coverage', 'decision', indexedDecision.meta.id,
      '--filter', 'all', '--limit', '2', '--json',
      ...(filteredKind ? ['--kind', filteredKind] : []),
      ...(filteredRunner ? ['--runner', filteredRunner] : []),
    ],
    {
      cwd: resolve(root, 'tests/fixtures', fixture),
      encoding: 'utf8',
      maxBuffer: 128 * 1024 * 1024,
    },
  );
  assert.equal(detailReference.status, 0, `${fixture}: ${detailReference.stderr || detailReference.stdout}`);
  const detailRust = spawnSync(binary, ['__query-index-files'], {
    cwd: root,
    input: JSON.stringify({
      archivePath,
      runId,
      generatedAt,
      filter: 'all',
      command: 'decision',
      metric: 'all',
      kind: filteredKind,
      runner: filteredRunner,
      selector: indexedDecision.meta.id,
      offset: 0,
      limit: 2,
    }),
    encoding: 'utf8',
    maxBuffer: 128 * 1024 * 1024,
  });
  assert.equal(detailRust.status, 0, `${fixture}: ${detailRust.stderr || detailRust.stdout}`);
  assert.equal(detailRust.stdout, detailReference.stdout, `${fixture}: indexed decision JSON differs`);

  const decisionLocations = new Map();
  for (const decision of expected.decisions) {
    const key = `${decision.meta.file}:${decision.meta.line}`;
    decisionLocations.set(key, [...(decisionLocations.get(key) ?? []), decision]);
  }
  const ambiguousDecision = [...decisionLocations.entries()].find(([, decisions]) => decisions.length > 1);
  if (ambiguousDecision) {
    const [selector] = ambiguousDecision;
    const matchesReference = spawnSync(
      process.execPath,
      [
        resolve(root, 'bin/supercov.js'),
        'runs', runId, 'coverage', 'decision', selector,
        '--filter', 'all', '--limit', '1', '--json',
      ],
      {
        cwd: resolve(root, 'tests/fixtures', fixture),
        encoding: 'utf8',
        maxBuffer: 128 * 1024 * 1024,
      },
    );
    assert.equal(matchesReference.status, 0, `${fixture}: ${matchesReference.stderr || matchesReference.stdout}`);
    const matchesRust = spawnSync(binary, ['__query-index-files'], {
      cwd: root,
      input: JSON.stringify({
        archivePath,
        runId,
        generatedAt,
        filter: 'all',
        command: 'decision',
        metric: 'all',
        selector,
        offset: 0,
        limit: 1,
      }),
      encoding: 'utf8',
      maxBuffer: 128 * 1024 * 1024,
    });
    assert.equal(matchesRust.status, 0, `${fixture}: ${matchesRust.stderr || matchesRust.stdout}`);
    assert.equal(matchesRust.stdout, matchesReference.stdout, `${fixture}: indexed decision matches JSON differs`);
  }

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

  const reachableLineTarget = Math.min(50, expected.summary.lines.percentage);
  if (reachableLineTarget > 0) {
    const minimizeReference = spawnSync(
      process.execPath,
      [
        resolve(root, 'bin/supercov.js'),
        'runs', runId, 'coverage', 'minimize',
        '--filter', 'all', '--metric', 'lines', '--target', String(reachableLineTarget),
        '--limit', '1', '--json',
      ],
      {
        cwd: resolve(root, 'tests/fixtures', fixture),
        encoding: 'utf8',
        maxBuffer: 128 * 1024 * 1024,
      },
    );
    assert.equal(minimizeReference.status, 0, `${fixture}: ${minimizeReference.stderr || minimizeReference.stdout}`);
    const minimizeRust = spawnSync(binary, ['__query-index-files'], {
      cwd: root,
      input: JSON.stringify({
        archivePath,
        runId,
        generatedAt,
        filter: 'all',
        command: 'minimize',
        metric: 'lines',
        target: reachableLineTarget,
        maxStates: 5000,
        offset: 0,
        limit: 1,
      }),
      encoding: 'utf8',
      maxBuffer: 128 * 1024 * 1024,
    });
    assert.equal(minimizeRust.status, 0, `${fixture}: ${minimizeRust.stderr || minimizeRust.stdout}`);
    assert.equal(minimizeRust.stdout, minimizeReference.stdout, `${fixture}: indexed minimize JSON differs`);
  }

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

  const coveredLine = expected.lines[0];
  assert.ok(coveredLine, `${fixture}: expected at least one line obligation`);
  const fileDetailReference = spawnSync(
    process.execPath,
    [
      resolve(root, 'bin/supercov.js'),
      'runs', runId, 'coverage', 'file', coveredLine.file,
      '--filter', 'all', '--metric', metric, '--limit', '2', '--json',
      ...(filteredKind ? ['--kind', filteredKind] : []),
      ...(filteredRunner ? ['--runner', filteredRunner] : []),
    ],
    {
      cwd: resolve(root, 'tests/fixtures', fixture),
      encoding: 'utf8',
      maxBuffer: 128 * 1024 * 1024,
    },
  );
  assert.equal(fileDetailReference.status, 0, `${fixture}: ${fileDetailReference.stderr || fileDetailReference.stdout}`);
  const fileDetailRust = spawnSync(binary, ['__query-index-files'], {
    cwd: root,
    input: JSON.stringify({
      archivePath,
      runId,
      generatedAt,
      filter: 'all',
      command: 'file-detail',
      metric,
      kind: filteredKind,
      runner: filteredRunner,
      file: coveredLine.file,
      offset: 0,
      limit: 2,
    }),
    encoding: 'utf8',
    maxBuffer: 128 * 1024 * 1024,
  });
  assert.equal(fileDetailRust.status, 0, `${fixture}: ${fileDetailRust.stderr || fileDetailRust.stdout}`);
  assert.equal(fileDetailRust.stdout, fileDetailReference.stdout, `${fixture}: indexed file-detail JSON differs`);
  const coversArguments = [
    resolve(root, 'bin/supercov.js'),
    'runs', runId, 'coverage', 'covers', `${coveredLine.file}:${coveredLine.line}`,
    '--filter', 'all', '--limit', '2', '--json',
    ...(filteredKind ? ['--kind', filteredKind] : []),
    ...(filteredRunner ? ['--runner', filteredRunner] : []),
  ];
  const coversReference = spawnSync(process.execPath, coversArguments, {
    cwd: resolve(root, 'tests/fixtures', fixture),
    encoding: 'utf8',
    maxBuffer: 128 * 1024 * 1024,
  });
  assert.equal(coversReference.status, 0, `${fixture}: ${coversReference.stderr || coversReference.stdout}`);
  const coversRust = spawnSync(binary, ['__query-index-files'], {
    cwd: root,
    input: JSON.stringify({
      archivePath,
      runId,
      generatedAt,
      filter: 'all',
      command: 'covers',
      metric: 'all',
      kind: filteredKind,
      runner: filteredRunner,
      file: coveredLine.file,
      line: coveredLine.line,
      offset: 0,
      limit: 2,
    }),
    encoding: 'utf8',
    maxBuffer: 128 * 1024 * 1024,
  });
  assert.equal(coversRust.status, 0, `${fixture}: ${coversRust.stderr || coversRust.stdout}`);
  assert.equal(coversRust.stdout, coversReference.stdout, `${fixture}: indexed covers JSON differs`);

  const testReference = spawnSync(
    process.execPath,
    [
      resolve(root, 'bin/supercov.js'),
      'runs', runId, 'coverage', 'test', attributed.id,
      '--filter', 'all', '--limit', '2', '--json',
      ...(filteredKind ? ['--kind', filteredKind] : []),
      ...(filteredRunner ? ['--runner', filteredRunner] : []),
    ],
    {
      cwd: resolve(root, 'tests/fixtures', fixture),
      encoding: 'utf8',
      maxBuffer: 128 * 1024 * 1024,
    },
  );
  assert.equal(testReference.status, 0, `${fixture}: ${testReference.stderr || testReference.stdout}`);
  const testRust = spawnSync(binary, ['__query-index-files'], {
    cwd: root,
    input: JSON.stringify({
      archivePath,
      runId,
      generatedAt,
      filter: 'all',
      command: 'test',
      metric: 'all',
      kind: filteredKind,
      runner: filteredRunner,
      selector: attributed.id,
      offset: 0,
      limit: 2,
    }),
    encoding: 'utf8',
    maxBuffer: 128 * 1024 * 1024,
  });
  assert.equal(testRust.status, 0, `${fixture}: ${testRust.stderr || testRust.stdout}`);
  assert.equal(testRust.stdout, testReference.stdout, `${fixture}: indexed test JSON differs`);

  const testGroups = new Map();
  for (const test of expected.tests.filter((candidate) => candidate.role === 'test')) {
    const prefix = test.name.split(' > ')[0].toLowerCase();
    testGroups.set(prefix, [...(testGroups.get(prefix) ?? []), test]);
  }
  const ambiguousTest = [...testGroups.entries()].find(([, tests]) => tests.length > 1);
  if (ambiguousTest) {
    const [selector] = ambiguousTest;
    const matchesReference = spawnSync(
      process.execPath,
      [
        resolve(root, 'bin/supercov.js'),
        'runs', runId, 'coverage', 'test', selector,
        '--filter', 'all', '--limit', '1', '--json',
      ],
      {
        cwd: resolve(root, 'tests/fixtures', fixture),
        encoding: 'utf8',
        maxBuffer: 128 * 1024 * 1024,
      },
    );
    assert.equal(matchesReference.status, 0, `${fixture}: ${matchesReference.stderr || matchesReference.stdout}`);
    const matchesRust = spawnSync(binary, ['__query-index-files'], {
      cwd: root,
      input: JSON.stringify({
        archivePath,
        runId,
        generatedAt,
        filter: 'all',
        command: 'test',
        metric: 'all',
        selector,
        offset: 0,
        limit: 1,
      }),
      encoding: 'utf8',
      maxBuffer: 128 * 1024 * 1024,
    });
    assert.equal(matchesRust.status, 0, `${fixture}: ${matchesRust.stderr || matchesRust.stdout}`);
    assert.equal(matchesRust.stdout, matchesReference.stdout, `${fixture}: indexed test matches JSON differs`);
  }

  const anchoredOnly = [
    ...expected.decisions.map((decision) => decision.meta),
    ...expected.branches.map((branch) => branch.meta),
    ...expected.points.map((point) => point.meta),
  ].find((anchor) => !expected.lines.some(
    (line) => line.file === anchor.file && line.line === anchor.line,
  ));
  if (anchoredOnly) {
    const selector = `${anchoredOnly.file}:${anchoredOnly.line}`;
    const anchorReference = spawnSync(
      process.execPath,
      [
        resolve(root, 'bin/supercov.js'),
        'runs', runId, 'coverage', 'covers', selector,
        '--filter', 'all', '--limit', '2', '--json',
        ...(filteredKind ? ['--kind', filteredKind] : []),
        ...(filteredRunner ? ['--runner', filteredRunner] : []),
      ],
      {
        cwd: resolve(root, 'tests/fixtures', fixture),
        encoding: 'utf8',
        maxBuffer: 128 * 1024 * 1024,
      },
    );
    assert.equal(anchorReference.status, 0, `${fixture}: ${anchorReference.stderr || anchorReference.stdout}`);
    const anchorRust = spawnSync(binary, ['__query-index-files'], {
      cwd: root,
      input: JSON.stringify({
        archivePath,
        runId,
        generatedAt,
        filter: 'all',
        command: 'covers',
        metric: 'all',
        kind: filteredKind,
        runner: filteredRunner,
        file: anchoredOnly.file,
        line: anchoredOnly.line,
        offset: 0,
        limit: 2,
      }),
      encoding: 'utf8',
      maxBuffer: 128 * 1024 * 1024,
    });
    assert.equal(anchorRust.status, 0, `${fixture}: ${anchorRust.stderr || anchorRust.stdout}`);
    assert.equal(anchorRust.stdout, anchorReference.stdout, `${fixture}: indexed anchored covers JSON differs`);
  }

  const [, olderArchive] = archivesForFixture(fixture);
  if (olderArchive) {
    const diffFilter = fixtureIndex % 2 === 0 ? 'all' : 'passed';
    const diffReference = spawnSync(
      process.execPath,
      [
        resolve(root, 'bin/supercov.js'),
        'diff', olderArchive.id, runId,
        '--filter', diffFilter, '--limit', '2', '--json',
      ],
      {
        cwd: resolve(root, 'tests/fixtures', fixture),
        encoding: 'utf8',
        maxBuffer: 128 * 1024 * 1024,
      },
    );
    assert.equal(diffReference.status, 0, `${fixture}: ${diffReference.stderr || diffReference.stdout}`);
    const diffRust = spawnSync(binary, ['__query-index-files'], {
      cwd: root,
      input: JSON.stringify({
        archivePath: olderArchive.archivePath,
        runId: olderArchive.id,
        newerArchivePath: archivePath,
        newerRunId: runId,
        generatedAt,
        filter: diffFilter,
        command: 'diff',
        metric: 'all',
        offset: 0,
        limit: 2,
      }),
      encoding: 'utf8',
      maxBuffer: 128 * 1024 * 1024,
    });
    assert.equal(diffRust.status, 0, `${fixture}: ${diffRust.stderr || diffRust.stdout}`);
    assert.equal(diffRust.stdout, diffReference.stdout, `${fixture}: indexed diff JSON differs`);
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
  `[rust-archive-differential] ${fixtures.length} real archive families have exact report plus typed mmap summary, scope, file-gap, provenance, dimension, decision detail/group, minimization, diff, and bidirectional attribution query parity`,
);
