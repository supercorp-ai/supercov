# Evidence archive contract v1

The immutable published evidence artifact is `evidence.raw.gz`. Its evidence
schema version is `2`; this is independent of the encompassing contract
version.

After gzip decompression the byte stream is:

1. ASCII magic `SUPERCOV-EVIDENCE-2\n`.
2. Zero or more entries sorted by archive path using Unicode code-point order.
3. For each entry:
   - a four-byte unsigned big-endian header length;
   - a UTF-8 JSON header followed by `\n`, exactly
     `{"path":<string>,"bytes":<non-negative integer>}\n`;
   - exactly `bytes` uninterpreted payload bytes.

Archive paths use `/`, are unique, and are relative. Duplicate paths,
truncated headers/payloads, invalid byte lengths, invalid magic, and trailing
non-entry data are fatal corruption. Readers must not partially trust a corrupt
archive or call its measurement complete.

Required namespaces:

- `manifest.json`: the complete structural denominator.
- runner evidence at its transport-relative paths.
- `server/attempts/*.jsonl`: attributed server evidence.
- `server/background/*.jsonl`: aggregate/unattributed evidence.

JSONL preserves one JSON object per line and a final newline. File names are
transport only and do not establish attribution; record scope does. Evidence
may be batched and deduplicated only when doing so cannot remove a distinct
coverage vector, phase, attempt, outcome, or confidence observation.

Writers publish the gzip file atomically. Ordering and gzip output must be
deterministic for identical logical inputs. Publication metadata records file
count, uncompressed bytes, compressed bytes, format, and evidence schema.
