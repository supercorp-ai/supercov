# Strong mutation: `supercov mutants test`

Weak mutation (`docs/design/mutants.md`) says, from one ordinary run, which
mutants a test reached and which it infected. Strong mutation runs the tests
again with a mutant in place and records whether one fails. This record is
the design of that second pass.

The goal is StrykerJS's verdicts -- Killed, Survived, Timeout, NoCoverage --
for the same mutants (`stryker-js@10.1.1`), at a fraction of the cost, with
nothing estimated. Every shortcut below is exact: a mutant or a test is left
out only where the run already proves it cannot change a verdict, and where
the proof does not hold the pass falls back to running more, never fewer.

## Why it can be much cheaper than StrykerJS

StrykerJS starts every run blind. It runs every related test once without a
mutant to learn which test covers what (its dry run), then runs each mutant
against the tests that cover it. Supercov's measured run already holds what
the dry run learns and more:

| StrykerJS needs | Supercov already has |
| --- | --- |
| which tests cover a mutant (dry run) | each test's hits, per test |
| nothing better | which tests **infected** the mutant: a test that saw the original's value at every evaluation cannot kill it |
| per-test timings (dry run) | each test's attempt, and its duration (recorded from this design on) |
| a hit count for runaway loops (dry run) | each test's hits at the mutant's site |

So the pass has no dry run, runs no mutant that no test infected, and runs
each mutant only against the tests that infected it.

## The command

```sh
npx supercov runs latest mutants test                    # every infected mutant of the run
npx supercov runs latest mutants test --diff main        # only the change's mutants
npx supercov runs latest mutants test --file lib/x.ts    # one file's
npx supercov runs latest mutants test -- npx vitest run  # another command than the run's
```

It re-runs the run's command (or the one given after `--`) in strong mode
and saves the verdicts into the run, as `assertions assess` saves its
judgments. `mutants` then shows `killed`, `survived` and `timeout` beside the
weak statuses, and the mutation score appears. A second `mutants test` over
the same run only runs what has no strong verdict yet.

## Which mutants run, against which tests

For each listed mutant:

1. `equivalent` (a static rule): not run.
2. `not-reached`: `survived` is impossible to tell from `NoCoverage`; it is
   recorded as `no-coverage`, as StrykerJS records it. Not run.
3. `not-infected`: no test infected it, so none can kill it. Recorded as
   `survived` with the weak reason. Not run -- except where an operand is a
   clock or random reading (the `untestable` hint, `Math.floor(Date.now() /
   1000)` included): that this run's values never met says nothing of the
   next run's, so it runs against the tests that reached it.
4. `infected` or `unknown`: run, against its **candidate tests**.

A test is a candidate for a mutant when the mutant's weak verdict computed
from **that test's evidence alone** is `infected` or `unknown`. The
argument: with the mutant switched on, a test's execution is identical to
its original execution up to the first evaluation where the mutant's value
differs. A test whose own evidence shows no such evaluation runs exactly as
it did, and passes as it did. This is the same rule that makes `not-infected`
a certain survivor (Just, Ernst and Fraser, ISSTA 2014), applied one test at
a time.

A test's evidence is read with its file's module-load evidence: every test
of the file runs after its modules loaded. And where a test ran the mutant's
function but the statement around it ran elsewhere (a method of an exported
object: the export ran once, in whichever test loaded the module), the
statement counts as run for that test.

It holds only where a test's evidence is its own. So a test is always a
candidate, whatever its verdict, when:

- its attribution is not `exact` or `by-time` (`run-wide`, `partial`): its
  silence does not prove it did not run the code;
- the mutant's code ran in work no test was credited with: a server's
  start-up (any test of the run), or work a test file did between its tests
  -- a later `describe`'s `beforeAll`, an `afterAll`, async work a test left
  going -- which the Vitest hooks record as the file's setup evidence (every
  test of that file);
- it failed, was skipped or was flaky in the measured run: it is not run at
  all (it cannot pass as a baseline), and a mutant whose only candidates are
  such tests is recorded `survived` with that reason.

And a mutant in module-level code (no function around it) takes every test
of each file that ran it, whole: a module loaded lazily by one test keeps the
mutated value for the file's later tests, which never run that code again.

## The continuous pass (Vitest 4 and later)

Where Supercov's spawner forks Vitest's workers (Vitest 4 and later, files
isolated, `SUPERCOV_STRONG_STAGES` unset) the pass has no stages:

- **Baselines once per file.** Each test file's baseline runs once, the
  union of its mutants' candidates, at the head of the run, from a slot of
  the file's own (Vitest's "start" for it says the file's project and
  environment); a mutant's copy of the file runs once it is known. A union
  that fails, or does not finish, without a mutant means a candidate leans
  on a test the union left out: the file's copies run whole from then on.
  A file's whole baseline runs where a mutant takes the file whole (a
  mutant of module-level code takes every file that loads the module), and a
  whole copy goes on past a test that fails without the mutant too. Run only
  once a whole copy failed, it made tRPC's pass a third slower (a copy, the
  baseline and the copy again, one after another). Those are the only
  repeats, and none is per copy.
- **Few runs.** A run lists worker slots: each test file's own, then up
  to 2,000 more (and at most 1 GB of test files: Vitest reads every slot's
  file as the run starts, and the 50,000 slots of all of hono's mutants in
  one pass ran its process out of memory within seconds). A pass that needs
  more runs again with the same slots, for as long as runs make progress;
  between runs the workers wait for the last copies (with 1,000 slots this
  cost tRPC's shards 9%). A slot's stand-in answers Vitest's "start" and waits, at Vitest's
  "run", for work that is ready: a baseline a mutant now needs, a
  confirmation, or the next file of a mutant whose copy before ended with
  every test passing. The worker is then told to run that work's test file
  in place of the slot's (slots are files of the work's project and
  environment, learned from Vitest's "start" for each file's baseline).
  Vitest's own limit on workers running at once holds.
- **A mutant's files one after another**, the likeliest killer first (a
  file's kills over its runs so far, over its cost), and its chain stops at
  its first kill, as StrykerJS runs a mutant's covering files with `bail`.
  Parallelism comes from many mutants at once; of the mutants ready, the
  cheapest goes first (see "The order of a pass, its shares and its
  budget"). A worker no ready mutant takes goes to the one with the most
  work left (below), so long chains do not end the pass.
- **Confirmations only for left-out tests.** A kill is certain where the
  copy ran every test its file's baseline ran before the failing one (a
  copy of the whole file always does, against the whole baseline). Any
  other kill came from a copy that left out earlier tests: those tests, up
  to the failing one, run again with no mutant (one run for every mutant
  with the same tests), and where they fail that way too the mutant runs
  the file whole. This is the only trigger.
- **No worker idles while a mutant has files left.** Where no mutant is
  waiting for its next file, a slot takes the next file of a mutant whose
  copy is still running (the most work left first). Its verdict is the
  same; a copy that a kill of its mutant makes moot is stopped.
- **A failure before the mutant ran is run again.** Each test's record says
  how often the copy's mutant had been evaluated by its end. Up to the first
  evaluation a copy runs exactly as its file does without the mutant, so a
  test that failed before it failed for another reason (on tRPC, timing
  tests such as `lazy connection where the first connection fails` failed
  once in a run that way, and stood as kills the ground truth does not
  have). Such a copy runs again, once; a failure that repeats counts, since
  the mutant may have run in a process the copy started.
- **A hook that fails is the run failing.** A `beforeAll` (or `aroundAll`)
  that throws makes Vitest skip its suite's tests and fail the run. The copy
  reports the hook's error with its mutant's evaluations by the file's end,
  and where the file's baseline ran the hook without that error and the
  mutant ran, the mutant is killed, the error its reason (`a hook failed:
  relation "" does not exist`). Before, such a copy had run none of its
  tests, and its mutant was an `error`: a mutant of a database schema module
  that a test file's `beforeAll` seeds through breaks every such file's hook.
  A kill from a copy that left out tests is confirmed as any other.
- **A file that does not load before its mutant ran is not the mutant's.**
  A setup file that throws (a database that does not answer) or an import
  that does leaves the copy's file unfinished, which counts as a kill where
  the mutant had run. Vitest's collection of the file carries the error; the
  copy reports it with its mutant's evaluations so far. With none, the copy
  runs again, once, as a test that failed before the mutant ran does, and a
  second such failure makes the mutant an `error` that says the file's error
  (`its copy of test/a.test.ts did not load before the mutant ran, twice:
  ...`). A copy that ran none of its tests, or fewer, says why where it can:
  the hook's error, or the file's.
- **A worker that lost its channel to Vitest did not finish its file.** A
  mutant that disconnects the worker (`process.disconnect()`) leaves Vitest
  unable to hear the file end, as a plain run would fail it: the copy's
  last record says the channel was lost, and the mutant is killed.
- **A worker's end is told to Vitest as a worker's would be.** Errors of
  writes to a worker that has ended are dropped (its exit says the rest); a
  worker the pass stopped between its file's end and Vitest's "stop" exits
  only once the "stop" comes (or a forced stop), and an ended worker
  answers a "stop": before, the former threw "Worker exited unexpectedly"
  in Vitest's process, which ended the run, and the command started the
  pass again.
- When every mutant is decided the run is cancelled; its slots left never
  start a process.

On the public benchmark (16-CPU Linux machines, c8a/m8a.4xlarge; hono in 6
shards, tRPC in 32), against the staged pass: hono 69 to 68 ms a mutant
(63-68 over four full runs), tRPC 1,173 to 1,068 ms (StrykerJS's mutant
phase: 51 and 755 ms), with the
same disagreements with StrykerJS's full kill matrix and no extra kill. A
tRPC mutant ran 10.8 files (killed 6.3, survived 37.2), 2.69 baselines (each
shard is a pass with its own) and 0.31 confirmations; a hono mutant 2.5, 0.25
and 0.33.

Those figures compare Supercov's pass, baselines included, on 6 and 32
shards with StrykerJS's mutant phase, its dry run left out. Whole wall time
on the same machines (`benchmarks/mutation/README.md`, "End to end against
StrykerJS"), with the runs bounded as above:

| project | jobs | StrykerJS: set-up, dry run, mutants | Supercov: measured run, strong pass |
| --- | --- | --- | --- |
| hono | 1 each | 258.5 s | 177.1 s (pass: 39 ms a mutant) |
| tRPC | 8 each | 2,125.3 s without the 7 repeated dry runs (2,782.2 with) | 1,752.8 s without the 7 repeated measured runs and baselines (2,018.1 with; pass: 745 ms a mutant) |

Sharding cost Supercov more than its repeated baselines: hono's pass takes
168.5 s in one job and 298.7 s over 6 (28 s of it repeated baselines), as
each job starts and ends with workers idle.

## The order of a pass, its shares and its budget

A mutant's cost is estimated from the plan: each test file it would run, a
copy's start (300 ms, the Vitest driver's guess before it has timed one)
and the measured time of each test it runs there (its candidates, or every
test of a file it runs whole; a test with no time, a second). It is what the
mutant costs if it survives; a kill ends its copies sooner.

**Cheapest first.** The pass runs the mutants in order of that cost, the
plan's order between equals, and the continuous Vitest pass takes the
cheapest ready mutant first. A pass that is cut short -- a CI job's time
limit, `--budget` -- has then decided as many mutants as it could. On a
large application's integration suite (2,288 tests in 168 files, 41 s to
measure on 64 CPUs) the mutants' candidates were 7 at the median and 83 on
average: the top 5% of mutants held 83% of the candidate runs, about 1,100
of them with more than 500 candidates each, mostly module-level code of the
database schema module every test loads. The pass took the mutants with the
most work left first, so each 450 s shard of 2,460 mutants decided 29 to 40
of them, one mutant running 760 tests for 284 s. A worker no ready mutant
takes still goes to the mutant with the most work left, beside its copy
running, so that the costliest chains do not start last and end the pass
alone.

**Shares.** `--shard k/n` deals the mutants to run among n shares, most
costly first and back and forth (1, 2, ..., n, n, ..., 1, ...), and keeps the
k-th; the decided mutants go to the first. Each share gets its part of the
costly mutants, and their sum is about the same. Every job computes the same
split from the same plan. The cost the split reads is the plan's alone --
how many copies a mutant starts and how many tests they run, a copy counting
as one test -- not the tests' times: a sharded pass's jobs each measure the
suite (see `docs/mutation.md`, a pass split across jobs), their times differ,
and a split that read them would leave some mutants in no share and put some
in two. What each job's plan lists is the same where the jobs measure the
same commit, as a merge requires.

**Budget.** `--budget <seconds>` stops the pass once the time is spent, as a
termination stops it (below), and ends it as a finished pass: the mutants it
left have no verdict, for the next pass, and the summary says how many. A
pass that spent its budget before deciding any mutant is not one that could
run none.

**The time left.** The progress line's estimate is the plan's: the estimated
cost of the mutants left, at the pace of the cost decided so far over the
time it took. Counting mutants, the pass above said "37 of 2460 ... about
458m left", the estimate swinging from 158 to 1,229 minutes as a few costly
mutants ended.

## Order and early exit

A mutant's candidates are grouped by test file, cheapest file first. It runs
its cheapest file first: the first file kills most killed mutants. If that
leaves it alive, its other files run in the next stages in batches of 1, 4, 16,
... files, the files most likely to kill it first: a file's kills over the
mutants it ran so far in the pass, over its cost (its tests' time and a copy's
start). A copy listed in a stage costs a worker's start, and on hono a fork of
Vitest's own process, even where it ends at once; most of a mutant's later
files belong to survivors, which need them all, but listing them all at once
listed 30% more copies on the public benchmark's kill matrix than batches do,
and made hono's strong pass 2.3 times slower. A stage whose work would not keep
every worker busy for twice its longest copy takes each mutant's next batch
beside it, so that a small stage does not end waiting on one long copy. Within a file a copy stops
at its first failing test: that settles `killed` (or, where the kill waits on a
confirmation, below, holds the mutant until it has run), and the mutant's
copies of its other files that no worker has started yet end before their test
file loads. Should the confirmation overturn the kill, those files run in the
next stage.

A stage used to end before a mutant's next batch could start, so a mutant left
alive early in a stage waited for the stage's slowest copy (hono's workers were
busy 61% of the pass). Where the reporter gives each copy its assignment (see
below), each mutant in a stage also lists its next batch, after all the
stage's other work and held back: a held-back copy runs only if every other
copy of its mutant in the stage has ended with no failed test by the time a
worker takes it (and its mutant is still undecided); otherwise its file goes back to the mutant's
later files. So a mutant its first copy leaves alive goes on in the same
stage, in what used to be the stage's idle end, and a held-back copy that does
not run costs no process. Paired on hono's six shards and tRPC's shards 1-11:
hono 73 to 64 ms a mutant (workers busy 61% to 71%), tRPC 1,142 to 1,084 ms.
Running a mutant's later files one at a time in the stage instead (all of them
listed, each held back while another copy of its mutant runs) was slower:
most were taken while their mutant's copy still ran, and went back to the next
stage (hono 102 ms a mutant).

A mutant's copy that fails a test its file's known baseline passed is decided
by it: its later tests are skipped, and nothing it does after can change its
verdict. Its worker is stopped as soon as the failure is reported, not at the
end of its file. A test that timed out can leave a runaway loop going in its
worker until then: on tRPC twelve such workers held 2 GB each at once, and the
16-vCPU, 32 GB machine's job was ended with SIGTERM in about one shard run in
three. The profile counts them as `settledCopies`.

## Running many mutants at once without restarting anything (Vitest)

A Vitest run evaluates each test file in an isolated module graph (the
default `isolate: true`). Supercov's setup file runs in that graph before the
test file's imports. So different files of **one** Vitest run can each have a
different mutant switched on, and nothing one file does with its mutant can
reach another file's modules.

The pass therefore runs in stages. A stage is one run of the resident Vitest
instance (`runTestSpecifications`) holding every copy the pass can run yet:
first every mutant's cheapest file, then the other files of the mutants that
left alive, with the confirmations and the retries (see below), each beside
the baselines of the files it needs. A
stage can run one test file many times, each copy with its own mutant:
Vitest's pool runs a file listed twice twice, each in its own worker (with the
worker's own database, where a suite gives each worker one). The copies cannot
be told apart in Vitest's results, which are kept by file path, so each copy
claims its assignment as it starts: it appends a token of its own to its
file's claims in the stage's directory, and the line it landed on is its
assignment. It reports its tests' outcomes there itself. Supercov's setup file
switches the copy's mutant on before the file's imports run, and skips every
test the assignment did not choose. No new process, no new workers: the
project's own pool, so a suite that gives each worker its own database keeps
doing so.

Vitest's pool starts the next listed copy as soon as a worker is free, and
keeps the pass's order (the pass replaces its sequencer's sort, which would run
the files its cache says were slowest first). A stage lists the baselines
first, then each copy by its estimate (its file's time beyond its tests, from
its baseline, and its tests' durations as the baseline ran them, or as
measured; or its file's baseline copy where it runs the file whole) over its
rank among the mutant's files waiting: a mutant's next file comes before its later ones, so
most mutants are decided before their later copies start, and long copies come
early, so the stage does not end waiting on a long copy that started last. A
mutant's kill is recorded as soon as its copy ends, while the stage runs. A
copy with nothing left to run (its mutant decided meanwhile) ends in the setup
file, before its test file or Supercov's runtime loads.

An earlier design ran small rounds of two to sixteen copies per worker. Each
round ended when its slowest copy did, and the other workers waited: most of a
round's copies were done within a second, and one copy that waited on its
tests' own timers held the round for 4-20 s. The workers were busy 22-32% of
the pass on hono, tRPC and ufo alike, and the machine used about 4 of its 16
CPUs.

Where isolation is off (`isolate: false`, `--no-isolate`, a single-thread pool
sharing modules) a stage holds one mutant.

Each copy is a worker process, and Vitest 4 and later fork each from Vitest's
own process. A fork copies its parent's memory map, and Vitest's process held
about a gigabyte by the second stage on hono (its module graph, and the
bundler's native heap, 276 MB). Each fork held its event loop for 24 ms, 72%
of its time, and the workers waited on it to answer them. So the strong
reporter has the workers forked by a spawner instead
(`vitestStrongSpawner.mjs`, started while Vitest's process is still small):
`child_process.fork` of Vitest's worker entry returns a stand-in that relays
messages, output, signals and exits through the spawner. On hono's six
shards the strong pass took 340 s against 370 s with the same machines and
verdicts; the other corpus projects took the same time either way, and four
spawners sharing the workers were no faster than one. Vitest 1-3 fork
through tinypool and keep doing so; `SUPERCOV_STRONG_DIRECT_FORK=1` forks
from Vitest's process. The pass summary counts `spawnedWorkers`.

A tinypool worker that loses its channel (one the pass stops once a failed
test decided its copy, or one a mutant disconnects or ends) still gets
tinypool's relayed writes for a moment; tinypool 1.1 (Vitest 3.2) let the
failed write ("Channel closed", EPIPE) become an unhandled rejection, and
Vitest ended the run at that. On destr 2.0.5 installed with npm (Vitest 3.2.7)
most passes ended that way, 80 of its 141 mutants left errors after every
retry; with the lockfile's Vitest 3.1.1 (tinypool 1.0) none did. The process's
exit already tells tinypool the worker is gone, as for any worker that dies,
so the failed write's error is dropped: 40 passes since, each with the same
111 kills and 30 survivors.

A copy's worker spends its first few hundred milliseconds before it can
claim anything: Node starting, Vitest's worker loading, and in a DOM
environment that environment's package (jsdom took 300 ms on tRPC, where every
file runs in jsdom). On the public benchmark that was 75 ms of a hono copy's
205 and 430 ms of a tRPC copy's 860, and tRPC listed 46% of its copies for
mutants another copy had decided by the time they started: each paid the
whole start and then ended in its setup file. Two changes take that cost off
the copies without sharing anything between them:

- **Workers started ahead.** The spawner keeps one worker per Vitest worker
  started ahead of Vitest's requests, with the entry, arguments and options
  Vitest gives every worker of the project. Each loads what a fresh worker
  loads before Vitest tells it anything, in the order it would: Vitest's
  worker entry, then the package of the built-in environment Vitest named in
  its first message to a worker (`jsdom`, `happy-dom`), resolved as Vitest
  resolves it. Then it waits. Its script (`process.argv[1]`) is Vitest's
  entry and Supercov's variable is gone from its environment before the entry
  loads, so a test sees the process a fresh worker is; the spawner's own
  argument list stays in the operating system's view of the process only.
  Workers started before Vitest named the environment are stopped and
  started again with it. `SUPERCOV_STRONG_WORKERS_AHEAD=<n>` sets how many
  (0: each worker starts when Vitest asks). The profile counts
  `startedAhead` and `usedAhead`.
- **A process only once the file is known.** A stand-in answers Vitest's
  first message ("start") itself and keeps it. Vitest's next message ("run")
  names the test file; the strong reporter then gives the copy its file's
  next assignment, in the order Vitest starts them, and tells it which
  through the run's provided context (the setup file's claims race stays for
  Vitest 1-3 and direct forks). A copy whose mutant was decided meanwhile, or
  that is beyond its file's assignments, ends there with no process: the
  reporter records it as its setup file would have. Any other copy takes a
  worker, which gets the kept "start", answers it, and then gets the "run":
  the same messages in the same order as a worker forked for it.

Paired on the same machines (hono's six shards, tRPC's shards 1-12), with
nothing else changed: hono 81 to 73 ms a mutant, tRPC 1,443 to 1,142 ms; a
copy's start (handover to claim) 76 to 26 ms on hono and 411 to 201 ms on
tRPC; no mutant changed between detected and not (one hono and one tRPC
mutant went from timeout to killed, which flips between runs). The five
libraries took the same time (14-26 ms a mutant) with the same verdicts.

Warm workers were tried and dropped. Each copy ran in a worker Vitest kept
between test files (`isolate: false` for the pass), and the copy's setup file
reset the modules and mocks Vitest had evaluated, so only the packages Node
itself loaded stayed loaded. That cannot be made sound: whatever a test file
leaves in its process outlives a module reset, and against StrykerJS's ground
truth it made kills no isolated copy makes. Examples: a process-wide value one file
changed (pathe's `process.cwd`), a fake clock left on (hono's JWT tests), and
work a timed-out copy left running (37 extra kills on tRPC). Telling such kills
apart would mean running each one again in a fresh worker, which costs more than
warm workers save. They did not pay on the suites where time matters either:
the strong pass halved on the small corpus projects (ufo 26 to 12 s) and
used 2.3-2.6× less CPU on hono and tRPC, but hono's and tRPC's wall time did
not drop, because their copies wait on their tests' own timers, not on
starting.

Nor does the pass choose warm workers per suite. What a run knows of a suite
(its module graph's size, each file's setup time, its isolation settings, a
first stage timed cold and warm) says how much warm workers would save, not
whether a suite's files leave nothing behind in their process: pathe's,
hono's and tRPC's leaks came from ordinary test files with nothing in their
configuration to tell them apart, and one leak changes a verdict. A rule that
picks warm workers where they pay would pick them where they are wrong. The
sound part of what they saved, a worker's start, is what workers started
ahead save.

Node's compile cache (`NODE_COMPILE_CACHE`, which Vitest does not turn on) was
tried as a sound way to start copies faster, and also dropped: the strong pass
was 3-4% faster on hono, the same on tRPC and the small corpus projects, and
the measured run the same everywhere. What warm workers saved is starting a
process and Vitest for each copy, not compiling.

The last version with warm workers (`SUPERCOV_STRONG_WARM=1`) is kept on the
branch `archive/strong-warm-workers`, for a later attempt to make them exact:
resetting what a test file leaves behind in its process, or proving per file
that it leaves nothing.

The pass summary's `profile` records its copies (of mutants, confirmations,
and those that skipped), the copies stopped and hung, and for each stage its
copies, its wall time, the copies' own time (claim to end), when its last
copy started and, on Linux, the share of the machine's CPU time that was busy
(`cpuBusy`, also for the whole pass). A copy's start (`startMs`) counts from
when its worker was handed to Vitest. `SUPERCOV_STRONG_CPU_PROFILE=<file>` writes a CPU profile of
Vitest's own process for the whole pass.

## Baselines

Skipping tests can change what the remaining tests see (a test that relies on
an earlier one's state). Each file is first run with the union of its
candidates and no mutant; a file where that fails runs whole from then on, and
only a test that fails with the whole file and no mutant is no kill. A file
that does not finish without a mutant is reported, its mutants `error`.

A file's baseline runs in the stage that first needs the file, listed before
the mutants' copies of it, and is recorded as soon as its copy ends. A copy
beside it is judged by it: where the baseline had to run the file whole, the
copy runs again whole; where the copy stopped at a test that fails without the
mutant too, it runs again without that test. On tRPC a baseline stage of its
own ended 17 s after its last copy started, in every shard, waiting on one
long file.

A mutant's copy runs only its own candidates, a subset of that union, and a
test can still lean on one the subset left out. A kill counts at once where
the file's baseline ran the same tests before the killing one (or the copy ran
the file whole and so did its baseline). Any other kill is confirmed: the tests
the copy ran up to and including the failing one run again with no mutant
(tests after it cannot change it, so many mutants share one confirmation).
Where the test fails that way too, the kill is no kill and the mutant runs
again with its file whole. The pass summary counts `confirmations` and
`overturned`. On the public benchmark this removed every false kill of this
kind (hono: 17 overturned of 2,155 confirmed) at 5-20% of strong-pass time.

Running the baseline's earlier tests in the copy instead, so that every kill
is certain without a confirmation, was tried and dropped: a test can fail with
the mutant when its predecessors are left out and pass once they ran, and
StrykerJS, which also runs only a mutant's covering tests, counts that kill
(12 such kills on a private application's integration suite).

## Timeouts and runaway loops

In Vitest's pass (Playwright's limit is under "Time." below), a mutant's
test gets twice StrykerJS's limit (`factor × its measured duration
+ constant`, 1.5× and 5 s): its slowest test's duration (as measured or as its
baseline ran it, whichever is longer; for a copy that runs its file whole, the
file's slowest test) × 3 and 10 s. Each copy sets its own limit (`vi.setConfig`
in the setup file, before its test file's tests are collected). Rounds used to
give a first attempt 1.5× and 1 s and retry past that with the full limit, as a
round ended when its slowest copy did; in a stage a hanging copy holds only its
own worker, and a real timeout costs the full limit either way, so one attempt
at the full limit gives the same verdicts without the retries, which ended
every tRPC pass with a stage of retries alone, 13-18 s long. What a short first
limit was for, a stage's end, has a rule of its own: once every copy of a stage
has started, a mutant's copy still going well past what it should take (1.5×
its estimate or its file's baseline copy, and 1 s) holds the stage alone, so
its worker is stopped and it runs again in the next stage, with its whole
limit, beside that stage's work (a copy whose test had already failed keeps
its kill). On hono a first stage otherwise waited 11.6 s on one such copy. The short limit costs a retry, never a
verdict (0 differences on hono and tRPC; hono 294 to 227 ms a mutant, tRPC
3.4 to 2.8 s). A mutant switch counts its evaluations; past a
fixed limit (1,000,000 per copy) it throws a timeout error, so a loop the
mutant made endless ends in the file's worker rather than hanging it. Where
the code under test catches that error and carries on, the limit having been
reached still makes the mutant a `timeout`.

This differs from StrykerJS on purpose. StrykerJS also calls a mutant
`Timeout` once it is evaluated more than 100 times as often as in its dry
run, though every test passes and nothing hangs; a timeout counts as
detected, so the suite gets credit no assertion earned (tRPC's
`withPing.ts:37`: about 45,000 evaluations against 8, 37 of 37 tests pass).
Supercov calls such a mutant `survived`: no test tells it apart. An error no test catches (work a
test left running, a rejected promise nobody awaited) is the mutant's where
the file raised none without it: a kill, or a `timeout` where it is the
switch's own limit. A copy can also stop where no test's limit reaches: a
mutant that leaves a module's top-level `await` waiting forever never lets its
test file load. A stage's copies report as they go (their claim, each test,
their end), and the driver looks at the copies going every 0.5 s. A copy's
limit is its file's baseline copy × 3 and 10 s (twice StrykerJS's), plus the
longest of its test limit and the hook limit, so a test that hangs is still
Vitest's to end. Past that, the copy's worker process
(Vitest's `forks` pool, its default) is stopped: Vitest's pool counts the
worker as failed and starts the next copy, and the stage goes on. A copy whose
worker is a thread of Vitest's own process (`threads`) cannot be stopped
alone: once every copy still going is such a one past its limit, or nothing
has reported for twice the longest test or hook limit and 30 s more, the
stage's run is cancelled, its workers asked to stop and 3 s later stopped by
force. A cancelled copy ends gracefully with its remaining tests skipped, so
only what each copy reported before the cancel counts (a first version judged
cancelled copies by how they ended, and turned 19 of hono's timeouts into
survivors), and copies no worker had taken run in the next stage. A copy
stopped either way is a timeout, unless a test had already failed, or its tests
left out hang the same way without the mutant (the confirmation above). A copy
whose worker process ends without the copy's end (its test file failed to
load, its worker crashed) is over at once, and a kill: the test file did not
finish. A first version of stages waited for such a copy's limit and then
stopped a process that no longer existed, which turned 18 of tRPC's kills into
timeouts.
Only workers that do not stop even when forced end the pass, and
`mutants test` starts another for the mutants left.

## Instrumentation

In strong mode the instrumenter writes every listed mutant into the copy
behind a switch (mutant schemata, Untch 1992), as StrykerJS does, with
Supercov's own ids, and places no coverage or weak-mutation probe
(`crates/supercov-engine/src/js_schemata.rs`).

**Switching it on.** `js_instrumenter::set_strong_mode(Some(true))` for the
engine's own process (the frontend instruments in process), and
`SUPERCOV_STRONG=1` in the environment of any process that instruments
(`set_strong_mode(None)` defers to the variable). Either is part of the
instrumented copy's cache key (`probe_options()`), so a strong copy and a
measured one never share it. Strong mode lists the mutants whatever
`SUPERCOV_MUTANTS` says. A plain run lists none; the plan lists them from the
sources the run measured (see `docs/design/mutants.md`).

**Addressing a mutant.** A mutant is `(file, index)`: `file` is the key of
its file in `mutants.json` (its path from the project root, as in mutant
ids), `index` its place in that file's list, the list the weak enumeration
writes, in its order. Each file reads one record from the runtime when it
loads, `const __supercovStrong = strongSlot("src/a.ts")` (names follow the
file's allocator, so a clash or a classic script's namespace renames
them), and every switch reads the record's `a` when it is evaluated:

- an expression: `(__supercovStrong.a === 17 ? (__supercovStrong.h(),
  <replacement>) : __supercovStrong.a === 18 ? ... : <original>)`;
- a statement: `if (__supercovStrong.a === 19) { __supercovStrong.h();
  <replacement> } else { <original> }`, and the same around a function
  body's or a `case`'s statements.

The runtime (`runtime/javascript/runtime.mjs`, also on
`__SUPERCOV_DIRECT_RUNTIME__`) has `setActiveMutant(file, index)`,
`clearActiveMutant()`, `setMutantHitLimit(n)` (default 1,000,000),
`mutantHitCount()`, `MutantTimeoutError` and `isMutantTimeout(error)`.
`h()` counts the active mutant's evaluations and throws
`MutantTimeoutError` (`code: "SUPERCOV_MUTANT_TIMEOUT"`) past the limit.
The state lives on the global object, so the active mutant can change while
a server runs and copies of the runtime in one realm agree.

**Placement.** StrykerJS's placers decide where a switch goes: on the
mutated node where an expression, statement or `case` can be wrapped, else
on its nearest ancestor that can, with the mutant applied to a copy of that
ancestor (holding the switches StrykerJS's copy holds: those placed before
the node). Its rules are kept (no switch on an object key, a chain's inner
link, a template's tag, `delete`'s operand, an assignment target), and
Supercov also refuses where wrapping changes the program: declarations
(`let`, `const`, functions, classes, imports, exports), a label's body, a
`case` that declares lexically, a key written without brackets, anything
not in an expression slot. An anonymous function wrapped where its place
names it keeps the name (`({ "f": <it> })["f"]`). A mutant with nowhere to
go, or in code the instrumenter keeps as written (a function whose source
the program reads, a `with` body), gets no switch: `mutants.json` says
`"unplaceable": "<reason>"` (`no-placeable-ancestor`, `function-source`,
`with-statement`, `next-segment-config`), and the driver reports it `error`.

Next.js reads some exports from a file's source without running it: a
route's segment config (`dynamic`, `dynamicParams`, `revalidate`,
`fetchCache`, `runtime`, `preferredRegion`, `maxDuration`,
`experimental_ppr`) in any file, and `config` in middleware, a proxy and
under `pages/`. A switch there is code where Next.js wants a literal, and
`next build` refuses it ("needs to be a static string"), so their values
(outside a function) stay as written: `next-segment-config`. StrykerJS has
no ignorer for them.

**Only what the pass runs.** A strong pass switches only the files that
hold a mutant it runs (the plan's undecided mutants): the engine sets them
for its own process (`set_strong_files`) and lists them for others in
`SUPERCOV_STRONG_FILES` (a JSON list's path); both are in the cache key.
Every other file, and a file whose mutants all have no switch, is left as
written, with no runtime binding: the strong copy of a large app differs
from the project in a handful of files, and a file the measured run could
leave without the runtime (an edge-runtime module) does not gain it. The
command's log says `switches in N of M source file(s)`.

**Static mutants.** A mutant whose switch is evaluated while the module
loads (no function or instance field initializer around it) carries
`"static": true` in `mutants.json`.

**Parity.** Each replacement is checked against StrykerJS's own replacement
code (`text` in the parity goldens) for every mutant of the parity
fixtures, printed as `@babel/generator` prints it: same text, byte for byte.
And on the fixtures Node runs as they are, the strong copy with each
mutant switched on behaves as StrykerJS's mutated program does.

## Results

`strong` in each mutant's JSON: `{ status, killedBy, tests, durationMs,
reason }`, where `tests` is how many ran. Statuses: `killed`, `survived`,
`timeout`, `no-coverage`, `error`. The mutation score is
`(killed + timeout) / (killed + timeout + survived + no-coverage)`, as
StrykerJS's.

While a pass runs, a line on stderr every 15 s says how far it got: the
mutants with a verdict of those it runs, how many are killed, timed out,
survived and errors, the time so far, and the time left at this pace. The
CLI reads it from the pass's results as the driver writes them, so it works
the same for every driver.

## Gate

- `mutants test --diff <base>` runs and reports the change's mutants.
- Critical code: `--critical <glob>` (repeatable) or a
  `// supercov-critical` comment before a function. There every surviving
  mutant fails the command unless accepted.
- Accepted equivalents: `supercov.accepted.json` at the project root, each
  entry a mutant id and a reason; an entry whose mutant is killed or gone is
  listed as stale.
- `--fail-on survived[,timeout]` fails the command (exit 1).

## Validation

Every verdict is checked against StrykerJS run with `disableBail: true` (all
killers) on the same mutants: a mutant StrykerJS kills and the pass calls
`survived` is a soundness violation, and the release keeps their count at 0.
Speed is measured on the public benchmark in `benchmarks/mutation/`.

Where StrykerJS's kill and the pass disagree, the mutant is applied by hand
and the suite run as written, whole: StrykerJS runs a covering subset, and a
subset can kill what the suite as written does not (a test that passes only
after earlier tests, a fake clock left running into later tests). Such kills
are artifacts of the subset, not misses. On the benchmark's hono run all
seven disagreements were of this kind.

Module-level mutants: StrykerJS with `ignoreStatic` switches a mutant on
after the module loaded, so a module-level constant is never mutated and
such mutants read `Survived`. The pass switches a file's mutant on before the
file's imports, mutates them and can kill them.

## Browser tests against a server (Playwright)

A Playwright test drives an app server in another process, and the browser.
Supercov already scopes both to the test's attempt: every request a page or
`request` fixture sends carries the attempt's scope (a header, or a cookie),
and the server's runtime credits the work it does for that request to the
attempt. The strong pass uses the same scope to choose the mutant.

**The mutant travels in the scope.** A strong attempt's scope names its
mutant (`m=<file>#<index>`, `m=-` for none) beside the attempt's id. So it
reaches the server by every way the scope already does -- the `request`
fixture's header, the page's cookies and fetch header, a child process's
`SUPERCOV_CONTEXT`, a request the server sends on -- and stays with the
request's async work as the scope does. A separate `x-supercov-mutant` header
would have reached only what a header reaches: a page's first navigation
carries cookies, not the fetch patch's header.

**In the server**, the first scope that names a mutant turns the process's
switches into accessors: each switch's `a` is read from the request being
served (its scope's mutant), and outside any attempt's work from the
process-wide mutant -- none, unless the process was started with
`SUPERCOV_ACTIVE_MUTANT=<file>#<index>`. Each attempt's evaluations are
counted on their own against the hit limit. A process that never sees such a
scope (every Vitest pass) keeps plain switches, set process-wide as before.

**In the page**, the attempt's init script switches the document's mutant on
before the page's scripts run, as it already announces the attempt. A page
whose mutant passes the hit limit writes a marker to its console; a server
appends the attempt to `<results>.timeouts.jsonl`. Either makes the verdict
`timeout`, whether or not a test failed.

**One invocation, copies of the tests.** A Playwright test runs once per
test case, with fresh test fixtures, and a worker runs test cases one after
another with the browser it launched. So the pass declares each test a
mutant needs several times over as its spec loads: Supercov's test
declaration, on Playwright's test type, adds the test's copies after it, each
in a describe of its own (`[supercov copy <k>]`) so that its full title is
new; the title, the place (a `file:line` filter takes every copy) and the
fixtures are the test's. A copy takes its work as it starts, from a queue on
disk that every worker and every invocation of the pass share: the next
mutant whose candidate the test is, not yet tried against it, one no other
attempt runs first; making the claim's mark (a directory) is what takes it,
so two copies never take one; which one it takes is the next section's. A
test gets one copy for each mutant that may take it, one for each control a
kill may call for (below), one for each mutant with a candidate before it (a
sweep it may wait out, below), and its first run; the generated config sets
`repeatEach` to 1, `retries: 0` and no failure limit, and keeps the project's
workers.

This replaces rows of repetitions (`repeatEach`). Playwright gives every
repetition index a worker of its own, so each row started a worker and a
browser, and each later stage (the survivors' other candidates, the kills to
confirm) started the command again. On a large application's e2e suite
(Next.js production build, a harness that starts a server and a database for
each of several `playwright test` processes) a mutant took about 50 s, and an
area of 1,688 mutants decided none in a CI job's ten minutes: every kill there
ran beside another process's attempt and waited for a confirmation the budget
never reached, and verdicts were decided only once the command had ended.
With copies, on the same suite and machines (8 jobs of 32 CPUs, 6 slots each,
about 6 minutes of each job's 10 for the pass): the area decided 440 mutants
(417 killed) where it decided no kill, about 3 s of a slot a test run, and one
file's 12 mutants took 4 minutes where they took more than 5 for 6 of them,
the rest of the 4 one mutant that holds each test until its own timeout.

**A mutant from candidate to candidate in one invocation.** A copy that
takes a mutant runs it; a test that passes moves the mutant on, and the next
copy of another candidate that starts takes it there, in whichever worker or
invocation runs that test; a kill marks the mutant, and no copy takes it
again. A mutant every candidate let live has survived. A test's copies
outnumber what its queue can ask of it, so its mutants are all tried by the
time its copies are spent, whatever order the tests run in, and a mutant
never waits for a stage. Where every mutant left in a test's queue runs
elsewhere at that moment, the copy takes one of those too rather than leave
the test's copies unused: its verdict is the same either way.

**Which candidate first.** A mutant's candidates are ordered cheapest to a
kill first: each test's measured time (a second where the run timed none)
over how likely it is to kill the mutant, a test whose own evidence infected
it taken as four times likelier than one that only reached it (a module its
page loaded, a request no test named). So a test that infected it goes
first unless it is more than four times slower, and a slow test goes last:
a test that polls for ten seconds until a purchase is active is tried after
the half-second tests that merely load its page, though it infected the
mutant and they did not. The order has to be kept by the copies, since
Playwright fixes what a worker runs when it loads a file, so it is kept in
three places:

- *Sweeps.* A worker runs a file's tests in the order they are declared,
  and Supercov's declaration arranges them as it declares them: after the
  tests themselves (each test's first run), the first copy of every test,
  cheapest test first, then every test's second copy, and so on, each beside
  the tests of its own describe (a describe's hooks stay its tests'). A file
  whose tests all reach one module (a page's) used to run its first test with
  each of the mutants, then its second test with each: a mutant there was
  decided only once the file's last test ran it, and a pass its budget cut
  short decided none of those. In sweeps, its tests take one mutant each in
  turn, so mutants are decided one after another.
- *Which mutant a copy takes.* Of the mutants waiting for its test, a copy
  takes the one with the fewest candidates before this test still to try
  (none: its cheaper or likelier candidates let it live, and this test is the
  next that may kill it), then one this test infected, then the one whose
  candidates still to try take the least time (the closest to a verdict),
  then the test's queue order. A candidate that failed without any mutant
  holds nothing back.
- *Waiting a sweep.* Where even that mutant has a candidate before this test
  still to try, the copy takes none, while the test's copies after it still
  cover every mutant waiting for it and a control for each: its next copy, a
  sweep later, looks again. So a slow test's copies go to the mutants its
  cheaper candidates let live, while those candidates run (in this file, or
  in another invocation); and once its copies are short, it takes what is
  left all the same. Each test gets a copy for each mutant with a candidate
  before it, for these waits; a copy that takes nothing is skipped, which
  costs Playwright a test's start and end.

Verdicts stay what they were: a kill is any candidate's, a survivor ran every
candidate, and a test's copies still cover every mutant waiting for it. What
changes is when: a mutant a cheap test kills never waits for a slow one, and a
slow test holds a mutant to its time limit only when no cheaper test could
kill it first.

Measured on a large application's e2e suite, its billing area's tail: the
572 mutants four passes had left (most survivors-to-be, each with a median of
12 candidates and 119 at the 90th percentile; 403 of them reached by one file
of 15 tests whose 4 slowest poll 8-11 s until a purchase is active, a file
the harness runs whole in one of a job's 6 slots). With 8 jobs of 32 CPUs and
about 6 minutes of a pass each, from the same state, 4.29.5 decided 57: that
file's first test ran each of a job's 70 or so mutants before its second test
began, so a mutant there was decided only after the file's last test ran it,
and no pass reached it. In sweeps the pass decided 147 for the same machines
($0.60). Its slowest test, which 4.29.5 ran 210 times in that pass (each
job's first test of the file, so nearly all its slot's time), ran 16 times,
once its cheaper candidates had let each mutant live, and the file's other
slow tests, which 4.29.5 never reached, ran 73 times. That slot still bounds the tail (each of its mutants takes
about a minute there), and the progress lines say so: the costliest tests
left are that file's, and the time left in the round is the slot's.

**Controls: a run with no mutant first in each invocation.** A test's first
run in an invocation, its first there after it failed without a mutant, and
its first after a kill there have no mutant. The first such run of a test in
a main round is its baseline: a test that fails without a mutant is no
candidate (a second control that passes makes it one again). A test runs with
a mutant only on a server (an invocation's) where it passed without one. A
kill's next run of its test is a control: passing, it confirms the kills
before it on the same server; failing, it puts them in doubt, and they run
again one test at a time. A test whose control fails twice in an invocation
is not run there again. Controls stand in for the canaries rows had (rows with
no mutant, planted among the mutated ones): each kill is followed by one, at
the cost of a run of the test.

A failed test gives Playwright's next test a new worker, and the first run of
each test in each worker used to be a control too. With a file's copies in
sweeps that would follow every kill with a control of each test of the file;
a control is the invocation's, as its server is, so only the killing test runs
one. A test that leans on what an earlier test of its file left in the worker
(a serial describe, a `beforeAll`) is caught as before: a kill it makes for
that reason is followed by its control, which fails the same way and puts the
kill in doubt.

**Serial describes run in order but go on.** A `describe.serial` (or
`describe.configure({ mode: 'serial' })`) runs as a default describe: in
order, in one worker, but a failed test (a kill) does not skip the rest of
the describe, which would leave their mutants for another invocation.

**A worker a mutant ran in is not used again.** Requests carry their
attempt's mutant to the server, which serves many attempts at once; the
Playwright worker's own process runs a mutant only where it serves a request
itself. An attempt whose mutant ran in the worker's process fails as it ends
(after its outcome is kept), so that the next test gets a new worker.

**Verdicts as they come.** A kill confirmed by its control, a survivor's last
candidate, and a page's hit limit write the mutant's verdict into the results
as they happen (a mark per mutant, so once), and the progress line counts
them. Once the command has ended, with every invocation it started, the
engine runs the strong reporter's `judge`, which gives the rest theirs: a kill
no control came after (the round ended first) stands, as any kill did before;
a candidate that failed without the mutant is left out; what did not run is
unfinished, and runs in a `rest` round. A pass the budget stopped is judged
too, and keeps what it finished, but nothing that ended after the pass began
to stop: the stop takes the app's servers down under the tests still
running, and a test that failed then failed for that (a fixture's survivor
failed so, its page's script never served, and stood as a kill). The CLI
writes when it began to stop (`stopped-at`, beside the results, a poll early
for a signal the supervisor heard first), and the judge trusts no attempt
that ended after it, as it trusts none after an error outside any test. Such
a mutant runs that candidate again in the next pass; an attempt Playwright
reports as interrupted settles nothing. A kill that ended before the stop,
waiting only for its control, still stands.

**What the pass says as it goes.** Each invocation's runner writes a status
file (its round, the baselines, attempts and controls so far, the tests
running and since when, and the work its tests have left), and prints a line
on the command's output for each attempt with a mutant (`<mutant>: "<test>"
lives (1.4s)`), so a harness that watches its processes' last lines sees them
work. `mutants test` adds to its progress line what the round does, the
costliest tests left, what runs and for how long, and where nothing has ended
for two minutes what it waits on: a test running up to its time, or, with no
test running, the command's last line. Before any Playwright invocation began,
it says the command's last line. The command's process tree is listed after
ten minutes, not every minute.

The work left is counted from the queues every five seconds: for each test
the invocation runs, how many mutants wait for it (not decided, not yet tried
against it) at its baseline. The three tests whose runs left take longest are
named, with how many mutants wait for each, its baseline and its limit:

    [supercov]   costliest test left: "Social Media is added on the saved card, yearly or monthly, and the page waits until it is active", 38 mutant(s) wait for it, 7s a run (its limit 16s)

The time left is each invocation's work left at its own pace (the baseline
time of the runs it ended, over the time they took, which counts the skipped
copies and new workers too), the slowest invocation's: `about 12m40s left in
this round`. It is counted from what each mutant has left to try, so it
follows the order: kills that come early do not make the survivors' runs look
cheap, and a slow test that many mutants still wait for in one invocation is
what the estimate waits on, as the round does. Where no driver counts its work
(Vitest), the estimate is the plan's cost of the mutants left at the pace
since the first mutant ran, as before.

**Memory: copies in invocations of a bounded size.** Playwright makes every
test case of every file it loads before the first test runs, in the runner.
A large application's e2e suite, 77 tests and 15,603 mutants to run, ran out of
a 4 GB heap that way before its first test, with rows. So an invocation holds
at most about 20,000 copies (`SUPERCOV_PLAYWRIGHT_COPIES` sets it): the main
round takes the mutants in the plan's order while they fit, and asks for
another invocation (a `restart` record of kind `main`) for the rest. Each
process reads only what it needs: the runner reads the plan once and writes
the round's work; a worker reads `worker.json` (the tests, their copies, how
they run side by side), the queue of each test it runs and each mutant it
takes, and finds a copy's test by its project and title path in `originals/`
(one small file a test), never the plan.

**`--list` reads no plan.** A harness that shares tests out across processes
lists them first (`playwright test --list --reporter=json`). The generated
config sees `--list` and returns the project's own config, and the test
declaration adds no copies: the list shows each test once, and nothing of the
pass is read or written.

**Several invocations at once.** A harness may start several `playwright test`
processes in one command (slots, each with a share of the tests and a server
of its own), a few seconds apart. They share the round's directory: each
works out the same work from the same plan and writes it alike, and none
removes what another wrote. A test's baseline, a mutant's claims and its kill
are the pass's; a file's lock (one test of the file at a time), a kill's
control and what ran beside what are the invocation's, as its server is.

**Serial files and slots.** A harness that shares tests out to slots keeps
a file with a serial describe in one slot: its tests may lean on one
another. The strong pass runs a serial describe as a default one, so a file
has no order its copies must keep beyond the sweeps, and the queue its
copies take mutants from is the pass's, shared by every invocation. Running
the file in several slots is then exact as it stands: a mutant's claim on a
candidate is pass-wide, so no two slots run the same test with the same
mutant; each slot's first run of each test is its own control on its own
server and database; and a kill is confirmed by the next control of its test
in the slot that made it. What it costs is each slot's baseline of the
file's tests.

Supercov cannot put the file in a slot the harness did not give it. The
`playwright test` command reads its file filters from the command line before
it loads any config, and Supercov's config can narrow the run but not widen
it; and a slot's server and database belong to its own process, so a second
invocation Supercov started against them would run beside the slot's tests
on state they do not expect. So the harness must cooperate, and the pass
tells it it may: the command runs with `SUPERCOV_STRONG_SPREAD=1`, and a
harness that sees it may give a serial file (or any file) to every slot. With
`--list` a harness still sees each test once: copies stay out of the list
(see below), since a harness that shared them out would keep a serial file's
copies together just the same.

Where it does not, the progress lines say so. The runner records the spec
files it loaded with a serial describe (in `playwright test`'s runner
process, which loads every file before the first test); each invocation's
status counts the mutants waiting for such a file's tests; and where only one
of a round's invocations runs the file and it has more time left than any
other, the line names the file, its mutants, and the two times:

    [supercov]   "tests/e2e/buy.spec.ts" has a serial describe, and 1 of the 6 invocations runs it: its 37 mutant(s) wait about 12m00s there, the others about 1m00s; a harness that gives it to every invocation of a strong pass shares them out (SUPERCOV_STRONG_SPREAD, docs/mutation.md)

**A process that runs out of heap is named.** Every Node process of a strong
pass runs with `--report-on-fatalerror` into the pass's directory. A process
that dies out of heap writes Node's diagnostic report first; the pass reads
it, says which process it was (its command line and pid) and its heap, and,
for a test runner's process, that the plan was too large for its heap; each
mutant left without a verdict says so in its reason. The reports, which hold
the process's environment, are removed once read.

**Concurrency as the project runs it.** Copies of a file's tests in a
`fullyParallel` project are separate groups, which Playwright runs side by
side. A suite whose tests share a server's state (a harness's fault rules for
one server slot, rewritten by each test) then fails for reasons no mutant
made. So a file runs one test at a time here when its tests are known not to
run at once: two or more of them ran in the measured run and their spans
never overlapped, or, where the spans tell nothing (one test, none spanned),
the project does not run files `fullyParallel`. Each such attempt holds the
file's lock from its start to its end, across workers. A file whose tests
overlapped runs side by side. The workers are the project's own.

**Kills beside another mutant are confirmed.** A test that fails while
another mutant's attempt runs on the same server (the same invocation) may
have failed for the other's doing. Each attempt, as it starts, looks at the
invocation's attempts running and marks those with another mutant, and
itself; so of two that overlap one sees the other. Such a kill goes to a
**serial round** (one worker, one test at a time), which runs its killing
test first and its other candidates only if that test lets it live. An
invocation with one worker (a harness's slot) confirms nothing this way.
Attempts in different invocations are not beside one another: each drives a
server of its own, as the file locks already assume. A failing attempt's
error (its first line, ANSI stripped) is the verdict's reason.

**Projects others start from run as they are.** The tests of a project
another project depends on (`dependencies`, `teardown`: a login that stores
the session the others start from) run in every invocation with no mutant.
Such a test is no candidate this pass can run, as a test of another runner
is not: a mutant no other test kills is an `error` that names the project,
never a `survived` it has not earned. It used to wait for that candidate, as
`unfinished`, pass after pass: on a large application's e2e suite, 99 of the
572 mutants its billing area had left after four passes had the login among
their candidates.

**Time.** A test run with a mutant gets StrykerJS's limit,
`timeoutFactor × baseline + 5 s` (`timeoutFactor` 1.5 by default;
`--timeout-factor` or `SUPERCOV_STRONG_TIMEOUT_FACTOR` sets it), or the test's
own timeout where that is shorter (the project's, `test.slow()`,
`describe.configure`: Playwright would end it there anyway). The baseline is
the longest of the test's measured duration and every run of it without a
mutant that passed in this pass, in any worker, invocation or round (each
control records its time), with a second at least: a test whose time varies
is judged by its slowest run under the pass's own load, the cold first run of
each worker among them. A baseline and a control run with the test's own
timeout, never this limit.

The limit is Playwright's test timeout for the attempt, from the moment its
copy takes the mutant, so it interrupts whatever the test waits on: an action
with no timeout (Playwright's default) is cut by the test's timeout, and an
`expect` with a timeout of its own ends 250 ms before the test's deadline,
saying `Test timeout of N ms exceeded`. Neither Playwright's action and
navigation timeouts nor the expect timeout need changing. A
`test.setTimeout()` or `test.slow()` in the test's body or hooks may shorten
the limit, never lengthen it: the baseline ran with them, so its time already
says how long they let the test take. An attempt whose first error is the
test's timeout (any error after it failed because of it: a page closed under
a waiting click) ran out of its limit; one that failed first is a kill, as
before, even where it then ran out of time.

An attempt that ran out of its limit makes its mutant a `timeout`, detected,
as in StrykerJS. It is confirmed as a kill is: by the test's next control on
the same server, and one that ran beside another mutant's attempt, or whose
control failed, runs again in a serial round. A mutant whose evaluations pass
the hit limit is a `timeout` at once. The verdict says the limit: `ran past
its time limit of 9.5 s (1.5 × its 3.0 s baseline + 5 s)`; the summary counts
such timeouts and states the factor, and the progress lines show each running
attempt's limit and when it is near it.

This replaces a limit that was never shorter than the test's own timeout,
plus the expect and action timeouts, with a second attempt, one test at a
time and twice the time, for a test that only ran out of it. A mutant that
broke the page then cost the test's whole timeout: on a large application's
e2e suite (billing area, 742 kills in 8 jobs × 6 slots) the median kill took
6.2 s but the 90th percentile 60.7 s, 7.4 hours of slot time in all, nearly
all of it clicks and `expect`s with timeouts of 30 and 60 s waiting for an
element that never came. One attempt at the limit, final, is what Vitest's
pass does too (a stage's straggler rule aside), and StrykerJS's factor keeps
its convention; a Vitest copy gets twice it because its limit covers a whole
test file. The two-step retry is gone: under the old floor it decided nearly
nothing (1 timeout in that area), and at the new limit a real hang would cost
three times the limit, one test at a time.

Measured on the same suite's billing area, round 1 of 8 jobs with the same
10-minute budget each: 787 mutants decided where 611 were (645 detected: 360
killed and 285 timeouts, where 480 and 1), the 90th percentile kill 7.1 s
where it was 60.7 s, and a timeout's median 7.3 s (most tests there take about
a second, so their limit is 6.5 s). A detection by one test took 5.9 s of slot
time on average where it took 10.3 s; the rest of a pass's time is its
survivors' runs, its baselines and controls, and the mutants run alone with
the server. Of the mutants both passes ran, none that survived became a
timeout; 130 kills became timeouts, 253 stayed kills.

**Run alone, the server started with the mutant.** That per-request
switching is exact only where nothing one request does reaches another, and
a server keeps what its top level made. So the plan marks a mutant
`serverWide` where a server ran its code in work no request named (start-up,
timers: `background`), at a server module's top level (`module-level`), or
for a request that named no test, credited by time (`by-time`). Such a
mutant is not queued: the engine starts the command again for it, alone, with
`SUPERCOV_ACTIVE_MUTANT` set in every process, and its verdict's reason says
so; a mutant that keeps the server from starting is `killed`. Work for a
request that named no test while no test ran (a web server's readiness
check) is not such work: it ran with no mutant, as every request runs with
its own, and leaving it out keeps every handler's mutant from running alone.
That is the one place the pass trusts that a request's work does not reach
the tests' through shared state; the controls after each kill check the rest.

Each further round a driver asks for (a `restart` record in the results: one
mutant alone, a serial round, the rest of what did not finish, the main
round's mutants past one invocation's copies) is a new invocation of the
command, its output in a log beside the results, run in turn until none asks
for more. A `rest` round that decides nothing makes what it left an `error`.

A mutant's candidates are the attempts its weak verdict names, as in Vitest.
Candidates under another runner (a Vitest test of the same run) are not run
by this pass: a mutant no Playwright test kills is then an `error`, never a
`survived` it has not earned.

Validated on playwright-test-coverage (`910da570`, a Vite app served by
`npm run dev`, three tests in `e2e/`, whose config names no `testDir`, where
Supercov's own config from `.supercov/` used to find "No tests"): its 9
mutants run are 7 `timeout` and 2 `survived`, and each applied by hand to
the source with `npx playwright test` as written agrees: the 7 fail (their
tests wait out Playwright's 30 s), the 2 pass. The pass took 400 s, nearly
all of it those tests' own waits and their retry with twice the budget,
before the time limit above.

## Runners that start a process per test file (jobs)

Jest, node:test, Mocha, AVA, node-tap, bun:test and Vitest's Browser Mode have
no resident instance to hand copies to as Vitest's node pool does. Their pass
is `runtime/javascript/strongJobs.mjs`, which the engine starts in place of the
run's command (`node strongJobs.mjs -- <command>`, on Bun where the command is
Bun's) when a mutant to run has a candidate under one of them. A plan's test
runs under `vitest-browser` where its evidence came from the browser.

**A job** is one start of the command for one test file. The mutant is on in
every process from its start (`SUPERCOV_ACTIVE_MUTANT`), so its module-level
code is mutated too; under Browser Mode the setup file asks node for the job
over Vitest's command channel and switches the mutant on in the page before
the test file loads. The job file (`SUPERCOV_STRONG_JOB`, read through
`strongJob.mjs`) names the file's tests to run, every test file of the run,
and whether to stop at the first failure. Each runner's adapter does what the
runner allows:

| runner | the job's file only | its tests only | first failure |
| --- | --- | --- | --- |
| Jest | `testPathIgnorePatterns` for the run's other files | Circus's tests marked `skip` before the first test runs | the rest marked `skip` |
| Mocha | `Mocha#files` filtered before loading | tests removed from their suites (a suite left empty runs no hook) | `Runner#abort` |
| node:test | a child of another file ends before it loads | registered as skipped | `t.skip()` |
| AVA | (each worker loads its file) | `runTest` not run | later tests not run |
| tap | the file run by itself | top-level subtests `skip` | later subtests `skip` |
| bun:test | the file named on the command line | registered through `test.skip` | later tests return |
| Vitest Browser Mode | the file as a CLI filter | `context.skip()` | `context.skip()` |

tap and node:test count a test file as a test of its own: assertions at its
top level, outside any subtest, pass or fail it, and StrykerJS's tap runner
kills a mutant on them. So the adapters record a file's top level as a test,
`(top level)`, where it asserts there (tap) or has no `test()` (node:test),
with what ran there; in a file with subtests it is the file's module setup
(tap) or counts for the run (node:test). Before, what such a file ran was in
no test's evidence, and a mutant only its top-level assertions caught was
never run against it (ini: 14 mutants StrykerJS killed through `win32.js`
and `proto.js`). A tap job runs its JavaScript file by itself, `node <file>`,
as tap's runner and StrykerJS's do: tap's command keeps its working files in
the project, and two of them side by side broke each other's (`External ID
used by multiple processes`).

A subtest runs with its parent. A job's outcomes are the adapter's ordinary
evidence records, in the job's own evidence directory: nothing new is
reported, and Jest's two records of a test (its evidence, then its outcome)
are read as one.

A fresh process per job is how these runners isolate a test file anyway, so
what one mutant leaves behind (a timer, a changed global, a fake clock) never
reaches another's tests, for the reasons warm workers were dropped for Vitest.
The cost is a start of the runner per job: npx is skipped where the project
has the tool (`npx mocha` runs `node_modules/.bin/mocha`), and jobs run side
by side, one fewer than the machine's CPUs (`SUPERCOV_STRONG_WORKERS`).

**Order, baselines, confirmation, time.** As for Vitest, with one job at a
time per mutant and its files cheapest first:

- Each file's baseline runs, without a mutant, the union of the candidates
  every mutant has there. A file whose baseline fails or leaves a chosen test
  without a record (a subtest whose parent was not chosen) runs whole from
  then on, and a test that fails in the whole file's baseline is no kill (nor
  stops a job early).
- Where the baseline's tests took under a second together, a job runs all of
  them: its kill is then judged by a baseline that ran the same tests. Else it
  runs its own candidates, and a kill is confirmed by running the job's tests
  again without the mutant; where the killing test fails that way too, the
  mutant runs again with its file whole (`overturned`).
- A job's limit is 3× its baseline and 10 s; past it the job's process group
  is stopped, and the mutant is a `timeout` unless a test had already failed.
  A mutant that passed its hit limit (`<results>.timeouts.jsonl`; under
  Browser Mode, sent to node) is a `timeout`.
- A job that ended without reporting a test its baseline reported (a module
  that throws as it loads, a process that exits) killed the mutant: the test
  file did not finish.
- A mutant whose candidates ran under a runner this pass does not run (a
  Playwright test beside them) is an `error`, never `survived`. A Vitest run
  of node and browser tests runs both here: a node test's job runs Vitest
  with the job's file, its other tests skipped (`context.skip()`), as a
  browser test's does.

**Resident runners.** Jest and CommonJS Mocha suites run their jobs in
resident processes, one per worker, as StrykerJS's runners do, instead of
starting the runner for each job:

- `strongJestHost.mjs` keeps Jest loaded and runs each job's file in band
  (`runCLI` with `runTestsByPath`), in the fresh module registry and
  environment Jest gives every test file; the job's mutant is switched on
  before the file's modules load. It reads the project's configuration once
  and hands it to Jest as JSON, as StrykerJS's runner does (a configuration
  that holds a function is read from its file each job), with Supercov's
  reporter alone and the console silent.
- `strongMochaHost.mjs` keeps Mocha and what `--require` loads, takes the
  project's CommonJS modules out of `require.cache` before each job and
  loads the job's file with `require`, so it and the code it tests load
  afresh with the mutant on.
- A suite of ES modules cannot be loaded again in one thread: the host runs
  each job in a worker thread of its own, which has a module graph of its
  own, and makes and readies the next job's thread (Mocha and what
  `--require` loads) while a job runs. On deep-eql (426 mutants) and
  pathval (123) every verdict is that of a process per job
  (`SUPERCOV_STRONG_MOCHA_ESM=process`); deep-eql's pass takes 5.9 s
  against 7.2 s on a 16-CPU Linux machine, where jobs side by side hide most of a
  process's start, and 9 s against 27 s on a laptop.

The adapters write each job's evidence into the job's own directory, read
when each record is written (a journal kept per directory: a resident
process that kept the first job's wrote every later job's there, and their
mutants read as survivors). A host that cannot start leaves its jobs to a
process each; a job past its limit stops its host, and the next job starts
another.

The pass's summary line (`driver: "jobs"`) counts its jobs, baselines,
confirmations, overturned kills, stopped jobs and their time.

A mutant runs its first file alone, as most killed mutants die there, then
the rest in batches of 4, 16, ... side by side: a survivor needs every file,
and one after another they were the pass's longest wait (a luxon mutant of
module-level code has 58 test files). Within a batch a kill decides the
mutant, then a timeout, then a file that does not finish without it. Each
pick is the file likeliest to kill it for its cost: each file's kills over
the mutants it ran so far in the pass, over its tests' time and a job's start
(300 ms), as the Vitest driver orders its copies. So the pass learns as it
goes, eight times as many mutants as workers are under way at once (enough to
keep every worker busy while some wait on a file's baseline), and a mutant
picks its first file when it starts. A later batch's jobs wait behind every
first file's job (the job queue is ordered by batch, then by arrival): a
survivor's 37 files at once held luxon's first kills back for two and a half
minutes. Jest and Mocha record each test's
duration for this order and for each job's limit; before, every test cost
the same second.

The driver is a process of the command's group; each job and resident runner
is a process group of its own, which a terminal's Ctrl-C does not reach. The
driver stops them all when it gets `SIGINT`, `SIGTERM` or `SIGHUP`, and when
it exits. A job's directory (its evidence, 24 MB a job on nanoid) is removed
as soon as its outcome is read.

### Validation against StrykerJS

`benchmarks/mutation/js-runners/` holds the corpus (`corpus.json`: public
projects pinned to a commit) and `run.mjs`, which runs StrykerJS 10.1.1 with
`disableBail` (its Mocha, Jest and tap runners with per-test coverage; its
command runner for node:test, AVA, Bun and Browser Mode, which has no runner of
its own), then Supercov, and joins every mutant's verdicts.

Measured on Linux (2026-11-22), one project per 16-CPU machine (c8a/m8a.4xlarge), both
tools with 15 workers. Times in seconds; Supercov's are its measured run
(with the mutants' values) and its strong pass. "Joined" counts the mutants
both list, Regex mutants included.

| project | runner | StrykerJS runner | joined | disagreements | StrykerJS | Supercov measure + strong |
| --- | --- | --- | ---: | ---: | ---: | ---: |
| bytes.js | Mocha | mocha | 146 | 0 | 3.0 | 1.1 + 0.4 |
| negotiator | Mocha | mocha | 544 | 0 | 3.4 | 1.2 + 1.0 |
| ms | Jest | jest | 227 | 0 | 4.6 | 1.8 + 3.2 |
| cron-parser | Jest | jest | 460 | 1 (error) | 31.8 | 2.4 + 22.4 |
| classnames | node:test | command | 161 | 0 | 7.8 | 0.6 + 2.8 |
| camelcase | node:test | command | 234 | 0 | 10.1 | 1.2 + 3.9 |
| dot-prop | AVA | command | 590 | 0 | 46.4 | 2.7 + 26.7 |
| p-map | AVA | command | 512 | 1 | 354.6 | 25.9 + 118.4 |
| ini | tap | tap | 413 | 0 (28 StrykerJS errors) | 13.6 | 1.6 + 6.5 |
| remix-auth | bun:test | command | 625 | 0 | 7.3 | 0.4 + 1.9 |
| mini-throttle | Vitest Browser Mode | vitest | 77 | 1 (error) | 32.4 | 9.0 + 28.0 |
| pathval | Mocha (ES modules) | mocha | 123 | 13 | 1.6 | 0.9 + 1.3 |
| deep-eql | Mocha (ES modules) | mocha | (StrykerJS cannot run it) | | | 1.1 + 5.9 |
| uuid 9.0.1 | Jest 28 | jest | 682 | 1 (error) | 12.6 | 1.8 + 30.8 |
| yup 1.4.0 | Jest 27 | jest | (StrykerJS cannot run it) | | | 2.8 + 124 |
| luxon 3.4.4, `src/duration.js` | Jest 29 | jest | (StrykerJS cannot run it) | | | 3.8 + 344 |

No mutant StrykerJS detected (killed or timed out) is `survived` or
`no-coverage` in Supercov, and none it let live is detected by Supercov but
the ones explained here, each applied by hand:

- p-map, `index.js:426` `preserveOrder` to `true`: Supercov kills it with
  "with `preserveOrder: false`, a mapper error drops pending input while the
  consumer is busy". Applied by hand, that test fails run alone (`ava --match`)
  and passes in the whole suite, where AVA runs every test of the file at
  once and the consumer is never busy at that moment. StrykerJS's command
  runner runs the whole suite. A kill of the subset, as on hono.
- ini: StrykerJS's tap runner ended 28 mutants in `RuntimeError`, which its
  score leaves out; Supercov kills 22 and times out 6. Applied by hand
  (`ini.js:52` `&&` to `||`), `bar.js`, `foo.js` and `win32.js` fail.
- cron-parser `CronExpressionParser.ts:454` and mini-throttle `index.ts:25`,
  a function body emptied: no switch could be placed
  (`no-placeable-ancestor`), so Supercov says `error`; StrykerJS kills both.
  An error is no verdict either way.
- pathval: StrykerJS kills 13 mutants Supercov says survive, each by one
  test alone, `exclude __proto__`, which first asserts that no global
  `polluted` exists. StrykerJS runs every mutant in one process: once a
  mutant let `setPathValue(obj, '__proto__.polluted', true)` through, the
  global stayed, and that test failed for every mutant after it. Applied by
  hand (`index.js:84` without `^`, `index.js:174` `===` to `!==`), each
  survives `npx mocha`. StrykerJS's Mocha runner cannot run deep-eql at all:
  it needs Mocha's `lib/cli/run-helpers`, which Mocha 12 no longer has.
- uuid `v35.js:19` and the like, a function body emptied: `no-placeable-ancestor`,
  as for cron-parser. yup: StrykerJS's Jest runner reads no Jest 27
  configuration from `package.json`, and given it, still finds a file "must
  contain at least one test". luxon: StrykerJS 10.1.1's Babel 8 cannot load
  luxon's Babel 7 configuration ("Requires Babel ^7.0.0-0"). (uuid needs its
  Babel environment named to StrykerJS's transform, as StrykerJS's Jest runner
  sets `BABEL_ENV=test`; luxon's seven test files whose locale-week and
  calendar tests fail on Node 24's ICU are left out of both.)
- Verdicts StrykerJS and Supercov both detect differ in kind 52 times
  (`Killed`/`Timeout` either way): a mutant that makes a test both slow and
  wrong is whichever the runner sees first.

The validation found three soundness bugs, fixed before these numbers:
assertions at a tap or node:test file's top level belonged to no test (ini:
14 mutants only `win32.js` and `proto.js` killed survived); a condition that
calls something was decided `survived` from its value alone (dot-prop
`index.js:167`, `processSegment` in an `if`); and Mocha's module setup named
no file, so a module-level mutant had no candidate (bytes.js `index.js:28`).

**Jest suites after release QA.** QA's uuid pass read 114 errors and luxon's
estimated hours. The errors were mutants a capped pass never reached, each
recorded `error` when the pass ended; an interrupted pass now leaves them
without a verdict for the next. The time had three causes, each fixed: no Jest
test recorded its duration, so every file cost the same and a mutant's
cheapest file was a guess; a mutant's files ran one after another, so a
survivor of module-level code waited on 58 files in turn; and the resident
Jest loaded the project's configuration and reporters for every job. uuid's
pass went from 72 s to 31 s and yup's from 250 s to 124 s on the same
machines; luxon's `src/duration.js` (615 mutants) takes 344 s, most of it
module-level survivors' files. uuid is still 2.4 times StrykerJS's 12.6 s: its
tests take 26 ms in all, and a job (a Jest `runCLI` of one test file) costs
650 ms against StrykerJS's 290 ms a mutant, which runs a mutant's covering
tests of every file in one `runCLI`.

**Time.** Where StrykerJS has no runner of its own (node:test, AVA, Bun) it
runs the whole command per mutant, and Supercov's jobs of one file and its
candidates are 2.5-3.5 times faster (p-map 355 s against 144 s). Jest and
CommonJS Mocha jobs run in resident runners (see above), as StrykerJS's
runners do: ms takes 4.6 s under StrykerJS and 3.2 s in Supercov's strong
pass (5.0 s with its measured run), cron-parser 31.8 s and 22.4 s (24.8 s),
negotiator 3.4 s and 1.0 s. Before the resident runners each job started
Jest or Mocha: ms took 21.8 s, cron-parser 37.3 s, negotiator 5.9 s. Under
tap, a job runs its file with `node` alone, as StrykerJS's runner does: ini
13.6 s against 8.1 s.

## Partial progress across passes and rounds

A pass its budget stops, or a CI job's time limit, used to leave a mutant
open with nothing to show for the candidates it had already passed: the next
pass ran it from its first candidate. On a large application's e2e area
(about 1,600 of 2,160 mutants decided in a first round of 8 jobs), the
mutants left were mostly survivors-to-be with many candidates, a few of them
slow, and each further round decided only about 100: each one ran the same
cheap candidates again before it reached the ones it had not tried.

So a mutant's progress is kept: **the candidate tests that passed with it
switched on.** The argument is the plan's own. A kill needs one candidate
that fails with the mutant; a survivor needs every candidate to pass. A
candidate that passed with the mutant can make neither a kill nor a survivor
of it by running again with the same code and the same test, so a later pass
runs the mutant only against the rest, and a mutant every candidate of which
passed survived. Three conditions keep the fact the same fact:

- **What it applies to.** A mutant whose candidates run one at a time with
  the request's own switch: not one that runs a test file whole (Vitest's
  module-level mutants, where the file's run, not a test's, is the unit) and
  not one a server must start with (`serverWide`: a pass with a per-request
  switch says nothing of it; the alone rounds record no progress).
- **Which passes count.** The Playwright judge records, for each mutant its
  round leaves without a verdict, the candidates whose attempt passed before
  any error outside a test (the same set it would count toward a survivor),
  as a `progress` record in the results; in a stopped round, those before
  it began to stop. An attempt Playwright interrupted (the budget's
  termination) neither passed nor failed: it settles nothing.
- **When it stops holding.** It is invalidated as a verdict is, candidate by
  candidate: carried to a later run (`supercov mutants` after an edit, a
  later round's jobs) only where the mutant's file is byte for byte as it
  was and the candidate is one the earlier run found unaffected, with no test
  of its file reached; merged (`supercov merge`) as the union of every run's
  candidates, a run lending only tests it measured, for a file it recorded
  with the merged run's bytes. A candidate that is no longer a candidate
  lends nothing.

It is saved in `strong-mutation.json` beside the verdicts, as `progress`
(mutant id to test ids), an optional field older releases ignore; a verdict
that settles a mutant drops its progress, and an `error` (which runs again)
keeps it. The engine takes the candidates off the plan before it shares the
mutants out, so the shares, the order and the driver see the work that is
left; a survivor's verdict counts the candidates it passed before.

**Why the slow tail was bounded, and what this does to it.** Within a
Playwright pass the order already ran cheap candidates first; across passes
the same cheap candidates ran again, and a mutant reached its slow ones only
in a pass long enough for all of them. With progress kept, each pass adds to
what the last reached: a mutant with 15 candidates, four of which poll for
10 s, runs its cheap ones once and the slow ones in whichever later passes
reach them.

## After an edit: verdicts no change could move

`supercov mutants` measures the checkout again once it changed, and that
run has no verdicts. It keeps those of the earlier runs of the same command,
dependencies and configuration (and mutant set), mutant by mutant, where
nothing the verdict depends on changed; the rest run. The earlier runs are
read newest first, and each gives the mutants still without a verdict the ones
it can keep, each judged against that run's own `tests affected`: a pass split
across machines leaves each share's verdicts in its own run, and they are kept
together. Reading stops once every mutant has one. A run whose record of what
each test ran is gone lends nothing, and the next older run is read. `tests affected`
(`source_manifest::affected_tests`) says, from each test's recorded
execution, which tests a change since that run could have reached: a test
file's edit reaches its tests, a source edit the tests that ran the changed
code (a file no parser reads, any test that ran it), a helper the tests load
the tests whose files load it, and a test that ran with others and recorded
nothing of its own is undetermined wherever the change is inside what the run
covered. A mutant keeps its verdict (killed, survived or timeout; an error
runs again) where

- its file is as it was, byte for byte (a comment appended counts as a
  change: a mutant's id holds its lines);
- every candidate test it has now is one the earlier run had, unaffected, and
  so is every test of their test files (they share the file's state), and so
  is every test of the files it runs whole.

A new test that could kill it is a candidate the earlier run did not have, so
it runs. `runs <id> mutants test` keeps nothing of another run: it tests the
run it names.

A shadow comparison on uuid 9.0.1 (Jest 28, 567 mutants run of 682 listed),
one edit after another, each measured and passed by `supercov mutants`: a
comment appended to `src/v4.js` kept 536 verdicts and ran 31 mutants; a
declaration added to `src/v35.js`, which most tests load, kept 156 and ran
411; one added to `test/unit/v1.test.js` kept 399 and ran 168. The last run's
verdicts were then made again from nothing (all 567 run): every one of the
682 is the same, status for status.

## Saving, interrupting and cleaning up

A pass sent SIGINT, SIGTERM or SIGHUP stops its command's processes and ends
within seconds, as the signal ends a command (130, 143, 129). The signal is
recorded for the whole pass rather than obeyed at once, wherever the pass is
when it comes: while the command runs, its supervisor sends it on to the
command's process group and stops the group 5 s later; while the pass
prepares its copy, or between passes, the progress thread sends it again
each second until a supervisor hears it. Whatever the pass waits on after
that, it is ended 8 s after the signal, the verdicts its driver wrote saved
first. On a large application's integration suite every job's pass, sent
SIGINT by `timeout` 450 s in, was still running 25 s later and was killed;
the verdicts saved survived, but the jobs failed. A parent that left the
signals ignored no longer keeps the pass from hearing them.

The CLI saves each verdict a driver writes into the run within a second of it
(`strong-mutation.json`, written whole and renamed into place), so a pass
that is interrupted (Ctrl-C, a hang-up, `SIGTERM`: exit 130, 129, 143) or
killed keeps what it decided, and the next pass runs only the rest. An
interrupted pass leaves its undecided mutants without a verdict, where it
used to record each as an `error` ("the command ended before this mutant's
verdict"): QA's capped uuid pass read 114 such errors. Each pass's directory
under `.supercov/strong` names the process that works there; when the pass
ends, success or failure, only its logs stay, and the next pass removes every
directory whose process has ended. `SUPERCOV_STRONG_KEEP=1` keeps a pass's
directory whole, every job's evidence included, for whoever studies it.

## Not yet
- Rust compiled to wasm that Vitest Browser Mode's pages load: not measured,
  so not planned. Under Node and in Playwright's pages its mutants run
  (`mutation-drivers.md`).
- A mutant of Rust compiled to wasm that a switch cannot hold: `error`, as the
  command builds the wasm once.
