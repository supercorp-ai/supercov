import { existsSync, mkdirSync, readFileSync, rmSync } from "node:fs";
import { basename, resolve } from "node:path";
import { atomicRenameSync, atomicWriteFileSync } from "./atomic.ts";
import {
  EVIDENCE_ARCHIVE_SCHEMA_VERSION,
  readEvidenceArchive,
  writeEvidenceArchiveEntries,
  type EvidenceArchiveEntry,
} from "./evidenceArchive.ts";
import { acquireProjectLock } from "./workspace.ts";
import type { CoverageRunIntegrity } from "./types.ts";

interface MergeRunMetadata {
  id: string;
  startedAt?: string;
  testExitCode?: number | null;
  integrity?: CoverageRunIntegrity;
  rawEvidence?: { schemaVersion?: number };
}

function metadata(root: string, id: string): MergeRunMetadata {
  const path = resolve(root, ".supercov/runs", id, "run.json");
  try {
    const value = JSON.parse(readFileSync(path, "utf8")) as MergeRunMetadata;
    if (
      value.id !== id ||
      value.rawEvidence?.schemaVersion !== EVIDENCE_ARCHIVE_SCHEMA_VERSION ||
      !existsSync(resolve(root, ".supercov/runs", id, "evidence.raw.gz"))
    ) throw new Error("incomplete run");
    return value;
  } catch (error) {
    throw new Error(`Cannot merge coverage run ${id}: ${String(error)}`);
  }
}

function rewrittenContents(contents: string, mergedRunId: string): string {
  const rewrite = (value: unknown): unknown => {
    if (!value || typeof value !== "object") return value;
    if (Array.isArray(value)) return value.map(rewrite);
    const record = value as Record<string, unknown>;
    return Object.fromEntries(
      Object.entries(record).map(([key, nested]) => [
        key,
        key === "scope" && nested && typeof nested === "object"
          ? { ...(nested as Record<string, unknown>), runId: mergedRunId }
          : rewrite(nested),
      ]),
    );
  };
  return contents
    .split("\n")
    .map((line) => {
      if (!line) return line;
      try {
        return JSON.stringify(rewrite(JSON.parse(line)));
      } catch {
        return line;
      }
    })
    .join("\n");
}

function mergedPath(path: string, shard: number): string {
  if (path.startsWith("server/background/"))
    return `server/background/shard-${shard}-${basename(path)}`;
  if (path.startsWith("server/"))
    return `server/shard-${shard}/${path.slice("server/".length)}`;
  return `shards/${shard}/${path}`;
}

export function mergeCoverageRuns(root: string, runIds: string[]): string {
  if (runIds.length < 2) throw new Error("Usage: supercov merge <run-id> <run-id> [...]");
  if (new Set(runIds).size !== runIds.length)
    throw new Error("Each merged run must be unique");
  const inputs = runIds.map((id) => ({
    id,
    metadata: metadata(root, id),
    archive: readEvidenceArchive(resolve(root, ".supercov/runs", id, "evidence.raw.gz")),
  }));
  const integrity = inputs[0]!.metadata.integrity;
  if (!integrity) throw new Error(`Run ${inputs[0]!.id} has no source-integrity fingerprint`);
  for (const input of inputs.slice(1)) {
    if (
      !input.metadata.integrity ||
      input.metadata.integrity.fingerprint.combined !== integrity.fingerprint.combined ||
      input.metadata.integrity.schemaVersion !== integrity.schemaVersion ||
      input.metadata.integrity.instrumenterVersion !== integrity.instrumenterVersion
    ) {
      throw new Error(
        `Cannot merge incompatible run ${input.id}: source, tests, dependencies, configuration, or instrumenter differ`,
      );
    }
  }
  const manifests = inputs.map((input) =>
    input.archive.files.find((entry) => entry.path === "manifest.json")?.contents,
  );
  if (!manifests[0] || manifests.some((manifest) => manifest !== manifests[0]))
    throw new Error("Cannot merge runs with different coverage denominators");

  const mergedRunId = `${new Date().toISOString().replace(/[:.]/g, "-")}-merge`;
  const lock = acquireProjectLock(root, mergedRunId);
  const staging = resolve(root, ".supercov/work", mergedRunId, "run-publication");
  const destination = resolve(root, ".supercov/runs", mergedRunId);
  try {
    rmSync(staging, { recursive: true, force: true });
    mkdirSync(staging, { recursive: true });
    const entries: EvidenceArchiveEntry[] = [
      { path: "manifest.json", contents: manifests[0] },
      ...inputs.flatMap((input, shard) =>
        input.archive.files
          .filter((entry) => entry.path !== "manifest.json")
          .map((entry) => ({
            path: mergedPath(entry.path, shard),
            contents: rewrittenContents(entry.contents, mergedRunId),
          })),
      ),
    ];
    const rawEvidence = writeEvidenceArchiveEntries(
      entries,
      resolve(staging, "evidence.raw.gz"),
    );
    const startedAt = new Date().toISOString();
    atomicWriteFileSync(
      resolve(staging, "run.json"),
      `${JSON.stringify({
        id: mergedRunId,
        startedAt,
        durationMs: 0,
        command: ["supercov", "merge", ...runIds],
        testExitCode: inputs.every((input) => input.metadata.testExitCode === 0) ? 0 : 1,
        integrity,
        rawEvidence,
        merged: true,
        parents: runIds,
      }, null, 2)}\n`,
    );
    atomicRenameSync(staging, destination);
    return mergedRunId;
  } finally {
    rmSync(resolve(root, ".supercov/work", mergedRunId), { recursive: true, force: true });
    lock.release();
  }
}
