# Mutation-testing benchmark

Baselines and a harness for measuring mutation testing of TypeScript projects tested with Vitest. It
records what two existing tools cost and decide on a fixed corpus, so that a new mutation-testing
mode can later be measured against the same numbers:

- **StrykerJS** (`@stryker-mutator/core` and `@stryker-mutator/vitest-runner`), tuned for speed;
- **mizchi/mutator** (`@mizchi/mutator`), a diff-driven tool for Vitest;
- a **ground truth**: StrykerJS with `disableBail`, so every covering test runs against every
  mutant, which gives the full kill matrix (which tests kill which mutant);
- **Supercov**'s strong mutation (`supercov -- <tests>`, then `supercov runs latest mutants test`),
  built from the working tree of the branch under test.

Nothing here is a dependency of Supercov. Every tool is installed inside the CI job, into the
cloned project.

## What it measures

For each project and tool, each in a job of its own:

| scenario | what runs |
|---|---|
| `stryker-cold` | StrykerJS: `coverageAnalysis: "perTest"`, `concurrency` = number of CPUs, `incremental: false`, no checker (the TypeScript checker is off, so mutants that do not compile count as whatever the tests make of them). Before it, the plain test suite once, for the test count and its time. |
| `mutator-cold` | `mutator --reporter json --reporter text` with its defaults (`-j` = half the CPUs, type check on, weak mutation on), no `.mutator/` directory. |
| `truth` | StrykerJS as in `stryker-cold` with `disableBail: true`; large projects are split into shards of files or line ranges (see below). |
| `pr-stryker` | A StrykerJS run with `--incremental` on the unedited project (the warm-up), then the scripted edit, then StrykerJS with `mutate` limited to the changed lines (cold), then StrykerJS `--incremental` from the warm-up's file (warm). |
| `pr-mutator` | A full mutator run on the unedited project (the warm-up, a second cold sample), then the edit committed, then `mutator --since HEAD~1` without a snapshot (cold) and with the warm-up's snapshot (warm). |
| `supercov-cold` | `supercov -- <the project's test command>` (measure: per-test coverage and weak mutation), then `supercov runs latest mutants test --json` (the strong pass: each infected mutant against only the tests that infected it). Measure and strong pass are timed separately. A project that needs it sets `supercovShards`, and each job then measures and runs `--shard k/n` of the strong pass. |
| `pr-supercov` | The edit committed, then measure (as a PR's CI would, with no earlier run), then `mutants test --diff HEAD~1`. |

Each measured phase records wall time, CPU seconds and peak RSS (GNU `time -v`: the process and
every descendant it waited for), the exit code, and the full log with a timestamp on every line.
`summarize.mjs` reads the logs to split a run into set-up, dry run, (type check) and mutant
phases, and the mutation-testing-elements reports to count mutants and statuses.

`summarize.mjs` then reports, per project and tool:

- wall and CPU time, mutants, statuses and score (Killed + Timeout over Killed + Timeout +
  Survived + NoCoverage);
- the score of each tool on the mutants both generate, joined by file, start and end position,
  mutator name and replacement (outer parentheses, trailing block comments and whitespace in the
  replacement are ignored, as in the mutator benchmark);
- verdict disagreements with the ground truth: a mutant a tool calls undetected that some test
  kills ("missed kill"), or the reverse;
- the kill matrix of the ground truth: killers per mutant, mutants only one test kills, tests
  that kill anything;
- the PR scenario: time and mutants executed for each tool, cold and warm.

## Corpus

`corpus.json` pins every project to a commit, with its install and test commands, its pnpm and
Vitest versions, the files to mutate, and the edit for the PR scenario (`patches/`).

| project | commit | tests | mutated | edit |
|---|---|---:|---|---|
| [unjs/defu](https://github.com/unjs/defu) | `82632b6` | 23 | `src/**/*.ts` | `_defu`: `key.toString()` to `String(key)` |
| [unjs/scule](https://github.com/unjs/scule) | `5bc6804` | 63 | `src/**/*.ts` | `upperFirst`: `str[0]` to `str.charAt(0)` |
| [unjs/cookie-es](https://github.com/unjs/cookie-es) | `f89ede8` | 134 | `src/**/*.ts` | `valueSlice`: `min === max` to `max - min === 0` |
| [unjs/pathe](https://github.com/unjs/pathe) | `bc7477a` | 483 | `src/{_path,utils,_internal}.ts` | `join`: template literal to concatenation |
| [unjs/ufo](https://github.com/unjs/ufo) | `f06c800` | 489 | `src/**/*.ts` | `withoutBase`: also stop at `#` (changes behaviour) |
| [honojs/hono](https://github.com/honojs/hono) | `75ff8e3` | 5,111 | `src/*.ts`, `src/router/**`, `src/utils/**` | `getPath`: `'?'`/`'#'` char codes as hex literals |
| [trpc/trpc](https://github.com/trpc/trpc) | `596d03d` | 1,032 | `@trpc/server`'s `unstable-core-do-not-import/**` and `adapters/node-http/**` | `getHTTPStatusCode`: `Array.isArray` ternary to `[].concat` |

The first five are the libraries, commits, mutate sets and edits of mizchi/mutator's own
benchmark (`docs/benchmark.md` in that repository, measured on 2026-10-09), so its published
numbers can be compared with these. pathe leaves out `src/_glob.ts` there too.

The two larger projects were chosen for these reasons:

- **hono**: about 5,000 Vitest tests over ~10,000 lines of routing and utility code, most of them
  fast unit tests, some that start the Node adapter and serve requests. Every request goes through
  `getPath` and the routers, so many mutants are covered by thousands of tests and ~400 are
  static (module-level), which is where per-test coverage and early exit matter most. It already
  uses Vitest 5, so both tools run unmodified. The benchmark runs the upstream config's `main`
  project only (`configs/hono.vitest.bench.config.ts`: no v8 coverage, no workerd/fastly/bun
  runtime projects).
- **tRPC**: about 1,000 tests in `packages/tests/server` that start HTTP, WebSocket and SSE
  servers and call them through `@trpc/client`, plus `@trpc/server`'s unit tests. This is the
  slow, integration-style end: the plain suite takes ~20 s on 16 CPUs, each test file pays for
  servers and clients, and timeouts matter. Vitest aliases `@trpc/*` to source, so mutants in
  `packages/server/src` are seen by the integration tests. React suites are left out
  (`configs/trpc.vitest.bench.config.ts`, which also turns off upstream's CI retries).

Larger candidates were looked at and dropped: msw's integration tests import the built `lib/`, so
mutants in `src/` would be invisible to them; zod and valibot have many tests but no
integration-style ones.

## Versions

| | version |
|---|---|
| Node | 24 (setup-node; the exact version is in each job's `result.json`) |
| StrykerJS | 10.1.1 (`@stryker-mutator/core`, `@stryker-mutator/vitest-runner`) |
| mizchi/mutator | 0.5.0 (`@mizchi/mutator`, published 2026-11-19) |
| Vitest | the project's own for StrykerJS; 5.1.1 for mutator where the project is on Vitest 4 or older |
| pnpm | the project's own (`packageManager`) |
| Supercov | the binary built from the working tree (`cargo build --profile measure -p supercov`, 4.28.0, at `af7340e5` plus uncommitted changes), run through the published npm wrapper `supercov@4.28.0` with `SUPERCOV_RUST_BINARY`; each job records the build key, a hash of `crates/`, `runtime/`, `Cargo.*` and `rust-toolchain.toml` |

Vitest changes made inside the jobs, and recorded in each job's `result.json`:

- mutator needs Vitest 5. defu, cookie-es, pathe and ufo pin Vitest 4, scule Vitest 1.5.3: the
  mutator jobs remove the project's `@vitest/*` dev dependencies and add `vitest@5.1.1`.
- StrykerJS 10's Vitest runner needs Vitest 2 or later, so scule's StrykerJS jobs get
  `vitest@5.1.1` too. For scule both tools also get `vite@8.3.8`: its lockfile resolves Vite
  5.0.12, which Vitest 5 cannot load. The other projects run StrykerJS on their own Vitest (4.x, or 5.x for hono
  and tRPC). StrykerJS 10.1 supports Vitest 5; 10.0 did not, which is why mizchi/mutator's
  benchmark ran StrykerJS on Vitest 4.

Supercov runs on each project's own Vitest (4.x for the unjs libraries, 5.x for hono and tRPC).
scule runs twice: on its own Vitest 1.5.3 (variant `own-vitest`) and on Vitest 5.1.1 with Vite
8.3.8, as its StrykerJS jobs do. Supercov instruments only
the mutate set: `SUPERCOV_SOURCE_ROOTS` lists the mutate set's files, and only those mutants are
counted.

Tool settings changed for one project, also recorded in `result.json` notes:

- hono and tRPC: mutator runs with `--no-typecheck`. Its type check crashes on hono's
  solution-style `tsconfig.json` (`"files": []` with references) and on tRPC's TypeScript
  7.1.0-beta (no `typescript/unstable/sync` export).
- tRPC, StrykerJS: `tsconfigFile` points at a missing file, because its sandbox step rewrites
  `tsconfig.json` with an API TypeScript 7.1.0-beta lacks (no checker runs anyway);
  `vitest.pool` is `forks`, because its runner otherwise starts the threads pool, where tRPC's
  `--expose-gc` cannot start a worker (upstream relies on Vitest's default, forks); and `.claude`,
  `.agents`, `www` and `examples` are left out of its sandbox (symlinked directories under
  `.claude` made the copy fail).

## Machines

Every job runs on its own 16-vCPU AWS machine (all AMD EPYC 9R45) with the working directory on a
RAM disk: a c8a.4xlarge (32 GiB) for the StrykerJS, mutator and PR jobs. The ground-truth jobs
of the five libraries and hono ran on an m8a.4xlarge or r8a.4xlarge (16 vCPU, 64 or 128 GiB):
with `disableBail`, two hono ground-truth jobs on 32 GiB machines lost test-runner processes to
SIGKILL and then the whole step, which looks like the machine running out of memory (the largest
process of a hono shard that finished reached 6.6 GiB). tRPC's ground-truth runners stay under
1.3 GiB, and its last 17 shards ran on any of the three types, because jobs waiting for a scarce
m8a/r8a machine ran out of time before they started. Each job's `result.json` records the
instance type it got. Jobs must finish within ten minutes; the job script stops a tool run that
would overrun and marks it `timedOut`.

## Sharding

A run that cannot finish in one job is split:

- **Ground truth** (`shards` in `corpus.json`): hono in 6 shards, tRPC in 34. `plan-shards.mjs`
  packs files, or line ranges of large files, by estimated cost: the number of tests a mutant
  runs under `disableBail` (all tests for a static mutant), read from StrykerJS reports; files no
  report covers are costed by code lines. hono's plan comes from its cold report. tRPC's comes
  from the reports of an earlier 12-shard attempt in which 9 shards ran out of time, cut 24 ways;
  shards that still ran out of time were then cut by hand in three rounds (s12, s14, s15, s21,
  s23 and their parts). A line-range piece holds only the mutants inside its range, so a mutant
  spanning a cut (such as the body of `resolveResponse`) is in no shard: 18 of tRPC's 2,688
  StrykerJS mutants and 3 of hono's 4,480 are missing from the ground truth, and the summary
  counts them as "not in truth". Shard results are merged by mutant.
- **tRPC cold runs**: StrykerJS in 8 file shards (`coldShards`), mutator in 10 jobs of its own
  `--shard i/10` partition (`mutatorColdShards`). Wall and CPU seconds are summed over the shards,
  so they include each shard's set-up and dry run (StrykerJS's is 90 s here, see below).
- **Supercov strong pass** (`supercovShards`): hono 3 ways, tRPC 32 ways (`--shard k/n`, every
  n-th mutant to run from the k-th). Each job measures again before its part of the strong pass.
- **tRPC PR scenario** (`prMutate`): the warm-up and PR runs cover the edited file and its small
  siblings in `unstable-core-do-not-import/http/` (about 140 StrykerJS mutants): mutator needed
  8 minutes for the 390 mutants of `http/` without `resolveResponse.ts`.

## Reproducing

From the repository root, with a runner label for 16-vCPU machines:

```sh
# 0. for the Supercov scenarios: build the binary from this tree in one run, keyed by a hash of the tree
KEY=$(git ls-files -co --exclude-standard -- crates runtime Cargo.toml Cargo.lock rust-toolchain.toml | sort | xargs shasum | shasum | cut -c1-16)
node benchmarks/mutation/build-workflow.mjs --runs-on <64-vCPU label> > .github/workflows/mutation-bench-build.yml
#    run it with the variable SUPERCOV_BIN_KEY=$KEY; the benchmark run below gets the same variable,
#    and its Supercov jobs restore the binary from the CI cache by that key (matrix jobs cannot
#    depend on a build job in the same run on every CI service)

# 1. the workflow (one job per project x scenario x shard)
node benchmarks/mutation/workflow.mjs --runs-on <16-vCPU label> --truth-runs-on <16-vCPU, 64 GiB+ label> \
  > .github/workflows/mutation-bench.yml
#    --projects defu,ufo, --scenarios stryker-cold,truth and --shards s2,s6 narrow it; --variants own-vitest,first
#    runs those Supercov variants only ('' is the plain job). The full matrix (about 140 jobs) goes in several runs
#    by --projects and --shards, 32 or so jobs each. The CI service used here lists only its three
#    latest runs: an older one drops off the list but still runs, and its artifacts are still
#    retrieved by its id, so keep every run's id
#    Other Supercov variants: first (`supercov mutants -- <test command>` with no run before it, as a
#    first use), first-wN (first with Vitest's --maxWorkers=N),
#    first-prof (first with a CPU profile of the strong pass's main process, main.cpuprofile), prof (the
#    plain job with that profile)

# 2. run it on a workflow_dispatch-capable CI service, then download every artifact
#    (mutation-bench-<project>-<scenario>[-<shard>]) into one directory

# 3. the summary: results/<date>/summary.json, summary.md and each job's result.json
node benchmarks/mutation/summarize.mjs <artifacts dir> --date 2026-11-20 --runs <run ids>
```

One job can also be run by hand on a Linux machine with Node 24 and git:
`node benchmarks/mutation/run-job.mjs --project ufo --scenario stryker-cold --out out`. It clones
into `$RUNNER_TEMP` (or the system temp directory) and installs packages there.

To re-plan shards after a corpus change:
`node benchmarks/mutation/plan-shards.mjs hono <stryker-cold.json> 6 --write`.

## Results of 2026-11-23: the continuous pass

Supercov's cold rows again (`results/2026-11-23/summary.md`), on the continuous pass (no stages;
`docs/design/strong-mutation.md`), on 16-CPU Linux machines (c8a/m8a.4xlarge), as every row before:

| project | ms per mutant run, staged (2026-11-22) | continuous | StrykerJS's mutant phase | files / baselines / confirmations per mutant run | soundness violations | extra kills |
|---|---:|---:|---:|---|---:|---:|
| defu, scule, cookie-es, pathe, ufo | 14-25 | 14-19 | 12-17 | 1.0-1.6 / 0.01-0.03 / 0.06-0.21 | 0 | 0 |
| hono (6 shards) | 69 | 68 (63-68 over four full runs) | 51 | 2.5 / 0.25 / 0.33 | the same 10 | 0 |
| tRPC (32 shards) | 1,173 | 1,068 (1,066-1,071) | 755 | 10.8 / 2.69 / 0.31 | the same 2 | 0 |

- **Files per mutant, by verdict.** hono: killed 1.7, survived 9.8; tRPC: killed 6.3, timed out 2.6,
  survived 37.2 (StrykerJS's covering tests span 33.5 files for a killed tRPC mutant and 36.6 for a
  survivor). A tRPC survivor runs as many files as StrykerJS's; a killed mutant 6.4.
- **Baselines** are once per file and shape in each pass: 2.70 per tRPC mutant because each of the 32
  shards is a pass of its own (about 110 files' union baselines and 100 whole ones, for its 77
  mutants). Unsharded they would be about 0.1 per mutant. StrykerJS's dry run, which plays their part,
  is outside its mutant phase; Supercov's figure includes them.
- **Confirmations** run only for a kill by a copy that left out tests its file's baseline ran before
  the failing one.
- **A failure before the mutant ran** reran its copy 7 times on hono and never changed a verdict
  here; in two earlier runs, two timing tests' failures (`errors.test.ts > unauthorized()`,
  `websockets.test.ts > lazy connection where the first connection fails`) had stood as kills the
  ground truth does not have.
- **Still flaky:** the hono test `Logger by Middleware in NO_COLOR > Time in seconds` (expects `1s`,
  gets `999ms` under load) and a tRPC reconnect test failed once each in one of the five full runs of
  this pass while the mutant had run, and stood as kills; no rule that holds for every suite tells
  such a failure from a real one without running the copy again.

## Results of 2026-11-22, later: next batches within a stage, decided copies stopped

Supercov's cold rows again (`results/2026-11-22/summary.md`), on the binary of `2cd0441e`, against the morning's binary (`e19bd687`) on the same machine type. A mutant's
next batch now runs in the same stage once its copies before have ended with every test passing, and
a copy's worker stops as soon as a failed test decides it.

| project | ms per mutant run, morning | now | StrykerJS's mutant phase | soundness violations | extra kills |
|---|---:|---:|---:|---:|---:|
| defu, scule, cookie-es, pathe, ufo | 14-26 | 14-25 | 12-17 | 0 | 0 |
| hono (6 shards) | 73 | 69 (64-66 in two other full runs) | 51 | the same 10 | 0 |
| tRPC (32 shards) | 1,190 | 1,173 | 755 | the same 2 | 0 |

- **hono:** workers busy 61% to 68-74%; a mutant its first copy leaves alive goes on within the stage.
- **tRPC:** the same time; its workers were already busy 92%. Before the second change, one run in
  three of tRPC's shard 12 ended with SIGTERM about a minute in: twelve workers held 2 GB each,
  copies whose test had timed out, their runaway loops running until the copy's limit. Every shard
  has finished since.
- **Tried and dropped:** sending Vitest's "start" to workers started ahead (Vitest's setup in a
  worker calls back to Vitest's process, so a worker started that way waited until stopped and the
  workers ahead ran out); a mutant's later files all listed, one at a time (hono 102 ms); 24 workers
  instead of 15.

## Where StrykerJS's time goes on tRPC (2026-11-22)

Measured with `diag/stryker-profile.sh`: StrykerJS 10.1.1 as the cold
scenario runs it, on tRPC's first cold shard (122 mutants, 111 with coverage), every Node process
recording its start, end, forks and, in a Vitest worker, the test file of each "run" message.

- **Processes.** 16 test runners, each holding one Vitest instance for the whole run. StrykerJS sets
  Vitest's `maxWorkers: 1` and the pool it is told (`forks` here) and leaves `isolate` as the
  project has it (true): every test file of every mutant run is a fresh worker process, one file
  per worker. Nothing is shared between test files or mutants.
- **Per mutant.** 728 workers in the 123 s mutant phase, 6.6 per mutant run (Supercov ran 11.5 copies
  of a mutant over the whole of tRPC, plus 1.4 baselines and 0.4 confirmations). StrykerJS runs a
  mutant's covering test files one after another in its runner, with `bail: 1`, so it stops at the
  first file that fails; on this shard it ran 98 tests a mutant to Supercov's 49.
- **Per worker.** Its process starts in 18 ms; Vitest's "start" to its "run" takes 314 ms (p50), and
  a runner's next worker starts 25 ms after the one before ends. A worker lives 526 ms at the median
  (1,366 ms on average, a long tail of slow files). Supercov's tRPC copy holds a worker 650 ms: 200
  ms from handover to claim and 450 ms of the copy.
- **Covering files.** Over all of tRPC, StrykerJS's covering tests span 33.5 files for a killed mutant
  and 36.6 for a survivor (static mutants: every file); Supercov's candidates 30.1 and 38.0.

So StrykerJS's advantage is not in how a worker starts or what it shares (nothing), but in running
fewer files per mutant: a mutant's files one at a time with a stop at the first kill, across 16
runners that each work on another mutant. Supercov runs a mutant's later files in batches of 1, 4, 16,
which overshoot a kill that comes early in a batch. Running every mutant's files one at a time within
a stage was tried and was slower on hono (102 against 64 ms a mutant): a file can start only once
the copy before it has ended, and the stage barrier then holds the next file back.

## Results of 2026-11-22: workers started ahead, a process only once the file is known

Supercov's cold rows again (`results/2026-11-22/summary.md`; every other row is 2026-11-20's), on
the binary of `e19bd687` (key `7bde1dc6674d633b`), against the spawner binary of the rows before, on the same 16-vCPU machine type:

| project | strong pass s, spawner | strong pass s, now | ms per mutant run (StrykerJS's mutant phase) | soundness violations | extra kills |
|---|---:|---:|---|---:|---:|
| defu, scule, cookie-es, pathe, ufo | 2-18 | 2-16 | 14-26 (12-17) | 0 | 0 |
| hono (6 shards) | 339 | 301 | 73 (51) | the same 10 diagnosed ground-truth artifacts | 0 |
| tRPC (32 shards) | 3,774 | 2,912 | 1,190 (755) | the same 2 diagnosed ones | 0 |

No mutant changed between detected and not against the spawner binary (a few between Killed and
Timeout). Paired on the same day and machines (hono's 6 shards, tRPC's 1-12), with
`SUPERCOV_STRONG_WORKERS_AHEAD=0` as the control: hono 81 to 73 ms a mutant, tRPC 1,443 to 1,142.

Where the time goes now:

- **tRPC.** 24 copies a mutant, of which 11 had nothing left to run and now cost no process; the
  other 13 hold a worker 650 ms each: 200 ms from the worker's handover to its claim (Vitest's
  start, the jsdom instance, setup files) and 450 ms of the copy itself (its test file's imports,
  about 130 ms, and its tests). Workers are busy 92% of the pass but the machine's CPUs only 54%:
  copies wait on their tests' servers and timers and on Vitest's process. 24 or 32 workers instead
  of 15 gave 2-7% (`w24`, `w32`), within the spread between shards.
- **hono.** 3 copies a mutant at 26 + 138 ms; workers busy 61%, CPUs 51%: what is left is each
  stage's end, where a mutant's next files wait for the stage before.
- **Tried and dropped:** Node's compile cache in the workers started ahead (no change on hono, 4% on
  tRPC), and more workers (above).

## Results of 2026-11-21: the strong pass in stages

Supercov's cold rows again (`results/2026-11-21/summary.md`), on the binary of the strong pass in
stages (key `0f34df1d56509c31`); every other row is
2026-11-20's. Against the binary of the previous rows (`e8f192cd`, rounds), on the same 16-vCPU
machines:

| project | strong pass s, rounds | strong pass s, stages | ms per mutant run (StrykerJS's mutant phase) | soundness violations | extra kills |
|---|---:|---:|---|---:|---:|
| defu | 2.4 | 2.1 | 22 (17) | 0 | 0 |
| scule | 2.1 | 1.9 | 25 (17) | 0 | 0 |
| cookie-es | 11.9 | 10.2 | 15 (12) | 0 | 0 |
| pathe | 12.8 | 11.7 | 24 (17) | 0 | 0 |
| ufo | 26.9 | 18.7 | 21 (14) | 0 | 0 |
| hono (6 shards) | 1,181 | 366 | 89 (51) | 10, the diagnosed ground-truth artifacts above | 0 |
| tRPC (32 shards) | 6,343 | 3,737 | 1,528 (755) | 2, the diagnosed ones above | 0 |

Verdicts are those of the rounds, but for Killed against Timeout (both detected) and one hono mutant
that a flaky timing test (`Logger by Middleware > Time in seconds`) had killed under the rounds and
that every stages run calls survived, as the ground truth does. Where the time went, and what each
step of the change did, is in `docs/design/strong-mutation.md`: workers waited at each round's end
(22-32% busy); a mutant's later files listed at once cost a worker's start each, and on hono a fork
of Vitest's 1 GB main process (24 ms, 72% of its time); retries and long copies held stages' ends.

## Results of 2026-11-20

`results/2026-11-20/summary.md` has every table; `summary.json` the same with per-shard detail and
examples of every verdict disagreement; `jobs/` each job's `result.json`. In short (wall seconds on
one 16-vCPU machine; tRPC's full runs are summed over their shards):

| project | StrykerJS cold | mutator cold | Supercov cold (measure + strong) | ground truth | PR, cold: Stryker lines / mutator `--since` | PR, warm: Stryker `--incremental` / mutator `--since` | PR: Supercov measure / `--diff` |
|---|---:|---:|---:|---:|---:|---:|---:|
| defu | 3.4 | 3.9 | 3.1 | 3.5 | 1.7 / 1.5 | 1.7 / 1.5 | 1.2 / 0.9 |
| scule | 2.9 | 6.8 | 2.5 | 3.9 | 1.4 / 1.6 | 1.7 / 1.5 | 1.0 / 0.8 |
| cookie-es | 10.4 | 17.7 | 12.6 | 11.4 | 1.7 / 1.3 | 1.9 / 1.2 | 0.7 / 0.9 |
| pathe | 10.8 | 18.9 | 13.6 | 14.4 | 2.0 / 1.9 | 3.4 / 1.9 | 3.3 / 1.5 |
| ufo | 16.9 | 33.1 | 28.1 | 27.9 | 2.5 / 2.2 | 2.9 / 2.0 | 1.1 / 1.4 |
| hono | 258.5 | 301.4 | 1,452 (strong pass 6 shards) | 1,192 (6 shards) | 27.6 / 12.4 | 37.9 / 11.7 | 9.0 / 23.3 |
| tRPC | 2,782 (8 shards) | 3,446 (10 shards) | 9,758 (strong pass 32 shards) | 10,762 (34 shards) | 93.0 / 21.9 | 94.5 / 2.8 | 22.9 / 7.7 |

- **Verdicts.** On the mutants both tools generate, the two cold runs agree with each other and
  with the ground truth everywhere in the five libraries. On hono mutator calls 3 mutants in
  `src/utils/html.ts` Survived that one test kills; one of them carries mutator's weak-mutation
  reason ("not infected"). On tRPC both cold runs call one `contentType.ts` mutant Survived that
  one test kills under `disableBail`, and mutator kills three `rootConfig.ts` mutants (module-level
  checks of `VITEST_WORKER_ID`) that StrykerJS's ground truth calls NoCoverage or Survived: those
  depend on the process the code is loaded in, not on the tests.
- **Ground-truth cost.** Without early exit StrykerJS needs 1.0 to 1.7 times its cold time on the
  libraries, 4.6 times on hono and 3.9 times on tRPC. A killed mutant has on average 5 (defu) to
  95 (hono) killing tests; 339 of tRPC's 1,632 killed mutants have exactly one.
- **Mutant sets.** StrykerJS makes no mutants under `as` casts: the scule and tRPC edits are on
  such lines, so StrykerJS's changed-line run has 0 mutants for both.

Where the time goes:

- **Libraries.** StrykerJS spends about 1.2 s setting up and under 1.5 s in its dry run; the rest
  is mutants. mutator's dry run takes 0.1 to 0.2 s, but its type check of every mutant takes 2 to
  12 s, more than its mutant phase on defu and scule, and it removes 15 to 25% of mutants as
  CompileError before they run.
- **hono.** StrykerJS's dry run with per-test coverage takes 29 s in one test runner (the plain
  suite takes 6.6 s on all cores); its mutant phase 227 s, which StrykerJS itself says is two
  thirds static mutants (398 of 4,480). mutator's dry run takes 4.7 s and its mutant phase 294 s
  with 8 sessions; it uses less CPU (2,003 s against 2,783 s) but takes longer.
- **tRPC.** StrykerJS's dry run takes about 92 s in every job (one forked worker runs the whole
  integration suite, which takes 20 s on all cores), so its PR runs take 93 to 95 s for 0 or 1
  mutants: `--incremental` reuses results but still runs the dry run. mutator's dry run takes 20 s
  cold, and 1.7 s warm with a snapshot, where it re-collects 26 of the 115 test files.
- **PR scenario.** mutator's warm `--since` run is the fastest or within 0.1 s of it everywhere
  (1.2 to 11.7 s). Its cold
  `--since` runs only the diff's mutants but pays a full dry run; StrykerJS's warm
  `--incremental` run still instruments and dry-runs the whole PR scope.

### Supercov

Supercov cold rows come from one binary: `af7340e5` plus the working tree (build key
`dcb4c21223a29dde`), at the default strong-round size (twice the
Vitest workers). tRPC's shard 16 was re-run after
its measure hit a flaky test (`sse.test.ts > SSE on serverless - emit and disconnect early`). The PR
column is from an earlier binary (`8342afac`). Wall seconds; hono's and tRPC's
strong pass are summed over their shards (6 and 32), each of which measured again (one job's
measure is shown).

| project | measure s | strong pass s | total s | StrykerJS cold s | mutants | statuses | score on joined (Supercov / truth, mutants) | disagreements with the ground truth | kills confirmed without the mutant / overturned | PR: measure / `--diff` s, mutants run |
|---|---:|---:|---:|---:|---:|---|---|---|---|---|
| defu | 1.1 | 2.0 | 3.1 | 3.4 | 103 | K87 S16 | 84.5 / 84.5 (103) | 0 | 9 / 0 | 1.2 / 0.9, 6 |
| scule (Vitest 5.1.1) | 0.9 | 1.6 | 2.5 | 2.9 | 78 | K73 S5 | 93.6 / 93.6 (78) | 0 | 20 / 0 | 1.0 / 0.8, 1 |
| scule (own Vitest 1.5.3) | 1.4 | 2.8 | 4.3 | 2.9 | 78 | K73 S5 | 93.6 / 93.6 (78) | 0 | 20 / 0 | 1.3 / 1.0, 1 |
| cookie-es | 1.2 | 11.3 | 12.6 | 10.4 | 691 | K573 T32 S84 NC2 | 87.5 / 87.5 (691) | 0 | 122 / 0 | 0.7 / 0.9, 6 |
| pathe | 3.0 | 10.6 | 13.6 | 10.8 | 490 | K425 T4 S60 NC1 | 87.5 / 87.5 (488) | 0 | 102 / 0 | 3.3 / 1.5, 9 |
| ufo | 1.1 | 27.0 | 28.1 | 16.9 | 926 | K753 T11 S141 NC21 | 82.5 / 82.5 (926) | 0 | 163 / 0 | 1.1 / 1.4, 17 of 18 |
| hono | 9.2 | 1,443 (6 shards) | 1,452 | 258.5 | 4,295 | K3620 T91 S506 NC78 | 86.4 / 86.6 (4,290) | 10, all ground-truth artifacts; no extra kills | 2,155 / 17 | 9.0 / 23.3, 11 |
| tRPC | 22.7 | 9,736 (32 shards) | 9,758 | 2,782 | 2,688 | K1858 T208 S490 NC119 RE13 | 77.0 / 77.1 (2,646) | `withPing.ts:37` (StrykerJS hit-limit heuristic; Supercov: survived, by design), `contentType.ts:109` (flaky) | 1,248 / 0 | 22.9 / 7.7, 1 |

**Disagreements, classified.**

- Five libraries and scule (both Vitest versions): none.
- hono, 10 "soundness violations", all **ground-truth artifacts**:
  - the seven diagnosed earlier (below): `smart-router/router.ts:14:24` and `:15:7`, `jwt.ts:156:77`, `html.ts:63`, `:76`, `:81` and `:161`;
  - three new ones, `html.ts:82:9` `typeof r === 'object'` to `true`, `html.ts:100:21` (the `if (match === -1)` block emptied) and `html.ts:185:7` `preserveCallbacks` to `false`. Earlier binaries killed them with the same order-dependent tests as the false kills below, so they agreed with the ground truth. This binary overturns those kills and calls them survived. Applied by hand (`diag/hono-overturned.sh`), each passes `render to stream` alone, `streaming.test.tsx` and `components.test.tsx` whole, both together, and the whole suite (5,010). So survived is right, and StrykerJS's kills (by `render to stream` and the `ErrorBoundary > streaming` tests) are its own artifacts, like `html.ts:63`/`:76`/`:81`/`:161`.
- hono extra kills: **none**. The five of `6049a02f` are gone, `html.ts:83` included (now survived, as in the ground truth). They were Supercov false kills; four came from order-dependent tests run without their predecessors (`diag/hono-extra.sh`).
- tRPC `stream/utils/withPing.ts:37:9-37:20:ConditionalExpression:false`: **StrykerJS hit-limit heuristic (Supercov: survived, by design)**. Diagnosed with `diag/trpc-withping.sh`):
  - StrykerJS's report says "Hit limit reached (8224/4100)": its limit is 100 times the 41 evaluations of its dry run, and its Timeout means passing that limit, not a hang.
  - With plain Vitest the two covering files pass with the mutant (1.7 s). The covering tests take about twice as long (97–131 ms against 51–61 ms; `e2e, withPing` 144–155 ms against 6–8 ms), and line 37 is evaluated about 45,350 times against 8, all during the tests.
  - No test observes the mutant, so Supercov keeps "survived" deliberately. Its copies report `claimed`, every test passing, and `done`. mutator calls it Killed.
- tRPC `http/contentType.ts:109:7-110:39:LogicalOperator:||`: **flaky** ground-truth kill (one test; StrykerJS's cold run and mutator call it survived too).

**Diagnosis of hono's seven** (binary `568217e7`, key `62219e9f2b81e21b`). A run
kept every strong round (`SUPERCOV_STRONG_KEEP_ROUNDS=1`); `diag/strong-rounds.mjs` reads them.
A second run (`diag/hono-order.sh`) applied each mutant by hand and ran plain Vitest. For
every mutant, the plan's `index` names the same mutant in the strong copy's
`frontend-evidence/mutants.json` (router 8 and 9, html 14, 32, 42 and 115, jwt 160).

- **`smart-router/router.ts:14:24` BlockStatement and `:15:7` CallExpression.**
  - `Should be a read-only` is a candidate. It ran with each mutant switched on, in the whole
    `src/hono.test.ts` (round 10 and round 12, 225 tests, `wholeFile: true`), and reported
    `failed: false`. No test of any of the 58 copies failed. Round 0 (no mutant) passed too.
  - With the throw removed, `this.#routes.push(...)` on the built router's `undefined` routes
    throws a TypeError, so `expect(...).toThrow()` still passes.
  - StrykerJS's covering set for the block is that test alone. Run alone, it **fails without any
    mutant** (`1 failed | 224 skipped`): it depends on earlier tests in its `describe` building the
    router (`app.request(...)`).
  - The whole file passes with the mutant (225 passed). StrykerJS's kill comes from running an
    order-dependent test without its predecessors.
- **`jwt.ts:156:77` `<=` (`now < payload.iat`).**
  - With this binary the mutant has the `clock` hint (`untestable: clock`, "an operand is a clock
    reading: they differ only on one exact millisecond") and the plan runs it (`decided: null`,
    121 candidates, reason `unknown`). The binary of `2e394f5a` had decided it.
  - It ran in round 8 (`jwt.test.ts`, 73 tests), including `JwtTokenIssuedAt` and the `iat =
    Infinity` test, and nothing failed. It survived.
  - `JwtTokenIssuedAt` sets a fake clock to 1633046400 and never restores it. The `iat = Infinity`
    test restores real timers before the `JwtTokenIssuer` tests, whose tokens carry
    `iat: 1633046400`.
  - StrykerJS's covering set has `JwtTokenIssuedAt` but not the `Infinity` test (its `iat` check
    short-circuits before the comparison). In that subset the fake clock leaks, `now == iat`, and
    the five `JwtTokenIssuer` tests fail with the mutant. Run by hand: that subset kills it (5
    failed), and the whole file passes (98 passed).
- **`html.ts:63` (`i < 0` to `<=`), `:76` and `:81` (`typeof r === 'object'` to `true`), and
  `:161` (`instanceof Promise` to `true`).**
  - `render to stream` is not a candidate for these four. Its measured evidence does reach the
    lines: it is an infected candidate of `:63` `true`/`false`/`>=`, `:76` `false`/`!==`, `:81`
    `false`/`!==` and `:161` `false`. But for it the condition was always true (`:76`, `:81`,
    `:161`), and `i` never 0 (`:63`), so these four replacements cannot change its run.
  - Round 0 ran `streaming.test.tsx` (46 tests) and passed.
  - Applied by hand, each of the four passes `render to stream` alone, the whole
    `streaming.test.tsx` and the entire suite (5,010 passed).
  - StrykerJS's kill with that test does not reproduce outside StrykerJS's run, so the
    difference is in how StrykerJS's mutated program runs, not in the tests.

So none of hono's seven is a Supercov soundness violation. Each is a StrykerJS "kill" that the
suite, run in its own order or in full, does not make: two need a test run without its
predecessors, one needs a fake clock leaked from a test whose clean-up was skipped, and four do
not reproduce outside StrykerJS.

**Static (module-level) mutants**, joined to the ground truth:

- pathe 62: Supercov K48 S14, ground truth K46 S14 RE2. The two RuntimeErrors are Supercov's kills.
- hono 320: Supercov K295 T1 S24, ground truth K269 T26 S24 RE1 (the same 24 survive in both).
- tRPC 399: Supercov K291 T19 S87 RE2, ground truth K212 T92 S87 RE8 (the same 87 survive).
- defu 2 and ufo 5: same verdicts.

**Where the time goes** (strong pass per mutant it ran, wall / CPU):

| | `af7340e5` | `6049a02f` | StrykerJS mutant phase, per mutant |
|---|---|---|---:|
| defu, scule, cookie-es, pathe, ufo | 17–31 ms / 147–256 ms (scule on Vitest 1.5.3: 37 / 317) | 16–28 ms / 121–212 ms | 12–17 ms |
| hono | 352 / 1,038 ms | 296 / 941 ms | 51 ms |
| tRPC | 4.0 / 12.5 s | 3.7 / 11.9 s | 0.76 s |

- Confirming kills costs 5–20% wall time per mutant. hono confirms 2,155 kills and overturns
  17; tRPC confirms 1,248 and overturns none; the libraries confirm 9–163 each and overturn none.
- Per mutant, hono's strong pass is about 7 times StrykerJS's mutant phase and tRPC's about 5
  times; on the libraries Supercov is 1.2–2.2 times StrykerJS per mutant.
- Measure costs the plain test run plus 0.4–2.6 s.
- Three tRPC shards (5, 23, 25) end a pass at its deadline and finish in the next; no job is lost.

**Strong-round size**, measured with the earlier binary of `0331242b` (key `92c60964ce6caf76`), each setting a full cold run:

| project | setting | strong pass s (sum of shards) | strong ms per mutant run, wall / CPU | StrykerJS mutant phase, ms per mutant | verdicts different from setting 6 |
|---|---|---:|---|---:|---|
| hono | 2 | 1,104 | 269 / 907 | 51 | 3 (all Killed vs Timeout) |
| hono | 6 | 1,124 | 274 / 1,083 | 51 | – |
| hono | 12 | 886 | 216 / 1,139 | 51 | 16 (all Killed vs Timeout) |
| tRPC | 2 | 8,931 | 3,663 / 11,740 | 755 | 19: 18 Killed vs Timeout, 1 detected vs not (below) |
| tRPC | 6 | 8,594 | 3,525 / 14,365 | 755 | – |

- Larger rounds trade CPU for wall time: hono's setting 12 is 21% faster than 6, tRPC's 6 is 4%
  faster than 2.
- Per mutant, hono's strong pass is still about 5 times StrykerJS's mutant phase, and tRPC's
  about 5 times.
- The only change of detected versus undetected across settings is tRPC
  `rpc/codes.ts:68:4-68:10:UnaryOperator:+` (`[-32029]` key of the error-code map to
  `[+32029]`, a static mutant). Setting 6 killed it with `links.test.ts > batching > batching on
  maxURLLength` after 332 tests; setting 2 calls it survived after 938 tests; StrykerJS's cold
  run and ground truth call it survived. It is likely a timing-sensitive test failing under the
  heavier round, so a flaky kill rather than a real one.



## Files

| file | |
|---|---|
| `corpus.json` | projects, commits, commands, mutate sets, shards, tool versions |
| `patches/<project>.patch` | the PR-scenario edit, applied with `git apply` |
| `configs/` | the Vitest configs for hono and tRPC |
| `run-job.mjs` | one CI job: clone, install, run one scenario, write `result.json`, logs and reports |
| `workflow.mjs` | prints the matrix workflow |
| `plan-shards.mjs` | splits a mutate set into ground-truth or cold-run shards |
| `summarize.mjs` | artifacts to `results/<date>/summary.json` and `summary.md` |
| `supercov-join.mjs` | reads Supercov's mutants and joins them to StrykerJS's (used by `summarize.mjs`) |
| `build-workflow.mjs` | prints the workflow that builds the Supercov binary and caches it by key |
| `diag/hono-order.sh`, `diag/strong-rounds.mjs`, `diag/hono-extra.sh`, `diag/hono-overturned.sh`, `diag/trpc-withping.sh`, `diag/diag-*.sh` | the diagnoses of disputed mutants: hand-applied mutants under plain Vitest, evaluation counts, and Supercov's kept strong rounds (`shell-diag` and `supercov-diag` scenarios) |
| `results/<date>/` | a run's summary and every job's `result.json` |

## Caveats

- **One sample per number.** Each scenario ran once; the mutator warm-up in `pr-mutator` is a
  second cold mutator sample. Sub-second times carry noise from machine start-up and file caches.
- **Different machines.** Every job gets a fresh machine of the same type, but spot capacity
  decides which; `result.json` records the instance type and CPU model.
- **Defaults differ.** StrykerJS runs 16 test runners and no type check; mutator runs 8 sessions
  (half the CPUs), type-checks mutants (CompileError, left out of the score) where it can, and
  decides some covered mutants Survived without running them (weak mutation: no covering test
  reaches the mutated expression with a different value). Each tool is measured as its users
  would run it, not tuned to the other.
- **Mutator sets differ.** StrykerJS skips everything under `as` casts and has switch-case and
  richer regex mutators; mutator has return-value (FnValue), ternary-test and other mutators of
  its own. Total scores are not comparable; the score on shared mutants is.
- **Vitest versions differ** for the four unjs libraries on Vitest 4 and for scule (see Versions).
- **Ground truth is StrykerJS's view.** It inherits StrykerJS's test-runner behaviour, such as
  how failures while a test file is collected are classified, and Timeout/Killed can flip from run
  to run for mutants that make code loop.
- **CPU seconds** come from GNU `time`: only descendants that were waited for are counted, which
  covers both tools' worker processes; worker threads count with their process.
- **Sharded totals** include each shard's set-up and dry run, so they overstate what one
  unsharded run on one machine would cost.
- **Flaky tests.** One hono StrykerJS dry run failed on a timing assertion (`Logger by Middleware
  in NO_COLOR > Time in seconds`: `999ms` where `1s` was expected); that job was run again.
