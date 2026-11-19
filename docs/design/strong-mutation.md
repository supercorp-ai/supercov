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
Supercov's own ids. An expression mutant becomes
`(__sc.m(17) ? <replacement> : __sc.m(18) ? <replacement> : <original>)`,
a statement mutant `if (__sc.m(19)) { <replacement> } else { <original> }`.
`__sc.m(n)` is true when `n` is the file's active mutant, and counts. The
replacement text is checked against StrykerJS's own for every mutant of the
parity fixtures: same text, byte for byte.

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
