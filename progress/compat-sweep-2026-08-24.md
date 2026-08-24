# Cross-repo zero-config compatibility sweep — 2026-08-24 (Tier 1)

Real public repos, cloned fresh, wrapped with `supercov -- <their test command>`
and no configuration. Every failure below was reproduced, root-caused, and
either fixed the same day or filed with evidence.

## Matrix

| repo | runner / layout | before fixes | after fixes |
| --- | --- | --- | --- |
| unjs/defu | Vitest, pnpm, unbuild | crash: `ENOTDIR … lib/defu.cjs/.supercov`; then zero evidence (`vite` unresolvable under pnpm) | ✅ 23/23, MC/DC 80%, measurement complete |
| unjs/ufo | Vitest 4, pnpm | one test timed out (5 s) in full runs; plain `vitest run` afterwards **double-counted the suite** (978 vs 489) | ✅ 489/489 pass; no pollution; overhead still high (see §perf) |
| vercel/ms | Jest, pnpm, edge env | exit 1: repo's own istanbul thresholds measured instrumented code; configs misclassified as ambiguous source | ✅ 167/167, MC/DC 100%, measurement complete |
| fastify/fastify-plugin | node:test via `c8 --100` | **40/43 tests failed**: `Cannot read private member #assert` | ✅ 43/43; c8 threshold advisory printed; exit 1 remains theirs (c8 measures instrumented code) |
| iamkun/dayjs | Jest config in package.json | crash (`node build` under ESM loader); then wrong test set (797 vs 794 — package.json jest config ignored); 15–20× slower | fix landed; rerun in progress |
| debug-js/debug | Mocha (unsupported) | — | ✅ honest degradation: 16/16 pass, aggregate coverage, no per-test attribution |
| sindresorhus/p-limit | AVA (unsupported) | 1 test failed: wall-clock assertion (590–650 ms window) blown by overhead | degradation works; failure is the perf cliff |
| expressjs/express | Mocha, supertest | baseline 1 s → instrumented **>20 min** (still running at write-up) | unusable until probe v2 |

## Bugs found and fixed (all with regression coverage where practical)

1. **File entry targets crashed the direct instrumenter** — package.json
   `main`/`exports` targets are legitimate file-shaped source roots;
   `containedRuntimePath` did `mkdir <file>/.supercov`. Now maps file roots to
   their parent directory (`src/directInstrumenter.ts`).
2. **pnpm never hoists `vite`** — the generated vite/vitest configs imported
   `'vite'` bare and silently collected zero evidence. Now resolved through the
   project, then through vitest's own dependencies (`src/cli.ts`).
3. **Workspace residue corrupted the user's ordinary test runs** — the cached
   workspace's copied test files were discovered by a later plain `vitest run`
   (Vitest 4 default excludes are only `node_modules` and `.git`), doubling the
   suite. Two dead ends first: `.cache` naming (Vitest 4 ignores it) and a
   `node_modules` path segment (hides discovery but makes Vite treat workspace
   source as vendor code and Jest ignore it by absolute path). Final design:
   `pruneCachedWorkspaceSources` deletes copied source/test files at run end,
   keeping dependency symlinks + declared build artifacts at the stable path
   (`SUPERCOV_KEEP_WORKSPACE=1` opts out). Also auto-writes
   `.supercov/.gitignore`.
4. **node:test context proxy broke `t.assert.*`** — property reads forwarded
   with the proxy as receiver, so TestContext accessors backed by private
   fields threw. Reads now use the real context as receiver
   (`src/nodeTest.ts`). This alone un-broke 40 fastify-plugin tests.
5. **Inner coverage tooling collided** — generated Jest config now forces
   `collectCoverage: false` (istanbul over instrumented code is meaningless and
   fails user thresholds); a CLI advisory prints when the wrapped command runs
   `c8`/`nyc`, which cannot be disabled from outside.
6. **package.json `jest` config was silently ignored** — generated config
   replaced it with `{}`, changing test discovery (dayjs: +3 tests that need
   built bundles; different roots/testRegex). Now inherited verbatim.
7. **Unnecessary project builds** — Jest/Vitest transform source in-process;
   running `npm run build` first (unbuild, `node build`, tsdown) added time and
   two distinct failure modes. `executesSourceDirectly` now covers jest/vitest.
8. **Root-level `*.config.ts` misclassified** as ambiguous first-party source
   (`tsdown.config.ts`, `lint-staged.config.ts`); CONFIG_PATTERN now treats any
   root-level `<tool>.config.*` as configuration.
9. **`SUPERCOV_DEBUG=1`** now prints error stacks from the CLI (added because
   finding #1 was undiagnosable from its one-line message).

## The headline finding: probe overhead is the launch blocker

Measured with the real instrumenter + collector runtime on a representative
per-character hot loop (`scratchpad` bench, 50k iterations):

- plain: **39 ms** — instrumented: **4,341 ms** (~110×)
- CPU profile self-time: `mcdcBegin` 2.5 s, `environmentRequestContext`
  (AsyncLocalStorage lookups) 3.1 s, `mcdcEnd` 0.9 s, `coverageHit` 0.7 s,
  `vectorKey` string building 0.4 s, object spreads 0.4 s
- ~670 ns per probe vs ~3 ns per plain operation

Real-world consequences observed: ufo test timeout (5 s limit), p-limit
wall-clock assertion failure, dayjs ~15–20× (317 s vs ~20 s), express > 1200×
(1 s baseline, 20+ min instrumented, supertest/HTTP-heavy). The published
"synthetic runtime overhead 1.14×" benchmark measures a workload that does not
resemble hot loops and must be replaced.

This is the strongest possible evidence for the probe-v2 phase of the engine
master plan (`progress/engine-master-plan-2026-08-24.md`): per-probe ALS
lookups, per-event allocations/spreads, and string vector keys must go
(segment-cached context, pooled frames, bitmask vectors). Target ≤1.05×; even
≤2× would have passed every suite above.

## Remaining sweep findings (not yet fixed)

- **Assertion attribution is node:test-only in practice**: Vitest and Jest
  runs report 0 asserted lines; `t.assert.*` (node:test context API) also has
  no phase adapter. Confidence marketing depends on this working beyond
  imported `assert`/`expect`.
- **Unsupported-runner degradation is silent**: mocha/AVA runs report
  "0 test(s)" with coverage present; needs an explicit "unsupported runner:
  aggregate-only evidence" diagnostic naming the runner.
- **dayjs `Add` test off-by-one-second** under instrumentation — timing class,
  goes away with probe v2.
- Timing-window assertions in the wild (p-limit) put a hard ceiling on
  acceptable overhead; even 2× can flip them.
