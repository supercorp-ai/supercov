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

### 2. Rounds started by the pass itself

The rounds the engine started the build again for (the mutants a JVM left
when it stopped taking mutants, then the generous round for those past the
short limit) are started by the build's own test JVM once its pass ends:
the same java, options and class path, so a round there is what a round of
a new build is, without Maven's or Gradle's start and compilation. A round
that decides nothing and lists nothing new ends them, and the engine
carries on from there as before.

Result (run 46085994371 against 46082385337): every verdict the same
(commons-cli 882 of 882; kotlinpoet 1,238, one `killed` now a `timeout` at
the machine's speed). On kotlinpoet the five retry builds (242 s) are gone;
on commons-cli one shard still started the build again for a mutant whose
test exhausts the heap (`Option.java:856`), which stopped the test JVM's
pass before it could start its rounds. This run's runners were slower than
the last (a run with a mutant took 58 ms against 44 ms on kotlinpoet), so
its wall times are not comparable; the builds it no longer starts are.

What the per-shard builds show besides: eight of kotlinpoet's twelve
shards start the build twice, the first ending after about 20 s when the
compiler refuses a switch (a Kotlin mutant that does not compile is
`unviable`); that is 180 s of kotlinpoet's strong pass.

### 3. The tests that killed the file's other mutants first

A JVM keeps, for each source file, the tests that killed its mutants and
how many, and runs a mutant's candidates that did first, the most kills
first, the rest in the plan's order. The first kill ends a mutant's run, so
the order decides only which candidate kills it. Before, kotlinpoet's 54
mutants killed after more than 100 candidates ran 13,256 tests between them.

### 4. The pass's own JVMs, with C1 alone

Where the pass has JVMs of its own (`SUPERCOV_STRONG_JVMS` above one), they
run every mutant and the build's test JVM waits for them, and they run with
`-XX:TieredStopAtLevel=1`. Each mutant's classes are loaded afresh, so what
the JIT compiles is thrown away with its loader; C2's compilations of it
took as much processor time as the tests did (commons-cli 266 s of JIT
against 167 s of runs with a mutant), beside them, on as many processors as
there are JVMs.

Result of 3 and 4 together (run 46091426206 against 46085994371): every
verdict the same (commons-cli 882, one `killed` now a `timeout` at the
machine's speed; kotlinpoet 1,238).

| | commons-cli before | after | kotlinpoet before | after |
| --- | ---: | ---: | ---: | ---: |
| JIT time (s) | 266 | 14 | 2,485 | 108 |
| runs with a mutant | 7,395 | 6,534 | 38,371 | 32,839 |
| their time (s) | 167 | 143 | 2,239 | 1,717 |
| baselines' time (s) | 84 | 40 | 1,707 | 1,368 |
| strong pass (s, wall) | 194 | 124 | 2,023 | 1,745 |
| measured run (s, wall) | 36 | 25 | 1,018 | 936 |
| PIT (s) | 94 | 115 | 1,346 | 1,425 |

The measured durations the plan gives the JVM pass are all 0: the
measured run records no duration for a JVM test, so each test's limits come
from its baseline alone.

Where kotlinpoet's time is left: each of its twelve shards starts Gradle
three times (the measured run, the strong copy's build that the compiler
refuses a switch in, the build again without it), and each shard measures
and baselines the whole suite. Per shard the JVMs are busy for about 67 s
of a 145 s strong pass. Survivors every test reaches (22 mutants with 857
candidates each) are more than half the runs with a mutant.

### 5. Later candidates in batches (rejected)

Past a mutant's first four candidates, the rest ran in batches of up to 64,
one JUnit Platform request each, the first failing test of a batch
confirmed alone. It changed a verdict: commons-cli's
`DefaultParser.java:711` (`VoidMethodCall`) went from `survived` to
`killed`, a test that passes alone failing in a batch (a class's setup runs
once for the batch, and the batch's order is the engine's, not the
candidates'). Nor was it faster: a batch runs every test in it past the one
that kills, and the runs with a mutant rose from 6,534 to 9,406 on
commons-cli and from 32,839 to 43,832 on kotlinpoet (run 46096323495).
Dropped.

### 6. No second verification of each mutant's classes

The pass's own JVMs run with `-XX:+UnlockDiagnosticVMOptions
-XX:-BytecodeVerificationRemote`: each mutant's classes are loaded afresh,
and verifying classes the project's compiler wrote, again for every mutant,
was part of what each mutant's first test cost. Where none of the pass's
JVMs starts (a JVM that refuses an option, as the first try of this did
until the diagnostic options were unlocked), the build's test JVM runs the
pass itself.

Result (run 46103054380 against 46091426206): every verdict the same
(commons-cli 882, kotlinpoet 1,238). Each mutant's first run took 81 s of
JVM time on commons-cli instead of 98 s, and 157 s instead of 163 s on
kotlinpoet; the strong pass took 88 s against 124 s on commons-cli (where
this run had no retry build) and 1,660 s against 1,745 s on kotlinpoet.

Where commons-cli's strong pass goes now (shard 1, 42 s): the build and
its compilation about 6 s, the JVMs' first round 4 to 14 s each, then two
rounds of about 10.5 s for its seven mutants past the short limit, each a
`timeout` only after the generous limit's 10 s. PIT, set the same way
(`timeoutConstInMillis = 10000`), waits as long for each of its timeouts.

### 7. Reach for Kotlin expression bodies

A Kotlin expression body has no statement point, so a mutant in it was
linked to no point, and every test was its candidate unless a value probe
recorded where it ran: on kotlinpoet 22 survivors had 857 candidates each,
more than half of the runs with a mutant. A run measured for mutants (as
the value probes are, and not counted as coverage) now writes `fun f() = e`
as `fun f() = kotlin.run { Supercov.wreach(slot); e }` (`run` is inline:
the body's type is still `e`'s, a `return` in it still returns from `f`, a
suspending call still suspends). A `tailrec` function's body and a getter's
that reads `field` are left as written. Each mutant of the body with no
other point takes the body's probe; `js_weak_mutation::pit_mutator` reads
one that recorded nothing as not reached, and one that recorded as reached
with infection unknown, so the plan keeps as candidates only the tests that
ran the body.

Result (run 46146878655 against 46103054380 for kotlinpoet and 46046555220
for picnic): kotlinpoet 1,231 of 1,238 verdicts the same, picnic 319 of the
342 both runs list. Every one that changed is a mutant in a body no test
ran: `survived` before, `no-coverage` now (6 on kotlinpoet, 23 on picnic),
and PIT says `NO_COVERAGE` for each of them (on the line where it reports
a body that spans several). One `killed` is a `timeout` at the machine's
speed. PIT's soundness violations are the same ones as before. The pass ran
20,798 tests with a mutant on kotlinpoet against 32,125; its wall time did
not show it on this run's runners, whose builds took a third longer than
the last run's on the shards whose runs did not change (shard 6: the same
303 runs, 81 s against 55 s).

## Next

### Durations from the measured run

A JVM test's measured duration is 0 in the plan (`OwnedTestEvidence` and
the JVM runtime's evidence carry none), so each test's limits come from its
baseline alone. Recording them would not make the limits tighter: a limit
is 1.5 (or 3) times the longer of the measured time and the baseline, so a
measured time only ever raises it, and the baseline (a fresh class loader,
C1 in the pass's own JVMs) is the slower of the two. A tighter limit would
have to drop the baseline from it, which changes which mutants reach the
generous round and, there, which of them are timeouts: not a change that
can keep every verdict. Left as is.

### One build for the measured run and the strong pass

Each shard starts the build twice (three times where the compiler refuses
a switch). One build would compile the copy with its switches (all off) and
its value probes, run the suite measured, and keep its test JVM: the JVM
writes the run's evidence and waits; the engine plans the pass from it and
writes the plan; the JVM runs the pass. What it takes:

- The switches inserted after the coverage instrumentation, not before:
  `instrument_workspace(…, Some(&strong))` instruments the switched source
  today, which would put the switches' own branches in the measured run's
  coverage. `jvm_schemata::switch_source` works on the source's offsets, so
  it needs the instrumenter's edit map to place them in instrumented text.
- `run_direct_jvm` reading the evidence while the build runs (the runtime
  flushes it at a signal file instead of at the JVM's exit) and writing the
  plan where the waiting JVM reads it; then waiting for the build as now.
- A refused switch still needs a second build: the measured run's results
  stand, and only the strong pass starts again without it.
- The plan's mutants listed before the build, from the sources, as
  `mutants` lists them now on demand.
