import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { resolve } from 'node:path';

import { createMcdcReport } from '../dist/analyze.js';
import { minimumTestSet } from '../dist/query.js';

const root = resolve(import.meta.dirname, '..');
const binary = resolve(root, 'target/debug/supercov');

function vector(values, outcome) {
  return { values, outcome };
}

const canonical = [
  vector([false, null, null], false),
  vector([true, false, null], false),
  vector([true, true, false], false),
  vector([true, true, true], true),
];

function fixture(index) {
  const file = `src/decision-${index % 7}.js`;
  const decision = {
    id: `decision:${index}`,
    file,
    line: 10,
    column: 2,
    source: 'first && second && third',
    conditions: ['first', 'second', 'third'],
    kind: 'if',
  };
  const points = [
    {
      id: `statement:${index}:entry`,
      kind: 'statement',
      file,
      line: 9,
      column: 0,
      source: 'enter();',
    },
    {
      id: `function:${index}:run`,
      kind: 'function',
      file,
      line: 8,
      column: 0,
      source: 'function run() {}',
      label: 'run',
    },
  ];
  const branch = {
    id: `branch:${index}`,
    kind: 'if',
    file,
    line: 10,
    column: 2,
    source: decision.source,
    alternatives: [
      { id: `branch:${index}:false`, label: 'false' },
      { id: `branch:${index}:true`, label: 'true' },
    ],
  };
  const tests = canonical.map((observed, ordinal) => ({
    testId: `test:${index}:${ordinal}`,
    test: `case ${ordinal}`,
    testFile: 'tests/decision.test.js',
    retry: 0,
    status: 'passed',
    provenance: { runner: 'node:test', kind: 'unit', source: 'runner-default' },
    role: 'test',
    phases: [],
    runtime: [{
      decisions: [{ meta: decision, vectors: [observed] }],
      hits: [
        ...(ordinal === 0 ? [points[0].id, points[1].id] : []),
        ordinal === canonical.length - 1
          ? branch.alternatives[1].id
          : branch.alternatives[0].id,
      ],
      events: [],
    }],
    browser: [],
    server: [],
  }));
  for (let duplicate = 0; duplicate < index % 4; duplicate += 1) {
    const source = tests[(duplicate * 3 + index) % tests.length];
    tests.push({
      ...structuredClone(source),
      testId: `test:${index}:duplicate:${duplicate}`,
      test: `duplicate ${duplicate}`,
    });
  }
  if (index % 3 === 0) {
    tests.push({
      testId: `setup:${index}`,
      test: 'file setup',
      testFile: 'tests/decision.test.js',
      retry: 0,
      status: 'passed',
      provenance: { runner: 'node:test', kind: 'unit', source: 'runner-default' },
      role: 'setup',
      phases: [],
      runtime: [{ decisions: [], hits: [points[0].id], events: [] }],
      browser: [],
      server: [],
    });
  }
  const manifest = { decisions: [decision], points, branches: [branch] };
  const metric = ['all', 'lines', 'statements', 'functions', 'branches', 'mcdc'][index % 6];
  const target = index % 5 === 0 ? 50 : 100;
  return {
    manifest,
    rawResults: tests,
    metric,
    target,
  };
}

const fixtures = Array.from({ length: 120 }, (_, index) => fixture(index));
const requests = fixtures.map((fixture, index) => ({
  coverage: {
    runId: `run-${index}`,
    manifest: fixture.manifest,
    rawResults: fixture.rawResults,
    generatedAt: '2026-08-25T00:00:00.000Z',
    testExitCode: 0,
  },
  target: fixture.target,
  metric: fixture.metric,
  maxStates: 5_000,
}));
const expected = fixtures.map((fixture) =>
  minimumTestSet(
    createMcdcReport(fixture.manifest, fixture.rawResults),
    fixture.target,
    fixture.metric,
  )
);
const temporary = mkdtempSync(resolve(tmpdir(), 'supercov-minimize-differential-'));
let actual;
try {
  const input = resolve(temporary, 'requests.json');
  writeFileSync(input, JSON.stringify(requests));
  const child = spawnSync(binary, ['__minimum-test-set'], {
    cwd: root,
    encoding: 'utf8',
    maxBuffer: 64 * 1024 * 1024,
    timeout: 120_000,
    env: { ...process.env, SUPERCOV_INTERNAL_INPUT_FILE: input },
  });
  if (child.error) throw child.error;
  assert.equal(child.status, 0, child.stderr || child.stdout);
  actual = JSON.parse(child.stdout);
} finally {
  rmSync(temporary, { recursive: true, force: true });
}
assert.deepEqual(actual, expected);
console.log(`[rust-minimize-differential] ${actual.length} exact mixed-obligation models`);
