# Rust assertion aliases: immutable value copies

Worktree: `/Users/domas/Developer/supercorp/supercov-asserted-typescript-analyzer`.
Branch: `codex/asserted-typescript-analyzer`, based on `ccfd015`. All changes
remain local and uncommitted. No sample application changes, runtime probes,
compiler injection changes, or public Rust backend promotion.

This follows `rust-asserted-source-findings-2026-09-09.md`. Its historical
three-witness table remains the pre-alias result, not the current output.

## What changed

The source recognizer now follows `let copy = previously_tracked_local;` for
known primitive integer/bool values. These are Rust **value copies**, not
references, borrowing, mutable aliases, or custom Copy implementations.

Bindings are versioned in source order. A name rebind creates a new version;
already-created copies retain their original producer. For example:

```rust
let result = preserved_source(21);
let saved = result;
let result = source_replacement(2);
let alias = saved;
assert_eq!(alias, 42);
```

The witness follows `preserved_source(21) -> result -> saved -> alias` and does
not credit `source_replacement(2)`. Conversely, replacing an alias with a new
production call credits the new call and discards the old provenance for that
name. Self-shadowing `let result = result;` reads the preceding binding version.

Every witness now has a `bindings` array containing each relevant binding's
name, exact source location and initializer, ordered from producer to assertion
operand. Direct assertion calls have an empty array. The existing producer,
attempt, phase, assertion and primitive fields are retained. This is inspectable
source evidence, not a caller-supplied or independently submitted certificate.

An append-only arena stores parent version indices. Each copy adds one entry;
growing chains are not cloned at each declaration or recursively dropped.
Only emitted witnesses materialize their full path. Source parsing, identifier
maps, repeated test-file analysis and serialization still cost work; there is
no claim of constant-time analysis.

The prior guards remain: exact source/test hashes, authenticated Cargo identity,
closed macro namespace, measured production point, same passing nonflaky test
attempt, and a passing exact assertion phase with its own true decision event.
There is no promotion from execution-only evidence.

### What remains unsupported

An unknown initializer/rebinding stops analysis of subsequent statements rather
than leaving a stale same-named value active. This deliberately underclaims even
when an unaffected older copy could still be useful. Mutable bindings,
attributes on bindings, typed aliases, references, `.clone()`, destructuring,
blocks, transformed copies and nonliteral expected values remain unsupported.
Imports/modules/dependencies still have the prior narrow namespace restrictions.

A recognized value witness is conditional on normal return to the comparison.
It does not establish specification correctness, all inputs/branches/effects,
or mutant kills. No global assertion percentage is emitted. The checker is
implemented and tested, not formally verified against all Rust semantics.

## Calibration

The existing source fixture and its 55-mutant oracle are unchanged. Its previously
unsupported `aliased(21)` test now has an exact return witness with bindings
`result = aliased(21)` and `alias = result`. The report moves from three to four
recognized return boundaries, with the same 13-function inventory. Nine functions
remain unresolved. The earlier 40-mutant runtime fixture retains its two witnesses
and known phase-attribution TODO.

A new independent fixture has eight native Cargo tests and 11 production
functions, including an uncalled function retained in the denominator:

| Producer/test | Exact return witness | Caught | Survived |
| --- | --- | ---: | ---: |
| Multi-hop copy and self-shadowing | Yes | 5 | 0 |
| Boolean copy | Yes | 3 | 2 |
| Original source, saved before source rebind | Yes | 5 | 0 |
| New source binding, not compared | No | 0 | 5 |
| Old producer, discarded by alias rebind | No | 0 | 5 |
| New producer replacing the alias | Yes | 4 | 1 |
| Alias shadowed by a constant | No | 0 | 5 |
| Mutable copy overwritten | No | 0 | 5 |
| Copy transformed by modulo | No | 3 | 2 |
| Nested-block copy | No: unsupported | 5 | 0 |
| Never called | No: not reached | 0 | 5 |
| **All outcomes** | **4 witnessed, 7 unresolved functions** | **25** | **30** |

All 55 `cargo-mutants 27.1.0` outcomes are preserved: zero unmapped, unbuildable,
or timed out. The checked-in oracle records source hashes, names, function
locations and outcomes, not just totals. Recording and checking are distinct:
`--record` requires native mutation execution and writes a candidate only to its
temporary directory; a normal fresh run must match the complete saved oracle.

Three mutants associated with witnessed functions survive. Two preserve the
boolean result; another replaces `value * 2` with `value + 2` at input 2, preserving
4. Exact return observation is not a function-mutation guarantee. Conversely,
all five nested-block mutants are caught while that grammar remains unsupported.

This is a targeted adversarial calibration, not a representative unseen corpus.
Native mutation outcomes calibrate the listed changes; they are not identical
labels for the narrower claim that a primitive return value was observed.

## Cost and artifacts

First alias-fixture run, single-machine observations rather than a controlled
performance benchmark (TypeScript parity was also running):

| Work | Time |
| --- | ---: |
| Warm native Cargo suite, median of three | 23.5 ms |
| Already-built native test executable, median of three | 2.15 ms |
| Native cold Cargo build/run | 0.780 s |
| Matching libtest preparation | 1.10 s |
| Private instrumented Cargo build/run | 6.64 s |
| Complete reader process | 509.6 ms |
| Source read/verify/analyze inside reader | 43.4 ms |
| Native 55-mutant campaign, including baseline/builds/tests | 43.75 s |

There are no additional runtime probes in this slice. These costs do not prove a
10% overall runtime budget or an equivalent-information speedup over mutation
testing: this analysis emits zero mutation verdicts. In this tiny fixture one
already-built native execution is cheaper than source verification. Startup,
discovery/checking, unresolved work and engineering effort must not disappear
from any future comparison.

First alias oracle and raw artifacts:
`/var/folders/4q/cdhj37516zzbc7z9ckrst0x40000gn/T/supercov-rust-asserted-IQM0Hl`.

The second full native campaign at `supercov-rust-asserted-VKgwSx` in that same
temporary parent matched every saved mutant name, location and outcome. It took
43.34 s; source read/verify/analyze took 45.0 ms and the full reader process took
60.1 ms. The startup difference from the first run is retained, not hidden in a
best-case speedup ratio. The prior source fixture was rerun at
`supercov-rust-asserted-YjkKxm` against its unchanged 55-outcome saved oracle;
the original runtime fixture at `supercov-rust-asserted-NvB0Dy` retained its
40-outcome saved oracle and the known failing TODO. Those two runs reused saved
mutation outcomes; they did not execute the old mutants anew.

## Reproduce

From the worktree above, with the existing Rust 1.95.0 compiler/libtest companion
prerequisites described in the earlier findings:

```sh
cargo build --offline -p supercov
cargo build --offline -p supercov-engine --example rust_asserted_runtime

# New real Cargo fixture and saved, source-hashed native oracle.
node scripts/rust-asserted-source-calibration.mjs --aliases

# Re-execute all 55 mutants and require complete oracle parity.
SUPERCOV_CARGO_MUTANTS=/absolute/path/to/cargo-mutants \
  node scripts/rust-asserted-source-calibration.mjs --aliases

# Recheck the previous fixtures without changing their sources or oracle labels.
node scripts/rust-asserted-source-calibration.mjs
node scripts/rust-asserted-runtime-calibration.mjs
```

The shared harness always copies fixtures into fresh temporary directories and
retains logs, archived evidence, analysis, comparison and timings. It performs
source validation before mutation output changes the configuration fingerprint.
The alias fixture is selected with `--aliases`; there is no additional platform
or public CLI surface.

## Verification and next step

- Workspace tests: 464 engine unit tests, 20 CLI tests, 19 contract tests and
  seven Rust runtime integration tests pass.
- Twelve source-recognizer unit tests include five new test groups for copy
  paths, rebindings, guarded rejection, same-attempt provenance and a 1,024-copy
  self-shadowing chain. Synthetic evidence tests are not presented as E2E tests.
- Clippy with `--workspace --all-targets -- -D warnings` passes.
- Rustfmt, script formatting and diff whitespace checks pass.
- Both external TypeScript parity runs pass: nine package tests, none skipped,
  and all 2,474 site resolutions unchanged. This does not change the prototype's
  existing imperfect mutation agreement.
- `RUST-ASSERT-001` remains unfixed. Its missing assertion outcome is not replaced
  with a guessed pass. No new existing-product bug was established in this slice.
  The original fixture's TODO still fails its desired two-links assertion, so
  this is not an entirely green end-to-end release gate.

Next bounded step: obtain compiler-resolved function/macro identities for one
renamed import, test a genuine producer against a same-named decoy, and only then
relax that namespace restriction. Keep the existing source and mutation oracles
as gates. Do not extend the score or public Rust backend on the strength of these
small fixtures alone.

Follow-up completed: see `rust-asserted-identity-findings-2026-09-09.md` for the
opt-in compiler identity export, real renamed-import/decoy calibration and the
next bounded performance gate. The counts above describe the historical alias
step; they are not the latest total test counts.
