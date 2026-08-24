import {
  existsSync,
  lstatSync,
  readFileSync,
  readdirSync,
} from "node:fs";
import { relative, resolve, sep } from "node:path";
import { gunzipSync, gzipSync } from "node:zlib";
import { atomicWriteFileSync } from "./atomic.ts";

export const EVIDENCE_ARCHIVE_SCHEMA_VERSION = 2;
export const EVIDENCE_ARCHIVE_MAGIC = "SUPERCOV-EVIDENCE-2\n";

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

export type EvidenceArchiveSource =
  | {
      directory: string;
      /** Namespace used when more than one evidence transport is archived. */
      prefix?: string;
    }
  | {
      /** A single immutable input, such as the coverage denominator manifest. */
      file: string;
      path: string;
    };

const ARCHIVE_MAGIC = Buffer.from(EVIDENCE_ARCHIVE_MAGIC);

function compareCodePoints(left: string, right: string): number {
  const leftPoints = [...left];
  const rightPoints = [...right];
  const length = Math.min(leftPoints.length, rightPoints.length);
  for (let index = 0; index < length; index += 1) {
    const leftPoint = leftPoints[index]!.codePointAt(0)!;
    const rightPoint = rightPoints[index]!.codePointAt(0)!;
    if (leftPoint !== rightPoint) return leftPoint - rightPoint;
  }
  return leftPoints.length - rightPoints.length;
}

function validArchivePath(path: string): boolean {
  return (
    path.length > 0 &&
    !path.startsWith("/") &&
    !path.endsWith("/") &&
    !path.includes("\\") &&
    !path.includes("\0") &&
    path.split("/").every((part) => part.length > 0 && part !== "." && part !== "..")
  );
}

function requireArchivePath(path: string): void {
  if (!validArchivePath(path))
    throw new Error(`Invalid Supercov evidence archive path: ${JSON.stringify(path)}`);
}

function canonicalEntries<T extends { path: string }>(entries: T[]): T[] {
  const files = [...entries].sort((left, right) =>
    compareCodePoints(left.path, right.path)
  );
  for (const file of files) requireArchivePath(file.path);
  for (let index = 1; index < files.length; index += 1) {
    if (files[index - 1]!.path === files[index]!.path)
      throw new Error(`Duplicate Supercov evidence archive path: ${files[index]!.path}`);
  }
  if (!files.some((file) => file.path === "manifest.json"))
    throw new Error("Supercov evidence archive is missing manifest.json");
  return files;
}

function writeEntries(
  entries: Array<{ path: string; contents: Buffer }>,
  destination: string,
): EvidenceArchiveMetadata {
  const files = canonicalEntries(entries);
  const chunks: Buffer[] = [ARCHIVE_MAGIC];
  for (const { path, contents } of files) {
    const header = Buffer.from(`${JSON.stringify({ path, bytes: contents.byteLength })}\n`);
    const headerSize = Buffer.allocUnsafe(4);
    headerSize.writeUInt32BE(header.byteLength);
    chunks.push(headerSize, header, contents);
  }
  const serialized = Buffer.concat(chunks);
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

function evidenceFiles(directory: string): string[] {
  if (!existsSync(directory)) return [];
  return readdirSync(directory, { withFileTypes: true })
    .flatMap((entry) => {
      const path = resolve(directory, entry.name);
      if (entry.isDirectory()) return evidenceFiles(path);
      if (entry.isFile()) return [path];
      throw new Error(`Unsupported raw evidence entry: ${path}`);
    })
    .sort(compareCodePoints);
}

/**
 * Pack the evidence needed to reproduce a report into one deterministic gzip
 * artifact. Publication remains the caller's responsibility so loose files
 * are never removed before the immutable run directory is atomically visible.
 */
export function writeEvidenceArchive(
  evidence: string | EvidenceArchiveSource[],
  destination: string,
): EvidenceArchiveMetadata {
  const sources = typeof evidence === "string"
    ? [{ directory: evidence }]
    : evidence;
  const files = sources.flatMap((source) => {
    if ("file" in source) {
      requireArchivePath(source.path);
      if (!lstatSync(source.file).isFile())
        throw new Error(`Unsupported raw evidence entry: ${source.file}`);
      return [{ path: source.path, sourcePath: source.file }];
    }
    if (source.prefix) requireArchivePath(source.prefix);
    return evidenceFiles(source.directory).map((path) => ({
      path: [source.prefix, relative(source.directory, path).split(sep).join("/")]
        .filter(Boolean)
        .join("/"),
      sourcePath: path,
    }));
  });
  return writeEntries(
    files.map(({ path, sourcePath }) => ({ path, contents: readFileSync(sourcePath) })),
    destination,
  );
}

/** Write already-decoded evidence, used for integrity-checked shard merging. */
export function writeEvidenceArchiveEntries(
  entries: EvidenceArchiveEntry[],
  destination: string,
): EvidenceArchiveMetadata {
  return writeEntries(
    entries.map((entry) => ({ path: entry.path, contents: Buffer.from(entry.contents) })),
    destination,
  );
}

export function readEvidenceArchive(path: string): EvidenceArchive {
  const serialized = gunzipSync(readFileSync(path));
  if (!serialized.subarray(0, ARCHIVE_MAGIC.byteLength).equals(ARCHIVE_MAGIC))
    throw new Error(`Unsupported Supercov evidence archive: ${path}`);
  const files: EvidenceArchiveEntry[] = [];
  let previousPath: string | undefined;
  let offset = ARCHIVE_MAGIC.byteLength;
  while (offset < serialized.byteLength) {
    if (offset + 4 > serialized.byteLength)
      throw new Error(`Truncated Supercov evidence archive: ${path}`);
    const headerSize = serialized.readUInt32BE(offset);
    offset += 4;
    if (offset + headerSize > serialized.byteLength)
      throw new Error(`Truncated Supercov evidence archive: ${path}`);
    const encodedHeader = serialized.subarray(offset, offset + headerSize);
    if (encodedHeader.at(-1) !== 0x0a)
      throw new Error(`Invalid Supercov evidence archive entry: ${path}`);
    const header = JSON.parse(
      encodedHeader.subarray(0, -1).toString("utf8"),
    ) as { path?: string; bytes?: number };
    offset += headerSize;
    if (
      typeof header.path !== "string" ||
      !validArchivePath(header.path) ||
      !Number.isSafeInteger(header.bytes) ||
      header.bytes! < 0 ||
      !encodedHeader.equals(
        Buffer.from(`${JSON.stringify({ path: header.path, bytes: header.bytes })}\n`),
      ) ||
      offset + header.bytes! > serialized.byteLength
    ) {
      throw new Error(`Invalid Supercov evidence archive entry: ${path}`);
    }
    if (previousPath !== undefined && compareCodePoints(previousPath, header.path) >= 0)
      throw new Error(`Invalid Supercov evidence archive entry ordering: ${path}`);
    files.push({
      path: header.path,
      contents: serialized
        .subarray(offset, offset + header.bytes!)
        .toString("utf8"),
    });
    offset += header.bytes!;
    previousPath = header.path;
  }
  if (!files.some((file) => file.path === "manifest.json"))
    throw new Error(`Coverage manifest is missing from ${path}`);
  return {
    schemaVersion: EVIDENCE_ARCHIVE_SCHEMA_VERSION,
    format: "supercov-evidence",
    files,
  };
}
