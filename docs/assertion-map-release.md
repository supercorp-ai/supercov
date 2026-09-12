# JS/TS assertion map release readiness

The feature passes the local gates below. A release-base integration remains:
this experimental branch still declares 0.0.42, while the npm registry and
remote tags report 0.0.44 (checked on 2026-09-12). Do not publish this branch as
0.0.42 or choose an already published version. Reconcile it with current main,
then run the native artifact/release workflow and choose an unused version. No tag, push or publication was performed by this investigation.
Ruby/Python adapter work is outside this release scope.

## Local verification, 2026-09-12

Environment: macOS arm64; Node 24.18.0; Rust 1.95.0. Runner matrix uses
TypeScript 7.0.2, Vitest 4.1.11, Jest 30.5.1 and Playwright 1.62.1. The six-case
assertion-map matrix also passed locally under Node 22.23.1.

| Gate | Result |
| --- | --- |
| Rust formatting and clippy with warnings denied | Passed |
| Workspace Rust tests | 481 passed |
| JavaScript runtime tests | 22 passed |
| Assertion map lifecycle | Init, exact source anchors, edit/review, carry, dirty-state persistence and selective repair passed |
| JS/TS assertion map matrix | Node ESM/CJS/native TS; Vitest TS with parameterized cases; Jest CJS named imports with parameterized cases; Playwright TS with a custom fixture module |
| Awaited operands and rejection assertions | Exact passing site identity and statement credit passed in all six configurations |
| CLI and schema | Generated editor schema agrees with Rust; nested JSON error paths, inventory/source paging, completion/evidence/threshold/freshness gates passed |
| Existing JS regressions | Node, Vitest, Jest, multi-worker Playwright, generic tsc/esbuild, webpack/SWC and host-loader suites passed |
| Native npm package | Actual macOS arm64 release binary packed and installed; JS/TS runs, map creation, validation, review, gates, bundled docs and schema passed |
| Packaging | Package preflight, 55 embedded asset copies and all eight binstall metadata targets passed |

A real Supergateway `tests/websocketLifecycle.test.ts` trial passed normally and
under Supercov using `node --import tsx --test --experimental-test-module-mocks`.
Run `run_bc08d137feebc233` has passing identities for all five assertions,
including the awaited WebSocket broadcast check. All 790 measured statement
anchors resolve against frozen source. This is a single-test trial, not a
completed suite map: inventory contains 544 project assertion sites, and entries
remain unmapped. It does not establish an assertion percentage for Supergateway.

The checkout's `ts-node/esm` command failed even without Supercov. Diagnostic
mode exposed TypeScript errors; that diagnostic run is not treated as a clean
pass. The clean real-project evidence above uses `tsx` explicitly.

## Reproduce the feature checks

```sh
npm ci
npm run check
npm run test:assertion-maps:js
npm run test:native-package
```

`npm run check` includes the map lifecycle and JS/TS matrix. The dedicated JS
command is useful for a focused rerun. `npm run test:native-package` also checks
the native release-set machinery. Regenerate schema/assets only after editing
their sources: `npm run sync:assertion-schema` and `npm run sync:rust-assets`.

The `JavaScript assertion maps` pull-request workflow checks Node 22 and 24 on
Linux. The existing native artifact matrix now exercises packed JS/TS assertion
maps on every host marked `packedInstall`. These workflow changes are in source;
the remote matrix has not been dispatched or observed passing in this task.

## Before publishing

1. Review this branch and the agent-facing contract. Preserve the old inference
   checkpoint. The `runs <run> asserted` command is removed; ordinary assertion
   phase links no longer earn credit. Legacy confidence JSON fields remain zero
   for compatibility. Agent maps are optional and require frozen run inputs.
2. Reconcile the feature with current main (`1f393c4` at audit time), preserving
   published fixes while removing the superseded semantic analyzer. The Node
   registration/worker identity, source-path parsing and lazy qualified stack
   fallback fixes from 0.0.44 have been ported into this branch with their
   regression tests. A full main/release integration is still outstanding.
3. Confirm the intended support claim: an agent-authored assessment with exact
   occurrence/execution evidence and mechanical bookkeeping checks. Optional,
   nested or unrecognized assertions can lack individual identities. Generated
   sources, browser transports and unusual loaders need representative map
   trials before broader support is advertised. No gate proves the causal graph
   or mutation resistance.
4. Let the Node 22/24 PR jobs and full native artifact matrix pass on the final
   commit. A local macOS package test does not replace Linux/Windows validation.
   Run the repository's full release checks for any broader changes bundled into
   this release; this task's adapter work concentrated on JS/TS.
5. Use `npm run release:bump -- <current-branch-version> <unused-next-version>` with the chosen version, check the
   resulting package versions/lockfiles, move the intended Unreleased notes into
   the new version section, then follow the existing
   tag and trusted-publishing workflow. Do not reuse old native binaries: the
   assertion runtime, compiled schema and bundled docs changed together.

For users and coding agents, the entry points are `supercov docs assertion-agent`,
`supercov docs assertion-maps`, `supercov assertions schema`, and
`supercov runs <run> assertions --help`.
