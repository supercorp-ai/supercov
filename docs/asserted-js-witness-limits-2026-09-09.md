# JS-ASSERT-005: preserve missing-witness provenance

## Outcome

JS-ASSERT-005 is fixed in the main Supercov checkout on
`codex/asserted-js-integration`. The former TODO now passes. This change corrects
reporting; it does not increase the amount of behavior we claim is asserted.
Nothing was committed or pushed. Existing dirty work was preserved.

No runtime probes, sample application code or sample tests changed. The original
assertion phases and statement markers remain. All new work happens during
analysis/querying; no new test-time instrumentation was added.

## Representation and meaning

The analyzer emits optional, typed `tests[].witnessIssues`, separate from
`observations`. Rejected observations are retained only as provenance for limits,
never fed back into the positive-observation join.

| Issue kind | What the captured evidence establishes |
| --- | --- |
| `capture-unavailable` | This test's phase evidence was missing or its loading was disabled. Applies to its covered sites, including when no operand could be modeled. |
| `call-not-recorded` | Phase data exists, but no call matches this exact file/line/column and method. It does **not** distinguish non-execution from incomplete capture. |
| `call-incomplete` | Matching call records exist but have no recognized terminal passed/failed status. |
| `mixed-call-outcomes` | Matching calls have different statuses; the previous rule declines all their value credit. |
| `call-failed` | Every matching recorded call failed. Kept as rejected-witness provenance, not automatically reclassified as missing-capture uncertainty. |
| `uninstrumented-observation` | A modeled observation such as an implicit await has no instrumented assertion call identity. |

All matching calls must still have passed before an observation receives credit.
An empty phase file differs from an absent file. No absence-of-records case is
renamed “known not executed”; the current capture does not certify that inference.

The Rust join uses its existing boundary, sink and mock rules, plus recorded
flow/control/derivation edges, to conservatively identify affected sites. Only
tests covering the root site can contribute issues. Cycles are bounded by a
visited set. This is a conservative relevance analysis, not a new dependency
proof. A suppressed absence assertion can be relevant through a decision edge
even when the target effect never ran.

The final reporting pass adds `candidate.witnessIssues`. Where relevant uncertainty
would otherwise be reported as a missing-assertion gap, the reason becomes
`limit:assertion-witness`. Known not-reached sites and untaken decision outcomes
remain execution gaps. Existing internal-state/operand limits remain limits.
Successful independent evidence still counts; a missing witness elsewhere does
not downgrade it.

**Unchanged:** candidate status, strength, credited test set, decision flags,
observations, site identities, coverage and denominator. Tests check this on
every site in both real samples by projecting away only the reporting fields.

The public handshake is now `source-linked-v3/archive-2`, with ABI/facts schema 1
and capabilities `requiresTotal-v1` plus `assertion-witness-issues-v1`. Old rule
revisions and a missing witness capability are rejected instead of silently
dropping these limits. Legacy internal facts and Rust frontend facts omit the
new optional field and preserve their existing behavior.

## Calibration

All 2,474 sample sites and 608 linked test identities remain. Positive observations
remain 0 for the legacy Supergateway capture and 1,019 for Essential SEO. The
old Supergateway capture has no phase files; zero credited observations is **not**
a statement that its tests assert nothing.

| Sample | Contractual test-gap reasons | Contractual analysis-limit reasons | `evident` candidates |
| --- | --- | --- | --- |
| Supergateway | 298 → 13 | 66 → 351 | 0 → 0 |
| Essential SEO | 357 → 334 | 61 → 84 | 518 → 518 |

Thus 285 + 23 contractual sites are no longer mislabeled as test gaps. Across
all classifications, 483 Supergateway and 23 Essential SEO reason fields change.
The remaining gap counts are current model output, not independently proven
global test deficiencies. Scope and other analysis limitations remain.

[Saved summaries, changed site IDs and native fixture results](asserted-js-witness-limits-results-2026-09-09.json).
Previous artifacts are retained unchanged. Exact engine/prototype parity still
uses original prototype facts; no unsafe observation was restored to preserve a
historical count.

The ordinary-archive Node and Vitest integration also passes: unreachable and
same-line-unreachable assertion cases now carry `call-not-recorded` provenance
and an analysis-limit reason. The caught-failure case remains a rejected failed
witness, not missing-capture evidence. Positive direct/alias/TypeScript cases
retain credit; discarded, overwritten and misleading-helper cases remain
uncredited. All 14 native variant outcomes remain identical under both runners.

## Verification

```sh
npm run test:asserted-integration
SUPERCOV_ASSERTED_INTEGRATION=1 npm test --prefix analyzers/typescript
SUPERCOV_ASSERTED_PROTOTYPE=/path/to/asserted-coverage-prototype \
SUPERCOV_ASSERTED_SECOND_PROJECT=/path/to/essential-seo \
  npm run test:parity --prefix analyzers/typescript
cargo test --workspace --quiet
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check
npm run test:runtime
npm run test:rust-assets
git diff --check
```

Passed: 475 engine unit tests (six new witness-reporting tests), 21 CLI tests,
19 contract tests, seven Rust assertion integration tests, 14 JS runtime tests,
all 53 packaged runtime assets, both external sample comparisons, formatting and
lint. Analyzer integration: 23 passed, two external comparisons skipped in that
invocation and run separately, **zero TODOs**. Missing/wrong-location/wrong-method,
unfinished, failed and mixed-status phase cases have separate parser checks.

This is not a new overhead benchmark. The prior total <10% target remains
unestablished; no runtime work was added here. Query-side graph traversal and
larger provenance output may cost more, as permitted for this step. Large shared
evidence pagination remains a separate limitation.

## Next

Validate one source- and instance-specific Supergateway `launchGateway` path,
starting with its exit status and then its pipe capture. Add useful positive
cases alongside discarded/transformed/unrelated-process negative controls before
crediting that helper. Do not reinstate name-based shortcuts. The real CLI/header
assertion dependencies remain unresolved; JS-ASSERT-005 repairs their reporting
infrastructure, not their helper analysis.

Separately, use controlled alternating native/instrumented runs to establish the
runtime overhead budget. `assertionScore` remains null and all candidates remain
unverified; this reporting fix proves neither arbitrary-change safety nor a
global assertion percentage.
