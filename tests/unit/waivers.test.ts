import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { resolve } from "node:path";
import { afterEach, describe, it } from "node:test";
import { expect } from "../support/expect.ts";
import { createMcdcReport } from "../../src/analyze.ts";
import {
  evaluateCoverageWaivers,
  readCoverageWaivers,
  WAIVERS_FILE,
} from "../../src/waivers.ts";
import type { McdcDecisionMeta, McdcVector } from "../../src/types.ts";

const meta: McdcDecisionMeta = {
  id: "decision-1",
  file: "src/example.ts",
  line: 7,
  column: 5,
  source: "left && (right || other)",
  conditions: ["left", "right", "other"],
  kind: "if",
};

function reportWith(vectors: McdcVector[]) {
  return createMcdcReport({ decisions: [meta], points: [], branches: [] }, [
    {
      testId: "test-1",
      test: "test-1",
      retry: 0,
      runtime: [{ decisions: [{ meta, vectors }], hits: [] }],
      browser: [],
      server: [],
    },
  ]);
}

const temporaryDirectories: string[] = [];

function project(content?: string): string {
  const root = mkdtempSync(resolve(tmpdir(), "supercov-waivers-"));
  temporaryDirectories.push(root);
  if (content !== undefined)
    writeFileSync(resolve(root, WAIVERS_FILE), content);
  return root;
}

afterEach(() => {
  for (const directory of temporaryDirectories.splice(0))
    rmSync(directory, { recursive: true, force: true });
});

describe("reviewed MC/DC waivers", () => {
  it("returns nothing when no waivers file exists", () => {
    expect(readCoverageWaivers(project())).toBeUndefined();
  });

  it("rejects malformed waiver files with exact problems", () => {
    expect(() => readCoverageWaivers(project("{"))).toThrow(
      /is not valid JSON/,
    );
    expect(() => readCoverageWaivers(project("[]"))).toThrow(
      /must be \{"version": 1, "waivers": \[\.\.\.\]\}/,
    );
    expect(() =>
      readCoverageWaivers(
        project(JSON.stringify({ version: 1, waivers: [{ file: "a.ts" }] })),
      ),
    ).toThrow(/waiver 1 requires a non-empty condition/);
    expect(() =>
      readCoverageWaivers(
        project(
          JSON.stringify({
            version: 1,
            waivers: [{ file: "a.ts", condition: "left", reason: "  " }],
          }),
        ),
      ),
    ).toThrow(/waiver 1 requires a non-empty reason/);
    expect(() =>
      readCoverageWaivers(
        project(
          JSON.stringify({
            version: 1,
            waivers: [{ file: "a.ts", condition: "C2", reason: "why" }],
          }),
        ),
      ),
    ).toThrow(/uses the positional condition C2 without a decision/);
  });

  it("matches by decision ID, source text, and positional label", () => {
    const report = reportWith([
      { values: [false, null, null], outcome: false },
      { values: [true, true, null], outcome: true },
    ]);
    const evaluation = evaluateCoverageWaivers(report.decisions, {
      path: WAIVERS_FILE,
      waivers: [
        {
          file: "src/example.ts",
          decision: "decision-1",
          condition: "right",
          reason: "right cannot flip the outcome independently here",
        },
        {
          file: "src/example.ts",
          decision: "left  &&  (right || other)",
          condition: "C3",
          reason: "other is unreachable in this configuration",
        },
        {
          file: "src/example.ts",
          condition: "left",
          reason: "stale: left is actually coverable",
        },
        {
          file: "src/missing.ts",
          condition: "anything",
          reason: "matches nothing in this run",
        },
        {
          file: "src/example.ts",
          line: 99,
          condition: "right",
          reason: "line scoping must exclude other locations",
        },
      ],
    });

    expect(evaluation.applied).toMatchObject([
      { decisionId: "decision-1", conditionIndex: 1, covered: false },
      { decisionId: "decision-1", conditionIndex: 2, covered: false },
    ]);
    expect(evaluation.contradicted).toMatchObject([
      { decisionId: "decision-1", conditionIndex: 0, covered: true },
    ]);
    expect(evaluation.unmatched).toMatchObject([
      { file: "src/missing.ts" },
      { file: "src/example.ts", line: 99 },
    ]);
    expect(
      evaluation.waivedByDecision.get("decision-1")?.get(1)?.reason,
    ).toBe("right cannot flip the outcome independently here");
    expect(evaluation.appliedByFile.get("src/example.ts")).toBe(2);
  });

  it("applies the first waiver when two waive the same condition", () => {
    const report = reportWith([
      { values: [false, null, null], outcome: false },
    ]);
    const evaluation = evaluateCoverageWaivers(report.decisions, {
      path: WAIVERS_FILE,
      waivers: [
        {
          file: "src/example.ts",
          decision: "decision-1",
          condition: "C2",
          reason: "first",
        },
        {
          file: "src/example.ts",
          decision: "decision-1",
          condition: "right",
          reason: "second",
        },
      ],
    });
    expect(
      evaluation.applied.filter((match) => match.conditionIndex === 1),
    ).toHaveLength(1);
    expect(evaluation.waivedByDecision.get("decision-1")?.get(1)?.reason).toBe(
      "first",
    );
  });
});
