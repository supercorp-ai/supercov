# JS/TS assertion map release readiness

Release candidate **0.0.45** integrates main `1f393c4` and replaces the old
assertion analyzer with agent-authored maps. The released Node registration,
worker-attempt and source-location safeguards are retained. Candidate code is
commit `0b6394a3c37a51a1d332c25b7e42789ef7fd4393` on
`codex/agent-assertion-maps`. This checklist records preparation and validation;
no release tag or publication has been made.

## Verification, 2026-09-12

Environment: macOS arm64, Node 24.18.0, Rust 1.95.0. The JS/TS runner matrix
uses TypeScript 7.0.2, Vitest 4.1.11, Jest 30.5.1 and Playwright 1.62.1.

| Gate | Result |
| --- | --- |
| Full local `npm run release:check` | Passed, including existing language adapters, runner/browser matrices, packed npm, MC/DC oracle and transform benchmark |
| Final `npm run check` after report optimization | Passed: formatting, clippy, 483 Rust tests, 22 runtime tests, map lifecycle, six JS/TS configurations, schema, assets and package preflight |
| [Linux Node 22/24 CI](https://github.com/supercorp-ai/supercov/actions/runs/34708624833) | Both passed on corrected candidate code |
| Final local `npm run test:native-package` | Passed: actual macOS arm64 package installation, JS/TS maps, all 14 bundled guides, generated schema and release-set corruption gates |
| [Native platform matrix](https://github.com/supercorp-ai/supercov/actions/runs/34708623064) | All eight targets and the complete release-set verification passed |

The native matrix covers macOS arm64/x64, Linux glibc and musl arm64/x64,
and Windows arm64/x64. Installed-package hosts exercise JS/TS assertion maps;
musl binaries exercise the same map workflow in actual Alpine Node consumers.
The release-set job validates the complete native package, wheel and gem set.
Ruby/Python feature development is outside this release scope; existing adapter
regressions and distribution gates remain enabled.

The first native matrix caught a Windows-only frozen-input path mismatch:
canonical paths carried a verbatim prefix while discovery paths did not. The
collector now uses the existing workspace path normalization for both paths,
and deduplicates relative/absolute references to the same source. A regression
checks both Windows spellings plus rejection of inputs outside the project.
Both Windows path regressions passed; the corrected candidate passed on all
eight targets.

The map tests cover exact assertion identity, awaited operands, same-test
execution, explicit statement credit, dirty-state persistence, selective review,
carry across runs, invalid JSON diagnostics and scope/freshness/completion gates.
The regular summary is checked before mapping, after review, after a map edit,
after malformed JSON, and after repair. Invalid or changed maps cannot reuse an
old percentage.

A real Supergateway `tests/websocketLifecycle.test.ts` trial passed normally and
under Supercov using `node --import tsx --test --experimental-test-module-mocks`.
Run `run_a7a997bb35c093ec` records all five exact passing assertion identities,
including the awaited broadcast check; all 790 measured statement anchors
resolve. Its 544-site inventory remains unmapped. This is execution/identity
validation, not a completed assertion assessment for Supergateway. That
checkout's `ts-node/esm` command also failed without Supercov; the clean trial
uses `tsx` explicitly.

## Percentage display and cost

`supercov runs <run>` automatically includes **Assertions** beside Lines,
Branches and MC/DC when `assertions.json` exists. The JSON path is
`data.assertionCoverage.summary.statements.percentage`. The ratio is the union
of credited measured statements divided by all measured statements in the
archived run. Structural filters do not change this separately labelled score.
Incomplete maps, dirty flows and review problems remain visible. A zero
statement denominator displays `n/a`.

On the release binary, a synthetic fixture with 500 passing assertions, 500
mapped flows, 500 measured statements and a 529 KB map reported 100% (500/500).
Seven complete summary queries had a median of **118 ms** (117–133 ms), compared
with **15 ms** before adding the map. This is a local fixture measurement, not a
latency guarantee for arbitrary repositories. The query recalculates from the
map, state and local archive; it neither reruns tests nor calls a model. Exact
assertion witnesses are indexed once per query to avoid repeated inventory
scans. No cached percentage can become stale after a map edit.

## Reproduce and publish

```sh
npm ci
npm run release:check
npm run test:native-package
```

The candidate already uses the unused version 0.0.45 in Cargo and npm manifests,
lockfiles and native package URLs. Public guides and the JSON schema are
packaged, and 67 embedded assets are checked for exact agreement. The old
TypeScript analyzer and its build/install steps are removed.

The native matrix above produced and verified the complete 0.0.45 artifact
set from the corrected candidate. Publication remains a separate action:
recheck that the version is unused, select the release commit on main, then
follow the existing tag and trusted-publishing workflow. The validated
workflow run can supply the 0.0.45 native artifacts. Do not reuse 0.0.44 binaries.

The support claim is an agent-authored assessment with mechanical bookkeeping
and exact execution evidence. Optional, nested or unrecognized assertion forms
can lack individual runtime identity. Generated/browser transports and unusual
loaders need representative map trials before broader claims are made. No
percentage proves the semantic graph, mutation resistance or safe arbitrary
changes.

User and agent entry points: `supercov docs assertions`,
`supercov docs assertion-agent`, `supercov docs assertion-maps`,
`supercov assertions schema`, and `supercov runs <run> assertions --help`.
