import {
  existsSync,
  mkdirSync,
  readFileSync,
  readdirSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { resolve } from "node:path";
import { after, before, describe, it } from "node:test";
import { Worker } from "node:worker_threads";
import { expect } from "../support/expect.ts";
import {
  beginBufferedServerEvidence,
  coverageHit,
  flushBufferedBackgroundEvidence,
  flushBufferedServerEvidence,
  mcdcBegin,
  mcdcCondition,
  mcdcEnd,
  writeExclusiveBackgroundRecord,
  withCoverageCarrier,
  withRequestPhase,
} from "../../src/runtime.ts";
import {
  COVERAGE_PHASE_HEADER,
  COVERAGE_PHASE_COOKIE,
  COVERAGE_CARRIER_ENV,
  COVERAGE_SCOPE_COOKIE,
  COVERAGE_SCOPE_HEADER,
  decodeCoverageCarrier,
  decodeCoverageScope,
  encodeCoverageCarrier,
  encodeCoverageScope,
  serverEvidencePath,
  serverRunEvidenceDirectory,
} from "../../src/transport.ts";
import type {
  CoverageCarrier,
  CoverageExecutionScope,
  CoverageServerRecord,
  McdcDecisionMeta,
} from "../../src/types.ts";

const transportRoot = resolve(
  tmpdir(),
  `supercov-transport-test-${process.pid}`,
);
let previousTransportRoot: string | undefined;

before(() => {
  previousTransportRoot = process.env.SUPERCOV_SERVER_EVIDENCE_ROOT;
  process.env.SUPERCOV_SERVER_EVIDENCE_ROOT = transportRoot;
});

after(() => {
  if (previousTransportRoot === undefined)
    delete process.env.SUPERCOV_SERVER_EVIDENCE_ROOT;
  else process.env.SUPERCOV_SERVER_EVIDENCE_ROOT = previousTransportRoot;
  rmSync(transportRoot, { recursive: true, force: true });
});

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
    expect(serverRunEvidenceDirectory(execution.runId)).toBe(
      resolve(transportRoot, "run_with_spaces"),
    );
    expect(decodeCoverageScope(encodeCoverageScope(execution))).toEqual(
      execution,
    );
    expect(
      decodeCoverageScope(
        encodeCoverageScope({ ...execution, testKey: "../escape" }),
      ),
    ).toBeUndefined();
    expect(serverEvidencePath(execution)).toContain(
      `/run_with_spaces/attempts/${execution.attemptId}.jsonl`,
    );
  });

  it("rejects each malformed scope and carrier field independently", () => {
    const valid = scope("run-1", "worker-1", "test one", 0);
    const tampered = (key: string, value: string): string => {
      const params = new URLSearchParams(encodeCoverageScope(valid));
      params.set(key, value);
      return params.toString();
    };

    const configured = process.env.SUPERCOV_SERVER_EVIDENCE_ROOT;
    try {
      delete process.env.SUPERCOV_SERVER_EVIDENCE_ROOT;
      expect(serverRunEvidenceDirectory("run-1")).toBe(
        "/tmp/supercov-server-evidence/run-1",
      );
    } finally {
      process.env.SUPERCOV_SERVER_EVIDENCE_ROOT = configured;
    }

    expect(decodeCoverageScope(undefined)).toBeUndefined();
    expect(decodeCoverageScope(encodeCoverageScope(valid))).toEqual(valid);
    expect(decodeCoverageScope(tampered("v", "2"))).toBeUndefined();
    expect(decodeCoverageScope(tampered("r", ""))).toBeUndefined();
    expect(decodeCoverageScope(tampered("w", ""))).toBeUndefined();
    expect(decodeCoverageScope(tampered("t", ""))).toBeUndefined();
    expect(decodeCoverageScope(tampered("k", ""))).toBeUndefined();
    expect(decodeCoverageScope(tampered("i", ""))).toBeUndefined();
    expect(decodeCoverageScope(tampered("i", "../escape"))).toBeUndefined();
    expect(decodeCoverageScope(tampered("a", "not-a-number"))).toBeUndefined();
    expect(decodeCoverageScope(tampered("a", "0.5"))).toBeUndefined();
    expect(decodeCoverageScope(tampered("a", "-1"))).toBeUndefined();

    expect(decodeCoverageCarrier(undefined)).toBeUndefined();
    expect(decodeCoverageCarrier("!!not-base64-json!!")).toBeUndefined();
    expect(
      decodeCoverageCarrier(
        encodeCoverageCarrier({ version: 2 } as unknown as CoverageCarrier),
      ),
    ).toBeUndefined();
    expect(
      decodeCoverageCarrier(
        encodeCoverageCarrier({
          version: 1,
          scope: { ...valid, testKey: "../escape" },
        }),
      ),
    ).toBeUndefined();
    expect(
      decodeCoverageCarrier(encodeCoverageCarrier({ version: 1, phaseId: "" })),
    ).toBeUndefined();
    expect(
      decodeCoverageCarrier(
        encodeCoverageCarrier({ version: 1, phaseId: "phase-1" }),
      ),
    ).toEqual({ version: 1, phaseId: "phase-1" });
    expect(
      decodeCoverageCarrier(encodeCoverageCarrier({ version: 1, scope: valid })),
    ).toEqual({ version: 1, scope: valid });
  });

  it("keeps interleaved async requests in separate flat attempt files", async () => {
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

  it("recovers WebSocket request context from browser cookies", async () => {
    const execution = scope(
      `websocket-${process.pid}-${Date.now()}`,
      "worker-1",
      "websocket-test",
      0,
    );
    const phase = "phase-websocket";
    const handler = withRequestPhase(async (request: { headers: Headers }) => {
      await Promise.resolve();
      coverageHit("websocket-hit");
      return request;
    });

    try {
      await handler({
        headers: new Headers({
          cookie: `${COVERAGE_SCOPE_COOKIE}=${encodeURIComponent(encodeCoverageScope(execution))}; ${COVERAGE_PHASE_COOKIE}=${encodeURIComponent(phase)}`,
        }),
      });
      expect(records(execution)).toMatchObject([
        {
          type: "hit",
          id: "websocket-hit",
          phaseId: phase,
          scope: { attemptId: execution.attemptId },
        },
      ]);
    } finally {
      rmSync(serverRunEvidenceDirectory(execution.runId), {
        recursive: true,
        force: true,
      });
    }
  });

  it("keeps unscoped health requests out of the launching test", () => {
    const execution = scope(
      `health-boundary-${process.pid}-${Date.now()}`,
      "worker-1",
      "launches-server",
      0,
    );
    const previousCarrier = process.env[COVERAGE_CARRIER_ENV];
    const previousRun = process.env.SUPERCOV_RUN_ID;
    process.env[COVERAGE_CARRIER_ENV] = encodeCoverageCarrier({
      version: 1,
      scope: execution,
    });
    process.env.SUPERCOV_RUN_ID = execution.runId;
    const healthHandler = withRequestPhase(
      (request: { headers: Headers }) => {
        coverageHit("health-request-hit");
        return request;
      },
    );

    try {
      healthHandler({ headers: new Headers({ accept: "*/*" }) });
      flushBufferedBackgroundEvidence(execution.runId);
      expect(existsSync(serverEvidencePath(execution))).toBe(false);
      const directory = resolve(
        serverRunEvidenceDirectory(execution.runId),
        "background",
      );
      const background = readdirSync(directory).flatMap((file) =>
        readFileSync(resolve(directory, file), "utf8")
          .trim()
          .split("\n")
          .map((line) => JSON.parse(line) as CoverageServerRecord),
      );
      expect(background).toContainEqual(
        expect.objectContaining({
          type: "hit",
          id: "health-request-hit",
        }),
      );
      expect(
        background.find(
          (record) =>
            record.type === "hit" && record.id === "health-request-hit",
        )?.scope,
      ).toBeUndefined();
    } finally {
      if (previousCarrier === undefined)
        delete process.env[COVERAGE_CARRIER_ENV];
      else process.env[COVERAGE_CARRIER_ENV] = previousCarrier;
      if (previousRun === undefined) delete process.env.SUPERCOV_RUN_ID;
      else process.env.SUPERCOV_RUN_ID = previousRun;
      rmSync(serverRunEvidenceDirectory(execution.runId), {
        recursive: true,
        force: true,
      });
    }
  });

  it("inherits explicit request scope in nested framework callbacks", () => {
    const execution = scope(
      `nested-request-${process.pid}-${Date.now()}`,
      "worker-1",
      "nested-framework-handler",
      0,
    );
    const inner = withRequestPhase(() => coverageHit("nested-request-hit"));
    const outer = withRequestPhase((request: { headers: Headers }) => {
      inner();
      return request;
    });

    try {
      outer({
        headers: new Headers({
          [COVERAGE_SCOPE_HEADER]: encodeCoverageScope(execution),
        }),
      });
      expect(records(execution)).toMatchObject([
        {
          type: "hit",
          id: "nested-request-hit",
          scope: { attemptId: execution.attemptId },
        },
      ]);
    } finally {
      rmSync(serverRunEvidenceDirectory(execution.runId), {
        recursive: true,
        force: true,
      });
    }
  });

  it("persists evidence with no test scope under the run background scope", () => {
    const runId = `background-${process.pid}-${Date.now()}`;
    const previous = process.env.SUPERCOV_RUN_ID;
    const previousShard = process.env.SUPERCOV_EXECUTION_LOG_SHARD;
    process.env.SUPERCOV_RUN_ID = runId;
    process.env.SUPERCOV_EXECUTION_LOG_SHARD = "replicated-snapshot";
    try {
      coverageHit("detached-hit");
      coverageHit("second-detached-hit");
      flushBufferedBackgroundEvidence(runId);
      const directory = resolve(serverRunEvidenceDirectory(runId), "background");
      const files = readdirSync(directory);
      expect(files).toHaveLength(1);
      expect(files.every((file) => file.startsWith("replicated-snapshot-"))).toBe(true);
      const background = files.flatMap((file) =>
        readFileSync(resolve(directory, file), "utf8")
          .trim()
          .split("\n")
          .map((line) => JSON.parse(line) as CoverageServerRecord),
      );
      expect(background).toContainEqual(
        expect.objectContaining({ type: "hit", id: "detached-hit" }),
      );
      expect(background).toContainEqual(
        expect.objectContaining({ type: "hit", id: "second-detached-hit" }),
      );
      expect(background[0]?.scope).toBeUndefined();
    } finally {
      if (previous === undefined) delete process.env.SUPERCOV_RUN_ID;
      else process.env.SUPERCOV_RUN_ID = previous;
      if (previousShard === undefined)
        delete process.env.SUPERCOV_EXECUTION_LOG_SHARD;
      else process.env.SUPERCOV_EXECUTION_LOG_SHARD = previousShard;
      rmSync(serverRunEvidenceDirectory(runId), {
        recursive: true,
        force: true,
      });
    }
  });

  it("buffers and de-duplicates local Node evidence until the attempt ends", () => {
    const execution = scope(
      `buffered-${process.pid}-${Date.now()}`,
      "node-worker",
      "hot-loop",
      0,
    );
    const meta: McdcDecisionMeta = {
      id: "buffered-decision",
      file: "src/hot.ts",
      line: 1,
      column: 1,
      source: "ready && enabled",
      conditions: ["ready", "enabled"],
      kind: "if",
    };
    try {
      beginBufferedServerEvidence(execution);
      withCoverageCarrier({ version: 1, scope: execution }, () => {
        for (let index = 0; index < 1_000; index += 1) {
          coverageHit("repeated-hit");
          const frame = mcdcBegin(meta.id, meta);
          mcdcCondition(frame, 0, true);
          mcdcCondition(frame, 1, true);
          mcdcEnd(frame, true);
        }
      });
      expect(existsSync(serverEvidencePath(execution))).toBe(false);
      flushBufferedServerEvidence(execution);
      const persisted = records(execution);
      expect(persisted).toHaveLength(2);
      expect(persisted.map((record) => record.type)).toEqual([
        "hit",
        "decision",
      ]);
    } finally {
      rmSync(serverRunEvidenceDirectory(execution.runId), {
        recursive: true,
        force: true,
      });
    }
  });

  it("keeps buffered evidence on the destination pinned at attempt start", () => {
    const execution = scope(
      `pinned-${process.pid}-${Date.now()}`,
      "node-worker",
      "env-mutating-test",
      0,
    );
    const configured = process.env.SUPERCOV_SERVER_EVIDENCE_ROOT;
    try {
      beginBufferedServerEvidence(execution);
      withCoverageCarrier({ version: 1, scope: execution }, () => {
        // A test may mutate Supercov's public environment mid-attempt while
        // exercising the transport itself. Records before, during, and after
        // the mutation must land on the destination pinned at attempt start.
        coverageHit("before-mutation");
        delete process.env.SUPERCOV_SERVER_EVIDENCE_ROOT;
        coverageHit("during-mutation");
        process.env.SUPERCOV_SERVER_EVIDENCE_ROOT = configured;
        coverageHit("after-mutation");
      });
      const flushedPath = flushBufferedServerEvidence(execution);
      expect(flushedPath).toBe(serverEvidencePath(execution));
      expect(
        records(execution).map((record) =>
          record.type === "hit" ? record.id : record.type,
        ),
      ).toEqual(["before-mutation", "during-mutation", "after-mutation"]);
    } finally {
      process.env.SUPERCOV_SERVER_EVIDENCE_ROOT = configured;
      rmSync(serverRunEvidenceDirectory(execution.runId), {
        recursive: true,
        force: true,
      });
    }
  });

  it("de-duplicates explicit remote records within one test phase", () => {
    const execution = scope(
      `remote-dedup-${process.pid}-${Date.now()}`,
      "remote-worker",
      "remote-hot-loop",
      0,
    );
    const meta: McdcDecisionMeta = {
      id: "remote-decision",
      file: "src/remote.ts",
      line: 1,
      column: 1,
      source: "ready && enabled",
      conditions: ["ready", "enabled"],
      kind: "if",
    };
    try {
      withCoverageCarrier(
        { version: 1, scope: execution, phaseId: "explicit-action" },
        () => {
          for (let index = 0; index < 1_000; index += 1) {
            coverageHit("remote-repeated-hit");
            const frame = mcdcBegin(meta.id, meta);
            mcdcCondition(frame, 0, true);
            mcdcCondition(frame, 1, true);
            mcdcEnd(frame, true);
          }
        },
      );
      const persisted = records(execution);
      expect(persisted).toHaveLength(2);
      expect(persisted.map((record) => record.type)).toEqual([
        "hit",
        "decision",
      ]);
      expect(persisted.every((record) => record.phaseId === "explicit-action")).toBe(true);
    } finally {
      rmSync(serverRunEvidenceDirectory(execution.runId), {
        recursive: true,
        force: true,
      });
    }
  });

  it("buffers and de-duplicates unattributed hot-loop evidence into one file", () => {
    const runId = `background-buffer-${process.pid}-${Date.now()}`;
    const configuredRun = process.env.SUPERCOV_RUN_ID;
    try {
      process.env.SUPERCOV_RUN_ID = runId;
      for (let index = 0; index < 10_000; index += 1)
        coverageHit(`background-hit-${index % 3}`);
      const path = flushBufferedBackgroundEvidence(runId);
      expect(path && existsSync(path)).toBe(true);
      const files = readdirSync(resolve(serverRunEvidenceDirectory(runId), "background"));
      expect(files).toHaveLength(1);
      const persisted = readFileSync(path!, "utf8").trim().split("\n");
      expect(persisted).toHaveLength(3);
    } finally {
      if (configuredRun === undefined) delete process.env.SUPERCOV_RUN_ID;
      else process.env.SUPERCOV_RUN_ID = configuredRun;
      rmSync(serverRunEvidenceDirectory(runId), { recursive: true, force: true });
    }
  });

  it("keeps the same remote hit across distinct explicit phases", () => {
    const execution = scope(
      `remote-phases-${process.pid}-${Date.now()}`,
      "remote-worker",
      "remote-phases",
      0,
    );
    try {
      for (const phaseId of ["action-one", "action-two"]) {
        withCoverageCarrier({ version: 1, scope: execution, phaseId }, () => {
          coverageHit("shared-phase-hit");
          coverageHit("shared-phase-hit");
        });
      }
      expect(records(execution)).toMatchObject([
        { type: "hit", id: "shared-phase-hit", phaseId: "action-one" },
        { type: "hit", id: "shared-phase-hit", phaseId: "action-two" },
      ]);
    } finally {
      rmSync(serverRunEvidenceDirectory(execution.runId), {
        recursive: true,
        force: true,
      });
    }
  });

  it("preserves unphased repeats needed by timestamp attribution", () => {
    const execution = scope(
      `remote-unphased-${process.pid}-${Date.now()}`,
      "remote-worker",
      "remote-unphased",
      0,
    );
    try {
      withCoverageCarrier({ version: 1, scope: execution }, () => {
        coverageHit("unphased-repeat");
        coverageHit("unphased-repeat");
      });
      expect(records(execution)).toHaveLength(2);
    } finally {
      rmSync(serverRunEvidenceDirectory(execution.runId), {
        recursive: true,
        force: true,
      });
    }
  });

  it("allocates collision-free records for identical snapshot clones", () => {
    const runId = `clone-collision-${process.pid}-${Date.now()}`;
    const directory = resolve(serverRunEvidenceDirectory(runId), "background");
    mkdirSync(directory, { recursive: true });
    try {
      for (let clone = 0; clone < 32; clone += 1) {
        const next = writeExclusiveBackgroundRecord(
          { writeFileSync },
          runId,
          "identical-pid-and-shard",
          0,
          `${JSON.stringify({ clone })}\n`,
        );
        expect(next).toBe(clone + 1);
      }
      const files = readdirSync(directory);
      expect(files).toHaveLength(32);
      const clones = files
        .map((file) => JSON.parse(readFileSync(resolve(directory, file), "utf8")))
        .map((record) => record.clone)
        .sort((left, right) => left - right);
      expect(clones).toEqual(Array.from({ length: 32 }, (_, index) => index));
    } finally {
      rmSync(serverRunEvidenceDirectory(runId), {
        recursive: true,
        force: true,
      });
    }
  });

  it("allocates one record per concurrently restored identical runtime", async () => {
    const runId = `concurrent-clones-${process.pid}-${Date.now()}`;
    const directory = resolve(serverRunEvidenceDirectory(runId), "background");
    mkdirSync(directory, { recursive: true });
    const runtime = new URL("../../src/runtime.ts", import.meta.url).href;
    const workerSource = `
      const { parentPort, workerData } = require("node:worker_threads");
      const { writeFileSync } = require("node:fs");
      import(workerData.runtime).then(({ writeExclusiveBackgroundRecord }) => {
        const next = writeExclusiveBackgroundRecord(
          { writeFileSync },
          workerData.runId,
          "identical-snapshot-pid-and-counter",
          0,
          JSON.stringify({ clone: workerData.clone }) + "\\n",
        );
        parentPort.postMessage({ next });
      }).catch((error) => {
        throw error;
      });
    `;
    try {
      const results = await Promise.all(
        Array.from({ length: 16 }, (_, clone) =>
          new Promise<number>((resolveWorker, rejectWorker) => {
            const worker = new Worker(workerSource, {
              eval: true,
              workerData: { clone, runId, runtime },
            });
            worker.once("message", ({ next }: { next: number }) =>
              resolveWorker(next),
            );
            worker.once("error", rejectWorker);
            worker.once("exit", (code) => {
              if (code !== 0)
                rejectWorker(new Error(`collision worker exited ${code}`));
            });
          }),
        ),
      );
      expect(results.sort((left, right) => left - right)).toEqual(
        Array.from({ length: 16 }, (_, index) => index + 1),
      );
      const files = readdirSync(directory);
      expect(files).toHaveLength(16);
      const clones = files
        .map((file) => JSON.parse(readFileSync(resolve(directory, file), "utf8")))
        .map((record) => record.clone)
        .sort((left, right) => left - right);
      expect(clones).toEqual(Array.from({ length: 16 }, (_, index) => index));
    } finally {
      rmSync(serverRunEvidenceDirectory(runId), {
        recursive: true,
        force: true,
      });
    }
  });

  it("does not hide a background-record filesystem failure", () => {
    const failure = Object.assign(new Error("disk unavailable"), { code: "ENOSPC" });
    expect(() =>
      writeExclusiveBackgroundRecord(
        {
          writeFileSync() {
            throw failure;
          },
        },
        "failed-run",
        "writer",
        0,
        "{}\n",
      ),
    ).toThrow(failure);
  });

  it("reports exhaustion instead of claiming unwritten background evidence", () => {
    let attempts = 0;
    expect(() =>
      writeExclusiveBackgroundRecord(
        {
          writeFileSync() {
            attempts += 1;
            throw Object.assign(new Error("already exists"), { code: "EEXIST" });
          },
        },
        "exhausted-run",
        "identical-writer",
        0,
        "{}\n",
      ),
    ).toThrow(
      expect.objectContaining({ code: "SUPERCOV_BACKGROUND_COLLISION_LIMIT" }),
    );
    expect(attempts).toBe(10_000);
  });
});
