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

## Order and early exit

A mutant's candidates are grouped by test file, cheapest file first. It runs
its cheapest file first; if that leaves it alive, all its other files run in
the next round at once: the first file kills most killed mutants, and a
survivor takes two rounds rather than one per file. Within a file a copy stops
at its first failing test: that settles `killed`.

## Running many mutants at once without restarting anything (Vitest)

A Vitest run evaluates each test file in an isolated module graph (the
default `isolate: true`). Supercov's setup file runs in that graph before the
test file's imports. So different files of **one** Vitest run can each have a
different mutant switched on, and nothing one file does with its mutant can
reach another file's modules.

The pass therefore schedules rounds. A round can run one test file several
times, each copy with its own mutant: Vitest's pool runs a file listed twice
twice, each in its own worker (with the worker's own database, where a suite
gives each worker one). The copies cannot be told apart in Vitest's results,
which are kept by file path, so each copy claims its assignment from the
round's directory and reports its tests' outcomes there itself. Supercov's
setup file switches the copy's mutant on before the file's imports run, and
skips every test the assignment did not choose. A round holds
`SUPERCOV_STRONG_ROUND` (default 6) × the workers assignments: a round ends
when its slowest copy does. One round is one call into the resident Vitest
instance (`runTestSpecifications`): no new process, no new workers, the
project's own pool, so a suite that gives each worker its own database keeps
doing so.

Where isolation is off (`isolate: false`, `--no-isolate`, a single-thread pool
sharing modules) a round holds one mutant.

## Baselines

Skipping tests can change what the remaining tests see (a test that relies on
an earlier one's state). Each file is first run with the union of its
candidates and no mutant; a file where that fails runs whole from then on, and
only a test that fails with the whole file and no mutant is no kill. A file
that does not finish without a mutant is reported, its mutants `error`.

## Timeouts and runaway loops

A mutant's test gets `factor × its measured duration + constant` (defaults
1.5× and 5 s, as StrykerJS). A mutant switch counts its evaluations; past
`max(1,000, 10 × the hits seen without the mutant)` it throws a timeout
error, so a loop the mutant made endless ends in the file's worker rather
than hanging it. A test that runs out of time is run once more with twice
the budget before it counts as `timeout`. An error no test catches (work a
test left running, a rejected promise nobody awaited) is the mutant's where
the file raised none without it: a kill, or a `timeout` where it is the
switch's own limit. A round past its deadline (twice its time limit plus 30 s:
a worker that will not end) ends the pass: its unfinished copies are
timeouts, every verdict so far is saved, and `mutants test` starts another
pass for the mutants left.

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
`SUPERCOV_MUTANTS` says.

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

## Not yet

- Jest, Mocha and node:test drivers (Vitest and Playwright first).
- Reuse of verdicts across commits: kept out until a shadow run shows it
  never disagrees with a fresh run.
