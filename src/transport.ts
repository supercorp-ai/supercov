import type { CoverageCarrier, CoverageExecutionScope } from "./types.ts";

export const COVERAGE_SCOPE_HEADER = "x-supercov-scope";
export const COVERAGE_PHASE_HEADER = "x-supercov-phase";
export const COVERAGE_SCOPE_COOKIE = "__supercov_scope";
export const COVERAGE_PHASE_COOKIE = "__supercov_phase";
export const COVERAGE_CARRIER_ENV = "SUPERCOV_CONTEXT";
export const DEFAULT_SERVER_EVIDENCE_ROOT =
  "/tmp/supercov-server-evidence";

function configuredServerEvidenceRoot(): string {
  return typeof process !== "undefined" &&
    process.env?.["SUPERCOV_SERVER_EVIDENCE_ROOT"]
    ? process.env["SUPERCOV_SERVER_EVIDENCE_ROOT"]!
    : DEFAULT_SERVER_EVIDENCE_ROOT;
}

function nonEmpty(value: string | null): value is string {
  return typeof value === "string" && value.length > 0;
}

function safeKey(value: string): boolean {
  return /^[a-zA-Z0-9_-]+$/.test(value);
}

function pathComponent(value: string): string {
  const safe = value.replace(/[^a-zA-Z0-9_-]/g, "_");
  return safe || "unscoped";
}

export function encodeCoverageScope(scope: CoverageExecutionScope): string {
  return new URLSearchParams({
    v: String(scope.version),
    r: scope.runId,
    w: scope.workerId,
    t: scope.testId,
    k: scope.testKey,
    a: String(scope.retry),
    i: scope.attemptId,
  }).toString();
}

export function decodeCoverageScope(
  encoded: string | undefined,
): CoverageExecutionScope | undefined {
  if (!encoded) return undefined;
  try {
    const values = new URLSearchParams(encoded);
    const runId = values.get("r");
    const workerId = values.get("w");
    const testId = values.get("t");
    const testKey = values.get("k");
    const attemptId = values.get("i");
    const retry = Number(values.get("a"));
    if (
      values.get("v") !== "1" ||
      !nonEmpty(runId) ||
      !nonEmpty(workerId) ||
      !nonEmpty(testId) ||
      !nonEmpty(testKey) ||
      !safeKey(testKey) ||
      !nonEmpty(attemptId) ||
      !safeKey(attemptId) ||
      !Number.isSafeInteger(retry) ||
      retry < 0
    )
      return undefined;
    return {
      version: 1,
      runId,
      workerId,
      testId,
      testKey,
      retry,
      attemptId,
    };
  } catch {
    return undefined;
  }
}

export function encodeCoverageCarrier(carrier: CoverageCarrier): string {
  return Buffer.from(JSON.stringify(carrier), "utf8").toString("base64url");
}

export function decodeCoverageCarrier(
  encoded: string | undefined,
): CoverageCarrier | undefined {
  if (!encoded) return undefined;
  try {
    const value = JSON.parse(
      Buffer.from(encoded, "base64url").toString("utf8"),
    ) as CoverageCarrier;
    if (value.version !== 1) return undefined;
    if (value.scope) {
      const roundTrip = decodeCoverageScope(encodeCoverageScope(value.scope));
      if (!roundTrip) return undefined;
    }
    if (value.phaseId !== undefined && value.phaseId.length === 0)
      return undefined;
    return value;
  } catch {
    return undefined;
  }
}

export function serverRunEvidenceDirectory(
  runId: string,
  root = configuredServerEvidenceRoot(),
): string {
  return `${root.replace(/\/+$/, "")}/${pathComponent(runId)}`;
}

export function serverEvidenceDirectory(
  scope: CoverageExecutionScope,
  root = configuredServerEvidenceRoot(),
): string {
  return `${serverRunEvidenceDirectory(scope.runId, root)}/attempts`;
}

export function serverEvidencePath(
  scope: CoverageExecutionScope,
  root = configuredServerEvidenceRoot(),
): string {
  return `${serverEvidenceDirectory(scope, root)}/${pathComponent(scope.attemptId)}.jsonl`;
}

export function backgroundEvidenceDirectory(
  runId: string,
  root = configuredServerEvidenceRoot(),
): string {
  return `${serverRunEvidenceDirectory(runId, root)}/background`;
}

export function backgroundEvidencePath(
  runId: string,
  processId = typeof process === "undefined" ? "unknown" : String(process.pid),
  root = configuredServerEvidenceRoot(),
): string {
  return `${backgroundEvidenceDirectory(runId, root)}/${pathComponent(processId)}.jsonl`;
}
