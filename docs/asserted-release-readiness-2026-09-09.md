# JS/TS assertion-analysis release readiness

Read-only implementation audit, 2026-09-09. This document records findings;
no product code, tests, release version, or publication was changed.

Follow-up: source hints, the accepted 84/100 regression floor, bounded evidence
pagination and clean npm-package integration have since landed locally. See
[the packaging verification record](asserted-packaging-2026-09-09.md) for current
results and the TypeScript 7 compatibility limit. The audit below is historical;
nothing has been published.

## Accepted baseline

The user accepted the fresh Supergateway calibration of 84 correct predictions
out of the fixed 100-mutant cohort and the approximately 16.9% observed test
command overhead. Neither is a remaining target to improve before packaging.
The overhead is one paired measurement, not a universal performance guarantee.
The 84/100 is mutation-outcome agreement, not the percentage of application
behavior proved safe to change. Existing regression scripts still require
85/100 and need an explicitly documented baseline update, without changing
the denominator, hiding unknowns, or relaxing the false-kill guard.

## Working locally

- Ordinary archives feed the JS/TS analyzer and Rust join through
  `supercov runs <run-id> asserted`.
- Public help lists the command. It accepts `--file`, `--site`, `--offset`,
  `--limit`, and `--json`.
- Source/run freshness, archive stability, analyzer identity, and the exact
  passed assertion witness are checked. Existing statement/phase probes stay.
- Candidate evidence, test gaps, and analysis limits remain distinct. The
  public result explicitly has `assertionScore: null` and unverified candidates.
- Node and Vitest archive integrations have been exercised. This is not an
  assertion-analysis release claim for every runner or language.

## Release blockers and incomplete surfaces

### Distribution omits the analyzer

`npm pack --dry-run --ignore-scripts --json` reports 60 files for the root
package and zero under `analyzers/`. The CLI requires
`analyzers/typescript/bin/query.mjs`, so the current npm artifact cannot serve
this query. The npm launcher already supplies `SUPERCOV_PACKAGE_ROOT`; the
missing assets are a separate problem. Direct native installations fall back
to a build-tree path when that variable is absent and need an explicit asset
delivery strategy or an explicit unsupported-command contract.

Merely packing the analyzer's current `dist` and `bin` is insufficient:
`bin/identity.mjs` also reads its source files, tsconfig, and package lock at
query time. The analyzer's own pack manifest excludes those inputs. Choose a
distribution-safe identity contract and test it outside the checkout.

The root pack also includes 25 assertion research/bug documents via the broad
`docs` allowlist. Audit which documentation belongs in the published artifact.

### JSON evidence is not bounded by site pagination

Previously reproduced on fresh run `run_2e5d74892cf7c201`:
`asserted --limit 1 --json` produces 129,943 bytes against the 65,536-byte
response budget and exits with `RESPONSE_TOO_LARGE`. Global tests, attempts,
execution links, and diagnostics repeat regardless of the site page size.
See `supercov-bugs-js-recapture-2026-09-09.md`.

Provide bounded summaries and separately pageable evidence, including cases
where one site's evidence is large. Do not silently discard evidence or just
suggest a smaller page when a one-site page already fails.

### Assertion pragmas are parsed but not implemented end to end

The test analyzer recognizes a leading assertion comment of the form:

```ts
// observes: <file>#<function> [snippet] [via ...]
```

However, the resulting observation lacks `assertionSource`/`assertionMethod`,
so the witness gate rejects it as uninstrumented (or capture-unavailable).
Additionally, `factObservation` drops the pragma target payload, and the Rust
observation schema has no corresponding target field. The current fresh
capture records suppressed pragma observations. This is not a usable escape
hatch and must not be documented as working.

Before enabling it: attach the actual assertion identity, validate target
resolution, diagnose missing/ambiguous targets, preserve target metadata across
the facts boundary, and test the public result. A user declaration is not a
proof: display its provenance separately from automatic analysis, and do not
let a comment hide the site or manufacture an automatically verified result.

### Public workflow and support contract are incomplete

The separate `asserted` query exists; the normal summary, gaps, file/line, and
diff queries do not yet provide the proposed integrated assertion workflow.
The text query prints candidate labels but directs users to JSON for evidence,
which is currently blocked on the full sample. Root user/agent documentation
does not yet describe the feature.

The analyzer currently requires the analyzed project to install the TypeScript
compiler API, including JavaScript projects. Development uses TypeScript 5.8.3;
define and test a supported version range and missing-compiler behavior.
Scope the first release explicitly rather than implying browser, merged-run,
retry, or non-JS/TS analysis support.

### Release gates do not exercise the shipped assertion workflow

The root `check`/`release:check` scripts do not include the new TS analyzer and
archive integration scripts. The native-package consumer fixture copies no
analyzer assets and never invokes the assertion query. The fixed mutation
calibration still uses converted prototype inputs and an internal join; the
ordinary archive adapter's site inventory differs (636 versus 661 on this
sample). Preserve the existing benchmark, but additionally calibrate the public
path on an explicit fixed scope rather than claiming that internal replay is
already packaged-command parity.

## Recommended completion sequence

1. Ship a bounded, useful CLI evidence loop: summary, site/test links, gaps
   versus limits, pagination, help, and user/agent documentation. Existing
   coverage commands can link to the experimental view without conflating the
   two metrics.
2. Complete pragma target/witness plumbing and validation, with declared-link
   provenance and adversarial tests. No extra test-time instrumentation is
   required by this proposal.
3. Package a relocatable built analyzer and its identity inputs; establish the
   compiler dependency contract. Test a clean installed consumer without
   checkout-relative dependencies.
4. Wire analyzer/adversarial/public-query/packed-consumer checks into release
   gates; document the accepted 84/100 baseline and retain unknowns and false
   positives. Compare public-path calibration explicitly. Recheck overhead to
   detect regressions, not to revive the superseded less-than-10% target.

Release scope recommendation: an experimental JS/TS assertion-evidence feature,
not a verified percentage of behavior safe to change. A clean packed consumer
must complete run → summary → site evidence → validated pragma → rerun/query.
No new analysis platform or additional mutation campaign is needed just to
close the integration and packaging gaps above.
