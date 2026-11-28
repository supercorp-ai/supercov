# Mutation pipeline speed

What makes the strong pass cheaper without moving a single verdict. Every
change here is measured with `.github/workflows/mutation-pipeline.yml`
(`benchmarks/mutation-pipeline/README.md`) against its base, and
`compare.mjs` must report no decided verdict that moved.

## Baseline (main `ce132bc2`, run 46077199638)

| language | project | mutants | decided without running | ran | test executions | per ran mutant | test s | strong pass s |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| go | btree | 367 | 211 | 156 | 896 | 5.74 | 27.6 | 16.8 |
| go | cast | 190 | 27 | 163 | 2979 | 18.28 | 1.3 | 8.2 |
| go | go-humanize | 332 | 87 | 245 | 3716 | 15.17 | 8.4 | 13.8 |
| go | golang-lru | 165 | 16 | 149 | 1593 | 10.69 | 51.3 | 24.0 |
| go | uuid | 205 | 25 | 180 | 4760 | 26.44 | 20.1 | 18.7 |
| js | cookie-es | 725 | 45 | 680 | 7565 | 11.13 | 215.0 | 73.0 |
| js | defu | 103 | 16 | 87 | 578 | 6.64 | 25.8 | 10.1 |
| js | pathe | 524 | 30 | 494 | 17155 | 34.73 | 148.4 | 47.1 |
| js | scule | 81 | 7 | 74 | 465 | 6.28 | 19.9 | 7.9 |
| js | ufo | 1011 | 89 | 922 | 22057 | 23.92 | 370.2 | 107.7 |
| python | cachetools | 1660 | 0 | 1660 | 12397 | 7.47 | 81.8 | 22.8 |
| python | humanize | 1348 | 8 | 1340 | 12307 | 9.18 | 55.6 | 23.0 |
| python | parse | 1306 | 34 | 1272 | 8152 | 6.41 | 70.0 | 35.2 |
| python | python-slugify | 1124 | 0 | 1124 | 8948 | 7.96 | 109.2 | 30.5 |

Go's own phase timings (`[supercov] Go pass timings`), milliseconds:

| project | check | of it, the type checker's call | vet | binaries | runs | whole pass |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| btree | 8 011 | 1 480 | 596 | 795 | 7 188 | 16 800 |
| cast | 6 522 | 975 | 747 | 377 | 338 | 8 200 |
| go-humanize | 7 234 | 1 519 | 1 359 | 378 | 4 619 | 13 800 |
| golang-lru | 6 796 | 1 222 | 129 | 1 040 | 15 893 | 24 000 |
| uuid | 7 021 | 996 | 1 250 | 381 | 9 880 | 18 700 |

The check phase is the type checker's: building its helper
(`runtime-assets/go/mutants`, the standard library's `go/types` and
`go list -export`) when `.supercov/cache` has none, then one call. The call
takes 1 to 1.5 s; the other 5.5 to 6.5 s are the helper's build, which a CI
job with an empty `.supercov/cache` pays on every pass.

The benchmark measures with `supercov -- go test ./...` and then runs
`runs latest mutants test`. `supercov mutants -- go test ./...`, the
documented command, measures with `SUPERCOV_MUTANTS=1`, which asks the type
checker for the operations' types while instrumenting: there the helper is
built during the measured run, before its tests, and the pass finds it
built. The cost is the same; only the phase it lands in differs.

## Baseline measured with values (run 46082195960)

The same binary, measured as `supercov mutants` measures:

| language | mutants | decided without running | ran | test executions | test s | strong pass s |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| go | 1 259 | 435 (366 without values) | 824 | 12 024 | 107.0 | 51.2 (81.6) |
| js | 2 444 | 216 (187) | 2 228 | 44 393 | 745.1 | 233.1 (245.8) |
| python | 5 438 | 59 (42) | 5 379 | 39 946 | 289.3 | 95.5 (111.4) |

Go's pass is shorter mostly because the helper is now built while
measuring. `compare.mjs` against the run without values: 7 of 9,141
verdicts moved, none a wrong decision without running. 5 went from
`survived` to `no-coverage` (`comparison-not-evaluated`, `not-evaluated`):
the values show that the mutated expression never evaluated. 1 Python
`timeout` became a kill, and 1 uuid kill (`version6.go:45`, `TestVersion6`,
whose input is the clock) did not repeat. Both are noise of mutants that
ran.

Values decide little more on Python: its mutants are mostly infected. Its
cost sits in the tail of killed mutants: the median runs 1 test before the
kill, but 76% of the executions spent on killed mutants come from those
that ran more than 10 (JavaScript: 87%).

## Candidates

### Go: the type-check helper's build

Not pursued. `supercov mutants` builds the helper while it measures, and a
CI job that keeps Go's build cache (setup-go's default) has the helper's
packages compiled already, so only the link is left. The benchmark's 5.5 s
are a cold-cache figure.

The probe (run 46079225619) built the helper five ways on each Go job,
seconds (the five projects within 0.5 s of each other):

| build cache | `-trimpath` (today) | without it |
| --- | ---: | ---: |
| empty | 5.7 to 6.1 | |
| what the measured `go test` left | 5.6 to 6.0 | 1.6 to 1.9 |
| with the helper's packages (setup-go's cache) | 0.13 | 0.13 |

`-trimpath` changes every package's build ID, so the standard library the
project's own tests compiled is compiled again. Without it the same program
builds in a third of the time wherever the cache does not hold the
helper's packages yet (a first run, a cache saved by a job without
Supercov). Taken: the helper is built without `-trimpath`.

Found on the way: the helper's cache key was its source alone, so a pass
after a Go upgrade (or for another `GOOS`, `GOARCH`, `GOFLAGS`) reused a
helper whose `go/importer` was the old toolchain's. Keyed on `go env` since
PR 356.

### The benchmark measured without values

The first baseline measured with `supercov -- <tests>`, not as
`supercov mutants` does, with `SUPERCOV_MUTANTS=1`. Without the values its
probes record, the plan cannot tell a mutant no test infected
(`not-infected`, a survivor without running) or drop a candidate test that
reached a mutant but never infected it (`could_infect`). Python decided 42
of 5,438 mutants that way. From run 2 on the benchmark measures with the
values.

### JavaScript: where the test time goes

Per mutant, from `supercov-strong.mutants.jsonl` (test time summed over
workers, seconds):

| project | killed | their tests | their s | survived | their tests | their s |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| cookie-es | 598 | 4 437 | 170.7 | 93 | 3 061 | 32.3 |
| defu | 87 | 427 | 21.8 | 16 | 151 | 4.0 |
| pathe | 454 | 10 996 | 126.9 | 65 | 6 137 | 19.8 |
| scule | 75 | 198 | 18.6 | 6 | 267 | 1.3 |
| ufo | 824 | 10 067 | 254.9 | 158 | 11 966 | 113.6 |

A killed mutant runs about 7 to 24 tests and costs about 0.3 s: most of it
is the copy (a worker and the test file's module graph), not its tests. So
ordering a mutant's tests saves little on these projects; deciding a mutant
without a copy, or running fewer copies, is what pays. The copy's own cost
is the Vitest strong pass's (`docs/design/strong-mutation.md`), not this
pipeline's.

### Go: mutant order across workers

Mutants run in the plan's candidate order. Sorting them longest first
across workers changes no verdict (each mutant's packages and their order
are the same) and shortens the tail. Not yet measured.

Reordering a mutant's own packages ("cheapest first", as `go-mutation.md`
describes) is not exact: a timeout in one package can become a kill in
another. Most corpus mutants have one candidate package, so it would gain
little anyway.

### Higher-order batching (several mutants in one process)

Exact only when no other mutant in the batch is evaluated anywhere in the
shared process. Mutants that share candidate tests almost always reach each
other, and those are the only batches that would save work. Dropped.

### Trivial compiler equivalence and duplicates beyond JavaScript

JavaScript has both (`js_compiled_equivalence.rs`). A Go compile per mutant
costs about as much as running its package on these projects; Python's
syntax-tree comparison catches only rare edits (removing one of two equal
arguments). Low value; not pursued.

### Sharding by measured cost

Exact only when every job plans from the same measured run, which is why
the hash split replaced it. Not pursued.

### JavaScript: the first file a mutant runs

A Vitest mutant runs its cheapest test file first. Its later files are
ordered by how often each file has killed so far in the pass. Copies that
ran before the killing file's, with the files ordered by their tests'
measured time (an estimate of the driver's order):

| project | cost order | kill-history order (leave one out) |
| --- | ---: | ---: |
| cookie-es | 69 | 0 |
| defu | 24 | 4 |
| pathe | 181 | 104 |
| scule | 0 | 0 |
| ufo | 728 | 154 |

The second column uses kills from the whole run (each mutant's own left
out), so it is an upper bound on what learning during the pass gets. Kills
counted by test file and the mutant's source file, and used for the first
file too, would come closest. A copy costs about 0.3 s of a worker, so on
ufo that is up to about 170 s of its 337 s of test time. Killed and
survived verdicts cannot move, since each copy is its own module graph.
A timeout can turn into a kill when a killing file runs before the one that
timed out, and `compare.mjs` reports that. This is
`vitestStrongReporter.mjs`, which belongs to the JavaScript strong pass's
work. Handed to it with these figures.

### Python: the order of a mutant's tests

Each mutant runs in one forked process and stops at its first failing
test. The driver's docstring said it ran them in collection order. The code
sorts them cheapest first, by the baseline's times. It keeps the verdicts
exact another way: a kill that came after other tests than the baseline ran
before the killer is confirmed by running the same tests, and then the
killer, without the mutant (`_confirmed`, cached by prefix and killer). The
kill stands only if the killer passes there. So the order is already free,
and a different one changes when a verdict comes, not what it is. One fork
per test (dropped above) is not needed for that.

The lever, then: run first the tests that killed the most of the mutant's
function's (then its file's) other mutants so far in the pass, then the
cheapest (`_likeliest_first`). A mutant whose killer goes first has an
empty prefix, so its confirmation is the killer alone without the mutant,
cached per test. Branch `perf/python-kill-history-order`. Measured with
`compare.mjs` on the Python corpus against current main: see the log.

### Already in place

- Per-test candidate selection, every language (`strong_plan.rs`).
- Kill-history ordering for Playwright (`strong_tiers::ordered`).
- Same-value and same-token mutants marked equivalent in Python and Go.

## Experiment log

- 2026-11-28, run 46077199638: baseline above (measured without values).
- 2026-11-28, run 46079225619: probe of the Go helper's build by cache and
  flags (table above).
- 2026-11-28, run 46082195960: baseline measured with values.
- 2026-11-28, run 46146364654: Python-only base on main `55a8e970`, for
  the kill-history order.
