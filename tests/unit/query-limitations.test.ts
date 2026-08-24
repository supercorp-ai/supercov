import { describe, it } from "node:test";
import { createMcdcReport } from "../../src/analyze.ts";
import {
  coverageMeasurement,
  fileGaps,
} from "../../src/query.ts";
import type { CoverageManifest, McdcRawTestResult } from "../../src/types.ts";
import { expect } from "../support/expect.ts";

function limitedReport() {
  const manifest: CoverageManifest = {
    decisions: [],
    branches: [],
    points: [{
      id: "covered",
      kind: "statement",
      file: "src/covered.ts",
      line: 1,
      column: 1,
      source: "covered();",
    }],
    limitations: [
      {
        id: "dynamic",
        kind: "dynamic-code",
        file: "src/dynamic.ts",
        line: 4,
        column: 3,
        source: "eval(source)",
        reason: "Runtime-generated source cannot be instrumented statically.",
      },
      {
        id: "scope",
        kind: "source-scope",
        file: "src/ambiguous.ts",
        line: 1,
        column: 1,
        source: "src/ambiguous.ts",
        reason: "First-party source could not be classified automatically.",
      },
    ],
  };
  const evidence: McdcRawTestResult = {
    testId: "test",
    test: "test",
    status: "passed",
    runtime: [{ decisions: [], hits: ["covered"] }],
    browser: [],
    server: [],
  };
  return createMcdcReport(manifest, [evidence]);
}

describe("coverage measurement limitations", () => {
  it("keeps denominator blockers separate from exercised obligations", () => {
    const report = limitedReport();
    expect(report.summary.lines.percentage).toBe(100);
    expect(coverageMeasurement(report)).toEqual({
      complete: false,
      limitations: 2,
      blocking: 2,
      files: 2,
      byKind: {
        "dynamic-code": 1,
        "semantic-safety": 0,
        "source-scope": 1,
      },
    });
  });

  it("places limitation-only files in the existing files and gaps inventory", () => {
    const gaps = fileGaps(limitedReport());
    expect(gaps.map((gap) => gap.file)).toContain("src/dynamic.ts");
    expect(
      gaps.find((gap) => gap.file === "src/dynamic.ts"),
    ).toMatchObject({
      uncoveredLines: 0,
      missingBranches: 0,
      missingMcdcConditions: 0,
      measurementLimitations: 1,
      limitationKinds: ["dynamic-code"],
    });
  });
});
