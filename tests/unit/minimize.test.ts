import { describe, it } from "node:test";
import { expect } from "../support/expect.ts";
import { createMcdcReport } from "../../src/analyze.ts";
import { minimumTestSet } from "../../src/query.ts";
import type { CoverageManifest, McdcRawTestResult, McdcVector } from "../../src/types.ts";

const manifest: CoverageManifest = {
  points: [],
  branches: [],
  decisions: [
    {
      id: "decision",
      file: "src/permission.js",
      line: 1,
      column: 1,
      source: "admin || owner",
      conditions: ["admin", "owner"],
      kind: "if",
    },
  ],
};

function result(id: string, vector: McdcVector): McdcRawTestResult {
  return {
    testId: id,
    test: id,
    testFile: "tests/permission.test.js",
    status: "passed",
    provenance: { runner: "node:test", kind: "unit", source: "runner-default" },
    runtime: [
      {
        hits: [],
        decisions: [{ meta: manifest.decisions[0]!, vectors: [vector] }],
      },
    ],
    browser: [],
    server: [],
  };
}

describe("exact smallest test-set solver", () => {
  it("recomputes MC/DC witnesses and removes a redundant vector", () => {
    const report = createMcdcReport(manifest, [
      result("admin", { values: [true, null], outcome: true }),
      result("owner", { values: [false, true], outcome: true }),
      result("both", { values: [true, null], outcome: true }),
      result("neither", { values: [false, false], outcome: false }),
    ]);
    const minimized = minimumTestSet(report, 100, "mcdc");
    expect(minimized.optimal).toBe(true);
    expect(minimized.selected).toHaveLength(3);
    expect(minimized.selected).toContain("owner");
    expect(minimized.selected).toContain("neither");
    expect(minimized.summary.conditionCoveragePct).toBe(100);
  });

  it("refuses to call aggregate background evidence a zero-test subset", () => {
    const aggregate = result("aggregate", {
      values: [false, false],
      outcome: false,
    });
    aggregate.role = "background";
    const report = createMcdcReport(manifest, [aggregate]);
    expect(() => minimumTestSet(report, 100, "mcdc")).toThrow(
      "background/unattributed",
    );
  });

  it("bounds an exact search before combinatorial suites can hang an agent", () => {
    const report = createMcdcReport(manifest, [
      result("admin", { values: [true, null], outcome: true }),
      result("owner", { values: [false, true], outcome: true }),
      result("both", { values: [true, null], outcome: true }),
      result("neither", { values: [false, false], outcome: false }),
    ]);
    expect(() => minimumTestSet(report, 100, "mcdc", 1)).toThrow(
      expect.objectContaining({ code: "MINIMIZATION_COMPLEXITY_LIMIT" }),
    );
  });
});
