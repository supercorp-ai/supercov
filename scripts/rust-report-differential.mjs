import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { resolve } from 'node:path';

import { analyzeCoverageResults } from '../dist/runAnalysis.js';

const root = resolve(import.meta.dirname, '..');
const binary = resolve(root, 'target/debug/supercov');
const generatedAt = '2026-08-25T00:00:00.000Z';

function vector(values, outcome) {
  return { values, outcome };
}

function request(index) {
  const decision = {
    id: `decision:${index}`,
    file: `src/file-${index % 5}.js`,
    line: 10 + index,
    column: 2,
    source: 'left && right',
    conditions: ['left', 'right'],
    kind: 'if',
  };
  const points = [
    {
      id: `statement:${index}:0`,
      kind: 'statement',
      file: decision.file,
      line: decision.line,
      column: 0,
      source: 'run();',
    },
    {
      id: `function:${index}:0`,
      kind: 'function',
      file: decision.file,
      line: decision.line + 1,
      column: 0,
      source: 'function run() {}',
      label: 'run',
    },
  ];
  const branch = {
    id: `branch:${index}`,
    kind: index % 2 === 0 ? 'logical-value' : 'optional-chain',
    file: decision.file,
    line: decision.line + 2,
    column: 1,
    source: 'left && right',
    alternatives: [
      { id: `branch:${index}:short`, label: 'short-circuit' },
      { id: `branch:${index}:right`, label: 'right-evaluated' },
    ],
  };
  const first = vector([false, null], false);
  const second = vector([true, false], false);
  const third = vector([true, true], true);
  const action = {
    id: `phase:${index}:action`,
    kind: 'action',
    operation: 'click',
    source: 'test.spec.js:10',
    startedAtMs: 100,
    endedAtMs: 120,
    status: 'passed',
  };
  const assertion = {
    id: `phase:${index}:assertion`,
    kind: 'assertion',
    operation: 'equal',
    causedByPhaseId: action.id,
    startedAtMs: 130,
    endedAtMs: 140,
    status: index % 7 === 0 ? 'failed' : 'passed',
  };
  const primaryId = `test:${index}:primary`;
  const primary = {
    testId: primaryId,
    test: `primary test ${String(index).padStart(3, '0')}`,
    testFile: 'tests/integration/main.spec.js',
    title: 'primary',
    retry: index % 3 === 0 ? 1 : 0,
    status: 'passed',
    flaky: index % 3 === 0,
    provenance: {
      runner: index % 2 === 0 ? 'playwright' : 'vitest',
      kind: index % 2 === 0 ? 'e2e' : 'integration',
      project: 'default',
      source: 'project',
    },
    role: 'test',
    phases: [assertion, action],
    runtime: [
      {
        decisions: [{ meta: decision, vectors: [first, second] }],
        hits: [points[0].id, branch.alternatives[0].id],
        events: [
          {
            type: 'decision',
            id: decision.id,
            vector: first,
            timestampMs: 105,
            phaseId: action.id,
            environment: 'server',
          },
          {
            type: 'hit',
            id: points[0].id,
            timestampMs: 106,
            phaseId: action.id,
            environment: 'server',
          },
          {
            type: 'decision',
            id: decision.id,
            vector: second,
            timestampMs: 135,
            phaseId: assertion.id,
            environment: 'server',
          },
        ],
      },
    ],
    browser: [],
    server: [],
  };
  const secondary = {
    testId: `test:${index}:secondary`,
    test: `secondary test ${String(index).padStart(3, '0')}`,
    testFile: 'tests/unit/helper.test.js',
    retry: 0,
    status: index % 11 === 0 ? 'failed' : 'passed',
    expectedStatus: index % 13 === 0 ? 'failed' : 'passed',
    provenance: {
      runner: 'node:test',
      kind: 'unit',
      source: 'runner-default',
    },
    role: index % 17 === 0 ? 'setup' : 'test',
    phases: [],
    runtime: [],
    browser: [
      {
        decisions: [{ meta: decision, vectors: [third, third] }],
        hits: [points[1].id, branch.alternatives[1].id],
        events: [
          {
            type: 'decision',
            id: decision.id,
            vector: third,
            timestampMs: 300,
            environment: 'browser',
          },
          {
            type: 'hit',
            id: points[1].id,
            timestampMs: 301,
            environment: 'browser',
          },
        ],
      },
    ],
    server: [
      {
        type: 'hit',
        id: points[1].id,
      },
    ],
  };
  const rawResults = [];
  if (index % 3 === 0) {
    rawResults.push({
      ...primary,
      retry: 0,
      status: 'failed',
      flaky: false,
      phases: [],
      runtime: [],
    });
  }
  rawResults.push(primary, secondary);
  if (index % 5 === 0) {
    rawResults.push({
      testId: `background:${index}`,
      test: 'Background / unattributed',
      title: 'Background / unattributed',
      status: 'unknown',
      provenance: { runner: 'background', kind: 'background', source: 'explicit' },
      role: 'background',
      browser: [],
      server: [{ type: 'decision', meta: decision, vector: first }],
    });
  }
  return {
    runId: 'differential',
    manifest: {
      decisions: [decision],
      points,
      branches: [branch],
      ...(index % 19 === 0
        ? {
            limitations: [
              {
                id: `limitation:${index}`,
                kind: 'dynamic-code',
                file: decision.file,
                line: 1,
                column: 0,
                source: 'eval(source)',
                reason: 'runtime source is unavailable ahead of execution',
              },
            ],
          }
        : {}),
      ...(index % 23 === 0
        ? {
            scope: {
              version: 1,
              mode: 'automatic',
              roots: ['src'],
              entries: [],
            },
          }
        : {}),
    },
    rawResults,
    generatedAt,
  };
}

const requests = Array.from({ length: 100 }, (_, index) => request(index));
const expected = JSON.parse(
  JSON.stringify(
    requests.map(({ manifest, rawResults, generatedAt, runId }) =>
      analyzeCoverageResults(manifest, structuredClone(rawResults), {
        runId,
        generatedAt,
      }),
    ),
  ),
);
const child = spawnSync(binary, ['__analyze-coverage-results'], {
  cwd: root,
  input: JSON.stringify(requests),
  encoding: 'utf8',
  maxBuffer: 64 * 1024 * 1024,
});
assert.equal(child.status, 0, child.stderr || child.stdout);
const actual = JSON.parse(child.stdout);
assert.deepEqual(actual, expected);
console.log(
  `[rust-report-differential] ${requests.length} generated evidence models have exact report, attribution, outcome, and filter parity`,
);
