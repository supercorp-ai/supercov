import { describe, it } from "node:test";
import { expect } from "../support/expect.ts";
import { createMcdcReport } from "../../src/analyze.ts";
import { inferTestProvenance } from "../../src/provenance.ts";
import type {
  McdcDecisionMeta,
  McdcVector,
} from "../../src/types.ts";

describe("coverage provenance", () => {
  it("uses explicit, project, path, and runner-default classifications in order", () => {
    expect(
      inferTestProvenance({
        runner: "playwright",
        file: "tests/e2e/example.spec.ts",
        explicitKind: "journey",
      }),
    ).toMatchObject({ kind: "journey", source: "explicit" });
    expect(
      inferTestProvenance({
        runner: "playwright",
        project: "component-chrome",
      }),
    ).toMatchObject({ kind: "component", source: "project" });
    expect(
      inferTestProvenance({
        runner: "vitest",
        file: "tests/integration/db.test.ts",
      }),
    ).toMatchObject({ kind: "integration", source: "path" });
    expect(inferTestProvenance({ runner: "playwright" })).toMatchObject({
      kind: "e2e",
      source: "runner-default",
    });
  });

  it("recomputes MC/DC independently for every test kind", () => {
    const meta: McdcDecisionMeta = {
      id: "decision",
      file: "app/example.ts",
      line: 1,
      column: 1,
      source: "left && right",
      conditions: ["left", "right"],
      kind: "if",
    };
    const raw = (
      testId: string,
      kind: string,
      vectors: McdcVector[],
    ): Parameters<typeof createMcdcReport>[1][number] => ({
      testId,
      test: testId,
      testFile: `tests/${kind}/${testId}.spec.ts`,
      provenance: {
        runner: kind === "e2e" ? "playwright" : "vitest",
        kind,
        source: "path",
      },
      browser: [{ decisions: [{ meta, vectors }], hits: [] }],
      server: [],
    });
    const report = createMcdcReport(
      { decisions: [meta], points: [], branches: [] },
      [
        raw("unit-complete", "unit", [
          { values: [false, null], outcome: false },
          { values: [true, false], outcome: false },
          { values: [true, true], outcome: true },
        ]),
        raw("e2e-partial", "e2e", [
          { values: [false, null], outcome: false },
          { values: [true, true], outcome: true },
        ]),
      ],
    );

    expect(report.coverageByKind).toMatchObject([
      {
        kind: "e2e",
        tests: 1,
        summary: { coveredConditions: 1, conditionCoveragePct: 50 },
      },
      {
        kind: "unit",
        tests: 1,
        summary: { coveredConditions: 2, conditionCoveragePct: 100 },
      },
    ]);
  });
});
