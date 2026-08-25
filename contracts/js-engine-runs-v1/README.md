# JavaScript engine run corpus v1

This directory contains immutable, complete Supercov runs used by the Rust
analysis and query differentials. Tests read this corpus instead of mutable,
ignored `.supercov` histories under the executable fixture projects.

Each runner family has at least two runs so the corpus exercises both
single-run queries and diffs. The node:test family also keeps two complementary
partial runs for uncovered-condition and waiver queries. `run.json` and
`evidence.raw.gz` are authoritative input;
disposable query indexes are never checked in.

The corpus is intentionally not refreshed automatically. A deliberate schema
or coverage-model revision must add a new versioned corpus and explain the
contract change.
