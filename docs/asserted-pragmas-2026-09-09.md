# Source assertion hints: first checked implementation

Implemented on `codex/asserted-js-integration`, directly in the Supercov checkout.
Nothing was published. No Supergateway/Essential SEO application code or tests
were changed, and no runtime probes were added or removed.

## Public workflow

```ts
// observes: src/core.ts#compute return value
assert.equal(compute(), 4);
```

Run the suite normally, then from its project directory:

```sh
supercov runs <run-id> asserted --pragmas
supercov runs <run-id> asserted --pragmas --limit 1 --offset 0 --json
```

`--file` and `--site` select suggested production targets. The new view pages
hints separately instead of repeating all tests/attempts/execution links.
Normal candidate queries include a hint count and a text pointer to this view.
This does not fix the separate full-site JSON response-budget bug.

The syntax requires a project-relative file, exact function/owner name, and an
optional literal source substring identifying one inventory site. Optional
`via ...` text is explanation, never executable input or trusted flow evidence.

## Validation contract

Origin remains `user-suggested`; validation is independently
`analyzer-supported`, `unresolved`, or `invalid`. Supported links expose the
selected observation and retain its presence/value/total strength. They remain
subject to the existing analyzer's limitations, not formal safety proofs.

The frontend resolves the target and exact assertion attachment, then checks
all recorded calls at that source position and method in the owning test.
The Rust checker selects only that assertion's observations and applies the
existing reached-effect boundary/flow rules. It cannot borrow support from a
different assertion or test. Shared indexes are built once per hint query;
checking a hint does not rerun the suite, run mutants, or clone the full facts.

Hints never enter `observations`, never change the normal join, and never remove
sites from the denominator. This implementation validates proposed connections;
it does not add new inference rules to resolve previously opaque dependencies.
Decision, absence and internal-derivation hints can remain unresolved. A target
absent from the inventory is unresolved rather than invalid: unsupported
source constructs can be missing from that inventory.

Transport revision is `source-linked-v3/archive-3` with `assertion-hints-v1`.
Analyzer build identity now includes the new `src/pragmas.ts`/compiled module.
The Rust observation schema preserves the exact assertion source/method already
exported by JS. The Rust-source experimental adapter supplies absent identities
for its existing observations; its inference/runtime behavior is unchanged.

## Results

- Twelve public-query cases were exercised through **both Node and Vitest**:
  two supported (value and presence), six unresolved, four invalid.
- Cases include discarded results, borrowing another assertion, caught failure,
  unexecuted and mixed-outcome calls, missing/ambiguous targets, malformed and
  unattached comments, and multiple assertions in one statement. Unit tests also
  cover exact columns/methods, outside-project paths, missing capture, incomplete
  calls, and cross-test borrowing.
- All 14 existing explicit native mutation counterexamples retained their
  expected outcomes under both runners. These are specific counterexamples,
  not an exhaustive proof of all hint conclusions.
- Fresh Supergateway archive replay remains **84/100**, with TP 78, FP 4, FN 10,
  TN 6 and two unknown killed mutants in availability-aware scoring. Its 101
  retained observations and candidate summary are unchanged: 78 evident,
  8 partial, 278 unresolved across 364 contractual sites.
- The accepted regression floor is now explicitly 84/100, with the same
  false-kill ceiling of four, true-kill floor of 78, and fixed denominator.
  `asserted-pragmas-calibration-2026-09-09.json` passes that gate. The earlier
  `asserted-pragmas-supergateway-calibration-2026-09-09.json` preserves the
  pre-baseline-update check; its 85/100 gate fails with the same predictions.

Real Supergateway hints in run `run_2e5d74892cf7c201`:

| Comment                              | Target                                         | Passing witness | Result                                     |
| ------------------------------------ | ---------------------------------------------- | --------------- | ------------------------------------------ |
| `tests/httpLifecycleE2e.test.ts:143` | `SessionAccessCounter.inc`, timer cancellation | yes             | unresolved: assertion operand not modeled  |
| `tests/httpLifecycleE2e.test.ts:144` | stateful gateway's `sessionCounter?.inc(...)`  | yes             | unresolved: target not in public inventory |

The complete public hint query returned 3,198 bytes in 2.87 seconds in one local
measurement, while other checks were running. That includes archive reading,
freshness hashing, compiler setup and full source analysis; it is **not** a
benchmark of the small checker alone or a claimed speedup over mutation testing.
No new test-time overhead benchmark was performed. The previous approximately
16.9% accepted paired measurement remains the runtime baseline, not a guarantee.

## Verification

Passed: 17 analyzer unit tests (three opt-in checks skipped in that invocation),
the separately enabled nine-test archive integration with both runners, 28
asserted-engine unit tests, the Rust join parity fixture, three metric tests,
14 runtime tests, 53 exact runtime-asset checks, targeted Clippy with warnings
denied, formatting, syntax and diff checks. This is not a full release matrix.

Reproduce the fresh fixed-cohort gate with a new output path:

```sh
cargo build -p supercov
cargo build -p supercov-engine --example asserted_join
npm --prefix analyzers/typescript run build
node scripts/asserted-supergateway-recapture.mjs \
  /path/to/asserted-coverage-prototype run_2e5d74892cf7c201 \
  /new/path/pragma-calibration.json --check
```

## Next

Complete bounded normal assertion queries and installable analyzer packaging.
Keep the two real e2e dependency/inventory limitations visible; follow-up work
must establish those connections rather than credit the comments. See
`supercov-bugs-pragma-validation-2026-09-09.md` for the remaining findings.
