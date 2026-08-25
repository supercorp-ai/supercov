#!/usr/bin/env node

import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { createMcdcReport, isIndependencePair } from "../dist/analyze.js";

let state = 0x51_7c_0a_2d;
function random() {
  state = (Math.imul(state, 1_664_525) + 1_013_904_223) >>> 0;
  return state / 0x1_0000_0000;
}
function integer(maximum) {
  return Math.floor(random() * maximum);
}
function value() {
  return [null, false, true][integer(3)];
}

const cases = Array.from({ length: 250 }, (_, caseIndex) => {
  const decisions = Array.from({ length: 1 + integer(5) }, (_, decisionIndex) => {
    const conditionCount = 1 + integer(8);
    return {
      id: `case-${caseIndex}-decision-${decisionIndex}`,
      file: `case-${caseIndex}.js`,
      line: decisionIndex + 1,
      column: 1,
      source: `decision ${decisionIndex}`,
      conditions: Array.from({ length: conditionCount }, (_, index) => `c${index}`),
      kind: "if",
    };
  });
  const points = Array.from({ length: integer(12) }, (_, pointIndex) => ({
    id: `case-${caseIndex}-point-${pointIndex}`,
    kind: pointIndex % 3 === 0 ? "function" : "statement",
    file: `case-${caseIndex}.js`,
    line: 20 + pointIndex,
    column: 1,
    source: `point ${pointIndex}`,
  }));
  const branches = Array.from({ length: integer(8) }, (_, branchIndex) => ({
    id: `case-${caseIndex}-branch-${branchIndex}`,
    kind: branchIndex % 3 === 0 ? "logical-value" : "optional-chain",
    file: `case-${caseIndex}.js`,
    line: 40 + branchIndex,
    column: 1,
    source: `branch ${branchIndex}`,
    alternatives: Array.from({ length: 2 + integer(3) }, (_, alternativeIndex) => ({
      id: `case-${caseIndex}-branch-${branchIndex}-alternative-${alternativeIndex}`,
      label: `alternative ${alternativeIndex}`,
    })),
  }));
  const snapshots = decisions.map((meta) => ({
    meta,
    vectors: Array.from({ length: integer(24) }, () => ({
      values: Array.from({ length: meta.conditions.length }, value),
      outcome: random() < 0.5,
    })),
  }));
  const hits = [...points.map((point) => point.id), ...branches.flatMap((branch) =>
    branch.alternatives.map((alternative) => alternative.id),
  )].filter(() => random() < 0.6);
  const report = createMcdcReport(
    { decisions, points, branches },
    [{
      testId: `case-${caseIndex}`,
      test: `case ${caseIndex}`,
      browser: [],
      server: [],
      runtime: [{ decisions: snapshots, hits }],
    }],
  );
  const input = {
    decisions: report.decisions.map((decision) => ({
      conditionCount: decision.meta.conditions.length,
      vectors: decision.vectors,
    })),
    points: report.points.map((point) => ({
      kind: point.meta.kind,
      covered: point.covered,
    })),
    branches: report.branches.map((branch) => ({
      kind: branch.meta.kind,
      alternatives: branch.alternatives.map((alternative) => alternative.covered),
    })),
    lines: report.lines.map((line) => line.covered),
  };
  const witnesses = input.decisions.map((decision) =>
    Array.from({ length: decision.conditionCount }, (_, condition) => {
      for (let first = 0; first < decision.vectors.length; first += 1) {
        for (let second = first + 1; second < decision.vectors.length; second += 1) {
          if (isIndependencePair(decision.vectors[first], decision.vectors[second], condition))
            return { first, second };
        }
      }
      return null;
    }),
  );
  return { input, expected: { witnesses, summary: report.summary } };
});

const binary = process.env.SUPERCOV_RUST_BINARY ??
  new URL("../target/debug/supercov", import.meta.url).pathname;
const child = spawnSync(binary, ["__analyze-coverage-core"], {
  input: JSON.stringify(cases.map((entry) => entry.input)),
  encoding: "utf8",
  maxBuffer: 1024 * 1024 * 64,
});
if (child.error) throw child.error;
if (child.status !== 0)
  throw new Error(`Rust coverage analyzer failed (${child.status}): ${child.stderr}`);
const actual = JSON.parse(child.stdout);
assert.equal(actual.length, cases.length);
for (const [index, entry] of cases.entries())
  assert.deepStrictEqual(actual[index], entry.expected, `coverage analysis case ${index}`);

console.log(
  `[rust-analysis-differential] ${cases.length} generated coverage models have exact witness and summary parity`,
);
