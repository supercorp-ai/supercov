import {
  EVIDENCE_ARCHIVE_SCHEMA_VERSION,
  writeEvidenceArchive,
  writeEvidenceArchiveEntries,
  type EvidenceArchiveEntry,
  type EvidenceArchiveMetadata,
  type EvidenceArchiveSource,
} from "./evidenceArchive.ts";
import { runRustEngineJson, rustEngineEnabled } from "./engineProcess.ts";

function validatedMetadata(value: EvidenceArchiveMetadata): EvidenceArchiveMetadata {
  if (
    value.schemaVersion !== EVIDENCE_ARCHIVE_SCHEMA_VERSION ||
    value.format !== "framed+gzip" ||
    value.file !== "evidence.raw.gz" ||
    !Number.isSafeInteger(value.files) ||
    value.files < 1 ||
    !Number.isSafeInteger(value.uncompressedBytes) ||
    value.uncompressedBytes < 1 ||
    !Number.isSafeInteger(value.compressedBytes) ||
    value.compressedBytes < 1
  )
    throw new Error("Rust engine returned invalid evidence archive metadata");
  return value;
}

/**
 * Private cutover boundary. A Rust-selected run already uses the final Rust
 * framing implementation; the shipped engine remains available only while the
 * complete CLI is still migrating.
 */
export function writeEngineEvidenceArchive(
  sources: EvidenceArchiveSource[],
  destination: string,
): EvidenceArchiveMetadata {
  if (!rustEngineEnabled()) return writeEvidenceArchive(sources, destination);
  return validatedMetadata(
    runRustEngineJson<
      { destination: string; sources: EvidenceArchiveSource[] },
      EvidenceArchiveMetadata
    >("__pack-evidence", { destination, sources }),
  );
}

export function writeEngineEvidenceArchiveEntries(
  entries: EvidenceArchiveEntry[],
  destination: string,
): EvidenceArchiveMetadata {
  if (!rustEngineEnabled())
    return writeEvidenceArchiveEntries(entries, destination);
  return validatedMetadata(
    runRustEngineJson<
      { destination: string; entries: EvidenceArchiveEntry[] },
      EvidenceArchiveMetadata
    >("__pack-evidence", { destination, entries }),
  );
}
