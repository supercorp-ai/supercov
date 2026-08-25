import { describe, it } from "node:test";
import { createMcdcReport } from "../../src/analyze.ts";
import {
  coverageDiagnostics,
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
      evidenceCorruptions: 0,
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

  it("blocks completeness when archived transport evidence is corrupt", () => {
    const report = limitedReport();
    report.limitations = [];
    report.transport = {
      processes: 1,
      childLaunches: 0,
      remoteLaunches: 0,
      workspaceCapabilities: 0,
      scopedServerRecords: 10,
      backgroundServerRecords: 4,
      corruptRecords: 2,
      corruptFiles: 1,
    };
    expect(coverageMeasurement(report)).toEqual({
      complete: false,
      limitations: 0,
      evidenceCorruptions: 2,
      blocking: 2,
      files: 1,
      byKind: {
        "dynamic-code": 0,
        "semantic-safety": 0,
        "source-scope": 0,
      },
    });
  });

  it("flags a test whose phases arrived without any coverage evidence", () => {
    const manifest = {
      decisions: [],
      branches: [],
      points: [
        {
          id: "point",
          kind: "statement" as const,
          file: "src/app.ts",
          line: 1,
          column: 1,
          source: "run();",
        },
      ],
    };
    const phases = [
      {
        id: "phase-1",
        kind: "assertion" as const,
        operation: "expect.toBe",
        startedAtMs: 1,
        status: "passed" as const,
      },
    ];
    const empty = createMcdcReport(manifest, [
      {
        testId: "lost-test",
        test: "loses its evidence",
        retry: 0,
        status: "passed",
        phases,
        runtime: [],
        browser: [],
        server: [],
      },
    ]);
    expect(coverageDiagnostics(empty)).toMatchObject([
      {
        code: "TEST_EVIDENCE_MISSING",
        severity: "warning",
        message: expect.stringContaining("static or uninstrumented data"),
      },
    ]);

    const healthy = createMcdcReport(manifest, [
      {
        testId: "healthy-test",
        test: "keeps its evidence",
        retry: 0,
        status: "passed",
        phases,
        runtime: [{ decisions: [], hits: ["point"] }],
        browser: [],
        server: [],
      },
    ]);
    expect(coverageDiagnostics(healthy)).toEqual([]);
  });
});
