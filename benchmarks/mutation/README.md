# Mutation-testing benchmark

Baselines and a harness for measuring mutation testing of TypeScript projects tested with Vitest. It
records what two existing tools cost and decide on a fixed corpus, so that a new mutation-testing
mode can later be measured against the same numbers:

- **StrykerJS** (`@stryker-mutator/core` and `@stryker-mutator/vitest-runner`), tuned for speed;
- **mizchi/mutator** (`@mizchi/mutator`), a diff-driven tool for Vitest;
- a **ground truth**: StrykerJS with `disableBail`, so every covering test runs against every
  mutant, which gives the full kill matrix (which tests kill which mutant).

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

Vitest changes made inside the jobs, and recorded in each job's `result.json`:

- mutator needs Vitest 5. defu, cookie-es, pathe and ufo pin Vitest 4, scule Vitest 1.5.3: the
  mutator jobs remove the project's `@vitest/*` dev dependencies and add `vitest@5.1.1`.
- StrykerJS 10's Vitest runner needs Vitest 2 or later, so scule's StrykerJS jobs get
  `vitest@5.1.1` too. For scule both tools also get `vite@8.3.8`: its lockfile resolves Vite
  5.0.12, which Vitest 5 cannot load. The other projects run StrykerJS on their own Vitest (4.x, or 5.x for hono
  and tRPC). StrykerJS 10.1 supports Vitest 5; 10.0 did not, which is why mizchi/mutator's
  benchmark ran StrykerJS on Vitest 4.

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
- **tRPC PR scenario** (`prMutate`): the warm-up and PR runs cover the edited file and its small
  siblings in `unstable-core-do-not-import/http/` (about 140 StrykerJS mutants): mutator needed
  8 minutes for the 390 mutants of `http/` without `resolveResponse.ts`.

## Reproducing

From the repository root, with a runner label for 16-vCPU machines:

```sh
# 1. the workflow (one job per project x scenario x shard)
node benchmarks/mutation/workflow.mjs --runs-on <16-vCPU label> --truth-runs-on <16-vCPU, 64 GiB+ label> \
  > .github/workflows/mutation-bench.yml
#    --projects defu,ufo, --scenarios stryker-cold,truth and --shards s2,s6 narrow it

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

## Results of 2026-11-20

`results/2026-11-20/summary.md` has every table; `summary.json` the same with per-shard detail and
examples of every verdict disagreement; `jobs/` each job's `result.json`. In short (wall seconds on
one 16-vCPU machine; tRPC's full runs are summed over their shards):

| project | StrykerJS cold | mutator cold | ground truth | PR, cold: Stryker lines / mutator `--since` | PR, warm: Stryker `--incremental` / mutator `--since` |
|---|---:|---:|---:|---:|---:|
| defu | 3.4 | 3.9 | 3.5 | 1.7 / 1.5 | 1.7 / 1.5 |
| scule | 2.9 | 6.8 | 3.9 | 1.4 / 1.6 | 1.7 / 1.5 |
| cookie-es | 10.4 | 17.7 | 11.4 | 1.7 / 1.3 | 1.9 / 1.2 |
| pathe | 10.8 | 18.9 | 14.4 | 2.0 / 1.9 | 3.4 / 1.9 |
| ufo | 16.9 | 33.1 | 27.9 | 2.5 / 2.2 | 2.9 / 2.0 |
| hono | 258.5 | 301.4 | 1,192 (6 shards) | 27.6 / 12.4 | 37.9 / 11.7 |
| tRPC | 2,782 (8 shards) | 3,446 (10 shards) | 10,762 (34 shards) | 93.0 / 21.9 | 94.5 / 2.8 |

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

CI runs used for these results: 7918046463203622 (all projects), 6816238857423170 (scule),
8402334254856692 (hono PR, StrykerJS), 6506939654971330 (tRPC PR), 7095666567225085,
7210302856247792, 8297132922154682 and 6783811573326500 (tRPC full runs and ground truth).

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
