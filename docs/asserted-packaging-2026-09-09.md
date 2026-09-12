# Assertion query release path — local verification, 2026-09-09

Follow-up: [release review](asserted-release-review-2026-09-09.md) corrects the
opt-in parity result below and records the subsequently enabled comparisons.

Implemented in the Supercov checkout on `codex/asserted-js-integration`.
Nothing published, pushed, version-bumped, or committed by this step. Unrelated
Rust work remains untouched. No Supergateway application/test changes and no
runtime-probe changes were made.

## Completed

- `asserted --evidence <JSON-pointer>` exposes shared tests, attempts, execution
  links, diagnostics, limits and per-site/pragma detail. Site filters retain
  pointers into the complete inventory.
- Report schema 2 replaces repeated shared arrays with references. Pages adapt
  to the 65,536-byte JSON envelope budget. Large individual records remain
  navigable, with Unicode-scalar string chunks rather than truncation.
- `--analysis <analysisId>` pins the complete derived document and provenance;
  changed analyses are rejected. Existing source/run, archive and analyzer
  freshness checks remain. Every query recomputes its analysis; no cache yet.
- The npm tarball includes analyzer bin/dist/source, tsconfig and package
  metadata. Its identity checks work from the installed tarball. Research docs,
  fixtures and development dependencies are excluded. Source and compiled-file
  tampering both fail the query.
- Clean-consumer checks use the **installed native binary and installed analyzer**
  with no `SUPERCOV_RUST_BINARY`, checkout package-root or NODE_PATH override.
- Build, prepack, primary CI, compatibility and native artifact workflows now
  include analyzer preparation/verification. The native matrix has a packed
  JS/TS query gate on its executable-host targets; workflow dispatch was not run.
- Public commands, hints, limitations and pagination are documented in
  [the shipped guide](assertion-evidence.md), README and the agent workflow.

## Measured results

| Check                                        | Result                                                                                                                    |
| -------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------- |
| CLI option/paging tests                      | 4 passed                                                                                                                  |
| Analyzer unit/adapter tests                  | 17 passed; 3 opt-in integrations skipped in this command                                                                  |
| Opt-in Node/Vitest archive integration       | 10 passed, including oversized pragma round-trip and 14 native counterexamples on each runner                             |
| Rust assertion join                          | 28 unit tests and 1 frozen parity test passed                                                                             |
| Runtime and embedded assets                  | 14 tests passed; all 53 assets exact                                                                                      |
| Fixed-cohort metric tests                    | 3 passed                                                                                                                  |
| CLI Clippy                                   | all targets, warnings denied, passed                                                                                      |
| Package preflight                            | passed                                                                                                                    |
| Workflow syntax                              | all six modified workflow files parsed                                                                                    |
| Release-mode clean npm consumer, TS 5.8.3    | 16 sites; supported hints, source refresh and tamper rejection passed                                                     |
| Release-mode clean npm consumer, TS 7.0.2    | normal coverage passes; assertion query explicitly rejects incompatible API                                               |
| Root npm artifact                            | 54 files; checked analyzer included, research documents excluded                                                          |
| Supergateway public walk                     | 636 sites, 82 tests, 84 attempts, 84 execution links, 2 hints; no omissions or duplicates                                 |
| Largest response in that walk                | 65,517 bytes, below 65,536                                                                                                |
| Walk cost                                    | 22 queries, about 61.6 seconds; query-time work, not test-time overhead                                                   |
| Historical Supergateway mutation calibration | unchanged: 84 correct / fixed 100, 78 true kills, 4 false kills, 10 false-survival predictions, 2 unresolved killed cases |

Calibration records: [fixed cohort](asserted-packaging-calibration-2026-09-09.json)
and [final public pagination](asserted-pagination-supergateway-final-2026-09-09.json).

The existing packed-native Rust smoke test also passed on macOS arm64. A normal
`npm pack --dry-run --json` executed prepack successfully (54 files, 859,840
unpacked bytes). The release-mode installed assertion check was repeated after
the final pagination metadata change and passed again.

The public archive inventory has 636 sites versus the historical converted
inventory's 661. The 84/100 gate remains a **historical-cohort regression**, not a
new calibration of every packaged-command site. Its denominator was not shrunk.
The 66 public `evident` contractual candidates are not an accuracy percentage.
`assertionScore` is still null; all candidate verdicts remain unverified.

The previously accepted ~16.9% test-command overhead was not rebenchmarked here.
The runtime assets and probes were unchanged; no new test-time instrumentation
was introduced. This step makes no new overhead or cross-platform performance
claim.

## Reproduce

From the Supercov repository root, install the root dependencies and the
analyzer's separately pinned toolchain:

```sh
npm ci --omit=optional
npm --prefix analyzers/typescript ci --ignore-scripts
npm run test:asserted-typescript
npm run test:asserted-integration
npm run test:asserted-package
cargo build --release -p supercov
node scripts/asserted-packed-integration.mjs --binary target/release/supercov
node scripts/native-package-integration.mjs --binary target/release/supercov
node scripts/package-preflight.mjs
```

`asserted-pagination-check.mjs` walks a real project without changing it; supply
its root, immutable run id and a **new** output filename. The recapture script
likewise refuses to overwrite its calibration output.

## Next / release gate

1. Review the combined branch and run the native-platform matrix. Only the local
   macOS arm64 installed package was exercised here; Linux/Windows remain CI
   gates. The complete destructive-clean release campaign was not run.
2. Keep the TypeScript 7 limitation explicit for an experimental release, or
   separately scope an adapter before promising support. The new finding is in
   [the packaging bug log](supercov-bugs-asserted-packaging-2026-09-09.md).
3. Before claiming public-path accuracy parity, calibrate the packaged query on a
   fixed, disclosed scope and explain the 636/661 inventory difference. Do not
   turn the existing 84/100 result into a universal assertion-coverage score.
4. After review and platform gates, choose a release version/changelog and
   publish only with release authorization. No additional scoring platform is
   required to try the current experimental commands.
