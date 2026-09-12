# Rust assertion source recognition: bounded value witnesses

Follow-up: `rust-asserted-alias-findings-2026-09-09.md` records the subsequent
immutable-copy extension. The counts below are the original pre-alias results;
the current source calibration recognizes four return boundaries, not three.

Worktree: `/Users/domas/Developer/supercorp/supercov-asserted-typescript-analyzer`.
Branch: `codex/asserted-typescript-analyzer`, based on `ccfd015`.
Follows the TypeScript extraction and the first Rust runtime slice, all still
uncommitted. No sample application changes, publishing, or public backend
promotion. This is an additional partial Step 5, not a completed scoring system.

## What this slice establishes

The new `rust_asserted_source` module recognizes a narrow source-level argument:

> A particular normal-returning integer/bool invocation supplied the value
> compared with a fixed literal by this passing standard equality assertion,
> either directly or through a straight-line immutable local.

Under defined Rust execution and unchanged surrounding test/standard-library
semantics, a different value of the same primitive type at that comparison
would make the assertion fail. This does **not** say that every change inside
the producer changes that value, or reaches the comparison normally. Nor does
it validate the expected literal against the intended specification.

This is a fixed, source-aware recognizer, not an AI-generated proof language or
a general theorem prover. Its small rule is implemented and tested, not formally
verified against Rust's full semantics. No universal accuracy percentage follows
from this calibration.

### Evidence and checks

1. Analyze the actual complete Rust source/test bytes, not extracted snippets.
   Their sorted path/content hashes and counts must match the recorded run.
   The internal reader also checks current dependency/configuration fingerprints
   against `run.json` before using the Cargo library identity.
2. Parse with the existing Rust syntax parser. Accept a dependency-free, flat
   single-library namespace; reject imports/type aliases/modules/expansions or
   attributes that require compiler-resolved identities. Primitive results must
   be bool/integer, not floats or custom equality types.
3. Resolve only approved own-library imports or qualified calls. Reject local
   item shadowing (including hoisted definitions), mutable bindings, copies
   through aliases, unknown statements, predicates, transformed operands,
   nonliteral expected values, and custom assertion messages.
   Scan the entire test file for macro definitions/unknown expansions, including
   other helpers and opaque assertion token trees: a helper-exported macro can
   affect root macro lookup. This deliberately also rejects innocent nested
   macro calls and bang operators inside macro arguments rather than guessing.
4. Match the production function and the exact assertion decision to measured
   compiler manifest locations. Require a passing nonflaky test attempt, passing
   assertion phase, and true decision event in that phase. Direct calls also
   require a production hit in that same phase. Immutable producers may precede
   the phase, but require a hit in that attempt and the restricted source path.
5. Emit an explicit witness with call, expected expression, primitive type,
   producer/assertion locations, attempt and phase. The existing join can then
   mark the **observed return boundary** evident at value strength.

All original function-entry sites remain in the inventory. They remain
classified `review`; an evident `function-result` is not all function code being
protected. Unsupported covered sites stay limits, not asserted-to-be-missing
tests. This is not yet an effect/decision-site inventory or a global denominator.

Trust boundary: the reader relies on the existing compiler/archive provenance,
stock Rust primitive equality and the standard test harness. Hashes bind bytes;
they do not prove the compiler correct or rule out undefined behavior. The source
module's caller must authenticate Cargo identities, not pass guessed names.

### Deliberate limitations

- The current archive does not supply this reader with a complete source bundle;
  `--source-root` must still match the run. Missing or changed sources fail closed.
- Cargo discovery may omit an extra uncompiled `.rs` file included in the broader
  integrity hash. That causes rejection, not silent analysis of a subset.
- Namespace resolution is intentionally restrictive. Dependencies, workspaces,
  aliases, user macros, conditional declarations and helper chains need more
  evidence. This is not a general-purpose Rust name resolver.
- Cross-thread returns, mutable state, floats, custom equality, transformed
  results, decisions and side effects are not covered by this rule.
- The known `RUST-ASSERT-001` context bug remains documented and unfixed. Missing
  assertion outcomes are never reconstructed from a passing surrounding test.
- There are no mutation predictions and no global assertion score.

## Calibration: preserve the old oracle and add counterexamples

The original 40-mutant fixture and its source hashes are unchanged. The new
recognizer identifies `direct(21)` and the precomputed immutable value. Its other
six functions remain unresolved, including the real exact cross-thread assertion.
The ten mutants associated with the two recognized functions were all caught in
the saved native oracle. That alone would misleadingly suggest a kill guarantee.

The new fixture deliberately challenges that interpretation, not just parsing:

| Producer/test | Exact return witness | Caught | Survived | Unbuildable |
| --- | --- | ---: | ---: | ---: |
| Reversed, qualified integer comparison | Yes | 5 | 0 | 0 |
| Immutable boolean local | Yes | 3 | 2 | 0 |
| Exact integer comparison at input zero | Yes | 3 | 2 | 0 |
| Result masked with bitwise AND | No | 3 | 2 | 0 |
| Discarded result | No | 0 | 5 | 0 |
| Overwritten mutable local | No | 0 | 5 | 0 |
| Immutable alias copy | No: unsupported | 5 | 0 | 0 |
| Hoisted local function discarding the library result | No | 0 | 5 | 0 |
| Same producer on both comparison sides | No | 0 | 5 | 0 |
| Floating-point result | No: unsupported | 5 | 0 | 0 |
| Custom result with equality ignoring its contents | No | 0 | 2 | 1 |
| Custom Debug method | No | 0 | 1 | 0 |
| Custom PartialEq method | No: unsupported | 1 | 0 | 0 |
| **All outcomes** | **3 witnessed, 10 unresolved functions** | **25** | **29** | **1** |

All 55 generated mutants are retained: zero unmapped and zero timeouts. The
unbuildable mutant replaces `custom_result` with `Default::default()`, but its
result type does not implement Default. It is not relabeled survived or excluded.
The pinned oracle records each name, function location and outcome, plus hashes
of every fixture file. A fresh calibration must match every row, not just totals.

Two important results:

- Exact observation does not imply all function mutations are killed: four
  mutants of recognized functions preserve the observed value and survive.
- Unknown does not imply untested: all five alias-function mutants are caught,
  but alias tracing is deliberately outside the current rule.

These are designed adversarial fixtures, not an unseen representative benchmark.
Mutation execution is independent native ground truth for the listed mutants;
it is not a ground-truth label for the different claim "primitive value observed".

## Cost: no new runtime probes, but not an equivalent mutation replacement

The first new-fixture calibration on this machine measured:

| Work | Time |
| --- | ---: |
| Warm native Cargo suite, median of three | 26.0 ms |
| Already-built native test executable, median of three | 2.35 ms |
| Native cold Cargo build/run | 1.07 s |
| Matching libtest preparation | 1.08 s |
| Private instrumented Cargo build/run | 6.53 s |
| Reader process including archive load and source analysis | 63.7 ms |
| Source reading, integrity verification and analysis inside reader | 48.3 ms |
| All 55 native mutants, including baseline/builds/tests | 43.61 s |

This slice changes no probes, compiler injection or transport records. Source
work happens after the existing instrumented run. These tiny-sample timings do
not establish an end-to-end 10% overhead budget. Source verification itself is
more expensive here than one already-built native execution. There is **no fair
speedup ratio against mutation testing**: this report answers a narrower question
and emits zero mutant outcomes. Engineering/discovery and tool-build costs are
also not counted as free. No 10,000x or equivalent-accuracy claim is established.

The first new oracle and raw logs are retained at:
`/var/folders/4q/cdhj37516zzbc7z9ckrst0x40000gn/T/supercov-rust-asserted-Th7qcO`.

A second fresh 55-mutant run matched every saved name/location/outcome, including
the unbuildable case, at `supercov-rust-asserted-5LdRHT` in the same temporary
parent. It took 44.30 s. Source reading/verification/analysis took 47.2 ms, while
the full reader process took 444.3 ms on that invocation (versus 63.7 ms in the
first); startup must not be omitted or replaced by the best observed number.
The original fixture was also rerun against its unchanged saved 40-mutant oracle
at `supercov-rust-asserted-Q4yL0T`, retaining the known failing TODO.

## Reproduce

From the worktree above with Rust 1.95.0 and its compiler development/library
sources installed (same prerequisites as the previous slice):

```sh
cargo build --offline -p supercov
cargo build --offline -p supercov-engine --example rust_asserted_runtime
RUSTC_BOOTSTRAP=1 RUSTUP_TOOLCHAIN=1.95.0 \
  cargo build --offline --manifest-path spikes/rustc-backend/Cargo.toml

# Real instrumented suites, compared with source-hashed saved native oracles.
node scripts/rust-asserted-runtime-calibration.mjs
node scripts/rust-asserted-source-calibration.mjs

# Re-execute every mutant; requires the pinned cargo-mutants 27.1.0 binary.
SUPERCOV_CARGO_MUTANTS=/absolute/path/to/cargo-mutants \
  node scripts/rust-asserted-source-calibration.mjs

# Keep the known phase defect visible as an actual red gate.
SUPERCOV_ASSERTED_STRICT_PHASES=1 \
  node scripts/rust-asserted-runtime-calibration.mjs
```

Both scripts share `rust-asserted-calibration-harness.mjs`, which uses a fresh
temporary copy and retains all outputs. Source analysis happens before creating
`mutants.out`, which otherwise changes the current configuration fingerprint.
The new script's explicit `--record` mode requires a fresh mutant run and writes
only a candidate oracle into its temp directory for review; it cannot silently
replace the checked-in oracle. A parser-only synthetic-evidence test is never
presented as an end-to-end compiler test.

## Verification and next step

Workspace: 459 engine unit tests (seven source-recognizer tests added), 20 CLI
tests, 19 contract tests and seven Rust runtime integration tests pass. Both
external TypeScript parity runs pass: all 2,474 sites unchanged, nine package
tests passed with none skipped. This parity preserves the existing prototype's
results; it does not improve its previously reported mutation agreement.

Clippy (`--workspace --all-targets -- -D warnings`), rustfmt and diff whitespace
checks also pass. After the final macro-namespace guards, both real Cargo
fixtures were rerun against the complete saved oracles: the new fixture passed
at `supercov-rust-asserted-7YBYQ3`; strict-phase mode on the original reproduced
exactly the known regression (exit 1, one failure, no TODO) at
`supercov-rust-asserted-kms9Ul`, in the same temporary parent above. This is not
an all-green end-to-end release gate while that attribution defect remains.

Next bounded extension: trace immutable alias copies with the same provenance
checks, using the already-held-out `aliased` test as a positive calibration and
adding overwritten/shadowed alias counterexamples. After that, compiler-resolved
identities are needed to move beyond this intentionally flat Cargo subset.
Do not turn these witnesses into a global percentage or promote the public Rust
route. Keep `RUST-ASSERT-001` as a TODO until bug fixing is authorized.
