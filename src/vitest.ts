import { createHash } from "node:crypto";
import { mkdirSync } from "node:fs";
import { relative, resolve, sep } from "node:path";
import { afterEach, beforeEach } from "vitest";
import {
  coverageSnapshot as localCoverageSnapshot,
  activateCoverageScope as localActivateCoverageScope,
  enableRuntimeSnapshotEvidence as localEnableRuntimeSnapshotEvidence,
  resetCoverage as localResetCoverage,
  takeNodeAssertionPhases as localTakeNodeAssertionPhases,
} from "./runtime.ts";
import { inferTestProvenance } from "./provenance.ts";
import { atomicWriteFileSync } from "./atomic.ts";
import type {
  CoverageExecutionScope,
  McdcRawTestResult,
} from "./types.ts";

const evidenceDirectory = process.env["SUPERCOV_EVIDENCE_DIR"];
const emittedSetupFiles = new Set<string>();
const attempts = new Map<string, number>();
const activeScopes = new Map<string, CoverageExecutionScope>();
const runtimeGlobal = globalThis as typeof globalThis & {
  __SUPERCOV_DIRECT_RUNTIME__?: {
    activateCoverageScope: typeof localActivateCoverageScope;
    coverageSnapshot: typeof localCoverageSnapshot;
    enableRuntimeSnapshotEvidence: typeof localEnableRuntimeSnapshotEvidence;
    resetCoverage: typeof localResetCoverage;
    takeNodeAssertionPhases: typeof localTakeNodeAssertionPhases;
  };
};
const runtime = runtimeGlobal.__SUPERCOV_DIRECT_RUNTIME__ ?? {
  activateCoverageScope: localActivateCoverageScope,
  coverageSnapshot: localCoverageSnapshot,
  enableRuntimeSnapshotEvidence: localEnableRuntimeSnapshotEvidence,
  resetCoverage: localResetCoverage,
  takeNodeAssertionPhases: localTakeNodeAssertionPhases,
};

// Vitest persists one in-memory runtime snapshot per test. Writing the same
// events through the server JSONL transport would be both redundant and
// unattributed, and turns hot unit-test loops into synchronous filesystem IO.
runtime.enableRuntimeSnapshotEvidence();

function attemptStatus(
  state: string | undefined,
): McdcRawTestResult["status"] {
  if (state === "pass") return "passed";
  if (state === "fail") return "failed";
  if (state === "skip" || state === "todo") return "skipped";
  return "unknown";
}

function titlePath(
  task: Readonly<{ name: string; suite?: unknown }>,
): string[] {
  const names: string[] = [task.name];
  let suite = task.suite as { name?: string; suite?: unknown } | undefined;
  while (suite?.name) {
    names.unshift(suite.name);
    suite = suite.suite as typeof suite;
  }
  return names;
}

function writeEvidence(payload: McdcRawTestResult, suffix: string): void {
  if (!evidenceDirectory) return;
  const directory = resolve(process.cwd(), evidenceDirectory, suffix);
  mkdirSync(directory, { recursive: true });
  atomicWriteFileSync(
    resolve(directory, "mcdc.json"),
    `${JSON.stringify(payload)}\n`,
  );
}

beforeEach((context) => {
  const task = context.task;
  const testFile = relative(process.cwd(), task.file.filepath)
    .split(sep)
    .join("/");
  if (!emittedSetupFiles.has(testFile)) {
    emittedSetupFiles.add(testFile);
    const setupSnapshot = runtime.coverageSnapshot();
    if (setupSnapshot.hits.length || setupSnapshot.decisions.length) {
      writeEvidence(
        {
          testId: `vitest:${task.file.id}:setup`,
          test: `${testFile} > module setup`,
          testFile,
          title: "module setup",
          retry: 0,
          status: "passed",
          provenance: inferTestProvenance({
            runner: "vitest",
            file: testFile,
            project: task.file.projectName,
            explicitKind: process.env["SUPERCOV_TEST_KIND"],
          }),
          role: "setup",
          runtime: [setupSnapshot],
          browser: [],
          server: [],
        },
        `vitest-${task.file.id}-setup`,
      );
    }
  }
  const testId = `vitest:${task.id}`;
  const retry = attempts.get(testId) ?? 0;
  attempts.set(testId, retry + 1);
  const testKey = createHash("sha256").update(testId).digest("hex").slice(0, 24);
  const scope: CoverageExecutionScope = {
    version: 1,
    runId: process.env["SUPERCOV_RUN_ID"] ?? "unscoped",
    workerId: `vitest-${process.env["VITEST_POOL_ID"] ?? process.pid}`,
    testId,
    testKey,
    retry,
    attemptId: `${testKey}-${retry}`,
  };
  activeScopes.set(task.id, scope);
  runtime.activateCoverageScope(scope);
  runtime.resetCoverage(testId);
});

afterEach((context) => {
  const task = context.task;
  const testFile = relative(process.cwd(), task.file.filepath)
    .split(sep)
    .join("/");
  const scope = activeScopes.get(task.id);
  const retry = scope?.retry ?? task.result?.retryCount ?? 0;
  const payload: McdcRawTestResult = {
    testId: scope?.testId ?? `vitest:${task.id}`,
    ...(scope ? { scope } : {}),
    test: [...titlePath(task)].join(" > "),
    testFile,
    title: task.name,
    retry,
    status: attemptStatus(task.result?.state),
    provenance: inferTestProvenance({
      runner: "vitest",
      file: testFile,
      project: task.file.projectName,
      explicitKind: process.env["SUPERCOV_TEST_KIND"],
    }),
    ...(scope ? { phases: runtime.takeNodeAssertionPhases(scope) } : {}),
    runtime: [runtime.coverageSnapshot()],
    browser: [],
    server: [],
  };
  writeEvidence(payload, `vitest-${task.id}-${retry}`);
  activeScopes.delete(task.id);
  runtime.activateCoverageScope();
});
