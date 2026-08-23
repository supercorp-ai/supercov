import { describe, expect, it } from "vitest";
import { createMcdcReport } from "../../src/analyze";
import { instrumentMcdc } from "../../src/instrumenter";
import type {
  CoverageManifest,
  McdcDecisionMeta,
  McdcVector,
} from "../../src/types";

function executeInstrumented(source: string): {
  decide: (left: unknown, right: unknown) => unknown;
  vectors: McdcVector[];
  hits: string[];
  manifest: CoverageManifest;
} {
  const transformed = instrumentMcdc(source, "app/example.ts");
  const executable = transformed.code.replace(/^import[^;]+;\s*/, "");
  const vectors: McdcVector[] = [];
  const hits: string[] = [];

  interface TestFrame {
    values: Array<boolean | null>;
  }
  const begin = (_id: string, meta: McdcDecisionMeta): TestFrame => {
    return {
      values: Array.from({ length: meta.conditions.length }, () => null),
    };
  };
  const condition = <T>(frame: TestFrame, index: number, value: T): T => {
    frame.values[index] = Boolean(value);
    return value;
  };
  const end = <T>(frame: TestFrame, value: T): T => {
    vectors.push({ values: frame.values, outcome: Boolean(value) });
    return value;
  };
  interface SelectionFrame {
    shortId: string;
    rightId: string;
    rightEvaluated: boolean;
  }
  const selectionBegin = (
    shortId: string,
    rightId: string,
  ): SelectionFrame => ({
    shortId,
    rightId,
    rightEvaluated: false,
  });
  const selectionRight = <T>(frame: SelectionFrame, value: T): T => {
    frame.rightEvaluated = true;
    return value;
  };
  const selectionEnd = <T>(frame: SelectionFrame, value: T): T => {
    hits.push(frame.rightEvaluated ? frame.rightId : frame.shortId);
    return value;
  };
  const pendingDefaults = new Map<string, number>();
  const optionalSelect = <T>(shortId: string, continuedId: string, value: T): T => {
    hits.push(value === null || value === undefined ? shortId : continuedId);
    return value;
  };
  const defaultSelected = <T>(id: string, value: T): T => {
    pendingDefaults.set(id, (pendingDefaults.get(id) ?? 0) + 1);
    return value;
  };
  const defaultEntered = (defaultId: string, providedId: string): void => {
    const pending = pendingDefaults.get(defaultId) ?? 0;
    hits.push(pending > 0 ? defaultId : providedId);
    if (pending > 0) pendingDefaults.set(defaultId, pending - 1);
  };
  const tryBegin = (successId: string, catchId: string) => ({ successId, catchId, caught: false });
  const tryCatch = <T>(frame: { caught: boolean }, value: T): T => {
    frame.caught = true;
    return value;
  };
  const tryEnd = (frame: { successId: string; catchId: string; caught: boolean }) =>
    hits.push(frame.caught ? frame.catchId : frame.successId);
  const loopBegin = (zeroId: string, enteredId: string) => ({ zeroId, enteredId, entered: false });
  const loopEntered = (frame: { entered: boolean }) => {
    frame.entered = true;
  };
  const loopEnd = (frame: { zeroId: string; enteredId: string; entered: boolean }) =>
    hits.push(frame.entered ? frame.enteredId : frame.zeroId);

  const decide = new Function(
    "__supercovMcdcBegin",
    "__supercovMcdcCondition",
    "__supercovMcdcEnd",
    "__supercovCoverageHit",
    "__supercovSelectionBegin",
    "__supercovSelectionRight",
    "__supercovSelectionEnd",
    "__supercovOptionalSelect",
    "__supercovDefaultSelected",
    "__supercovDefaultEntered",
    "__supercovTryBegin",
    "__supercovTryCatch",
    "__supercovTryEnd",
    "__supercovLoopBegin",
    "__supercovLoopEntered",
    "__supercovLoopEnd",
    `${executable}\nreturn decide;`,
  )(
    begin,
    condition,
    end,
    (id: string) => hits.push(id),
    selectionBegin,
    selectionRight,
    selectionEnd,
    optionalSelect,
    defaultSelected,
    defaultEntered,
    tryBegin,
    tryCatch,
    tryEnd,
    loopBegin,
    loopEntered,
    loopEnd,
  ) as (left: unknown, right: unknown) => unknown;

  return { decide, vectors, hits, manifest: transformed.manifest };
}

describe("MC/DC instrumenter", () => {
  it("preserves short-circuit behavior and records unevaluated conditions", () => {
    const { decide, vectors } = executeInstrumented(`
      function decide(left, right) {
        if (left && right) return 'yes';
        return 'no';
      }
    `);

    expect(decide(false, true)).toBe("no");
    expect(decide(true, false)).toBe("no");
    expect(decide(true, true)).toBe("yes");
    expect(vectors).toEqual([
      { values: [false, null], outcome: false },
      { values: [true, false], outcome: false },
      { values: [true, true], outcome: true },
    ]);
  });

  it("records an atomic negation rather than its inner operand", () => {
    const { decide, vectors } = executeInstrumented(`
      function decide(value, expected) {
        if (!value || value === expected) return 'yes';
        return 'no';
      }
    `);

    expect(decide(false, true)).toBe("yes");
    expect(decide(true, false)).toBe("no");
    expect(vectors).toEqual([
      { values: [true, null], outcome: true },
      { values: [false, false], outcome: false },
    ]);
  });

  it("records single-condition decisions and value-selection alternatives", () => {
    const control = executeInstrumented(`
      function decide(left) {
        if (left) return 'yes';
        return 'no';
      }
    `);
    expect(control.decide(false, undefined)).toBe("no");
    expect(control.decide(true, undefined)).toBe("yes");
    expect(control.manifest.decisions).toHaveLength(1);
    expect(control.vectors).toEqual([
      { values: [false], outcome: false },
      { values: [true], outcome: true },
    ]);

    const valueSelection = executeInstrumented(`
      function decide(left, right) {
        return left || right;
      }
    `);
    expect(valueSelection.decide("left", "right")).toBe("left");
    expect(valueSelection.decide("", "right")).toBe("right");
    const branch = valueSelection.manifest.branches[0];
    expect(branch?.kind).toBe("logical-value");
    expect(valueSelection.hits).toEqual(
      expect.arrayContaining([
        branch!.alternatives[0]!.id,
        branch!.alternatives[1]!.id,
      ]),
    );
  });

  it("measures optional links, logical assignments, and parameter defaults", () => {
    const optional = executeInstrumented(`
      function decide(left) {
        return left?.value;
      }
    `);
    expect(optional.decide(null, undefined)).toBeUndefined();
    expect(optional.decide({ value: 3 }, undefined)).toBe(3);
    expect(optional.manifest.branches[0]?.kind).toBe("optional-chain");
    expect(optional.hits).toEqual(
      expect.arrayContaining(
        optional.manifest.branches[0]!.alternatives.map((item) => item.id),
      ),
    );

    const optionalMethod = executeInstrumented(`
      function decide(left) {
        return left.method?.();
      }
    `);
    expect(
      optionalMethod.decide(
        {
          value: 4,
          method(this: { value: number }) {
            return this.value;
          },
        },
        undefined,
      ),
    ).toBe(4);
    expect(optionalMethod.decide({ method: undefined }, undefined)).toBeUndefined();
    expect(optionalMethod.manifest.limitations).toEqual(
      expect.arrayContaining([
        expect.objectContaining({ kind: "semantic-safety" }),
      ]),
    );

    const assignment = executeInstrumented(`
      function decide(left, right) {
        left.value ||= right;
        return left.value;
      }
    `);
    expect(assignment.decide({ value: "kept" }, "new")).toBe("kept");
    expect(assignment.decide({ value: "" }, "new")).toBe("new");
    expect(assignment.manifest.branches.some((branch) => branch.kind === "logical-assignment")).toBe(true);

    const defaults = executeInstrumented(`
      function decide(left = "fallback") {
        return left;
      }
    `);
    expect(defaults.decide(undefined, undefined)).toBe("fallback");
    expect(defaults.decide("given", undefined)).toBe("given");
    const defaultBranch = defaults.manifest.branches.find((branch) => branch.kind === "default-value");
    expect(defaults.hits).toEqual(
      expect.arrayContaining(defaultBranch!.alternatives.map((item) => item.id)),
    );

    const destructuring = executeInstrumented(`
      function decide(left) {
        const { value = "fallback" } = left;
        return value;
      }
    `);
    expect(destructuring.decide({}, undefined)).toBe("fallback");
    expect(destructuring.decide({ value: "given" }, undefined)).toBe("given");
    const destructuringBranch = destructuring.manifest.branches.find(
      (branch) => branch.kind === "default-value",
    );
    expect(destructuring.hits).toEqual(
      expect.arrayContaining(
        destructuringBranch!.alternatives.map((item) => item.id),
      ),
    );
  });

  it("makes source-reflection safety boundaries explicit", () => {
    const transformed = instrumentMcdc(
      `const rendered = String(function () { return 1; });`,
      "app/source-reflection.ts",
    );
    expect(transformed.manifest.limitations).toEqual([
      expect.objectContaining({
        kind: "semantic-safety",
        reason: expect.stringContaining("Function source text"),
      }),
    ]);
    expect(transformed.code).toMatch(
      /String\(function \(\) \{\s*return 1;\s*\}\)/,
    );

    const invoked = instrumentMcdc(
      `const value = (function () { return 1; })() + 2;`,
      "app/iife.ts",
    );
    expect(invoked.manifest.limitations ?? []).toHaveLength(0);
    expect(
      invoked.manifest.points.some((point) => point.kind === "function"),
    ).toBe(true);

    const dynamicEnvironment = instrumentMcdc(
      `with (scope) { if (left && right) value = 1; }`,
      "app/sloppy-script.js",
    );
    expect(dynamicEnvironment.manifest.limitations).toEqual(
      expect.arrayContaining([
        expect.objectContaining({
          kind: "semantic-safety",
          reason: expect.stringContaining("with-statement"),
        }),
      ]),
    );
    expect(dynamicEnvironment.manifest.decisions).toHaveLength(0);
  });

  it("measures try/catch, zero/entered enumeration, and switch no-match", () => {
    const guarded = executeInstrumented(`
      function decide(left) {
        try {
          if (left) throw new Error("boom");
          return "ok";
        } catch {
          return "caught";
        }
      }
    `);
    expect(guarded.decide(false, undefined)).toBe("ok");
    expect(guarded.decide(true, undefined)).toBe("caught");
    const tryBranch = guarded.manifest.branches.find((branch) => branch.kind === "try-catch");
    expect(guarded.hits).toEqual(
      expect.arrayContaining(tryBranch!.alternatives.map((item) => item.id)),
    );

    const loop = executeInstrumented(`
      function decide(left) {
        let count = 0;
        for (const value of left) count += value;
        return count;
      }
    `);
    expect(loop.decide([], undefined)).toBe(0);
    expect(loop.decide([2, 3], undefined)).toBe(5);
    const loopBranch = loop.manifest.branches.find((branch) => branch.kind === "for-of");
    expect(loop.hits).toEqual(
      expect.arrayContaining(loopBranch!.alternatives.map((item) => item.id)),
    );

    const switched = instrumentMcdc(
      `function decide(value) { switch (value) { case 1: return "one"; } }`,
      "app/switch.ts",
    );
    expect(switched.manifest.branches[0]?.alternatives.at(-1)?.label).toBe(
      "no matching case",
    );
  });

  it("blocks completeness when dynamic source is discovered", () => {
    const transformed = instrumentMcdc(
      `function decide(source) { return eval(source); }`,
      "app/dynamic.ts",
    );
    expect(transformed.manifest.limitations).toMatchObject([
      { kind: "dynamic-code", file: "app/dynamic.ts" },
    ]);
    const report = createMcdcReport(transformed.manifest, []);
    expect(report.summary).toMatchObject({
      coverageComplete: false,
      completenessBlocked: true,
    });
  });

  it("declares loop frames before async-generator predicates use them", () => {
    const transformed = instrumentMcdc(
      `async function* values(reader) {
        while (true) {
          const item = await reader.read();
          if (item.done) break;
          yield item.value;
        }
      }`,
      "app/generator.ts",
    ).code;

    const functionStart = transformed.indexOf("async function* values");
    const loopStart = transformed.indexOf("while (", functionStart);
    const frameDeclaration = transformed.indexOf(
      "let _supercovMcdcFrame",
      functionStart,
    );
    expect(frameDeclaration).toBeGreaterThan(functionStart);
    expect(frameDeclaration).toBeLessThan(loopStart);
  });

  it("automatically wraps Remix loaders, actions, and re-exports for request tracing", () => {
    const variable = instrumentMcdc(
      `export const loader = async ({ request }) => request.url;`,
      "app/routes/example.ts",
    ).code;
    expect(variable).toContain(
      "loader = __supercovWithRequestPhase(async ({ request })",
    );
    expect(variable).toContain(
      "withRequestPhase as __supercovWithRequestPhase",
    );

    const declaration = instrumentMcdc(
      `export async function action({ request }) { return request.method; }`,
      "app/routes/example.ts",
    ).code;
    expect(declaration).toContain(
      "const action = __supercovWithRequestPhase(",
    );

    const reexport = instrumentMcdc(
      `export { generateAction as action } from './generateAction';`,
      "app/routes/example.ts",
    ).code;
    expect(reexport).toContain("from './generateAction'");
    expect(reexport).toContain("const action = __supercovWithRequestPhase(");

    const serverEntry = instrumentMcdc(
      `export default async function handleRequest(request) { return request.url; }`,
      "app/entry.server.tsx",
    ).code;
    expect(serverEntry).toContain(
      "export default __supercovWithRequestPhase(",
    );

    const websocket = instrumentMcdc(
      `server.on("upgrade", handleUpgrade); wss.on("connection", (socket, request) => socket.send(request.url));`,
      "app/websocket.server.ts",
    ).code;
    expect(websocket).toContain(
      `server.on("upgrade", __supercovWithRequestPhase(handleUpgrade))`,
    );
    expect(websocket).toContain(
      `wss.on("connection", __supercovWithRequestPhase((socket, request)`,
    );
  });

  it("wraps Next route handlers for request-context attribution", () => {
    const transformed = instrumentMcdc(
      `export function GET(request) { return Response.json({ url: request.url }); }`,
      "app/api/items/route.ts",
    ).code;
    expect(transformed).toContain("const GET = __supercovWithRequestPhase(");
    expect(transformed).toContain("withRequestPhase as __supercovWithRequestPhase");
  });

  it("reports masking MC/DC witnesses for every independently effective condition", () => {
    const meta: McdcDecisionMeta = {
      id: "decision",
      file: "app/example.ts",
      line: 1,
      column: 1,
      source: "left && right",
      conditions: ["left", "right"],
      kind: "if",
    };
    const report = createMcdcReport(
      { decisions: [meta], points: [], branches: [] },
      [
        {
          testId: "example-test",
          test: "example",
          testFile: "tests/example.spec.ts",
          title: "example",
          retry: 0,
          browser: [
            {
              decisions: [
                {
                  meta,
                  vectors: [
                    { values: [false, null], outcome: false },
                    { values: [true, false], outcome: false },
                    { values: [true, true], outcome: true },
                  ],
                },
              ],
              hits: [],
            },
          ],
          server: [],
        },
      ],
    );

    expect(report.summary).toMatchObject({
      decisions: 1,
      coveredDecisions: 1,
      conditions: 2,
      coveredConditions: 2,
      conditionCoveragePct: 100,
    });
    expect(report.decisions[0]?.vectorObservations).toMatchObject([
      {
        vector: { values: [false, null], outcome: false },
        tests: ["example-test"],
      },
      {
        vector: { values: [true, false], outcome: false },
        tests: ["example-test"],
      },
      {
        vector: { values: [true, true], outcome: true },
        tests: ["example-test"],
      },
    ]);
    expect(report.decisions[0]?.conditions[0]?.witnessTests).toEqual([
      ["example-test"],
      ["example-test"],
    ]);
    expect(report.tests).toMatchObject([
      {
        id: "example-test",
        file: "tests/example.spec.ts",
        title: "example",
        retries: [0],
      },
    ]);
    expect(report.testFiles).toEqual([
      {
        file: "tests/example.spec.ts",
        tests: ["example-test"],
        runners: ["unknown"],
        kinds: ["unknown"],
        lines: [],
      },
    ]);
  });

  it("keeps exact vector provenance when MC/DC needs two tests", () => {
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
      vector: McdcVector,
    ): Parameters<typeof createMcdcReport>[1][number] => ({
      testId,
      test: testId,
      browser: [
        {
          decisions: [{ meta, vectors: [vector] }],
          hits: [],
        },
      ],
      server: [],
    });
    const report = createMcdcReport(
      { decisions: [meta], points: [], branches: [] },
      [
        raw("false-test", { values: [false, null], outcome: false }),
        raw("true-test", { values: [true, true], outcome: true }),
      ],
    );

    expect(report.decisions[0]?.conditions[0]?.witnessTests).toEqual([
      ["false-test"],
      ["true-test"],
    ]);
    expect(report.decisions[0]?.conditions[1]?.covered).toBe(false);
  });

  it("attributes browser and server coverage events to action/assertion phases", () => {
    const meta: McdcDecisionMeta = {
      id: "decision",
      file: "app/example.ts",
      line: 2,
      column: 1,
      source: "ready",
      conditions: ["ready"],
      kind: "if",
    };
    const point = {
      id: "statement",
      kind: "statement" as const,
      file: "app/example.ts",
      line: 3,
      column: 1,
      source: "save();",
    };
    const report = createMcdcReport(
      { decisions: [meta], points: [point], branches: [] },
      [
        {
          testId: "save-test",
          test: "save test",
          phases: [
            {
              id: "click",
              kind: "action",
              operation: "Locator.click",
              startedAtMs: 100,
              endedAtMs: 120,
              status: "passed",
            },
            {
              id: "assert",
              kind: "assertion",
              operation: "expect.toBeVisible",
              causedByPhaseId: "click",
              startedAtMs: 200,
              endedAtMs: 220,
              status: "passed",
            },
          ],
          browser: [
            {
              decisions: [
                {
                  meta,
                  vectors: [{ values: [true], outcome: true }],
                },
              ],
              hits: [point.id],
              events: [
                {
                  type: "hit",
                  id: point.id,
                  timestampMs: 110,
                  phaseId: "click",
                  environment: "browser",
                },
                {
                  type: "decision",
                  id: meta.id,
                  vector: { values: [true], outcome: true },
                  timestampMs: 210,
                  phaseId: "assert",
                  environment: "browser",
                },
              ],
            },
          ],
          server: [
            {
              type: "hit",
              id: point.id,
              timestampMs: 215,
            },
          ],
        },
      ],
    );

    expect(report.points[0]?.phases).toEqual(["assert", "click"]);
    expect(report.points[0]?.confidence).toMatchObject({
      level: "asserted",
      asserted: true,
    });
    expect(report.decisions[0]?.vectorObservations[0]?.phases).toEqual([
      "assert",
    ]);
    expect(report.decisions[0]?.vectorObservations[0]?.confidence).toMatchObject({
      level: "asserted",
    });
    expect(report.phases).toMatchObject([
      {
        id: "click",
        kind: "action",
        lines: [{ file: "app/example.ts", line: 3 }],
        browserEvents: 1,
        serverEvents: 0,
        explicitEvents: 1,
        inferredEvents: 0,
      },
      {
        id: "assert",
        kind: "assertion",
        lines: [{ file: "app/example.ts", line: 3 }],
        browserEvents: 1,
        serverEvents: 1,
        explicitEvents: 1,
        inferredEvents: 1,
      },
    ]);
  });

  it("does not promote timestamp-fallback evidence to asserted confidence", () => {
    const point = {
      id: "fallback-only",
      kind: "statement" as const,
      file: "app/example.ts",
      line: 1,
      column: 1,
      source: "background();",
    };
    const report = createMcdcReport(
      { decisions: [], points: [point], branches: [] },
      [
        {
          testId: "test",
          test: "test",
          phases: [
            {
              id: "assert",
              kind: "assertion",
              operation: "expect.toBeVisible",
              startedAtMs: 100,
              endedAtMs: 110,
              status: "passed",
            },
          ],
          browser: [],
          server: [
            { type: "hit", id: point.id, timestampMs: 105 },
          ],
        },
      ],
    );
    expect(report.points[0]?.confidence).toMatchObject({
      level: "executed",
      asserted: false,
    });
  });
});
