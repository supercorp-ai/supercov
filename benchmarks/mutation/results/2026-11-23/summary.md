# Mutation benchmark 2026-11-23

StrykerJS 10.1.1, mizchi/mutator 0.5.0, Node 24. Statuses: K Killed, T Timeout, S Survived, NC NoCoverage, RE RuntimeError, CE CompileError, I Ignored, P Pending. Score = (K+T) / (K+T+S+NC). Sharded runs: wall and CPU seconds are summed over the shards.

## Cold full runs

| project | tests | tool | wall s | CPU s | peak RSS MiB | mutants | statuses | score | shared mutants | score on shared | vs ground truth: missed kills / extra kills (of compared) |
|---|---:|---|---|---:|---:|---:|---|---:|---:|---:|---|
| defu | 23 | StrykerJS | 3.4 | 21 | 255 | 103 | K80 S15 T7 NC1 | 84.5 | 76 | 80.3 | 0 / 0 (of 103) |
| defu | 23 | mutator | 3.9 | 15 | 617 | 107 | K67 CE25 S14 NC1 | 81.7 | 76 | 80.3 | 0 / 0 (of 76) |
| scule | 63 | StrykerJS | 2.9 | 16 | 261 | 81 | K75 S5 NC1 | 92.6 | 62 | 90.3 | 0 / 0 (of 81) |
| scule | 63 | mutator | 6.8 | 22 | 670 | 153 | K98 CE39 S12 NC4 | 86.0 | 62 | 90.3 | 0 / 0 (of 62) |
| cookie-es | 134 | StrykerJS | 10.4 | 119 | 322 | 725 | K598 S92 T32 NC3 | 86.9 | 518 | 83.2 | 0 / 0 (of 725) |
| cookie-es | 134 | mutator | 17.7 | 92 | 635 | 727 | K441 CE168 S86 T29 NC3 | 84.1 | 518 | 83.2 | 0 / 0 (of 518) |
| pathe | 483 | StrykerJS | 10.8 | 128 | 394 | 524 | K452 S63 T4 NC3 RE2 | 87.4 | 399 | 85.0 | 0 / 0 (of 522) |
| pathe | 483 | mutator | 18.9 | 109 | 952 | 547 | K365 CE116 S59 T4 NC3 | 85.6 | 399 | 85.0 | 0 / 0 (of 399) |
| ufo | 489 | StrykerJS | 16.9 | 208 | 619 | 1011 | K823 S112 NC64 T12 | 82.6 | 779 | 79.7 | 0 / 0 (of 1011) |
| ufo | 489 | mutator | 33.1 | 195 | 2084 | 1109 | K760 CE165 S111 NC63 T10 | 81.6 | 779 | 79.7 | 0 / 0 (of 779) |
| hono | 5111 | StrykerJS | 258.5 | 2783 | 1474 | 4480 | K3789 S512 NC115 T64 | 86.0 | 3769 | 86.8 | 0 / 0 (of 4475) |
| hono | 5111 | mutator | 301.4 | 2003 | 4228 | 4734 | K4035 S520 NC115 T40 RE23 I1 | 86.5 | 3769 | 86.7 | 3 / 0 (of 3764) |
| trpc | 1032 | StrykerJS | 2782.3 (8 shards, slowest 408.9) | 16140 | 902 | 2688 | K2036 S421 NC189 T33 RE9 | 77.2 | 2086 | 80.7 | 1 / 0 (of 2661) |
| trpc | 1032 | mutator | 3445.9 (10 shards, slowest 436.6) | 18566 | 1294 | 2841 | K2158 S463 NC178 RE33 T9 | 77.2 | 2086 | 80.9 | 1 / 3 (of 2069) |

## Ground truth (StrykerJS, disableBail)

| project | shards | complete | wall s (sum) | CPU s | mutants | statuses | score | mean killers per killed mutant | killed by one test only | distinct killing tests |
|---|---|---|---:|---:|---:|---|---:|---:|---:|---:|
| defu | 1 of 1 | yes | 3.5 | 26 | 103 | K74 S15 T13 NC1 | 84.5 | 4.96 | 35 | 22 |
| scule | 1 of 1 | yes | 3.9 | 27 | 81 | K75 S5 NC1 | 92.6 | 24.32 | 5 | 63 |
| cookie-es | 1 of 1 | yes | 11.4 | 134 | 725 | K596 S92 T34 NC3 | 86.9 | 9.10 | 162 | 133 |
| pathe | 1 of 1 | yes | 14.4 | 188 | 524 | K451 S63 T5 NC3 RE2 | 87.4 | 41.64 | 55 | 472 |
| ufo | 1 of 1 | yes | 27.9 | 392 | 1011 | K819 S112 NC64 T16 | 82.6 | 19.74 | 120 | 487 |
| hono | 6 of 6 | yes | 1192.1 | 12945 | 4477 | K3647 S512 T201 NC115 RE2 | 86.0 | 95.27 | 453 | 3858 |
| trpc | 34 of 34 | yes | 10761.8 | 49210 | 2671 | K1632 T421 S420 NC189 RE9 | 77.1 | 50.01 | 339 | 858 |

## PR scenario (one scripted edit)

StrykerJS cold = `mutate` restricted to the changed lines, no incremental file. StrykerJS warm = the PR scope with `--incremental` from the pre-edit run. mutator = `--since HEAD~1` with the edit committed, without (cold) and with (warm) the pre-edit snapshot. Executed = mutants the run actually ran (StrykerJS: instrumented minus reused). Statuses leave out Pending (outside the diff).

| project | changed lines | Stryker cold: s / mutants / statuses | Stryker warm: s / executed | mutator cold: s / executed / statuses | mutator warm: s / executed / statuses | base runs: Stryker / mutator s |
|---|---|---|---|---|---|---|
| defu | src/defu.ts:33-33 | 1.7 / 3 / K3 | 1.7 / 4 | 1.5 / 6 / K6 CE2 | 1.5 / 6 / K6 CE2 | 3.3 / 4.1 |
| scule | src/index.ts:84-84 | 1.4 / 0 / – | 1.7 / 1 | 1.6 / 6 / K5 CE2 S1 | 1.5 / 6 / K5 CE2 S1 | 3.1 / 6.8 |
| cookie-es | src/_utils.ts:26-26 | 1.7 / 5 / K3 S2 | 1.9 / 5 | 1.3 / 6 / K4 S2 | 1.2 / 6 / K4 S2 | 10.4 / 17.1 |
| pathe | src/_path.ts:80-80 | 2.0 / 5 / K3 S2 | 3.4 / 52 | 1.9 / 7 / K5 CE3 S2 | 1.9 / 7 / K5 CE3 S2 | 10.0 / 19.0 |
| ufo | src/utils.ts:328-328 | 2.5 / 16 / K14 S2 | 2.9 / 8 | 2.2 / 15 / K14 CE2 S2 | 2.0 / 15 / K14 CE2 S2 | 16.4 / 34.6 |
| hono | src/utils/url.ts:144-144 | 27.6 / 7 / K7 | 37.9 / 10 | 12.4 / 13 / K13 | 11.7 / 13 / K13 | 257.9 / 304.8 |
| trpc | packages/server/src/unstable-core-do-not-import/http/getHTTPStatusCode.ts:80-80 | 93.0 / 0 / – | 94.5 / 1 | 21.9 / 2 / K2 | 2.8 / 2 / K2 | 207.3 / 375.2 |

## Where the time goes (cold runs)

StrykerJS: set-up = start to "Starting initial test run" (instrumenting, sandbox copy, starting test-runner processes); dry run = the initial test run with per-test coverage; mutants = until "Done in". mutator: set-up = start to "dry run:"; dry run = until "dry run finished"; type check + plan = until "N mutants, M to run" (the TypeScript check of every mutant, on by default); mutants = until the score line. Sharded runs show their first shard.

| project | install s | plain tests s (Stryker's / mutator's Vitest) | Stryker set-up / dry run / mutants s | mutator set-up / dry run / type check + plan / mutants s |
|---|---:|---|---|---|
| defu | 1.0 | 0.4 / 0.4 | 1.2 / 0.4 / 1.7 | 0.3 / 0.1 / 2.0 / 1.5 |
| scule | 1.2 | 0.3 / 0.4 | 1.2 / 0.3 / 1.4 | 0.3 / 0.1 / 4.1 / 2.2 |
| cookie-es | 1.1 | 0.4 / 0.4 | 1.2 / 0.6 / 8.5 | 0.3 / 0.1 / 6.8 / 10.3 |
| pathe | 1.1 | 2.6 / 2.6 | 1.3 / 0.6 / 8.8 | 0.4 / 0.2 / 8.6 / 9.7 |
| ufo | 0.9 | 0.4 / 0.4 | 1.3 / 1.3 / 14.2 | 0.4 / 0.2 / 12.4 / 20.1 |
| hono | 0.7 | 6.6 / 6.6 | 2.0 / 29.3 / 227.1 | 1.4 / 4.7 / 0.7 / 294.1 |
| trpc | 4.2 | 20.6 / 20.6 | 1.5 / 92.1 / 120.5 | 1.1 / 20.4 / 0.4 / 414.7 |

## Supercov (strong mutation)

Cold: `supercov -- <test command>` (measure: per-test coverage and weak mutation), then `supercov runs latest mutants test` (strong pass); a sharded strong pass is summed over its `--shard k/n` jobs, each of which measured again (the measure time shown is one job's). PR: the edit committed, measure, then `mutants test --diff HEAD~1`. Statuses: K killed, S survived, T timeout, NC no-coverage, RE error, Equivalent (static rule), P not run. Only mutants in the corpus mutate set are counted.

| project | measure s | strong s | total s | StrykerJS cold s | mutants | statuses | score | joined to truth | score on joined (Supercov / truth) | soundness violations | extra kills | PR: measure / diff s, mutants run of the diff, statuses |
|---|---:|---:|---:|---:|---:|---|---:|---:|---|---:|---:|---|
| defu | 1.1 | 1.9 | 2.9 | 3.4 | 103 | K87 S16 | 84.5 | 103 (Supercov only 0, StrykerJS only 0) | 84.5 / 84.5 (103) | 0 | 0 | – |
| scule | 1.2 | 1.5 | 2.7 | 2.9 | 78 | K73 S5 | 93.6 | 78 (Supercov only 0, StrykerJS only 3) | 93.6 / 93.6 (78) | 0 | 0 | – |
| scule (own-vitest) | 1.4 | 3.1 | 4.5 | 2.9 | 78 | K73 S5 | 93.6 | 78 (Supercov only 0, StrykerJS only 3) | 93.6 / 93.6 (78) | 0 | 0 | – |
| cookie-es | 1.2 | 8.7 | 9.9 | 10.4 | 691 | K573 S84 T32 NC2 | 87.5 | 691 (Supercov only 0, StrykerJS only 34) | 87.5 / 87.5 (691) | 0 | 0 | – |
| pathe | 3.2 | 8.1 | 11.3 | 10.8 | 490 | K425 S60 T4 NC1 | 87.5 | 490 (Supercov only 0, StrykerJS only 34) | 87.5 / 87.5 (488) | 0 | 0 | – |
| ufo | 1.3 | 14.5 | 15.7 | 16.9 | 926 | K753 S144 NC18 T11 | 82.5 | 926 (Supercov only 0, StrykerJS only 85) | 82.5 / 82.5 (926) | 0 | 0 | – |
| hono | 8.2 | 267.5 (6 shards, slowest 48.5) | 275.7 | 258.5 | 4295 | K3625 S507 T86 NC77 | 86.4 | 4292 (Supercov only 3, StrykerJS only 185) | 86.4 / 86.6 (4290) | 10 (0 after diagnosis) | 0 | – |
| trpc | 22.6 | 2631.8 (32 shards, slowest 108.5) | 2654.4 | 2782.3 | 2688 | K1873 S490 T193 NC119 RE13 | 77.2 | 2664 (Supercov only 24, StrykerJS only 7) | 77.0 / 77.1 (2646) | 2 (0 after diagnosis) | 0 | – |

### Where Supercov's time goes

Strong pass per mutant = strong-pass wall (CPU) seconds over the mutants it ran (not-infected, not-reached and equivalent mutants are decided without running). StrykerJS per mutant = its cold mutant phase over its mutants, for scale.

| project | plain tests s | measure s | strong pass s | mutants run | strong ms per mutant run (wall / CPU) | StrykerJS mutant phase ms per mutant | strong passes (stalled) | kills confirmed without the mutant / overturned |
|---|---:|---:|---:|---:|---|---:|---|---|
| defu | 0.4 | 1.1 | 1.9 | 100 | 19 / 9 | 17 | 1 (0) | 6 / 0 |
| scule | 0.3 | 1.2 | 1.5 | 77 | 19 / 11 | 17 | 1 (0) | 15 / 0 |
| scule (own-vitest) | 0.3 | 1.4 | 3.1 | 77 | 40 / 319 | 17 | 1 (0) | 15 / 0 |
| cookie-es | 0.4 | 1.2 | 8.7 | 680 | 13 / 5 | 12 | 1 (0) | 86 / 0 |
| pathe | 2.6 | 3.2 | 8.1 | 478 | 17 / 8 | 17 | 1 (0) | 106 / 0 |
| ufo | 0.4 | 1.3 | 14.5 | 886 | 16 / 6 | 14 | 1 (0) | 161 / 0 |
| hono | 6.6 | 8.2 | 267.5 | 4122 | 65 / 36 | 51 | 6 (0) | 1437 / 21 |
| trpc | 20.6 | 22.6 | 2631.8 | 2458 | 1071 / 141 | 755 | 32 (0) | 734 / 0 |

### Verdicts across strong-pass settings

| project | compared with | mutants compared | different verdicts |
|---|---|---:|---:|
| scule | own-vitest | 78 | 0 |

### Module-level (static) mutants

StrykerJS's ground truth runs static mutants (no `ignoreStatic`); Supercov switches them on before the module loads.

| project | static mutants joined | Supercov statuses | ground-truth statuses |
|---|---:|---|---|
| defu | 2 | K2 | K2 |
| scule | 0 | – | – |
| scule (own-vitest) | 0 | – | – |
| cookie-es | 0 | – | – |
| pathe | 62 | K48 S14 | K46 S14 RE2 |
| ufo | 5 | S5 | S5 |
| hono | 320 | K293 S24 T3 | K269 T26 S24 RE1 |
| trpc | 399 | K277 S87 T33 RE2 | K212 T92 S87 RE8 |

### Every disagreement with the ground truth

A soundness violation is a mutant the ground truth detects (Killed or Timeout) that Supercov calls survived, no-coverage or equivalent. An extra kill is the reverse.

- hono, soundness violation (ground-truth artifact): `src/router/smart-router/router.ts:14:24-16:6:BlockStatement:{}`: Supercov Survived (weak: infected, statement-ran), "the statement around it ran (a proxy: removing it may change nothing)"; ground truth Killed; ground truth killed by 1 (`src/hono.test.ts > c.var - with testing types > Should be a read-only`). Diagnosed: StrykerJS ran 'Should be a read-only' without the earlier tests of its describe that build the router; alone the test fails with no mutant, and the whole file passes with the mutant (diag/hono-order.sh)
- hono, soundness violation (ground-truth artifact): `src/router/smart-router/router.ts:15:7-15:56:CallExpression:empty`: Supercov Survived (weak: infected, statement-ran), "the statement around it ran (a proxy: removing it may change nothing)"; ground truth Killed; ground truth killed by 1 (`src/hono.test.ts > c.var - with testing types > Should be a read-only`). Diagnosed: StrykerJS ran 'Should be a read-only' without the earlier tests of its describe that build the router; alone the test fails with no mutant, and the whole file passes with the mutant (diag/hono-order.sh)
- hono, soundness violation (ground-truth artifact): `src/utils/html.ts:63:9-63:14:EqualityOperator:<=`: Supercov Survived (weak: unknown, no-comparison-probe), "needs `i` == `0`, and no probe recorded the operands"; ground truth Killed; ground truth killed by 1 (`src/jsx/streaming.test.tsx > Streaming > use() > render to stream`). Diagnosed: does not reproduce outside StrykerJS: applied by hand, the mutant passes 'render to stream' alone, its file and the whole suite (diag/hono-order.sh)
- hono, soundness violation (ground-truth artifact): `src/utils/html.ts:76:9-76:30:ConditionalExpression:true`: Supercov Survived (weak: infected, opposite-value), "`typeof r === 'object'` was false at least once"; ground truth Killed; ground truth killed by 1 (`src/jsx/streaming.test.tsx > Streaming > use() > render to stream`). Diagnosed: does not reproduce outside StrykerJS: applied by hand, the mutant passes 'render to stream' alone, its file and the whole suite (diag/hono-order.sh)
- hono, soundness violation (ground-truth artifact): `src/utils/html.ts:81:16-81:37:ConditionalExpression:true`: Supercov Survived (weak: infected, opposite-value), "`typeof r === 'object'` was false at least once"; ground truth Killed; ground truth killed by 1 (`src/jsx/streaming.test.tsx > Streaming > use() > render to stream`). Diagnosed: does not reproduce outside StrykerJS: applied by hand, the mutant passes 'render to stream' alone, its file and the whole suite (diag/hono-order.sh)
- hono, soundness violation (ground-truth artifact): `src/utils/html.ts:82:9-82:30:ConditionalExpression:true`: Supercov Survived (weak: infected, opposite-value), "`typeof r === 'object'` was false at least once"; ground truth Killed; ground truth killed by 1 (`src/jsx/streaming.test.tsx > Streaming > use() > render to stream`). Diagnosed: does not reproduce outside StrykerJS: applied by hand, the mutant passes its killing tests alone, their files and the whole suite (diag/hono-overturned.sh); Supercov killed it with an order-dependent test before af7340e5 and now overturns that kill
- hono, soundness violation (ground-truth artifact): `src/utils/html.ts:100:21-103:4:BlockStatement:{}`: Supercov Survived (weak: infected, statement-ran), "the statement around it ran (a proxy: removing it may change nothing)"; ground truth Killed; ground truth killed by 1 (`src/jsx/streaming.test.tsx > Streaming > use() > render to stream`). Diagnosed: does not reproduce outside StrykerJS: applied by hand, the mutant passes its killing tests alone, their files and the whole suite (diag/hono-overturned.sh); Supercov killed it with an order-dependent test before af7340e5 and now overturns that kill
- hono, soundness violation (ground-truth artifact): `src/utils/html.ts:161:9-161:61:ConditionalExpression:true`: Supercov Survived (weak: infected, opposite-value), "`(str as string | Promise<string>) instanceof Promise` was false at least once"; ground truth Killed; ground truth killed by 1 (`src/jsx/streaming.test.tsx > Streaming > use() > render to stream`). Diagnosed: does not reproduce outside StrykerJS: applied by hand, the mutant passes 'render to stream' alone, its file and the whole suite (diag/hono-order.sh)
- hono, soundness violation (ground-truth artifact): `src/utils/html.ts:185:7-185:24:ConditionalExpression:false`: Supercov Survived (weak: infected, opposite-value), "`preserveCallbacks` was true at least once"; ground truth Killed; ground truth killed by 4 (`src/jsx/streaming.test.tsx > Streaming > use() > render to stream`, `src/jsx/components.test.tsx > ErrorBoundary > streaming > no error`, `src/jsx/components.test.tsx > ErrorBoundary > streaming > with StreamingContext`, `src/jsx/components.test.tsx > ErrorBoundary > streaming > error with StreamingContext`). Diagnosed: does not reproduce outside StrykerJS: applied by hand, the mutant passes its killing tests alone, their files and the whole suite (diag/hono-overturned.sh); Supercov killed it with an order-dependent test before af7340e5 and now overturns that kill
- hono, soundness violation (ground-truth artifact): `src/utils/jwt/jwt.ts:156:77-156:94:EqualityOperator:<=`: Supercov Survived (weak: unknown, no-comparison-probe), "needs `now` == `payload.iat`, and no probe recorded the operands"; ground truth Killed; ground truth killed by 5 (`src/utils/jwt/jwt.test.ts > JWT > JwtTokenIssuer (none)`, `src/utils/jwt/jwt.test.ts > JWT > JwtTokenIssuer (wrong - string)`, `src/utils/jwt/jwt.test.ts > JWT > JwtTokenIssuer (wrong - RegExp)`, `src/utils/jwt/jwt.test.ts > JWT > JwtTokenIssuer (correct - string)`, `src/utils/jwt/jwt.test.ts > JWT > JwtTokenIssuer (correct - RegExp)`). Diagnosed: StrykerJS's covering set leaves out the test that restores real timers, so JwtTokenIssuedAt's fake clock (never restored) leaks into the JwtTokenIssuer tests; that subset kills it, the whole file passes (diag/hono-order.sh)
- trpc, soundness violation (ground truth: flaky kill): `packages/server/src/unstable-core-do-not-import/http/contentType.ts:109:7-110:39:LogicalOperator:||`: Supercov Survived (weak: infected, operands-differ), "one operand was true and the other false"; ground truth Killed; ground truth killed by 1 (`packages/tests/server/httpSubscriptionLink.test.ts > iterable event with bad yield`). Diagnosed: flaky: only the ground truth kills it, with one test (httpSubscriptionLink.test.ts > iterable event with bad yield); StrykerJS's cold run and mutator call it survived
- trpc, soundness violation (StrykerJS hit-limit heuristic; Supercov: survived, by design): `packages/server/src/unstable-core-do-not-import/stream/utils/withPing.ts:37:9-37:20:ConditionalExpression:false`: Supercov Survived (weak: infected, opposite-value), "`result.done` was true at least once"; ground truth Timeout. Diagnosed: by-design: StrykerJS hit-limit heuristic (Supercov: survived, by design). StrykerJS reports 'Hit limit reached (8224/4100)': 100 times the 41 dry-run evaluations; with plain Vitest the mutant's tests pass, about twice as slow, after about 45,000 evaluations of the line against 8 without it (diag/trpc-withping.sh)

## Notes and errors

- defu: Vitest upgraded in this job from 4.1.6 to 5.1.1 (removed @vitest/coverage-v8): mizchi/mutator needs Vitest 5
- defu: Supercov instruments only the mutate set: SUPERCOV_SOURCE_ROOTS lists its 3 files
- scule: Vitest upgraded in this job from 1.5.3 to 5.1.1 (removed @vitest/coverage-v8): mizchi/mutator needs Vitest 5
- scule: also added vite@8.3.8: scule's lockfile resolves Vite 5.0.12, which Vitest 5 cannot load (no vite/module-runner export)
- scule: Vitest upgraded in this job from 1.5.3 to 5.1.1 (removed @vitest/coverage-v8): StrykerJS 10's Vitest runner needs Vitest >= 2; scule pins Vitest 1.5.3
- scule: Vitest upgraded in this job from 1.5.3 to 5.1.1 (removed @vitest/coverage-v8): as StrykerJS's ground truth uses; the own-vitest variant runs Supercov on scule's Vitest 1.5.3
- scule: Supercov instruments only the mutate set: SUPERCOV_SOURCE_ROOTS lists its 2 files
- scule: variant own-vitest: Supercov on the project's own Vitest 1.5.3
- cookie-es: Vitest upgraded in this job from 4.1.2 to 5.1.1 (removed @vitest/coverage-v8): mizchi/mutator needs Vitest 5
- cookie-es: Supercov instruments only the mutate set: SUPERCOV_SOURCE_ROOTS lists its 8 files
- pathe: Vitest upgraded in this job from 4.1.9 to 5.1.1 (removed @vitest/coverage-v8): mizchi/mutator needs Vitest 5
- pathe: Supercov instruments only the mutate set: SUPERCOV_SOURCE_ROOTS lists its 3 files
- ufo: Vitest upgraded in this job from 4.1.5 to 5.1.1 (removed @vitest/coverage-v8): mizchi/mutator needs Vitest 5
- ufo: Supercov instruments only the mutate set: SUPERCOV_SOURCE_ROOTS lists its 7 files
- hono: mutator flags --no-typecheck: mutator 0.5.0's type check crashes on hono's solution-style tsconfig.json ("files": [] plus references): Could not find source file src/compose.ts
- hono: Supercov instruments only the mutate set: SUPERCOV_SOURCE_ROOTS lists its 49 files
- trpc: mutator flags --no-typecheck: mutator 0.5.0's type check crashes on trpc's TypeScript 7.1.0-beta: Package subpath './unstable/sync' is not defined by exports
- trpc: PR scenario base runs mutate ["packages/server/src/unstable-core-do-not-import/http/getHTTPStatusCode.ts","packages/server/src/unstable-core-do-not-import/http/contentTypeParsers.ts","packages/server/src/unstable-core-do-not-import/http/formDataToObject.ts","packages/server/src/unstable-core-do-not-import/http/parseConnectionParams.ts","packages/server/src/unstable-core-do-not-import/http/abortError.ts","packages/server/src/unstable-core-do-not-import/http/pathSeparator.ts"]: the PR scenario's base runs (StrykerJS's incremental base, mutator's snapshot) and PR runs cover the edited file and its small siblings in http/: mutator needs about 8 minutes for the 390 mutants of http/ without resolveResponse.ts, and about 45 for the full set
- trpc: StrykerJS options {"tsconfigFile":"tsconfig.stryker-none.json","vitest":{"configFile":"vitest.bench.config.ts","pool":"forks"}}: StrykerJS 10.1.1 rewrites tsconfig.json in the sandbox with ts.parseConfigFileTextToJson, which TypeScript 7.1.0-beta lacks, so tsconfigFile points at a missing file (no checker runs anyway); and its Vitest runner starts the threads pool unless told otherwise, where trpc's --expose-gc (execArgv) cannot start a worker, so pool is forks as upstream's default
- trpc: Supercov instruments only the mutate set: SUPERCOV_SOURCE_ROOTS lists its 46 files
