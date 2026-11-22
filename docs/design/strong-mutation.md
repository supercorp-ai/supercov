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
- **One run.** It lists more worker slots than the pass can use. A slot's stand-in answers Vitest's "start" and waits, at Vitest's
  "run", for work that is ready: a baseline a mutant now needs, a
  confirmation, or the next file of a mutant whose copy before ended with
  every test passing. The worker is then told to run that work's test file
  in place of the slot's (slots are files of the work's project and
  environment, learned from Vitest's "start" for each file's baseline).
  Vitest's own limit on workers running at once holds.
- **A mutant's files one after another**, the likeliest killer first (a
  file's kills over its runs so far, over its cost), and its chain stops at
  its first kill, as StrykerJS runs a mutant's covering files with `bail`.
  Parallelism comes from many mutants at once; of the mutants ready, the one
  with the most work left goes first, so long chains do not end the pass.
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
- When every mutant is decided the run is cancelled; its slots left never
  start a process.

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

A mutant's test gets twice StrykerJS's limit (`factor × its measured duration
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

**One invocation, rows of repetitions.** The generated config, on
`SUPERCOV_STRONG_PLAN`, works out each test file's rows: row 0 is the
baseline (every test of the file some mutant needs, no mutant), then one row
per mutant with a candidate in the file, with canaries among them. It sets
`repeatEach` to the most rows a file has, `retries: 0` and no failure limit,
and keeps the project's workers; the k-th repetition of a test runs its
file's row k if the row names the test, and skips otherwise. A row is found
from the file and `repeatEachIndex` -- nothing is claimed at run time, so
no two repetitions can take one job. (The Vitest driver claims because
Vitest's copies of a file cannot be told apart; Playwright's repetitions
can.) A row after the baseline waits for the test's own baseline: a test
that fails without a mutant is no candidate.

Playwright gives every repetition index its own worker (worker fixtures are
per repetition), so each row of a file starts a worker and, for page tests,
a browser. That is the price of rows; the server is not restarted.

**Concurrency as the project runs it.** Repetitions of one file are separate
groups, which Playwright runs side by side where the project ran the file's
tests one after another. A suite whose tests share a server's state (a
harness's fault rules for one server slot, rewritten by each test) then
fails for reasons no mutant made. So a file runs one test at a time here
when its tests are known not to run at once: two or more of them ran in the
measured run and their spans never overlapped, or, where the spans tell
nothing (one test, none spanned), the project does not run files
`fullyParallel`. Each such attempt holds the file's lock from its start to
its end, across workers. A file whose tests overlapped runs side by side.
(Each Playwright attempt has two records, the reporter's, with its status,
and the fixture's, with its span; the plan takes the span from whichever
carries it.) The workers are the project's own.

**Kills are confirmed.** A test that fails beside another mutant's attempt
(their times overlap) may have failed for the other's doing. Such a kill is
not recorded: the mutant goes to a **serial pass** (one worker, one test at
a time), which runs its killing test alone first and its other candidates
only if that test lets it live. What a serial pass kills, nothing beside it
did. On the fixture of the CLI test, 14 of 15 per-request kills ran beside
another mutant and were confirmed: the serial pass took about as long as
the main pass (30 s against 33 s), most of it two page kills waiting out an
8 s `expect`; a project with one worker confirms nothing. A failing
attempt's error (its first line, ANSI stripped) is the verdict's reason.

**Likeliest killer first.** The main pass runs each mutant against one test:
its candidate whose own evidence infected it, cheapest first (the plan's
order). A mutant it lets live goes to a **stage pass** with its other
candidates, side by side; a kill stops a mutant at its first test.

**Time.** A mutant's test gets `1.5 × its measured duration + 5 s` (the
duration from the attempt's span), plus the project's `expect` timeout
(default 5 s) and `actionTimeout`, and never less than the test's own
timeout (the project's, default 30 s, with `test.slow()` and
`describe.configure`): an assertion that fails waits out its own timeout
first (a spec's `{ timeout: 15_000 }`), Playwright ends the test at its own
timeout anyway, and the budget must not cut either short. A mutant that
makes a test hang costs up to the test's timeout. A test that fails
on an assertion or an action, even as its time runs out, is a kill; one
that only ran out of time is run again in a serial pass with twice the
budget before it counts as `timeout`.

**Run alone, the server started with the mutant.** That per-request
switching is exact only where nothing one request does reaches another, and
a server keeps what its top level made. So the plan marks a mutant
`serverWide` where a server ran its code in work no request named (start-up,
timers: `background`), at a server module's top level (`module-level`), or
for a request that named no test, credited by time (`by-time`). Such a
mutant gets no row: the engine starts the command again for it, alone, with
`SUPERCOV_ACTIVE_MUTANT` set in every process and only its candidate tests,
and its verdict's reason says so; a mutant that keeps the server from
starting is `killed`. Work for a request that named no test while no test
ran (a web server's readiness check) is not such work: it ran with no mutant,
as every request runs with its own, and leaving it out keeps every handler's
mutant from running alone. That is the one place the pass trusts that a
request's work does not reach the tests' through shared state without a
canary to check it.

Each further pass a driver asks for (a `restart` record in the results: one
mutant alone, a stage pass, a serial pass) is a new invocation of the
command, its output in a log beside the results, run in turn until none
asks for more. A serial pass also takes every mutant whose rows did not all
run or ended after an error outside any test (a shared server that exited).

**Canaries.** Every pass puts rows with no mutant among the mutated ones (in
the middle and at the end of the busiest files' rows). A canary is a test
that passed its baseline, run again after other attempts' mutants used the
server: one that fails means state leaked between attempts (or the test is
flaky), and every mutant that started before it ended is run again, one
mutant per server start. (A planted mutant no test can reach would tell the
same, at the cost of a switch.) The pass's summary line says how many ran
and which failed.

A mutant's candidates are the attempts its weak verdict names, as in Vitest.
Candidates under another runner (a Vitest test of the same run) are not run
by this pass: a mutant no Playwright test kills is then an `error`, never a
`survived` it has not earned.

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
  plan mixing Vitest's node tests and its browser tests) is an `error`, never
  `survived`.

The pass's summary line (`driver: "jobs"`) counts its jobs, baselines,
confirmations, overturned kills, stopped jobs and their time.

### Validation against StrykerJS

`benchmarks/mutation/js-runners/` holds the corpus (`corpus.json`: public
projects pinned to a commit) and `run.mjs`, which runs StrykerJS 10.1.1 with
`disableBail` (its Mocha, Jest and tap runners with per-test coverage; its
command runner for node:test, AVA, Bun and Browser Mode, which has no runner of
its own), then Supercov, and joins every mutant's verdicts.

Measured on SoroCI, one project per 16-CPU machine (c8a/m8a.4xlarge), both
tools with 15 workers. Times in seconds; Supercov's are its measured run
(with the mutants' values) and its strong pass. "Joined" counts the mutants
both list: StrykerJS's `Regex` mutants are not listed by Supercov (see
`mutants.md`), and are left out.

| project | runner | StrykerJS runner | joined | disagreements | StrykerJS | Supercov measure + strong |
| --- | --- | --- | ---: | ---: | ---: | ---: |
| bytes.js | Mocha | mocha | 126 | 0 | 2.7 | 1.0 + 1.7 |
| negotiator | Mocha | mocha | 544 | 0 | 2.9 | 1.1 + 5.9 |
| ms | Jest | jest | 204 | 0 | 4.9 | 2.0 + 21.8 |
| cron-parser | Jest | jest | 429 | 1 (error) | 33.6 | 2.8 + 37.3 |
| classnames | node:test | command | 159 | 0 | 7.8 | 0.7 + 2.5 |
| camelcase | node:test | command | 217 | 0 | 10.5 | 1.3 + 3.6 |
| dot-prop | AVA | command | 584 | 0 | 50.1 | 1.4 + 28.5 |
| p-map | AVA | command | 512 | 1 | 347.8 | 23.4 + 110.7 |
| ini | tap | tap | 389 | 0 (28 StrykerJS errors) | 13.4 | 1.6 + 5.9 |
| remix-auth | bun:test | command | 625 | 0 | 7.2 | 0.8 + 2.0 |
| mini-throttle | Vitest Browser Mode | vitest | 77 | 1 (error) | 32.3 | 9.0 + 27.9 |

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
- Verdicts StrykerJS and Supercov both detect differ in kind 48 times
  (`Killed`/`Timeout` either way): a mutant that makes a test both slow and
  wrong is whichever the runner sees first.

The validation found three soundness bugs, fixed before these numbers:
assertions at a tap or node:test file's top level belonged to no test (ini:
14 mutants only `win32.js` and `proto.js` killed survived); a condition that
calls something was decided `survived` from its value alone (dot-prop
`index.js:167`, `processSegment` in an `if`); and Mocha's module setup named
no file, so a module-level mutant had no candidate (bytes.js `index.js:28`).

**Time.** Where StrykerJS has no runner of its own (node:test, AVA, Bun) it
runs the whole command per mutant, and Supercov's jobs of one file and its
candidates are 3-4 times faster (p-map 348 s against 134 s). Against
StrykerJS's in-process Mocha runner a job's process start costs more: bytes.js
about even, negotiator 2.4 times slower. Jest is slowest: each job starts Jest
(about 1 s), where StrykerJS's runner keeps it resident; ms takes 4.9 s under
StrykerJS and 23.8 s under Supercov, cron-parser 33.6 s and 40.1 s. Under tap,
a job runs its file with `node` alone, as StrykerJS's runner does: ini 13.4 s
against 7.5 s.

## Not yet

- Reuse of verdicts across commits: kept out until a shadow run shows it
  never disagrees with a fresh run.
- A Vitest run whose tests run both in node and in the browser: the browser's
  mutants are run by the jobs pass, and those with candidates in node too are
  `error`.
- StrykerJS's `Regex` mutator: its mutants are not listed (bytes.js: 20 of 146).
