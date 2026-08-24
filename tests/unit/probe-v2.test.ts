import { mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { resolve } from "node:path";
import { describe, it } from "node:test";
import { expect } from "../support/expect.ts";
import {
  beginBufferedServerEvidence,
  coverageSnapshot,
  coverageHitV2,
  decodeProbeV2Vector,
  flushBufferedServerEvidence,
  mcdcEndV2,
  registerProbeV2,
  resetCoverage,
  withCoverageCarrier,
} from "../../src/runtime.ts";
import { instrumentMcdc } from "../../src/instrumenter.ts";
import type { CoverageExecutionScope, McdcDecisionMeta } from "../../src/types.ts";

const decision: McdcDecisionMeta = {
  id: "decision-v2",
  file: "app/probe-v2.ts",
  line: 1,
  column: 1,
  source: "left && right",
  conditions: ["left", "right"],
  kind: "if",
};

function scope(attemptId: string): CoverageExecutionScope {
  return {
    version: 1,
    runId: "probe-v2-run",
    workerId: "worker-0",
    testId: attemptId,
    testKey: attemptId,
    retry: 0,
    attemptId,
  };
}

describe("probe v2", () => {
  it("matches every frozen encoding vector", () => {
    const fixtures = JSON.parse(
      readFileSync(resolve("contracts/probe-v2/vectors.json"), "utf8"),
    ) as Array<{
      conditions: number;
      encoded: number;
      outcome: boolean;
      vector: { values: Array<boolean | null>; outcome: boolean };
    }>;
    for (const fixture of fixtures)
      expect(
        decodeProbeV2Vector(
          fixture.conditions,
          fixture.encoded,
          fixture.outcome,
        ),
      ).toStrictEqual(fixture.vector);
  });

  it("round-trips unreached, false, true, and the outcome exactly", () => {
    // digits [true, false, unreached] => 2*3^0 + 1*3^1 + 0*3^2
    expect(decodeProbeV2Vector(3, 5, false)).toStrictEqual({
      values: [true, false, null],
      outcome: false,
    });
    expect(decodeProbeV2Vector(33, 0, true)).toBeUndefined();
    expect(decodeProbeV2Vector(2, Number.MAX_SAFE_INTEGER, true)).toBeUndefined();
  });

  it("falls back to exact v1 frames beyond the numeric encoding cap", () => {
    const expression = Array.from({ length: 33 }, (_, index) => `v${index}`)
      .join(" && ");
    const transformed = instrumentMcdc(
      `function decide(${Array.from({ length: 33 }, (_, index) => `v${index}`).join(",")}) { if (${expression}) return 1; return 0; }`,
      "app/wide.ts",
      { probeVersion: 2 },
    );
    expect(transformed.code).toContain("mcdcBegin as");
    expect(transformed.code).toContain("mcdcCondition as");
  });

  it("saturates a dense decision only after every reachable vector in the epoch", () => {
    const transformed = instrumentMcdc(
      "function decide(a,b,c) { if ((a && b) || c) return 1; return 0; }",
      "app/saturated.ts",
      { probeVersion: 2 },
    );
    expect(transformed.code).toContain("decisionVectorCounts: [5]");

    resetCoverage();
    const file = registerProbeV2({
      decisions: [{
        ...decision,
        source: "(a && b) || c",
        conditions: ["a", "b", "c"],
      }],
      pointIds: [],
      decisionVectorCounts: [5],
    });
    const epoch = file.clock.epoch;
    for (const [encoded, outcome] of [
      [10, false],
      [19, true],
      [14, false],
      [23, true],
      [8, true],
    ] as const)
      mcdcEndV2(file, 0, encoded, outcome);
    expect(file.decisionCompleteEpochs[0]).toStrictEqual(epoch);

    resetCoverage();
    expect(file.decisionCompleteEpochs[0] === file.clock.epoch).toStrictEqual(false);
  });

  it("re-registers decision state lazily after a per-test reset", () => {
    resetCoverage();
    const file = registerProbeV2({ decisions: [decision], pointIds: [] });
    mcdcEndV2(file, 0, 8, true);
    expect(coverageSnapshot().decisions).toHaveLength(1);
    resetCoverage();
    mcdcEndV2(file, 0, 8, true);
    expect(coverageSnapshot().decisions).toHaveLength(1);
  });

  it("keeps interleaved async attempts separate and de-duplicates within each epoch", async () => {
    const root = mkdtempSync(resolve(tmpdir(), "supercov-probe-v2-"));
    const previous = process.env["SUPERCOV_SERVER_EVIDENCE_ROOT"];
    process.env["SUPERCOV_SERVER_EVIDENCE_ROOT"] = root;
    resetCoverage();
    const file = registerProbeV2({
      decisions: [decision],
      pointIds: ["point-v2"],
    });
    const first = scope("attempt-first");
    const second = scope("attempt-second");
    beginBufferedServerEvidence(first);
    beginBufferedServerEvidence(second);
    try {
      await Promise.all(
        [first, second].map((execution, index) =>
          withCoverageCarrier({ version: 1, scope: execution }, async () => {
            coverageHitV2(file, 0);
            await new Promise<void>((resolvePromise) => setImmediate(resolvePromise));
            coverageHitV2(file, 0);
            mcdcEndV2(file, 0, index === 0 ? 1 : 8, index !== 0);
            mcdcEndV2(file, 0, index === 0 ? 1 : 8, index !== 0);
          }),
        ),
      );
      const firstPath = flushBufferedServerEvidence(first)!;
      const secondPath = flushBufferedServerEvidence(second)!;
      const firstRecords = readFileSync(firstPath, "utf8").trim().split("\n");
      const secondRecords = readFileSync(secondPath, "utf8").trim().split("\n");
      expect(firstRecords).toHaveLength(2);
      expect(secondRecords).toHaveLength(2);
      expect(firstRecords.join("\n")).toContain('"attemptId":"attempt-first"');
      expect(secondRecords.join("\n")).toContain('"attemptId":"attempt-second"');
      expect(firstRecords.join("\n")).toContain('"values":[false,null]');
      expect(secondRecords.join("\n")).toContain('"values":[true,true]');
    } finally {
      if (previous === undefined)
        delete process.env["SUPERCOV_SERVER_EVIDENCE_ROOT"];
      else process.env["SUPERCOV_SERVER_EVIDENCE_ROOT"] = previous;
      rmSync(root, { recursive: true, force: true });
    }
  });
});
