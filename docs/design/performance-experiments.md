# Performance experiments: what worked, what didn't, what's left

A running log of speed work on Supercov, kept so future work starts from what is already known. The rule for every entry: **no accuracy loss.**
- Coverage answers must be byte-identical on deterministic fixtures.
- Mutation verdicts must not flip (killed↔survived, survivor→timeout, dropped kills). Speed may only turn undecided mutants into decided ones.
- Every kept change was checked with an old-vs-new comparison.

Numbers are from GitHub Actions 4-CPU Linux runners unless stated. Runner-to-runner noise is large (up to ±40% on one project between runs with no code change), so later comparisons run old and new in turns on the same runner.

Detailed logs live next to each area:
- `docs/performance.md`: JavaScript coverage breakdown and tooling.
- `docs/design/jvm-mutation-performance.md`: JVM mutation experiment log.
- `docs/design/mutation-pipeline-speed.md`: language-independent mutation pipeline (branch `perf/mutation-pipeline-exact`).
- `docs/design/non-js-coverage-overhead-2026-11-28.md`: coverage overhead outside JavaScript (branch `docs/non-js-overhead-log`).
- `docs/design/strong-mutation.md`: the resident Vitest and AVA hosts.

## Coverage overhead

The goal is a measured run at most 1.1× the plain test run.

### JavaScript / TypeScript

Where the time goes:
- **Large generated Next.js app:** +18–22% wall time and +22–23% CPU.
- **Excalidraw's Vitest suite:** +25% instructions.
- **Where it's spent:** the overhead is spread over about a dozen parts, and switching any one off saves at most about 2 points. The two biggest are both needed for exact attribution:
  - **Phase activation:** each action waits about 4–6 ms for the page to learn its phase.
  - **Evidence volume:** 779 MB written by Excalidraw's workers. The cost is per recorded item, not per byte.
- **After the tests:** about 13 s on Excalidraw's critical path for reading, mapping, building views and publishing.

| Tried | Result | Kept? |
|---|---|---|
| Shared browser runtime instead of a 37 KB copy per module | Client JS 12.8 → 4.8 MB, e2e extra time −43% | Kept (earlier) |
| Source maps off, `--enable-source-maps`, removing the async hook, compile-cache changes | ≈0; removing the compile cache made it worse | Dropped |
| Minified browser runtime | 0 | Dropped |
| Phase activation per action / phase cookies off | −2.2 / −1.3 points, but needed for exact attribution | Not changed |
| Compact Vitest evidence encoding (#366) | 46% smaller, +2.4% instructions: cost is per item | Dropped |
| Not copying already-parsed evidence | −0.09 s of 13 s | Dropped (too small) |
| Cheaper container-SDK export scan (skip listing big arrays) | 4.5 → 4.6 s, slightly slower; the earlier "−1 point" was noise | Dropped |
| Activation sent as a raw CDP call, still awaited | 4.7 → 6.3 ms, slower | Dropped |
| Time-stamped phase boundaries (unawaited, retroactive credit) | ≈0 gain | Dropped (earlier) |

**Not tried, on purpose: send-but-don't-wait activation.** For element actions, this would wait only until the activation is sent on the page's CDP session, relying on Chrome to process that session in order. It saves about 2 ms per element action, about 3% of e2e test time. It depends on Playwright internals that change between versions, and a wrong ordering assumption would misattribute work. Revisit only if Playwright exposes a supported hook.

### Rust

| Tried | Result | Kept? |
|---|---|---|
| Batch a file's doctests into one rustdoc (#388) | bytes (168 doctests) 4.53× → 3.11×; semver (4 doctests) slightly slower | Kept |
| Split a large file's doctests into parts across cores | bytes unchanged (10.4–10.6 s) | Dropped |
| Dropping probes inside straight-line code | Would credit statements a panic skipped | Not done (inexact) |

**Remaining on bytes:**
- About 30% is one test running instrumented code in a hot loop.
- About 25% is compiling doctests and about 15% is linking them; plain `cargo test` does that work too.
- Probes are already a few loads and compares.

### Go

| Tried | Result | Kept? |
|---|---|---|
| Fixed evidence path, so Go's build cache reuses test packages (#362) | gin 3.5× → 1.84×, cobra 3.5× → 2.25×; test command 2.27 → 1.07 s | Kept |

**Left:** about 0.3 s per run to copy and re-instrument the workspace.

### Ruby

| Tried | Result | Kept? |
|---|---|---|
| Decisions recorded once per test, no allocation per condition (#367) | rack 2.25× → 1.98× | Kept |
| Batch decision records | A test whose process ended mid-test lost its decisions | Dropped (inexact) |

### Python

| Tried | Result | Kept? |
|---|---|---|
| Name and order probe slots once per run (#389) | jinja evidence 1.76 → 1.69 s | Kept (small) |
| `sys.monitoring` with disable-after-first-hit; caching the probe array in a local | Can attribute lines to the wrong test | Not done (inexact) |

Python's remaining cost on jinja is mostly the post-test analysis every language shares; see "After the tests" below.

### Java / Kotlin

| Tried | Result | Kept? |
|---|---|---|
| Keep build output between runs, so Maven or Gradle compiles only what changed (#368) | commons-cli 1.48× → 1.14× | Kept |

### After the tests (all languages)

On small suites (jinja, cobra, rack) this fixed cost is what keeps Go, Python and Ruby above 1.1×. Building the coverage views (`create_coverage_view_with_model`) is the largest piece, about 23% of Supercov's CPU on jinja.

| Tried | Result | Kept? |
|---|---|---|
| Reports and gates read a run view kept beside the run (#359) | istanbul export 11.2 s / 10.3 GB → 0.45 s / 0.24 GB; HTML 11.1 → 0.31 s | Kept |
| Unfiltered diff skips per-line test lists; lock-free index reads (#360) | diff 10.0 → 2.1 s | Kept |
| Index stores a passed test's records once (#372) | index 941 → 554 MB, store 1.12 → 0.74 GB | Kept |
| Merge reads shard archives in parallel, once each (#358) | 16-shard merge 7.8 → 5.2 s | Kept |
| Build the merged run's index during the merge | −6 s, but peak memory 8.3 → 11.5 GB | Dropped |
| Decision slots by position instead of id strings | −4% index build on Excalidraw | Pending measurement on small suites |

**Next:** hashing and copying evidence twice at publication, and the remaining view building.

## Mutation testing

### Language-independent pipeline

| Tried | Result | Kept? |
|---|---|---|
| Shard by mutant-id hash (#348) | Merged totals equal the shards' sum; fixes dropped and duplicated mutants | Kept (correctness) |
| Go type-check helper cached per toolchain (#356) | Fixes reuse after a Go upgrade | Kept (correctness) |
| Go helper built without `-trimpath` (#380) | Helper build after the project's `go test` 5.6–6.0 s → 1.6–1.9 s | Kept |
| Python: likely killers first (kill history) | 0.7% fewer test runs; 10 killed → timeout | Dropped: order decides killed vs timeout when a mutant both fails one test and hangs another |
| Python: one fork per test | Changes what each test runs after | Dropped |
| Higher-order batching (several mutants per run) | Exact only if no other batch mutant was evaluated; such batches save nothing | Dropped |
| Shard by measured cost | Busiest shard does 13–90% (4 shards) to 32–206% (8 shards) more than an even split. Exact only if every job plans from one measured run | Not built: needs a shared plan, which isn't automatic |
| Go timeouts: parallel second attempts, or a lower hit limit | Timeouts are most of Go test time (uuid 18.4 of 20.1 s), but either changes what counts as a timeout | Not done (inexact) |
| Go mutants longest-first across workers | Exact but small | Not done |
| Trivial compiler equivalence beyond JS | A Go compile per mutant costs about as much as a test run; Python AST equality catches little | Not done (low value) |

### JavaScript

| Tried | Result | Kept? |
|---|---|---|
| Resident Vitest host for Browser Mode (#352) | mini-throttle strong pass 121 → 70 s, 0 of 77 verdicts changed | Kept |
| Resident AVA host (#376) | dot-prop 185 → 116 s, all verdicts matched | Kept |
| Order a mutant's test files by kill history per (test file, source file) | Upper bound: ufo copies before the kill 728 → 154 | In progress (`perf/js-strong-kill-order`) |
| Rerun a timed-out Jest or Mocha job as a command | Could move the timeout boundary | Removed; only the new hosts do this |

**Next:** the Playwright e2e strong pass, which is the most expensive per mutant. The Rallly benchmark splits time into attempts, baselines, controls, gaps and new workers.

### Rust

| Tried | Result | Kept? |
|---|---|---|
| Build each Worker once per pass and switch the mutant on in the served Worker (#351) | Removes a wasm rebuild per test launch, the cause of about one mutant per minute | Kept |
| Keep file times when a rebuild rewrites a file to the same bytes | rand: 6 switched builds 17.0 → 13.8 s, 1188/1188 verdicts identical | Kept |
| Run the likeliest killing test first | Changes `killedBy`; can turn a timeout into a kill | Dropped |
| Shared target dir for mutants built on their own | — | Pending measurement |

Native Rust mutants are already cheap: 0.4–1.4 s each once testing starts.

### Java / Kotlin

| Tried | Result | Kept? |
|---|---|---|
| Profile, and baselines shared across JVMs and rounds (#365) | commons-cli 5,712 → 2,744 baseline runs; strong pass 144 → 88 s | Kept |
| Retry rounds started from the test JVM (#387) | kotlinpoet: five retry builds (242 s) gone | Kept |
| Tests that killed the same file's mutants first | 12–14% fewer test runs | Dropped: the first timeout ends a mutant as the first kill does, so the order decides killed vs timeout |
| C1-only JIT in mutant JVMs, the generous round on the build's JIT (#404) | Strong pass 194 → 109 s (commons-cli); kotlinpoet even (JIT 2,485 → 151 s, tests slower by as much). C1 in the generous round turned two heap-filling mutants from killed to timeout; with the build's JIT there, no verdict changed beyond one that already flips between runs before C1 | Kept |
| Skip re-verifying classes | First test per mutant about 17% cheaper | Kept |
| Kotlin reach probes for expression bodies | 35% fewer tests per mutant; changed mutants are ones PIT reports NO_COVERAGE | In progress |
| Run later tests in batches | Changed a verdict (`DefaultParser.java:711` survived → killed) and ran more tests | Dropped |
| Swap mutated classes in a long-lived JVM, as PIT does | Shares statics and caches between mutants, which is where PIT's wrong verdicts come from | Not done (inexact) |
| Record JVM test durations in the measured run | Can only raise limits; tighter limits would need dropping the baseline | Not done |

**Next:** one Maven or Gradle build for both the measured run and the strong pass. Supercov still trails PIT end to end: commons-cli 128 s vs 100 s, kotlinpoet 2,520 s vs 1,430 s.

## Ideas not yet tried

- JS: a supported Playwright hook for unawaited phase activation, if one appears.
- JS coverage: an exact per-item reduction of evidence.
- After the tests: avoid hashing and copying evidence twice at publication; more view-building cuts.
- Mutation: a shared, automatic plan for cost-based sharding.
- JVM: one build for measuring and mutating.
- Go: copying and re-instrumenting the workspace (about 0.3 s per run).
