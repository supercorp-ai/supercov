# JVM mutation: where the time goes, and what was tried

The goal: Supercov's measured run and strong pass together faster than PIT
as a team runs it, on the benchmark projects (`benchmarks/mutation-jvm/`),
with every verdict as it was. Each experiment runs the benchmark on GitHub's
`ubuntu-latest` (4 vCPUs, 4 PIT threads, 4 strong-pass JVMs) through
`.github/workflows/mutation-jvm.yml`, and its verdicts are compared with the
run before it mutant by mutant (`benchmarks/mutation-jvm/compare.py`).

## The profile

Each strong-pass JVM writes a summary line beside the verdicts (`driver:
"jvm"`): its start-up before the pass, the baselines, the runs with a
mutant (the first test of each mutant, the one that loads its classes
afresh, apart), the confirmations of kills, the classes it loaded and the
JIT's time. The engine adds one for the builds it started (`driver:
"jvm-builds"`): each start, whether it was the generous retry round, and
how long it took. `summarize.py` sums them per project.

### Before (run 46079425710, 2026-11-27)

Times are summed over a project's shards; JVM times are summed over the
JVMs, four at a time.

| | commons-cli | kotlinpoet |
| --- | ---: | ---: |
| PIT (s) | 137 | 1,685 |
| suite without Supercov (s) | 24 | 883 |
| measured run (s) | 18 | 628 |
| strong pass (s) | 143 | 1,396 |
| builds started / of them retry rounds | 5 / 3 | 18 / 3 |
| build time in retry rounds (s) | 60 | 175 |
| JVM time: baselines (s, runs) | 144 (5,712) | 1,207 (17,958) |
| JVM time: runs with a mutant (s, runs) | 154 (7,394) | 829 (15,492) |
| of them each mutant's first run (s) | 95 | 148 |
| JVM time: confirmations (s) | 21 | 72 |
| classes loaded | 232,349 | 1,215,335 |

The measured run costs nothing beyond the suite (the first run without
Supercov also fills the build's caches). In the strong pass:

- **Baselines are the largest share of JVM time**: 43% on commons-cli, 56%
  on kotlinpoet. Each JVM ran every test it needed twice without a mutant,
  so with four JVMs a test was baselined up to eight times, and again in
  each round the engine started.
- **Retry rounds restart the build**: the mutants past the short limit wait
  for a round of their own, and the engine starts Maven or Gradle again for
  it. On commons-cli the three retry rounds were 60 s of a 143 s pass, for
  seven timeouts.
- **Each mutant's first test loads the project's classes afresh**: about
  60% of the run time on commons-cli. This is what keeps verdicts exact
  (no static state from another mutant or a baseline), so it stays; what
  it costs is the thing to cut.

## Experiments

### 1. Baselines shared by the pass's JVMs and rounds

A test's baseline is run once for the whole pass and written to
`baselines.tsv` in the pass's directory; another JVM, or a later round,
takes it from there. Each JVM runs a test in a fresh class loader and a
working directory of its own, so another JVM's baseline is what its own
would be; a kill is still confirmed in the JVM that saw it, so a JVM a
mutant left broken still stops.

Result (run 46082385337 against 46079425710): every verdict both runs
reached is the same (commons-cli 882 of 882; kotlinpoet, where the earlier
run's budget left 430 mutants without a verdict, every one both decided,
save a `timeout` that is now `killed` at the machine's speed). On
commons-cli the baselines run 2,744 times instead of 5,712 and take 88 s of
JVM time instead of 144 s; the strong pass took 136 s against 143 s. PIT
itself took 114 s in this run and 137 s in the one before: runs on shared
runners vary by about a fifth, so a change is judged by the profile's
counts as much as by the wall time. On kotlinpoet each of the twelve shards
is a pass of its own, and nearly every test is a candidate of a Kotlin
expression body's mutant, so each shard still baselines most of the suite.
