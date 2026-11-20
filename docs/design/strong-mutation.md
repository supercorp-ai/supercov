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
   `survived` with the weak reason. Not run.
4. `infected` or `unknown`: run, against its **candidate tests**.

A test is a candidate for a mutant when the mutant's weak verdict computed
from **that test's evidence alone** is `infected` or `unknown`. The
argument: with the mutant switched on, a test's execution is identical to
its original execution up to the first evaluation where the mutant's value
differs. A test whose own evidence shows no such evaluation runs exactly as
it did, and passes as it did. This is the same rule that makes `not-infected`
a certain survivor (Just, Ernst and Fraser, ISSTA 2014), applied one test at
a time.

It holds only where a test's evidence is its own. So a test is always a
candidate, whatever its verdict, when:

- its attribution is not `exact` or `by-time` (`run-wide`, `partial`): its
  silence does not prove it did not run the code;
- the mutant's code ran in work no test was credited with (a server's
  start-up, work between tests): that work may be any test's;
- it failed, was skipped or was flaky in the measured run: it is not run at
  all (it cannot pass as a baseline), and a mutant whose only candidates are
  such tests is recorded `survived` with that reason.

And a mutant is **static** when it can be evaluated while a module loads
(StrykerJS's `static`): every test of every file that loaded the module is a
candidate, and the files run whole.

## Order and early exit

A mutant's candidates run cheapest first by their measured duration, except
that a test that killed the same mutant in an earlier strong pass, or killed
a mutant at the same site, goes first. The mutant's run stops at its first
failing test: that settles `killed`.

## Running many mutants at once without restarting anything (Vitest)

A Vitest run evaluates each test file in an isolated module graph (the
default `isolate: true`). Supercov's setup file runs in that graph before the
test file's imports. So different files of **one** Vitest run can each have a
different mutant switched on, and nothing one file does with its mutant can
reach another file's modules.

The pass therefore schedules rounds. In a round each test file gets at most
one mutant, and a mutant whose candidates span several files gets all of them
in the same round. Supercov's setup file reads its file's assignment, switches
the mutant on before the file's imports run, and skips every test of the file
that is not a candidate. One round is one call into the resident Vitest
instance (`runTestSpecifications`): no new process, no new workers, the
project's own pool, so a suite that gives each worker its own database keeps
doing so.

Where isolation is off (`isolate: false`, `--no-isolate`, a single-thread pool
sharing modules) a round holds one mutant.

## Baselines

Skipping tests can change what the remaining tests see (a test that relies on
an earlier one's state). Each file's candidate selection is first run with no
mutant; a selection that fails is widened to the whole file, and a file that
fails whole is reported, its mutants `error`.

## Timeouts and runaway loops

A mutant's test gets `factor × its measured duration + constant` (defaults
1.5× and 5 s, as StrykerJS). A mutant switch counts its evaluations; past
`max(1,000, 10 × the hits seen without the mutant)` it throws a timeout
error, so a loop the mutant made endless ends in the file's worker rather
than hanging it. A test that runs out of time is run once more, alone, with
twice the budget before it counts as `timeout`; an error outside any test is
run once more too before it counts as a kill.

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
`with-statement`), and the driver reports it `error`.

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

## Not yet

- Jest, Mocha and node:test drivers (Vitest first).
- End-to-end tests against a server process (the next step: the server's
  mutant chosen per request).
- Reuse of verdicts across commits: kept out until a shadow run shows it
  never disagrees with a fresh run.
