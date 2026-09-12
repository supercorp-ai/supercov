# JS/TS assertion fixes and fresh calibration

Follow-up: [JS-ASSERT-005 is now fixed](asserted-js-witness-limits-2026-09-09.md).
This document and its result files preserve the earlier four-bug batch; its
remaining TODO and gap/limit counts below describe that earlier state.

## Outcome and location

Work remains directly in `/Users/domas/Developer/supercorp/supercov`, branch
`codex/asserted-js-integration`, based on `ccfd015`. Nothing was committed,
pushed or published. Previous Rust/extraction changes and unrelated `go.mod` /
`m_test.go` remain intact. No sample application or test source was changed.

JS-ASSERT-001–004 now have passing regressions. Existing JavaScript assertion
phases, statement markers and coverage instrumentation remain unchanged. All
analyzer changes run after testing; the capture repair accepts and preserves a
field the runtime already emitted. This is not a new assertion-score claim.

## Fixes

- **Discarded/transformed results:** follow the right operand of comma/simple
  assignment expressions. Decline general binary/conditional origin guessing
  and mutable scalar initializer guesses. Remove runtime entry-to-return/throw
  enrichment entirely. Execution still exists as separate evidence; it never
  supplies a value observation by itself.
- **Misleading helper names:** resolve the actual declaration rather than giving
  identifiers such as `fetch` a process/network contract by spelling. This is a
  conservative loss of unsupported helper credit, not a generic helper proof.
- **Assertion execution:** source observations require matching file, line,
  column and assertion method in the owning attempt. At least one matching
  phase must exist, and all matching phases must have passed. Missing, caught
  failed and unfinished witnesses do not lend credit. Implicit awaits/pragmas
  without a successful call witness are not trusted substitutes.
- **Node publication:** `ServerRecord` accepts `statementId`, and its runtime
  projection preserves the field. The archive query joins server records only
  with one exactly matching full execution scope; version, run, worker, test id,
  test key, retry and attempt id all matter. Background/foreign/unowned evidence
  remains outside the join and carries a limitation.

The analyzer/CLI handshake is now `source-linked-v2/archive-2`, ABI 1, facts
schema 1, capability `requiresTotal-v1`. Old rule revisions are rejected.
Source freshness and analyzer source/build/compiler identity checks remain.

## Independent native checks

The temporary ordinary JS/TS fixture now has **16 sites**. Both Node and Vitest
produce ordinary queryable archives with the existing probes enabled. Fourteen
specific variants were run under **both uninstrumented runners**, with matching
outcomes. This is a finite regression oracle, not exhaustive mutation accuracy.

| Change | Native Node and Vitest |
| --- | --- |
| Overwritten return 4→9 | survives |
| Ignored callback return 4→9 | survives |
| Return inside caught failing assertion 4→9 | survives |
| Return checked only by unreachable assertion beside a passing same-line assertion | survives |
| Retained comma-right alias return 4→9 | fails |
| Direct exact return 4→9 | fails |
| Discarded comma-left return 4→9 | survives |
| Boolean-coerced return 4→9 | survives |
| Boolean-coerced return 4→0 | fails |
| Production status 201→500 beside unrelated fake `fetch` | survives |
| Return checked only by unexecuted assertion 4→9 | survives |
| Diagnostic suffix alpha→omega, preserving substring | survives |
| Diagnostic substring removed | fails |
| Child exit status 3→0 | fails |

The query no longer credits any of the seven negative dependency/witness sites.
Direct return, retained alias and TypeScript `twice` remain positive controls.
Subprocess tests run during both captured suites too, but their production
effects remain unresolved: native outcomes validate these concrete mutations,
not the analyzer's ability to prove every pipe/capture/timing alternative.

[Saved fixture query and native outcomes](asserted-js-fixes-results-2026-09-09.json).
The older 11-site integration artifact remains unchanged as historical evidence.

## What changed in the real sample comparison

All **2,474 sites** retain their identity, classification, coverage and covering
test sets; all 608 linked test identities remain. No denominator was trimmed.

| Legacy sample | Sites / tests | Observations before→after | Contractual `evident` candidates before→after |
| --- | --- | --- | --- |
| Supergateway | 661 / 81 | 852→0 | 190→0, out of 364 |
| Essential SEO | 1,813 / 527 | 1,697→1,019 | 563→518, out of 936 |

Supergateway's old converted evidence has **no assertion-phase files**. It cannot
meet the new exact-witness requirement. Zero retained observations does not mean
its tests assert nothing. Essential SEO retains 1,019 witnessed observations;
678 old observations are dropped and none is newly invented. Suppressed facts
and unsupported shapes remain visible in diagnostics.

The external regression now requires unchanged inventories/test identities,
reviewed observation deltas and successful exact witnesses for every retained
observation. The old strict fact-parity helper remains tested. Engine-port
parity still uses **original prototype facts and original resolutions**, checking
all sites. Current candidate summaries are separate and intentionally different.
The old 94/100 and 714/827 mutation agreements belong to the reference only;
they are not new accuracy measurements for v2.

Analysis in the diagnostic comparison took approximately 1.1 s and 10.6 s,
excluding prototype regeneration and compilation. These are query-side timings,
not test overhead, and not a benchmark.

## Fresh Supergateway CLI/header runs

After `npm run build`, native and instrumented runs both passed:

- Nine CLI rejection/logging tests; archive `run_f8ae7f30ff5349ca`.
- Two real HTTP health/header/CORS/session tests; archive `run_a80b6a217f192d25`.

Both queries succeed through the public command, expose all 636 currently
discovered source sites and identify the executed test attempts. The 636-site
ordinary archive inventory is a different scope/layout from the legacy 661-site
prototype inventory; the counts are not interchangeable. Both fresh slices have
zero modeled observations: tracing `launchGateway`, `rpc` and `fetch` capture is
still unsupported. Queries report those operand shapes. This is not a finding
that the tested behavior lacks assertions, and these slices are not the full suite.

The runner invocation, from the Supergateway checkout, was:

```sh
TS_NODE_TRANSPILE_ONLY=1 node --test --test-concurrency=1 \
  --experimental-loader ts-node/esm --experimental-test-module-mocks \
  --test-name-pattern='CLI rejects|CLI none logging' tests/gatewayE2e.test.ts
# Same arguments after /path/to/supercov/target/debug/supercov --
# Header slice uses --test-name-pattern='health, headers, CORS'

TS_NODE_TRANSPILE_ONLY=1 /path/to/supercov/target/debug/supercov \
  runs run_f8ae7f30ff5349ca asserted --limit 1 --json
```

Without transpile-only mode, existing helper typing errors also fail the native
runner. We did not fix sample types or activate known-bug TODO tests.

[Saved sample deltas, run identities and timing observations](asserted-js-sample-comparison-2026-09-09.json).

## Performance and verification

No probes were added, removed or changed in this batch. The prior 8.90→8.98 s
measurement was **incremental** assertion-marker overhead, not total Supercov
overhead. The fresh CLI runner durations were 2.007 s native / 2.589 s captured
(roughly +29%); headers were 1.725 s / 1.895 s (roughly +9.8%). These are single
pairs, not a controlled benchmark. They do **not** establish the total <10%
target. Workspace/setup/evidence work is recorded separately; no speedup claim
is made. Query optimization remains deferred.

Verification commands, from the Supercov checkout:

```sh
npm run test:asserted-integration
SUPERCOV_ASSERTED_INTEGRATION=1 npm test --prefix analyzers/typescript
SUPERCOV_ASSERTED_PROTOTYPE=/path/to/asserted-coverage-prototype \
SUPERCOV_ASSERTED_SECOND_PROJECT=/path/to/essential-seo \
  npm run test:parity --prefix analyzers/typescript
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check
npm run test:runtime
npm run test:rust-assets
git diff --check
```

Passed: 469 engine, 21 CLI and 19 contract unit tests; seven Rust assertion
integration tests; both opt-in external comparisons; 14 JS runtime tests; all
53 packaged assets remain exact. The analyzer integration has 21 passed tests,
two external checks skipped in that invocation (run separately), and **one new
explicitly failing TODO**, JS-ASSERT-005. All four original bugs and their added
negative controls pass. Freshness/protocol/build rejection tests also pass.

## Next

1. Repair **JS-ASSERT-005**, recorded separately in
   [the new bug document](supercov-bugs-js-asserted-followup-2026-09-09.md): missing
   witness provenance must reach affected sites as an analysis limit, not a
   claimed test gap. It is documented and TODO-marked, not fixed in this batch.
2. Validate one source- and instance-specific `launchGateway` exit/pipe path with
   both useful positive cases and discarded/transformed/unrelated-object negative
   controls. Keep unsupported cases explicit; do not reinstate name shortcuts.
3. Independently benchmark native, ordinary instrumentation and incremental
   assertion markers with matched alternating runs to establish the runtime
   budget. Keep query time separate.

All sites still have `analysisCertainty: "unverified"`; `assertionScore` remains
null. No claim of perfect accuracy, arbitrary-change safety, full-suite assertion
coverage or release readiness follows from these fixes.
