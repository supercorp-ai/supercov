import { readFileSync, rmSync } from "node:fs";
import { describe, expect, it } from "vitest";
import {
  coverageHit,
  mcdcBegin,
  mcdcCondition,
  mcdcEnd,
  withRequestPhase,
} from "../../src/runtime";
import {
  COVERAGE_PHASE_HEADER,
  COVERAGE_SCOPE_HEADER,
  decodeCoverageScope,
  encodeCoverageScope,
  serverEvidencePath,
  serverRunEvidenceDirectory,
  backgroundEvidencePath,
} from "../../src/transport";
import type {
  CoverageExecutionScope,
  CoverageServerRecord,
  McdcDecisionMeta,
} from "../../src/types";

function scope(
  runId: string,
  workerId: string,
  testId: string,
  retry: number,
): CoverageExecutionScope {
  const key = Buffer.from(testId).toString("hex").slice(0, 24);
  return {
    version: 1,
    runId,
    workerId,
    testId,
    testKey: key,
    retry,
    attemptId: `${key}-${retry}`,
  };
}

function records(execution: CoverageExecutionScope): CoverageServerRecord[] {
  return readFileSync(serverEvidencePath(execution), "utf8")
    .trim()
    .split("\n")
    .map((line) => JSON.parse(line) as CoverageServerRecord);
}

describe("concurrent server evidence transport", () => {
  it("round-trips scoped headers and rejects malformed filesystem keys", () => {
    const execution = scope("run with spaces", "worker/1", "test > one", 2);
    expect(decodeCoverageScope(encodeCoverageScope(execution))).toEqual(
      execution,
    );
    expect(
      decodeCoverageScope(
        encodeCoverageScope({ ...execution, testKey: "../escape" }),
      ),
    ).toBeUndefined();
    expect(serverEvidencePath(execution)).toContain(
      "/run_with_spaces/worker_1/",
    );
  });

  it("keeps interleaved async requests in separate run/worker/test/retry files", async () => {
    const runId = `transport-${process.pid}-${Date.now()}`;
    const first = scope(runId, "worker-1", "test-a", 0);
    const second = scope(runId, "worker-2", "test-b", 1);
    const meta: McdcDecisionMeta = {
      id: "shared-decision",
      file: "app/example.ts",
      line: 1,
      column: 1,
      source: "enabled && ready",
      conditions: ["enabled", "ready"],
      kind: "if",
    };
    const handler = withRequestPhase(
      async (
        input: { request: { headers: Headers } },
        execution: CoverageExecutionScope,
        pauseMs: number,
      ) => {
        const frame = mcdcBegin(meta.id, meta);
        mcdcCondition(frame, 0, true);
        await new Promise((resolve) => setTimeout(resolve, pauseMs));
        coverageHit(`hit-${execution.testId}`);
        mcdcCondition(frame, 1, execution.retry === 0);
        mcdcEnd(frame, execution.retry === 0);
        return input;
      },
    );
    const invoke = (
      execution: CoverageExecutionScope,
      phaseId: string,
      pauseMs: number,
    ) =>
      handler(
        {
          request: {
            headers: new Headers({
              [COVERAGE_SCOPE_HEADER]: encodeCoverageScope(execution),
              [COVERAGE_PHASE_HEADER]: phaseId,
            }),
          },
        },
        execution,
        pauseMs,
      );

    try {
      await Promise.all([
        invoke(first, "phase-a", 20),
        invoke(second, "phase-b", 1),
      ]);

      const firstRecords = records(first);
      const secondRecords = records(second);
      expect(firstRecords).toHaveLength(2);
      expect(secondRecords).toHaveLength(2);
      expect(firstRecords.every((record) => record.scope?.attemptId === first.attemptId)).toBe(true);
      expect(secondRecords.every((record) => record.scope?.attemptId === second.attemptId)).toBe(true);
      expect(firstRecords.map((record) => record.phaseId)).toEqual([
        "phase-a",
        "phase-a",
      ]);
      expect(secondRecords.map((record) => record.phaseId)).toEqual([
        "phase-b",
        "phase-b",
      ]);
      expect(
        firstRecords.find((record) => record.type === "hit"),
      ).toMatchObject({ id: "hit-test-a" });
      expect(
        secondRecords.find((record) => record.type === "hit"),
      ).toMatchObject({ id: "hit-test-b" });
    } finally {
      rmSync(serverRunEvidenceDirectory(runId), {
        recursive: true,
        force: true,
      });
    }
  });

  it("persists evidence with no test scope under the run background scope", () => {
    const runId = `background-${process.pid}-${Date.now()}`;
    const previous = process.env.SUPERCOV_RUN_ID;
    process.env.SUPERCOV_RUN_ID = runId;
    try {
      coverageHit("detached-hit");
      const background = readFileSync(backgroundEvidencePath(runId), "utf8")
        .trim()
        .split("\n")
        .map((line) => JSON.parse(line) as CoverageServerRecord);
      expect(background).toContainEqual(
        expect.objectContaining({ type: "hit", id: "detached-hit" }),
      );
      expect(background[0]?.scope).toBeUndefined();
    } finally {
      if (previous === undefined) delete process.env.SUPERCOV_RUN_ID;
      else process.env.SUPERCOV_RUN_ID = previous;
      rmSync(serverRunEvidenceDirectory(runId), {
        recursive: true,
        force: true,
      });
    }
  });
});
