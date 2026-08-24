import { createHash } from "node:crypto";
import { existsSync, readFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { gunzipSync, gzipSync } from "node:zlib";
import { atomicWriteFileSync } from "./atomic.ts";
import { EVIDENCE_ARCHIVE_SCHEMA_VERSION } from "./evidenceArchive.ts";
import {
  analyzeCoverageArchive,
  type AnalyzeCoverageOptions,
} from "./runAnalysis.ts";
import type { McdcReport } from "./types.ts";

export const QUERY_INDEX_SCHEMA_VERSION = 1;
export const QUERY_INDEX_FILE = `query-index.v${QUERY_INDEX_SCHEMA_VERSION}.json.gz`;

interface CoverageQueryIndex {
  format: "supercov-query-index";
  schemaVersion: typeof QUERY_INDEX_SCHEMA_VERSION;
  producerVersion: string;
  evidence: {
    archiveSchemaVersion: typeof EVIDENCE_ARCHIVE_SCHEMA_VERSION;
    sha256: string;
    bytes: number;
  };
  analysisSha256: string;
  report: McdcReport;
}

function packageVersion(): string {
  try {
    const manifest = JSON.parse(
      readFileSync(fileURLToPath(new URL("../package.json", import.meta.url)), "utf8"),
    ) as { version?: string };
    return manifest.version ?? "unknown";
  } catch {
    return "unknown";
  }
}

function evidenceIdentity(path: string): {
  sha256: string;
  bytes: number;
} {
  const contents = readFileSync(path);
  return {
    sha256: createHash("sha256").update(contents).digest("hex"),
    bytes: contents.byteLength,
  };
}

function analysisIdentity(options: AnalyzeCoverageOptions): string {
  return createHash("sha256")
    .update(JSON.stringify(options))
    .digest("hex");
}

export function coverageQueryIndexPath(evidencePath: string): string {
  return resolve(dirname(evidencePath), QUERY_INDEX_FILE);
}

function validIndex(
  value: unknown,
  identity: ReturnType<typeof evidenceIdentity>,
  analysisSha256: string,
): value is CoverageQueryIndex {
  if (!value || typeof value !== "object") return false;
  const index = value as Partial<CoverageQueryIndex>;
  return (
    index.format === "supercov-query-index" &&
    index.schemaVersion === QUERY_INDEX_SCHEMA_VERSION &&
    index.producerVersion === packageVersion() &&
    index.evidence?.archiveSchemaVersion === EVIDENCE_ARCHIVE_SCHEMA_VERSION &&
    index.evidence.sha256 === identity.sha256 &&
    index.evidence.bytes === identity.bytes &&
    index.analysisSha256 === analysisSha256 &&
    Boolean(index.report)
  );
}

function readIndex(
  path: string,
  identity: ReturnType<typeof evidenceIdentity>,
  analysisSha256: string,
): McdcReport | undefined {
  try {
    const parsed = JSON.parse(gunzipSync(readFileSync(path)).toString("utf8"));
    return validIndex(parsed, identity, analysisSha256)
      ? parsed.report
      : undefined;
  } catch {
    return undefined;
  }
}

/** Read an already-materialized index without analyzing or writing anything. */
export function readCoverageQueryIndex(
  evidencePath: string,
  options: AnalyzeCoverageOptions,
): McdcReport | undefined {
  const indexPath = coverageQueryIndexPath(evidencePath);
  if (!existsSync(indexPath)) return undefined;
  const identity = evidenceIdentity(evidencePath);
  return readIndex(
    indexPath,
    identity,
    analysisIdentity(options),
  );
}

/**
 * Materialize a disposable query index on first use. Raw evidence remains the
 * sole source of truth: a changed archive, tool version, schema, or corrupt
 * cache always causes reconstruction. Concurrent writers are harmless because
 * they derive identical content and publish through unique atomic temp files.
 */
export function analyzeCoverageArchiveCached(
  evidencePath: string,
  options: AnalyzeCoverageOptions,
): McdcReport {
  const identity = evidenceIdentity(evidencePath);
  const analysisSha256 = analysisIdentity(options);
  const indexPath = coverageQueryIndexPath(evidencePath);
  const cached = readIndex(indexPath, identity, analysisSha256);
  if (cached) return cached;

  const report = analyzeCoverageArchive(evidencePath, options);
  const index: CoverageQueryIndex = {
    format: "supercov-query-index",
    schemaVersion: QUERY_INDEX_SCHEMA_VERSION,
    producerVersion: packageVersion(),
    evidence: {
      archiveSchemaVersion: EVIDENCE_ARCHIVE_SCHEMA_VERSION,
      ...identity,
    },
    analysisSha256,
    report,
  };
  atomicWriteFileSync(
    indexPath,
    gzipSync(Buffer.from(JSON.stringify(index)), { level: 9 }),
  );
  return report;
}
