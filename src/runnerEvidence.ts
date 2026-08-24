import { createHash } from "node:crypto";
import { mkdirSync, readFileSync } from "node:fs";
import { relative, resolve, sep } from "node:path";
import { fileURLToPath } from "node:url";
import { atomicWriteFileSync } from "./atomic.ts";
import { inferTestProvenance } from "./provenance.ts";
import { serverEvidencePath } from "./transport.ts";
import type {
  CoverageExecutionScope,
  CoverageServerRecord,
  McdcRawTestResult,
  TestAttemptStatus,
} from "./types.ts";

export interface RunnerTestIdentity {
  runner: string;
  name: string;
  file?: string;
  line?: number;
  column?: number;
  retry?: number;
}

function localFile(file?: string): string | undefined {
  if (!file) return undefined;
  const absolute = file.startsWith("file:") ? fileURLToPath(file) : file;
  return relative(process.cwd(), absolute).split(sep).join("/");
}

export function runnerTestId(identity: RunnerTestIdentity): string {
  const key = [
    identity.runner,
    localFile(identity.file) ?? "unknown",
    identity.line ?? 0,
    identity.column ?? 0,
    identity.name,
  ].join("\0");
  return `${identity.runner}:${createHash("sha256").update(key).digest("hex").slice(0, 24)}`;
}

export function runnerExecutionScope(
  identity: RunnerTestIdentity,
): CoverageExecutionScope {
  const testId = runnerTestId(identity);
  const retry = identity.retry ?? 0;
  const workerId = `${identity.runner}-${process.env["JEST_WORKER_ID"] ?? process.pid}`;
  const testKey = createHash("sha256").update(testId).digest("hex").slice(0, 24);
  return {
    version: 1,
    runId: process.env["SUPERCOV_RUN_ID"] ?? "unscoped",
    workerId,
    testId,
    testKey,
    retry,
    attemptId: `${testKey}-${retry}`,
  };
}

export function callerLocation(ignored: RegExp): {
  file?: string;
  line?: number;
  column?: number;
} {
  const lines = new Error().stack?.split("\n").slice(2) ?? [];
  for (const entry of lines) {
    if (ignored.test(entry) || entry.includes("node:internal")) continue;
    const match = /(?:\(|at\s+)(file:\/\/[^:)]+|(?:[A-Za-z]:)?[^():]+):(\d+):(\d+)\)?$/.exec(entry.trim());
    if (!match) continue;
    return {
      file: match[1],
      line: Number(match[2]),
      column: Number(match[3]),
    };
  }
  return {};
}

export function readScopedServerEvidence(
  scope: CoverageExecutionScope,
): CoverageServerRecord[] {
  try {
    return readFileSync(serverEvidencePath(scope), "utf8")
      .split("\n")
      .filter(Boolean)
      .map((line) => JSON.parse(line) as CoverageServerRecord)
      .filter((record) => record.scope?.attemptId === scope.attemptId);
  } catch {
    return [];
  }
}

export function writeRunnerEvidence(
  identity: RunnerTestIdentity,
  status: TestAttemptStatus,
  scope: CoverageExecutionScope,
  evidenceDirectoryOverride?: string,
): void {
  const evidenceDirectory =
    evidenceDirectoryOverride ?? process.env["SUPERCOV_EVIDENCE_DIR"];
  if (!evidenceDirectory) return;
  const testFile = localFile(identity.file);
  const payload: McdcRawTestResult = {
    testId: scope.testId,
    scope,
    test: identity.name,
    ...(testFile ? { testFile } : {}),
    title: identity.name.split(" > ").at(-1) ?? identity.name,
    retry: identity.retry ?? 0,
    status,
    provenance: inferTestProvenance({
      runner: identity.runner,
      file: testFile,
      explicitKind: process.env["SUPERCOV_TEST_KIND"],
    }),
    runtime: [],
    browser: [],
    server: readScopedServerEvidence(scope),
  };
  const directory = resolve(
    process.cwd(),
    evidenceDirectory,
    `${identity.runner.replace(/[^A-Za-z0-9_-]/g, "_")}-${scope.attemptId}`,
  );
  mkdirSync(directory, { recursive: true });
  atomicWriteFileSync(resolve(directory, "mcdc.json"), `${JSON.stringify(payload)}\n`);
}
