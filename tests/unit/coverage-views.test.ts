import { describe, it } from "node:test";
import { expect } from "../support/expect.ts";
import { createMcdcReport } from "../../src/analyze.ts";
import {
  failedCoverageResults,
  passingCoverageResults,
} from "../../src/reporter.ts";
import type {
  CoverageManifest,
  McdcRawTestResult,
} from "../../src/types.ts";

const manifest: CoverageManifest = {
  decisions: [],
  branches: [],
  points: [
    {
      id: "failed-line",
      kind: "statement",
      file: "app/example.ts",
      line: 1,
      column: 1,
      source: "failedPath();",
    },
    {
      id: "passed-line",
      kind: "statement",
      file: "app/example.ts",
      line: 2,
      column: 1,
      source: "passedPath();",
    },
  ],
};

function evidence(
  testId: string,
  retry: number,
  status: McdcRawTestResult["status"],
  hits: string[],
): McdcRawTestResult {
  return {
    testId,
    test: testId,
    retry,
    status,
    runtime: [{ decisions: [], hits }],
    browser: [],
    server: [],
  };
}

describe("coverage outcome views", () => {
  it("keeps all attempts observed but only a flaky test's passing attempt verified", () => {
    const raw = [
      evidence("flaky-test", 0, "failed", ["failed-line"]),
      evidence("flaky-test", 1, "passed", ["passed-line"]),
      evidence("failed-test", 0, "failed", ["failed-line"]),
      evidence("skipped-test", 0, "skipped", []),
    ];

    const observed = createMcdcReport(manifest, raw);
    const passed = createMcdcReport(manifest, passingCoverageResults(raw));
    const failed = createMcdcReport(manifest, failedCoverageResults(raw));

    expect(observed.summary.lines).toMatchObject({ covered: 2, total: 2 });
    expect(
      observed.tests.map((test) => [test.id, test.outcome]),
    ).toEqual([
      ["failed-test", "failed"],
      ["flaky-test", "flaky"],
      ["skipped-test", "skipped"],
    ]);
    expect(passed.summary.lines).toMatchObject({ covered: 1, total: 2 });
    expect(passed.lines.find((line) => line.line === 1)?.covered).toBe(false);
    expect(passed.tests).toMatchObject([
      {
        id: "flaky-test",
        outcome: "passed",
        attempts: [{ retry: 1, status: "passed" }],
      },
    ]);
    expect(failed.summary.lines).toMatchObject({ covered: 1, total: 2 });
    expect(failed.lines.find((line) => line.line === 1)?.covered).toBe(true);
    expect(failed.lines.find((line) => line.line === 2)?.covered).toBe(false);
    expect(failed.tests.map((test) => test.id)).toEqual([
      "failed-test",
      "flaky-test",
    ]);
  });

  it("joins a runner status record to coverage from the same attempt", () => {
    const raw = [
      { ...evidence("test", 0, "unknown", ["passed-line"]) },
      evidence("test", 0, "passed", []),
    ];

    expect(passingCoverageResults(raw)).toHaveLength(2);
    expect(
      createMcdcReport(manifest, passingCoverageResults(raw)).lines.find(
        (line) => line.line === 2,
      )?.covered,
    ).toBe(true);
  });

  it("does not verify expected-failure tests", () => {
    const raw = [
      {
        ...evidence("expected-failure", 0, "passed", ["passed-line"]),
        expectedStatus: "failed" as const,
      },
    ];
    expect(passingCoverageResults(raw)).toEqual([]);
  });
});
