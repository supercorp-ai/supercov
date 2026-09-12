# JS/TS archive-to-query integration and adversarial checks

Historical discovery record. The four bugs below have since been fixed on the
same branch; see [the follow-up calibration](asserted-js-fixes-2026-09-09.md).
The original 11-site artifact is retained unchanged. Current rules are
`source-linked-v2/archive-2`; current analyzer conclusions intentionally no longer
have exact parity with the unsafe prototype. The sections below describe the
pre-fix state, not the current release/readiness claim.

## Location and scope

Work now happens directly in `/Users/domas/Developer/supercorp/supercov` on
`codex/asserted-js-integration`, based on `ccfd015`. The earlier extraction and
Rust experiments were copied byte-for-byte from the
`supercov-asserted-typescript-analyzer` worktree before these changes. That
worktree is retained as a backup. The main checkout's unrelated `go.mod` and
`m_test.go` were preserved. Nothing was committed, pushed or published in this step.

No Supergateway or Essential SEO application/test files changed. No JavaScript
test-time instrumentation changed: existing assertion phases, statement markers
and logical outcomes are kept. All new work is post-run.

## What is connected

```sh
# Build in the Supercov checkout:
npm ci --ignore-scripts --prefix analyzers/typescript
npm run build --prefix analyzers/typescript
cargo build -p supercov

# Run in the project that produced the coverage archive:
/path/to/supercov/target/debug/supercov runs <run-id> asserted --json
/path/to/supercov/target/debug/supercov runs <run-id> asserted --file src/core.mjs --json
/path/to/supercov/target/debug/supercov runs <run-id> asserted --site <id> --json
```

The project needs a TypeScript installation exposing the compiler API, including
when its application sources are JavaScript. No prototype checkout, manually
prepared inventory, LCOV directory, or converted-input command is needed.

`crates/supercov-cli/src/asserted_query.rs` selects the stored run, uses the
existing strict Rust archive reader, validates typed per-test records, discovers
effect sites with the engine, and invokes the first-party analyzer. The TypeScript
adapter supplies source positions, matches archived decisions to source, extracts
decision atoms, and translates runtime snapshots in memory. It uses the existing
source-flow analyzer. Rust owns the facts-to-candidate join.

The old converted-input API remains for extraction parity, not as a prerequisite
of the public query. This first query path is experimental and local-development
only; the npm/native release packaging is not wired for it yet.

## Freshness and version contract

- Compare current source, test, dependency-manifest/lockfile, configuration and
  instrumenter fingerprints with the stored run, both before and after analysis.
  Missing freshness information or a mismatch stops the query. A post-run engine
  implementation change alone does not invalidate otherwise matching captured
  evidence; the analyzer identity is independent.
- Hash the evidence archive before/after the query to reject concurrent changes.
  No source files are extracted from archive paths onto disk. Local source/test
  paths must stay inside the project.
- The analyzer handshake requires ABI 1, facts schema 1, rule revision
  `prototype-7d838fa/archive-1` and `requiresTotal-v1`. Unknown combinations fail,
  avoiding old engines silently treating conditional timer candidates as unconditional.
- The analyzer build stamp binds source inputs and compiled JavaScript. Changes
  to either require a rebuild before the query imports the analyzer. Results
  record these hashes plus the loaded compiler version and compiler-entry hash.
- Derived results are not cached or reused. Compiler/analyzer identity is checked
  again after analysis. This does not certify every installed dependency byte or
  eliminate adversarial change-and-restore races; it is the existing run-fingerprint
  freshness model plus explicit analyzer identity, not a semantic proof.

## What the query means

Each row carries the source site, input facts and join result under `candidate`,
with `analysisCertainty: "unverified"`. The query separately exposes captured
execution links, test/attempt identities, scope, measurement limits and analyzer
diagnostics. Assertion-phase execution links explicitly mean `execution-only`.

`assertionScore` is **null** and `semanticallyVerifiedSites` is **0**. That does
not mean tests assert nothing. It means no independent semantic proof checker has
certified these candidates. In particular, prototype `evident` must not be read
as “every relevant change here would fail a test.”

The current denominator is engine effect sites plus supported archived decision
atoms. Logical-value operand sites and type-dependent effect classification are
not complete. Excluded/review sites and measurement limits remain visible; this
is not a percentage of all code. Unsupported decision/source layouts fail closed.

Per-site output has `--offset`/`--limit` pagination. Large shared evidence sets can
still exceed the standard agent-response budget; the query returns an explicit
size error rather than truncating evidence or claiming a complete result.

## Calibration result

The ordinary Vitest archive successfully yields **11 sites** for the new JS/TS fixture.
Repeated queries are deterministic, and per-site selection works. Captured
assertion-phase evidence is present, demonstrating that the existing probes were
retained. The corresponding native Node and native Vitest suites both pass.

Nine concrete variants were executed under **both** uninstrumented runners:

The [saved query and native outcomes](asserted-js-integration-results-2026-09-09.json)
retain the site/attempt evidence, source and analyzer fingerprints, and all nine
outcomes from the 11-site run. They are a calibration artifact, not a golden
endorsement of the candidate verdicts.

| Change                                            | Native Node | Native Vitest |
| ------------------------------------------------- | ----------- | ------------- |
| Exact returned 4 → 9                              | fails       | fails         |
| Discarded returned 4 → 9                          | passes      | passes        |
| Boolean-coerced 4 → 9                             | passes      | passes        |
| Boolean-coerced 4 → 0                             | fails       | fails         |
| Unobserved status 201 → 500 beside fake fetch     | passes      | passes        |
| Return checked only by unexecuted assertion 4 → 9 | passes      | passes        |
| Diagnostic suffix alpha → omega                   | passes      | passes        |
| Diagnostic token removed                          | fails       | fails         |
| Child exit status 3 → 0                           | fails       | fails         |

This is a finite, source-grounded oracle, **not** a mutation-equivalence score or
proof for all variants. The captured Vitest slice excludes the subprocess test;
its pipe/exit assertions are calibrated by the native suites only. The adapter
does not give those unjoined process effects assertion credit.

Three existing inference rules give false value credit; node:test publication
also rejects the current statement-marker field. These remain failing TODO
regressions, documented in [the new bug record](supercov-bugs-js-asserted-integration-2026-09-09.md).
The unsafe rules were not silently changed to make the calibration look better.

## Verification and performance

```sh
npm run test:asserted-integration
SUPERCOV_ASSERTED_INTEGRATION=1 npm test --prefix analyzers/typescript
SUPERCOV_ASSERTED_PROTOTYPE=/path/to/asserted-coverage-prototype \
SUPERCOV_ASSERTED_SECOND_PROJECT=/path/to/essential-seo \
  npm run test:parity --prefix analyzers/typescript
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
npm run test:runtime
npm run test:rust-assets
```

Verified: 469 engine, 21 CLI and 19 contract unit tests; seven Rust assertion
integration tests; 14 JS runtime tests; 53 synced runtime assets. Analyzer tests
pass with four explicitly failing TODO regressions. The two external extraction
and join parity checks still match all **2,474 sites** (661 + 1,813); this preserves
old behavior and does not establish soundness. Freshness tests reject changed
application source, tests and package metadata; protocol/build tests reject
unknown revisions/capabilities and stale or modified analyzer builds.

No execution-overhead benchmark was repeated here. This step adds no test-time
probes or analysis. The earlier 8.90 → 8.98 second result measured incremental
assertion instrumentation, not total Supercov overhead against native execution,
and must not be promoted to a general <10% guarantee. Query optimization remains
deferred as requested.

## Next

First review the four concrete bugs and decide when to fix them. Then require
the corresponding TODOs to pass without weakening their native oracles. The
node:test schema/ownership repair unlocks an ordinary Supergateway run; validate
its subprocess capture end to end before expanding claims. Broader decision-site,
runner/attempt and output-pagination support and release packaging remain open.
Do not derive an assertion percentage from these candidates yet.
