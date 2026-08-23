import { describe, expect, it } from "vitest";
import { createMcdcReport } from "../../src/analyze";
import type { CoverageBranchKind } from "../../src/types";
import { executeDifferential } from "./instrumenter-harness";
import clangGolden from "../fixtures/clang-mcdc/oracle.json";

function alternatives(
  result: Awaited<ReturnType<typeof executeDifferential>>,
  kind: CoverageBranchKind,
): string[] {
  return result.evidence.manifest.branches
    .filter((branch) => branch.kind === kind)
    .flatMap((branch) => branch.alternatives.map((item) => item.id));
}

describe("instrumenter coverage oracles", () => {
  it("records the exact short-circuit vectors for a three-condition decision", async () => {
    const calls = clangGolden.inputs
      .map((values) => `decide(${values.join(", ")})`)
      .join(",\n");
    const result = await executeDifferential(`
      const effects = [];
      function decide(a, b, c) {
        if (${clangGolden.expression}) return "yes";
        return "no";
      }
      function run() {
        return [
          ${calls}
        ];
      }
      function observe() { return effects; }
    `);

    expect(result.instrumented).toStrictEqual(result.original);
    expect(result.evidence.vectors).toEqual(
      clangGolden.observedVectors.map((values, index) => ({
        values,
        outcome: clangGolden.outcomes[index],
      })),
    );

    const decision = result.evidence.manifest.decisions[0]!;
    const report = createMcdcReport(result.evidence.manifest, [
      {
        testId: "truth-table",
        test: "truth table",
        status: "passed",
        provenance: {
          runner: "vitest",
          kind: "unit",
          source: "explicit",
        },
        runtime: [
          {
            decisions: [{ meta: decision, vectors: result.evidence.vectors }],
            hits: result.evidence.hits,
          },
        ],
        browser: [],
        server: [],
      },
    ]);
    expect(report.summary).toMatchObject({
      decisions: 1,
      conditions: clangGolden.conditions,
      coveredConditions: clangGolden.conditions,
      conditionCoveragePct: 100,
    });

    for (const oracleCase of clangGolden.cases) {
      const vectors = oracleCase.inputIndexes.map(
        (index) => result.evidence.vectors[index]!,
      );
      const subsetReport = createMcdcReport(result.evidence.manifest, [
        {
          testId: oracleCase.name,
          test: oracleCase.name,
          status: "passed",
          provenance: {
            runner: "vitest",
            kind: "unit",
            source: "explicit",
          },
          runtime: [
            {
              decisions: [{ meta: decision, vectors }],
              hits: result.evidence.hits,
            },
          ],
          browser: [],
          server: [],
        },
      ]);
      expect(
        subsetReport.summary.coveredConditions,
        oracleCase.name,
      ).toBe(oracleCase.coveredConditions);
    }
  });

  it("records both alternatives for optional, default, assignment, try, and loops", async () => {
    const result = await executeDifferential(`
      const effects = [];
      function fallback() { return 2; }
      function optional(value) { return value?.item; }
      function defaulted(value = fallback()) { return value; }
      function assigned(value) { const target = { value }; target.value ||= 3; return target.value; }
      function guarded(value) { try { if (value) throw new Error("x"); return 1; } catch { return 2; } }
      function listed(values) { let total = 0; for (const value of values) total += value; return total; }
      function keyed(values) { let total = 0; for (const key in values) total += key.length; return total; }
      function run() {
        return [
          optional(null), optional({ item: 1 }),
          defaulted(), defaulted(4),
          assigned(0), assigned(5),
          guarded(false), guarded(true),
          listed([]), listed([1]),
          keyed({}), keyed({ x: 1 }),
        ];
      }
      function observe() { return effects; }
    `);

    expect(result.instrumented).toStrictEqual(result.original);
    for (const kind of [
      "optional-chain",
      "default-value",
      "logical-assignment",
      "try-catch",
      "for-of",
      "for-in",
    ] as const) {
      const expected = alternatives(result, kind);
      expect(expected, kind).toHaveLength(2);
      expect(result.evidence.hits, kind).toEqual(
        expect.arrayContaining(expected),
      );
    }
  });

  it("does not report implicit switch no-match after a matched fallthrough", async () => {
    const result = await executeDifferential(`
      const effects = [];
      function run() {
        switch (1) {
          case 1:
            effects.push("one");
          case 2:
            effects.push("two");
        }
        return effects.slice();
      }
      function observe() { return effects; }
    `);

    expect(result.instrumented).toStrictEqual(result.original);
    const branch = result.evidence.manifest.branches.find(
      (item) => item.kind === "switch",
    )!;
    const noMatch = branch.alternatives.find(
      (item) => item.label === "no matching case",
    )!;
    expect(result.evidence.hits).not.toContain(noMatch.id);
    expect(result.evidence.hits).toEqual(
      expect.arrayContaining(
        branch.alternatives
          .filter((item) => item.label.startsWith("case"))
          .map((item) => item.id),
      ),
    );
  });

  it("records implicit switch no-match only when no case is entered", async () => {
    const result = await executeDifferential(`
      const effects = [];
      function run() {
        switch (9) {
          case 1: effects.push("one"); break;
          case 2: effects.push("two"); break;
        }
        return effects.slice();
      }
      function observe() { return effects; }
    `);

    expect(result.instrumented).toStrictEqual(result.original);
    const branch = result.evidence.manifest.branches.find(
      (item) => item.kind === "switch",
    )!;
    const noMatch = branch.alternatives.find(
      (item) => item.label === "no matching case",
    )!;
    expect(result.evidence.hits).toContain(noMatch.id);
    expect(
      branch.alternatives
        .filter((item) => item.label.startsWith("case"))
        .some((item) => result.evidence.hits.includes(item.id)),
    ).toBe(false);
  });
});
