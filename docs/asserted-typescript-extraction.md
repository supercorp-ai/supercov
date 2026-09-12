# TypeScript asserted-coverage analyzer extraction

Current follow-up: [witness-limit reporting fix](asserted-js-witness-limits-2026-09-09.md),
after [JS/TS fixes and calibration](asserted-js-fixes-2026-09-09.md),
after [archive integration and adversarial checks](asserted-js-integration-2026-09-09.md).
Work now lives in the main `supercov` checkout on `codex/asserted-js-integration`;
the extraction worktree is retained unchanged as a backup. The historical
extraction results below do not certify the analyzer's inference rules. Exact
current-analyzer/prototype parity was intentionally retired when the documented
false positives were fixed; denominator and reference engine-join checks remain.

## Scope and provenance

This implements step 4 of the asserted-coverage handover: a Node package using
the TypeScript compiler API, extracting the observation recognizers, test
linking, runtime-origin enrichment and source-flow analysis. The reference is
the prototype at commit `7d838fa`, whose `resolve-oracles.ts` is left unchanged.
The engine base is `ccfd015` on the statement-attribution branch.

The analyzer is in `analyzers/typescript`. It emits facts, not resolutions. It
does not import the prototype, copy its mutation runner, port the Rust join
back into TypeScript, modify sample tests, or add a public run/query command.
The existing helper adapters, depth bounds and inference limitations are
retained. This is an extraction, not an accuracy-improvement experiment.

## What extraction revealed

The prototype's facts export was not fully independent of its verdicts:

1. Early-exit candidates were recorded only when existing branch evidence was
   insufficient. The package emits the syntactic candidates unconditionally;
   the existing engine condition decides whether they are useful.
2. A cancelled timer's derivation was recorded only when a callback had already
   been resolved at total strength. The package emits callback site ids as a
   `requiresTotal` condition and the engine evaluates it during derivation.
3. The recursive flow cache retained partially built, bounded results. Starting
   traversal from decisions instead of effects changed the resulting paths.
   The extraction preserves the original effect-first traversal order rather
   than changing the inference algorithm.

No existing verdict rule was intentionally broadened. Schema 1 gains the
optional timer condition; the analyzer requires the companion engine change
because older engines silently ignore unknown fields. Public protocol wiring
must establish an explicit version/capability gate before exposing these facts
across independently versioned components.

## Parity contract

The old parity test did not reject an empty engine test set when the prototype
named tests, and it did not compare decision flags or the complete site set.
Those checks are now included. Reason text and human-readable evidence strings
are not part of engine parity; reason kind is.

Fact equality is checked separately, before the join. Object-key order and the
presentation-only project root are ignored. Arrays retain their order. Only
the two candidate enrichments above have an explicit test-only projection;
all existing fields must match. Additional early-exit candidates can be
projected away only where the reference already has strong branch evidence.

At initial verification:

| sample        | sites | linked tests | evident / contractual | candidate enrichments     |
| ------------- | ----: | -----------: | --------------------: | ------------------------- |
| Supergateway  |   661 |           81 |             190 / 364 | 2 timer candidates        |
| Essential SEO | 1,813 |          527 |             563 / 936 | 106 early-exit candidates |

Both fact comparisons and the stronger engine verdict comparisons pass. The
first extracted-analysis timings were approximately 1.1 seconds and 10.0
seconds respectively, excluding prototype regeneration and Rust compilation;
these are individual diagnostic runs, not a performance benchmark.

The reference comparisons remain 94/100 and 714/827. They use stored mutation
fixtures; the broad Essential SEO fixture predates its added tests. No mutation
campaign or fresh sample-suite evidence was collected for this extraction.

Final checks on the extraction worktree:

- TypeScript build and all 9 package tests passed, including both external
  parity runs (none skipped in that invocation).
- `cargo test --offline --workspace` passed: 451 engine unit tests, 20 CLI
  unit tests and 19 contract unit tests. The external join comparisons above
  were run with their required inputs, separately from the default opt-in
  integration checks.
- `cargo clippy --offline --workspace --all-targets -- -D warnings` passed.
- `cargo fmt --all -- --check` and `git diff --check` passed.
- All 14 JavaScript runtime tests passed; all 53 packaged runtime assets match.
- The standalone package's dry-run pack contains the built JavaScript, type
  declarations, CLI and README, with no runtime dependency on the prototype.

No sample application code or tests were changed. No commit, push or publish
was performed.

## Deferred work and known limits

- The package consumes the converter's inventory/evidence layout. Wiring raw
  archives, source-site identity and CLI/run storage remains separate work.
- Assertions and flow are still inferred structurally; a matching fact or
  passing parity test is not proof that every behavior change is detected.
- Depth-bounded flow remains traversal-order-sensitive. Preserve the current
  order until a separately calibrated change replaces it.
- The helper-name adapters inherited from Supergateway remain in the analyzer.
  They are not a generic proof of arbitrary subprocess capture semantics.
- Rust, Python and Ruby analyzers and E2E mutation ground truth remain outside
  this step.
- The package is private and unpublished. Shipping it, accepting independent
  fact producers and choosing a supported TypeScript-version range are not
  implied by this milestone.
