# Rust assertion identities: renamed import versus same-named decoy

Worktree: `/Users/domas/Developer/supercorp/supercov-asserted-typescript-analyzer`.
Branch: `codex/asserted-typescript-analyzer`, based on `ccfd015`. Local,
uncommitted work; no sample application/test edits, publishing, public CLI
promotion or changes to the Rust runtime probes.

This completes the bounded next step in
`rust-asserted-alias-findings-2026-09-09.md`.

## Result and exact scope

Two passing assertions now witness the return of a function imported under
another name. The compiler identifies both the callee and the standard equality
macro. A same-named function that executes inside the comparison receives no
return-value witness. Without the optional compiler records, the renamed import
remains unsupported, not guessed.

The independent Cargo fixture is intentionally adversarial:

```rust
// src/lib.rs
pub fn real(value: i32) -> i32 { compute(value); value * 2 }
pub fn compute(value: i32) -> i32 { value * 2 }

// tests/contract.rs: one assertion per test
use asserted_identity_fixture::real as compute;
// First test:
assert_eq!(compute(21), 42);
// Second test:
let result = compute(21);
let alias = result;
assert_eq!(alias, 42);
```

Both functions execute in the first assertion's phase. Execution order and the
spelling `compute` therefore cannot establish the returned value's source.
Compiler identity plus the restricted source/value path distinguishes them.

| Function | Runtime assertion link | Exact return witnesses | Native caught | Native survived |
| --- | --- | ---: | ---: | ---: |
| `real`, imported as `compute` | Yes | 2 | 5 | 0 |
| `compute`, whose result is discarded | Yes | 0 | 0 | 5 |

There are still two production functions in the inventory: one witnessed return
boundary, one unresolved boundary. **This is not a 50% global assertion score.**
No mutation verdict is emitted by this recognizer. A witness concerns an exact
integer/bool result at the tested invocation, conditional on normal return. It
does not prove the expected value correct, cover other inputs/internal effects,
or establish that every change inside that function would be rejected. An
unresolved function is not a declaration that it is safe to change.

## Implementation and guards

- `spikes/rustc-backend/src/asserted_identities.rs` exports authored direct HIR
  function-call identities and macro expansion identities during compilation.
  Enable only with the Cargo-tracked `--cfg supercov_assertion_identities` Rust
  flag. Default candidate serialization omits the optional field.
- A call target uses the existing source-derived function obligation ID, not a
  textual DefPath or last-segment match. `standardEquality` comes from rustc's
  `assert_eq_macro` diagnostic-item DefId. It is not inferred from the word
  `assert_eq` in a macro's name.
- The private manifest and archive source scope accept a strict optional
  `assertionIdentities` extension. Records include exact byte ranges, source,
  owner and target. Normalization checks ranges against complete captured source
  bytes. Structural validation rejects unknown fields, invalid kinds/identities,
  source lengths and missing/wrong-file owners. Consumers independently require
  matching whole-source/test hashes plus the reader's Cargo/configuration checks.
- Identical records from repeated compiler units are deduplicated; conflicting
  records remain visible. The consumer requires a unique matching call/macro
  record in the exact source region. Missing, nonstandard or ambiguous records
  do not fall back to spelling when compiler metadata is present.
- The call grammar permits exactly one direct call with literal arguments and
  a primitive literal expected value, or a previously checked immutable value
  copy. Matching within the original source statement/assertion avoids pretending
  that reparsed macro-argument AST offsets are original-file offsets.
- `compilerResolved: true` is added to these witnesses. This means both the
  callee and equality macro were compiler-resolved, not that the checker is
  formally verified or every production behavior is protected.

Only the renamed own-library import restriction was relaxed. The reader still
requires a single dependency-free library. Library modules/re-exports, type
aliases, dependency aliases, generated namespaces, mutable/borrowed values,
nonprimitive equality, arbitrary expected expressions and transformations remain
unsupported. The existing same-attempt, measured-function, passing/nonflaky test,
passing assertion phase and true decision-event guards remain.

## Calibration and cost

Two fresh `cargo-mutants 27.1.0` campaigns produced the same complete ten outcomes:
five caught, five survived, zero unviable, zero timeouts and zero unmapped. The
second campaign matched every saved name, function location and outcome, not just
totals. `rust-asserted-identity-oracle.json` binds Cargo/library/test source hashes.

The off/on gate runs the same Cargo fixture twice and requires identical complete
coverage inventories and runtime assertion analyses. Both repetitions passed.
It asserts the decoy really appears in the direct assertion phase. No MC/DC
obligation or runtime execution link was added or removed by identity collection.

Observed local development timings (Rust 1.95.0, Node 24, debug builds):

| Operation | First campaign | Repeat |
| --- | ---: | ---: |
| Source read + integrity verification + analysis, identities on | 25.6 ms | 27.1 ms |
| Whole analyzer process, identities on | 30.9 ms | 33.0 ms |
| Private instrumented Cargo run, identities off | 6.06 s | 6.26 s |
| Private instrumented Cargo run, identities on | 5.98 s | 7.24 s |
| Fresh native mutation campaign | 22.58 s | 12.28 s |

The already-built native test executable took about 1.9–2.6 ms; warm Cargo test
took about 22–32 ms. These are important cheaper execution baselines. Some gates
ran concurrently and cache conditions differ: **these numbers establish neither
a speedup over the strongest mutation alternative nor a <=10% overhead bound**.
Query time also includes source/configuration verification, not just AST logic.
Compiler collection has no new runtime probes but does add compilation, archive
and query work. Engineering/discovery cost is not included in the timing table.
Ten native mutants are calibration evidence, not a complete behavioral oracle.

Artifacts (local temporary directories, not required by future runs):

- Recording: `/var/folders/4q/cdhj37516zzbc7z9ckrst0x40000gn/T/supercov-rust-asserted-or7LqZ`.
- Fresh complete-oracle check: `/var/folders/4q/cdhj37516zzbc7z9ckrst0x40000gn/T/supercov-rust-asserted-AnvVdk`.
- Off-mode directories and all logs are linked by each `comparison.json`.

## Reproduction

Run from the worktree above, with Node 24 and Rust 1.95.0 selected:

```bash
RUSTC_BOOTSTRAP=1 RUSTUP_TOOLCHAIN=1.95.0 \
  cargo build --offline --manifest-path spikes/rustc-backend/Cargo.toml
cargo build --offline -p supercov -p supercov-engine \
  --bin supercov --example rust_asserted_runtime

# The script itself controls the off/on Rust flag. Do not pre-set that flag here.
SUPERCOV_CARGO_MUTANTS=/absolute/path/to/cargo-mutants \
  node scripts/rust-asserted-source-calibration.mjs --identities

# Without the environment variable: fresh coverage, saved source-hashed oracle.
node scripts/rust-asserted-source-calibration.mjs --identities

# Existing regression gates, including the unchanged known TODO:
node scripts/rust-asserted-source-calibration.mjs
node scripts/rust-asserted-source-calibration.mjs --aliases
node scripts/rust-asserted-runtime-calibration.mjs
```

`--record` writes a candidate oracle only to its temporary directory and requires
actual native mutation execution. It never overwrites the checked-in oracle.
Compiler rebuilds invalidate the generated libtest bundle attestation. Preserve
old generated artifacts and use the official builder to regenerate a matching
bundle; do not edit build IDs by hand. See the separate new issue document.

## Regression status and next step

- 469 engine unit tests, 20 CLI tests, 19 contract tests, seven Rust integration
  tests pass. Five new unit-test groups exercise identity recognition, missing/
  conflicting/nonstandard evidence, source corruption, strict ingestion and
  merge behavior. The archive-scope regression test also checks the extension.
- Workspace Clippy with all targets and warnings denied passes. Formatting and
  whitespace checks pass. The compiler companion builds; its test target has
  zero unit tests, so the real Cargo runs are the compiler collection gate.
- Source/alias/runtime fixtures retain 4/4/2 return witnesses and the unchanged
  55/55/40 source-hashed mutation oracles. The alias fixture also retains its
  four witnesses with compiler identity collection enabled.
- External TypeScript parity: nine tests pass, none skipped, 2,474 site
  resolutions unchanged. Existing imperfect mutation agreement is unchanged.
- `RUST-ASSERT-001` remains unfixed; its TODO still detects the missing second
  assertion outcome. This is not an all-green end-to-end release gate.
- A libtest artifact reproducibility mismatch was observed and documented, not
  repaired, in `supercov-bugs-identity-step-2026-09-09.md`.

Next bounded step: measure opt-in collection and query scaling on a larger,
held-out Cargo fixture before broadening syntax support. Separate compilation,
test execution, archive and source-verification costs; keep full inventories and
unknowns. This should decide whether source/record indexing or parser reuse is
needed before admitting a nested-module/re-export case. Do not turn this small
identity success into an accuracy or performance claim about an entire codebase.

Follow-up measured: `rust-asserted-scaling-findings-2026-09-09.md` records all nine
off/on pairs and the query-side scaling bottleneck. Its next step is source and
identity indexing with semantic parity, not namespace expansion.
