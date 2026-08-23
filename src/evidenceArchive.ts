import {
  existsSync,
  readFileSync,
  readdirSync,
} from "node:fs";
import { relative, resolve, sep } from "node:path";
import { gunzipSync, gzipSync } from "node:zlib";
import { atomicWriteFileSync } from "./atomic.ts";

export const EVIDENCE_ARCHIVE_SCHEMA_VERSION = 1;

export interface EvidenceArchiveEntry {
  path: string;
  contents: string;
}

export interface EvidenceArchive {
  schemaVersion: typeof EVIDENCE_ARCHIVE_SCHEMA_VERSION;
  format: "supercov-evidence";
  files: EvidenceArchiveEntry[];
}

export interface EvidenceArchiveMetadata {
  schemaVersion: typeof EVIDENCE_ARCHIVE_SCHEMA_VERSION;
  format: "framed+gzip";
  file: "evidence.raw.gz";
  files: number;
  uncompressedBytes: number;
  compressedBytes: number;
}

export interface EvidenceArchiveSource {
  directory: string;
  /** Namespace used when more than one evidence transport is archived. */
  prefix?: string;
}

const ARCHIVE_MAGIC = Buffer.from("SUPERCOV-EVIDENCE-1\n");

function evidenceFiles(directory: string): string[] {
  if (!existsSync(directory)) return [];
  return readdirSync(directory, { withFileTypes: true })
    .flatMap((entry) => {
      const path = resolve(directory, entry.name);
      if (entry.isDirectory()) return evidenceFiles(path);
      if (entry.isFile()) return [path];
      throw new Error(`Unsupported raw evidence entry: ${path}`);
    })
    .sort((left, right) => left.localeCompare(right));
}

/**
 * Pack the evidence needed to reproduce a report into one deterministic gzip
 * artifact. Publication remains the caller's responsibility so loose files
 * are never removed before the report directory is atomically visible.
 */
export function writeEvidenceArchive(
  evidence: string | EvidenceArchiveSource[],
  destination: string,
): EvidenceArchiveMetadata {
  const sources = typeof evidence === "string"
    ? [{ directory: evidence }]
    : evidence;
  const files = sources.flatMap(({ directory, prefix }) =>
    evidenceFiles(directory).map((path) => ({
      path: [prefix, relative(directory, path).split(sep).join("/")]
        .filter(Boolean)
        .join("/"),
      sourcePath: path,
    })),
  ).sort((left, right) => left.path.localeCompare(right.path));
  const chunks: Buffer[] = [ARCHIVE_MAGIC];
  for (const { path, sourcePath } of files) {
    const contents = readFileSync(sourcePath);
    const header = Buffer.from(`${JSON.stringify({ path, bytes: contents.byteLength })}\n`);
    const headerSize = Buffer.allocUnsafe(4);
    headerSize.writeUInt32BE(header.byteLength);
    chunks.push(headerSize, header, contents);
  }
  const serialized = Buffer.concat(chunks);
  // Node emits a zero gzip mtime, so identical evidence produces identical
  // bytes without depending on wall-clock time.
  const compressed = gzipSync(serialized, { level: 9 });
  atomicWriteFileSync(destination, compressed);
  return {
    schemaVersion: EVIDENCE_ARCHIVE_SCHEMA_VERSION,
    format: "framed+gzip",
    file: "evidence.raw.gz",
    files: files.length,
    uncompressedBytes: serialized.byteLength,
    compressedBytes: compressed.byteLength,
  };
}

export function readEvidenceArchive(path: string): EvidenceArchive {
  const serialized = gunzipSync(readFileSync(path));
  if (!serialized.subarray(0, ARCHIVE_MAGIC.byteLength).equals(ARCHIVE_MAGIC))
    throw new Error(`Unsupported Supercov evidence archive: ${path}`);
  const files: EvidenceArchiveEntry[] = [];
  let offset = ARCHIVE_MAGIC.byteLength;
  while (offset < serialized.byteLength) {
    if (offset + 4 > serialized.byteLength)
      throw new Error(`Truncated Supercov evidence archive: ${path}`);
    const headerSize = serialized.readUInt32BE(offset);
    offset += 4;
    if (offset + headerSize > serialized.byteLength)
      throw new Error(`Truncated Supercov evidence archive: ${path}`);
    const header = JSON.parse(
      serialized.subarray(offset, offset + headerSize).toString("utf8"),
    ) as { path?: string; bytes?: number };
    offset += headerSize;
    if (
      !header.path ||
      !Number.isSafeInteger(header.bytes) ||
      header.bytes! < 0 ||
      offset + header.bytes! > serialized.byteLength
    ) {
      throw new Error(`Invalid Supercov evidence archive entry: ${path}`);
    }
    files.push({
      path: header.path,
      contents: serialized
        .subarray(offset, offset + header.bytes!)
        .toString("utf8"),
    });
    offset += header.bytes!;
  }
  return {
    schemaVersion: EVIDENCE_ARCHIVE_SCHEMA_VERSION,
    format: "supercov-evidence",
    files,
  };
}
