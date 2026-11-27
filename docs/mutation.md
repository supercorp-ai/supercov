# Mutation testing

Coverage says which code the tests ran. It does not say whether they would
notice that code doing the wrong thing. Mutation testing asks that directly:
it makes small changes to the source, one at a time (`a < b` becomes
`a <= b`, a `return` gives `0`, a call is removed), and checks whether a test
fails. Each change is a **mutant**. A mutant a test fails on is **killed**; one
every test passes with **survived**, and points at a check no test makes.

Supercov makes the mutants each language's established tool makes, so its
counts and verdicts can be compared with that tool's:

| Language | Mutants of | Support |
| --- | --- | --- |
| JavaScript, TypeScript | StrykerJS 10.1.1 | Full, under nine runners |
| Python | mutmut 3.8.1 | Preview |
| Rust | cargo-mutants 27.1.0 | Full, under `cargo test` (libtest and doctests) and cargo-nextest |
| Java, Kotlin | PIT 1.17.1, its default mutators | Full, under JUnit 5, JUnit 4, Kotest, Spock and TestNG, with Maven, Gradle and Ant |
| Go | Gremlins 0.6.0, every mutator | Preview |
| Ruby | mutant 0.17.1, light operators | Preview |

A preview language works end to end and was validated against its tool on
public projects (see [Compared with the established tools](#compared-with-the-established-tools)),
but has been run on fewer projects than JavaScript and may change in a later
release. On Windows,
Python's and Ruby's mutants are listed and the weak report reads them, but
the strong pass does not run them: their drivers fork the loaded suite, which
Windows cannot, so each is counted "not run" under its runner.

```sh supercov-example
npx supercov mutants -- npm test
npx supercov mutants --diff origin/main
```

## Weak and strong mutation

Supercov answers in two steps.

**Weak mutation** reads the run you already measured. Nothing runs again. For
each mutant it says how far the tests got toward killing it:

| Status | Meaning |
| --- | --- |
| `not-reached` | No test ran the mutated code |
| `not-infected` | A test ran it, and every evaluation gave the mutant the same value as the original: these tests cannot kill it |
| `infected` | Some evaluation gave the mutant another value: a candidate kill, not a kill |
| `unknown` | The run cannot tell, such as code whose values it did not record |
| `equivalent` | No input can make it differ, shown from the code alone |

A `not-infected` mutant is a certain survivor, and its reason says what is
missing: `a never equal to b` for `a < b` made `a <= b` means no test tries
the boundary. The weak score is `infected / (infected + not-infected +
not-reached)`, with unknown and equivalent mutants left out.

**Strong mutation** (`mutants test`) runs the tests again with each mutant
switched on and records what the established tools record:

| Verdict | Meaning |
| --- | --- |
| `killed` | A test failed with the mutant in place |
| `survived` | Every test that could kill it passed |
| `timeout` | A test ran past its time limit with the mutant in place |
| `no-coverage` | No test ran the mutated code |
| `equivalent` | No test can tell it from the original, proven before running it; left out of the score |
| `unviable` | The mutant does not compile (Rust and Go); counted apart and left out of the score |
| `error` | The pass could not give it a verdict, with the reason; left out of the score |

The mutation score is `(killed + timeout) / (killed + timeout + survived +
no-coverage)`, as StrykerJS computes it. It appears only after a strong pass
and is never estimated from weak results. Both scores are shown beside
coverage and never change a coverage figure.

The strong pass is cheaper than running every test against every mutant
because the measured run already holds what the established tools learn in a
first pass of their own:

- a mutant no test reached is `no-coverage`, and one no test infected is
  `survived`, without running anything;
- every other mutant runs only against the tests whose own evidence shows they
  reached or infected it, the likeliest killer first, and stops at the first
  failure.

- a JavaScript or TypeScript mutant whose file compiles to the same code as
  the original is `equivalent`, and is not run; mutants whose files compile
  to the same code as each other run once, and each takes that run's verdict
  (see Equivalent mutants, below).

Each of these shortcuts is exact. Where the run cannot prove a test is
irrelevant, the test runs.

### Equivalent mutants

Some mutants change the source without changing the program: a branch that
can never run emptied, `if (false)` made `if (false)` again, `"a".trim()`
made `"a"`. No test can kill them, yet each one used to run every test
that reached it and then waited in the survivors for someone to dismiss it.

Before a strong pass runs a JavaScript or TypeScript mutant, Supercov
compiles its file twice -- once as written, once with the mutant -- with the
same optimising compiler and the same options (oxc's minifier, built into
Supercov, with every assumption that could change behaviour turned off),
and compares the two outputs. Where they are identical the mutant is the
original program, so it is `equivalent`: not run, not a survivor, and out of
the score. This is Trivial Compiler Equivalence (Papadakis et al., ICSE
2015). It only ever says `equivalent` on an exact match; anything else, a
file it cannot compile included, runs as before. Two mutants of one file
that compile to the same code are one program: the pass runs one of them and
gives the other the same verdict.

There is nothing to set up or turn on. The pass says what it spared:

```text
[supercov] 10 mutant(s) proven equivalent: each compiles to the same code as the original, so no test can tell it apart; 10 of them would have run and did not
[supercov] 6 mutant(s) compile to the same code as another mutant, which runs once and gives them its verdict
[supercov] about 1 s of test time saved by not running them
```

Its summary says the same, and each one says why where it is listed:

```text
$ supercov runs latest mutants src/shapes.ts:13
13:7  ConditionalExpression  infected  strong: equivalent
  empty → true
  why: `empty` was false at least once (opposite-value)
  strong: equivalent: compiles to the same code as the original: no test can tell it apart, so it was not run
  id:  src/shapes.ts:13:7-13:12:ConditionalExpression:true
```

Here `if (empty) { return "none"; } return "none";` returns `"none"` whichever
way the test goes, so making it `true` changes nothing. A mutant that takes
another's verdict says whose: `compiles to the same code as <id>, which ran
for both`.

The markdown and HTML reports fold them into a section of their own, apart
from the survivors. A file that does not compile (a syntax Supercov's parser
does not take) is named, and its mutants run as before:
`could not compile 1 file(s) to compare their mutants' code, so their mutants run as before: src/legacy.js`. So is a file
whose mutants would take longer to compile than to run, such as a
generated file with thousands of mutants.

### How this differs from coverage

A line can be covered, its branch taken both ways, and still have no test that
checks its result. Coverage counts that line as done; a `not-infected` or
`survived` mutant on it says it is not. A mutant on code no test runs is
`not-reached`, the same gap `runs latest gaps` already lists, so the mutants
worth reading first are the reached ones that survive.

## Commands

### Test the tests

```sh supercov-example
npx supercov mutants -- npm test
```

`supercov mutants` measures the command with the probes that record the values
its mutants' verdicts read (`SUPERCOV_MUTANTS=1`), lists the mutants, and runs
the strong pass. Without `--` it uses the latest run's command. The pass's
summary gives the mutation score, every verdict counted, and ends with the
mutants the tests let survive.

- **Unchanged checkout.** The next `supercov mutants` reads the same run again
  and runs nothing it already decided. With `--shard` it saves into a run of
  its own, so that each job's share is saved into a run no other job writes:
  made from the newest run of the checkout whose command passed, without
  measuring again (`not measuring: the checkout is unchanged since run r4, so
  this shard saves its verdicts into run r9, made from it`), or, where none
  passed, from the newest whose command failed only because tests failed,
  which are no candidates (`continuing from run r4, whose command failed: 1
  test failed while measuring and is not a candidate: checkout > pays`), or
  measured where there is none, saying why (see
  [A pass split across jobs](#a-pass-split-across-jobs)).
  An earlier run measured on the same files, byte for byte, keeps every
  verdict a pass reached, whatever the two measurements recorded: a test
  whose path depends on the time, or that failed in one of them, moves no
  verdict.
- **A baseline of the checkout.** Where no run of the command can be read or
  continued, a run of the checkout as it is that holds the tests the command
  runs is: a merge of a suite measured in parts on several machines (a
  weekly baseline restored into `.supercov/runs`), or a run of the whole
  suite. The pass's run is made from it with those tests' records alone, as
  measuring them would record, and nothing is measured (`not measuring: run
  r4 of this checkout holds the tests the command runs (18 test(s) in 6 of
  its 61 test file(s)); this pass saves its verdicts into run r9, made from
  it with those tests alone`). The command's tests are every test of a merge
  whose runs were each measured with the command itself, and otherwise those
  of the test files the command names, by path or by the end of one
  (`tests/e2e/billing.spec.ts`, `billing.spec.ts`), anywhere in its words,
  where the run holds every one of them that any run holds; a command that
  names none is measured. The baseline must be of the checkout
  as it is, by its fingerprint: on a pull request, whose checkout differs, the
  command is measured, so narrow it to the specs `tests affected --files`
  names; the change's own mutants are run only by tests that ran changed
  code, which those are. A test that failed in the baseline is no candidate.
  It needs a merged run made by this release, which keeps what its runs
  answer with.
- **After an edit.** A new commit runs only the mutants whose verdict could
  have changed. `supercov mutants` measures the checkout again and keeps an
  earlier run's verdict for each mutant where nothing it depends on changed;
  the earlier runs are those of the same command, dependencies and
  configuration in `.supercov/runs` (a baseline restored there among them),
  newest first, so a pass split across jobs, each holding part of the
  verdicts in its own run, is kept whole. A verdict is found by its mutant's
  code, not its line: a line added above a function keeps its mutants'
  verdicts. A kill is kept where the test that killed it still reaches the
  mutated code and runs as it did; a survival where every candidate test it
  has now ran with it then and runs as it did; a timeout, an error, a kill
  no record names a test for, and a mutant whose own declaration changed run
  again. A test runs as it did where nothing changed that it could run with
  the mutant switched on, which may be code it never ran without (a function
  no test calls that a mutant sends a test into counts): for a JavaScript or
  TypeScript test run in its runner's process (Jest, Vitest, Mocha, ...),
  nothing its imports load and its code can name; for a Rust test, nothing
  its own function can name in any Rust file (a trait implementation's
  method counts where its type is named, as the language calls it
  unnamed); for a Python or Ruby test, no code its imports load; for any other (a Playwright test, a mutant a
  server starts with), `runs latest tests affected` finds it unaffected and
  no code but tests' changed. The summary says how many verdicts were kept,
  from which runs and commits (`26 verdict(s) kept from run r5 (commit
  1a2b3c4), which took 25 test run(s) to reach: nothing they depend on
  changed`), and
  how many were not, by why (`13 earlier verdict(s) not kept, so run again:
  13 a test that could kill them loads code that changed, which they could
  lead it into`); `runs <id> mutants` says where each kept verdict was
  reached (`carried from run r5 (commit 1a2b3c4)`). On a fixture's eight
  commits it ran 72 mutants where passes from nothing ran 201, every verdict
  the same; `docs/design/strong-mutation.md` ("After an edit") has the rules
  and why they hold.
- **Interrupted.** Each verdict is saved as it is made. A pass stopped by
  Ctrl-C, a killed job or a test worker that never ended keeps what it
  decided, and the next pass runs only the rest. Sent SIGINT or SIGTERM, the
  pass stops every process it started and ends within seconds: one that has
  not ended 8 s after the signal is ended, what it decided saved first.
- **Order and budget.** The cheapest mutants run first, by the recorded times
  of the tests each runs against, so a pass cut short has decided as many as
  it could. `--budget <seconds>` is the whole command's, measuring and
  planning included: once it is spent the pass stops and ends as a finished
  one, its verdicts saved, so a job ends when its budget says. Nothing is
  judged once the pass begins to stop: a test the stop cut short says nothing
  of its mutant, which is left for the next pass. The summary
  says how many mutants it left for the next pass (`budget of 2400 s spent:
  812 mutant(s) left without a verdict, for the next pass`), and, where
  measuring took half the budget or more, what it took (`measuring took 310 s
  of the 450 s budget, which leaves the pass 140 s`). A measurement is never
  cut short, since one stopped halfway records nothing: one that outlasts the
  budget leaves the pass nothing, and the pass runs nothing.
- **Partial progress.** A mutant a pass left open keeps the candidate tests
  that already passed with it, saved with the run's verdicts, and the next
  pass runs it only against the rest (`37 mutant(s) resume where earlier
  passes left them, with 4 of 15 candidates left (the median)`): a kill needs
  one failing candidate, a survivor every candidate passing, and the ones
  that passed did. The candidates are kept as verdicts are, after an edit, a
  merge and in a later round: a candidate a change could reach runs again.
  The Playwright driver records them.
- **A first tier.** A Playwright mutant with more candidates than its first
  tier holds (32, or more where the run's kills come later, or as many as
  take 60 s of their tests' time, at least four, whichever comes first)
  runs against that tier first, every such mutant before any runs the rest. One the tier
  lets live is `survived-so-far` until the rest runs, in the same pass where
  it has the time: listed with the survivors as such, never in the score
  (`docs/design/strong-mutation.md`, A first tier, then the rest).
- **Exit codes.** `0` with an answer; `1` when `--fail-on` or `--critical`
  finds a mutant; `2`, after the answer, when the measured command failed (a
  failing test kills nothing, so the verdicts cannot gate) or no mutant could
  be run. A run that recorded no test is refused with exit `2`.

The steps can also be taken one at a time:

```sh supercov-example
npx supercov -- npm test                         # a plain run: no cost for mutants
npx supercov runs latest mutants                 # weak mutation, from the run
npx supercov runs latest mutants test            # strong mutation, saved into the run
npx supercov runs latest mutants --status survived
```

A plain run lists its mutants from its sources the first time they are asked
for. Without the value probes, a mutant whose verdict needs a recorded value
is `unknown`, and the strong pass runs it.

### Read the mutants

```sh supercov-example
npx supercov runs latest mutants                              # a table of files and the scores
npx supercov runs latest mutants src/billing.ts               # one file's mutants, by line
npx supercov runs latest mutants src/billing.ts:42            # one line's, with their reasons and ids
npx supercov runs latest mutants --status not-infected,not-reached
npx supercov runs latest mutants --mutator EqualityOperator
npx supercov runs latest mutants --json                       # the coverage.mutants reply, a page at a time
npx supercov runs latest mutants --format jsonl --output mutants.jsonl
```

After a strong pass, each mutant shows its verdict beside its weak status, the
mutation score appears with every verdict counted, and `--status` also takes a
verdict (`survived`, `killed`, `timeout`, `no-coverage`, `error`,
`unviable`). The table of files puts first the files with the most mutants a
test reaches and does not infect. A listed mutant shows its change as
`original → replacement`, or by the part it changes where the code is long
(`….slice(0, maxUsers) → …`). `--kind`, `--runner`, `--tag`, `--title` and
`--test-file` read the verdicts of those tests alone.

### Gate a pull request

`--diff <base>` keeps the mutants on lines the change added or modified since
it left `<base>`, and lists first the ones its tests leave alive:

```sh supercov-example
npx supercov runs latest mutants --diff origin/main
npx supercov runs latest mutants --diff origin/main --format markdown --output mutants.md
npx supercov runs latest mutants --diff origin/main --fail-on not-infected,not-reached
npx supercov mutants --diff origin/main --fail-on survived
npx supercov mutants --diff origin/main --format markdown --output mutants.md
```

- `--format markdown` writes a pull request's comment, ready to post as it
  is: the counts, the change's lines of code no test ran, then up to 25
  mutants left alive with where they are, what they change, how many of the
  run's tests ran their line and why, and the command that lists the rest.
  Once a strong pass gave verdicts, it has the mutation score and lists the
  survivors. Those that only change a log line are folded in a `<details>` of
  their own, and so are those `survived so far` (counted apart, never with
  the survivors). `supercov mutants` takes `--format markdown` and
  `--output <file>` too, and writes its own answer that way.
- A survivor that only changes a log line (its change lies wholly inside the
  arguments of `console.*`, of a method named `info`, `warn`, `error`,
  `debug`, `trace` or `log`, or of what makes a logger: `createLogger(...)`,
  `getLogger(...)`, `pino(...)`, and `.child(...)` of a logger) is still
  `survived`, but marked `(only changes a log line)` (`logOnly` in JSON) and
  counted on its own: `survived 185 (109 only change a log line)`. Triage can
  set those aside.
- `--fail-on` exits `1` when a mutant asked about has one of the statuses
  given. On `runs latest mutants` they are weak statuses, from the run alone;
  on `supercov mutants` and `mutants test` they are verdicts: `survived`,
  `timeout`, `no-coverage` and `error`. A second `mutants test` over the same
  run runs nothing already decided, so it can gate on the first's verdicts.
- The change is the commit the run measured against its merge base with
  `<base>`. A run with uncommitted changes is compared as the working tree,
  and the output says so. A base git does not know, as in a shallow checkout,
  exits `2`: fetch it (`fetch-depth: 0`).

In GitHub Actions, after the tests:

```yaml
- uses: actions/checkout@v6
  with:
    fetch-depth: 0 # the base and the merge base
- run: npx supercov -- npm test
- run: npx supercov runs latest mutants test --diff origin/${{ github.base_ref }}
- run: npx supercov runs latest mutants --diff origin/${{ github.base_ref }} --format markdown --output mutants.md
- run: gh pr comment ${{ github.event.number }} --body-file mutants.md --edit-last --create-if-none
  env:
    GH_TOKEN: ${{ github.token }}
- run: npx supercov runs latest mutants test --diff origin/${{ github.base_ref }} --fail-on survived
```

### Critical code and accepted mutants

```sh supercov-example
npx supercov runs latest mutants test --critical "src/auth/**" --fail-on survived
npx supercov runs latest mutants test --shard 1/4
npx supercov runs latest mutants test --plan --json
```

- `--critical <glob>` (repeatable): every mutant in those files must be killed
  or accepted, or the command fails.
- `--shard k/n` runs one of n shares of the mutants, of about the same cost,
  for parallel jobs (see
  [A pass split across jobs](#a-pass-split-across-jobs)).
- `--budget <seconds>` stops the pass once that time is spent since the
  command started, its verdicts saved, and leaves the rest for the next pass.
- `--timeout-factor <x>` (or `SUPERCOV_STRONG_TIMEOUT_FACTOR`) sets a
  Playwright test's time limit when it runs with a mutant: x times its
  baseline, + 5 s (default 1.5, StrykerJS's `timeoutFactor`). See
  [Playwright](#javascript-and-typescript).
- `--plan` prints what would run, and runs nothing.

Some mutants no test can tell apart from the original. Accept them in
`supercov.accepted.json` at the project root:

```json
[
  { "id": "src/quota.ts:12:21-12:26:EqualityOperator:>=", "reason": "n is never 0 here: the caller returns early" }
]
```

An accepted mutant still runs, is shown as accepted, and never fails the
command. An entry whose mutant is killed or no longer exists is listed as
stale. A mutant's id is its file, its span, its mutator and what it becomes,
and stays the same while that code does.

### A pass split across jobs

A large application suite's strong pass can be split across machines. Each
job measures the whole suite and runs its share of the mutants; a last job
merges the runs, and the merged run holds every job's verdicts. A pass whose
budgets leave mutants undecided goes on in another round, the same workflow run
again on the same commit, and later commits go on from where the last round
ended:

```yaml
# Each of four jobs (matrix.shard 1 to 4):
- uses: actions/cache/restore@v4
  with:
    path: .supercov/runs
    key: mutants-${{ github.sha }}-${{ github.run_id }}-${{ github.run_attempt }}
    restore-keys: |
      mutants-${{ github.sha }}-
      mutants-
- run: npx supercov mutants --shard ${{ matrix.shard }}/4 --budget 2400 -- npm test
- uses: actions/upload-artifact@v4
  with: { name: "mutants-${{ matrix.shard }}", path: .supercov/runs, include-hidden-files: true }
# Then one job:
- uses: actions/download-artifact@v4
  with: { pattern: "mutants-*", path: .supercov/runs, merge-multiple: true }
- run: npx supercov merge
- run: npx supercov runs latest mutants --status survived
- run: npx supercov runs clean --keep 5 # this round's four runs and their merge
- uses: actions/cache/save@v4
  with:
    path: .supercov/runs
    key: mutants-${{ github.sha }}-${{ github.run_id }}-${{ github.run_attempt }}
```

The cache key names the round, so each round saves its own; a round restores
the latest one saved for its commit, or else the latest one saved.

`supercov mutants --shard k/n -- <command>` is `supercov -- <command>` with
the mutants' probes on, then `runs latest mutants test --shard k/n`, keeping
earlier verdicts as after an edit. On a checkout unchanged since a run it
restored, it measures nothing, as below. Name the command after `--`: without it,
`supercov mutants` takes the latest run's, which after a merge is the merge.
Every job must measure the same commit with the same dependencies, as for any
merge ([CLI reference](cli.md#combine-shards)).

- **The shares.** Each job computes the same split from its own run: the
  mutants to run are dealt in turn, back and forth, most costly first, so
  every share gets its part of the costly ones. A mutant's cost there is what
  the plan lists of it, the copies of test files it starts and the tests they
  run, not the tests' times, which differ between jobs.
- **The budget.** `--budget <seconds>` ends a job in time without a
  `timeout` around it: it counts from the command's start, measuring
  included, and past it the pass stops, saves what it decided, and exits as
  a finished pass. Within a pass the cheapest mutants
  run first, so a job cut short has decided as many as it could, and the
  mutants it left run in the next commit's pass, where the restored runs lend
  the rest of their verdicts.

- **The merge.** `supercov merge` keeps the strong verdicts of the runs it
  merges, mutant by mutant, and says how many and from which runs. A kill in
  any run is a kill. A survival stands where the run that let the mutant
  survive measured every test of the merged suite that could kill it, as
  shards of one suite each do; where another run's tests could kill it and
  that run has no verdict for it, as a unit run merged with an end-to-end run
  can, the mutant is left undecided, and a pass over the merged run runs it.
  Verdicts of another mutant set are not kept, and the merge says whose.
- **Another round.** On the commit the restored runs measured, each job saves
  into a run of its own: `supercov mutants --shard` never saves into a run it
  restored, of which every job of the round holds a copy. It makes that run
  from the newest restored run that recorded its mutants' values and whose
  command passed, or failed only in tests that failed, without measuring
  again: the run holds that run's evidence
  under an id of its own. It keeps every verdict a pass reached that the
  restored runs hold (`1317 verdict(s) kept from runs r8, r7, r6, r5: the
  checkout is unchanged since them`), and the shares are of what is left.
  Mutants the plan decides without running, such as a survivor no test
  infected, are decided again, not kept. Each job's run replaces the
  restored ones of its command, so the last job's `supercov merge` takes this
  round's runs, whose verdicts hold the earlier rounds'. It says how many
  verdicts came from each run (`1265 strong verdict(s) kept from runs r8, r7,
  r6, r5: 802 from r8, 151 from r7, 160 from r6, 140 from r5, 12 the merged
  run's plan decides`): a verdict two runs hold alike, as every job's copy of
  the earlier rounds' verdicts, counts for the first run named.
- **The next pass.** The restored runs lend their verdicts on a later
  commit: each job keeps every verdict nothing changed could move, from
  whichever earlier job's run holds it, and the shares are of what is left.
  Keep the merged run's parts with it: a merged run's `tests affected` is
  answered from them.
- **Copies of one run.** Up to 4.29.5, `supercov mutants --shard` on a
  checkout unchanged since the runs it restored saved into the latest of them,
  so every job of a second round wrote its share into a copy of one run. The
  copies share an id, one directory of `.supercov/runs` holds one of them, and
  `supercov merge` refuses a run named more than once, saying so. Run that
  round again: each job now saves into a run of its own. In 4.29.6 that run
  was measured again, which cost each job the suite's time and could drop a
  verdict where a test failed in one of the measurements.

### Runners without a driver

A test runner Supercov has no adapter for, such as tape, records no test: a
strong pass over its run is refused with exit `2`, naming the runner and how
many mutants its command ran. Tests Supercov records under a runner no strong
driver runs keep the mutants list and the weak report; in a strong pass their
mutants are `error`, which the score leaves out, counted in a line per runner
(`12 mutant(s) not run: no strong driver for pytest`), never survivors.

### Join and export other tools' reports

```sh supercov-example
npx supercov runs latest mutants --stryker reports/mutation/mutation.json
npx supercov runs latest mutants --stryker reports/mutation
```

`--stryker` reads StrykerJS's `mutation.json` reports, or a directory of
them, and mutant's session results for Ruby (`.mutant/results`). Each mutant
then shows the other tool's status beside Supercov's, and the reply counts the
mutants each tool made alone. A mutant Supercov calls not reached, not
infected or equivalent that the other tool killed is a soundness violation:
it is listed, and the command exits `1`.

`mutants --format stryker` writes a run's mutants as StrykerJS's
`mutation.json` (the mutation testing report schema, version 2), which
mutation-testing-elements' HTML report, the Stryker Dashboard and CI
integrations read, and `mutants --format pit` as PIT's `mutations.xml`, which
PIT's report aggregator and Sonar's PIT plugin read. Each carries every
mutant's strong verdict where a pass gave one. These are file formats only:
they work for every language above, and are not Java or Kotlin support.

## The HTML reports

`npx supercov report` and `npx supercov runs latest report --format html`
show a run's mutation testing beside its coverage, for a run that listed
mutants:

- **The Mutation testing card** holds the weak score and the mutation score
  side by side, each with how many mutants it is over, every status and
  verdict counted, why the mutants without a verdict have none, each language
  where the run has several, and the mutants left alive by file, each opening
  its line.
- **The Files list** gets a Mutants column with each file's score, sortable,
  lowest first.
- **A file's source** marks each line with a mutant left alive. The line's
  evidence lists each mutant with what it changes, its weak status and reason,
  its verdict and why, and the test that killed it.

`--diff <base>` on either report keeps the mutants of a change, as
`runs latest mutants --diff` does; coverage is not narrowed. A run that listed
no mutants shows none of this.

## Languages and runners

| Language | Strong pass runs under |
| --- | --- |
| JavaScript, TypeScript | Vitest, Vitest Browser Mode, Playwright, Jest, `node:test`, Mocha, AVA, node-tap, `bun:test` |
| Python (preview) | pytest, `python -m unittest` |
| Rust | `cargo test` (libtest and doctests), cargo-nextest; wasm in Node, under WASI and in Chrome |
| Java, Kotlin | JUnit 5, JUnit 4 (through Vintage and under Ant's `<junit>`), Kotest, Spock, TestNG; Maven, Gradle, Ant |
| Go (preview) | `go test` |
| Ruby (preview) | RSpec, Minitest, test-unit, Cucumber |

### JavaScript and TypeScript

- **Vitest** runs many mutants at once in one resident Vitest: each test file
  can carry its own mutant, a mutant is switched on before the file's imports,
  and a mutant's test files run one after another, the likeliest killer first,
  stopping at its first failing test.
- **Playwright** carries each test's mutant with its requests, so one server
  serves different mutants to different workers. A mutant of a server's
  start-up runs with the server started again for it. A mutant in a
  module-level object or array literal (a translation table, a menu) runs
  against the tests that read its value, which a run measured with the
  mutants' probes records, not every test whose server loaded the module;
  the plan says how many were narrowed and by how much
  (`docs/design/strong-mutation.md`, Module-level values). A mutant whose
  code ran only for requests that named no test (a webhook a mock server
  sends while tests run) runs against the tests that were running then, not
  every test. A survivor whose code its candidates' servers ran, but which no
  test's request ran with its mutant in the pass (work in a route's
  `after()`, a worker process the first request started), is not trusted: it
  runs again alone, with the server started with it switched on, and the pass
  warns about it. A test runs without a mutant only after a failure, to confirm
  it; a failure before any such run passed on its server (a cold server)
  runs the mutant again once one has. StrykerJS has no
  Playwright runner, so Playwright's verdicts are checked by hand (see below).
  A test run with a mutant gets StrykerJS's time limit: its baseline (the
  longest of its measured run and its runs without a mutant in the pass)
  × 1.5 + 5 s, or its own timeout where that is shorter. Playwright's test
  timeout is set to it, and a `test.setTimeout()` or `test.slow()` in the
  test may shorten it but not lengthen it, so a click or an `expect` that
  waits for an element the mutant broke ends there, not at its own 30 s. A
  test that runs out of its limit makes its mutant a `timeout`, which counts
  as detected; the summary says how many, and against what limit. An attempt
  ends sooner, a `timeout` too, once it has stalled: its test's own code has
  run for longer than any run of the test that passed (baseline × 1.5 + 1 s)
  and waits on a Playwright call that cannot end by its own timeout before the
  limit, its pages quiet for a second with nothing left in them that could
  end the wait before the limit: no timer, animation frame or ending
  animation due before it, no request unanswered, no WebSocket open, no
  worker. An `expect` that gives up before the limit still fails the test, a
  kill; a page still sending requests runs on; and so does one that would
  poll again on a timer before the limit, to its limit, where a later poll
  may answer the wait (a timeout there says it `ran to its limit to
  confirm`, and the summary counts those). Nothing needs setting. Each timeout's verdict says what it waited on and
  where, for example `stopped at 2.5 s of its 6.5 s time limit, ..., waiting
  on Locator.click (#save) at tests/save.spec.ts:12:20`, and the summary says
  how much time ending them early saved. A worker is kept after a mutant
  fails its test, so the next test does not wait for a new worker and
  browser; a test that fails without any mutant still gets a new one.
  `--timeout-factor <x>` (or `SUPERCOV_STRONG_TIMEOUT_FACTOR`) changes the
  1.5: a suite whose tests' times vary widely between runs may want 2 or 3.
  A mutant tries its candidates cheapest to a kill first (a test's time over
  how likely it is to kill: one that infected the mutant counts as four
  times likelier than one that only reached it), and a slow test runs it only
  once its cheaper candidates let it live; a file's tests take one mutant
  each in turn, so mutants are decided one after another rather than all at
  the end. The progress lines name the tests with the most work left (how
  many mutants wait for each, and its time a run) and the time the round
  has left. A candidate in a project others start from (a login `setup`)
  never runs with a mutant: a mutant no other test kills is an `error` that
  says so.
- **A harness with slots.** A harness may run several `playwright test`
  processes at once (slots, each with its own server and database) and keep
  a spec file with a serial describe whole, in one slot. In a strong pass
  such a file runs in every slot whose command names spec files, and each
  slot takes its own mutants of it from a queue the pass's slots share, so one
  slot no longer runs all of them. Nothing in the harness or the command
  changes: Supercov's preload adds the file to each invocation's file filters
  before Playwright reads them, and leaves `--list` as it is. Each slot runs
  the file's tests once without a mutant before its first mutant there. A
  file it cannot spread (one that sets its serial mode in a helper of its
  own) is named in the progress lines, with what it costs (`"e2e/buy.spec.ts"
  has a serial describe, and 1 of the 6 invocations runs it: its 37 mutant(s)
  wait about 12m00s there, the others about 1m00s`).
- **Jest and Mocha** run in resident runners, as StrykerJS's do; `node:test`,
  AVA, node-tap, `bun:test` and Vitest Browser Mode run one test file per
  start of the command, with the mutant on from its start.
- Rust compiled to wasm and loaded by these tests is mutated too:
  the command builds the wasm with the mutants switched in, and each wasm
  instance, in Node or in a Playwright page, runs the test's mutant.
- Limits: wasm loaded in Vitest Browser Mode's pages is not measured, so its
  mutants are not planned. A function body StrykerJS empties where no switch
  can be placed is `error`.

### Python (preview)

The tests are collected once; each mutant then runs in a fork of that
process, its function switched to mutmut's copy, against its candidate tests
in collection order. A mutant whose code ran while the test modules loaded (a
decorator applied at import) runs in a fresh interpreter with the mutant on
from its start. `SUPERCOV_STRONG_WORKERS` sets how many forks run at once.

Limits: Windows, since the pass forks. A suite that runs unittest through its
own script (`TextTestRunner().run`) rather than `unittest.main` has its
mutants recorded `error`. mutmut's `do_not_mutate_patterns` and
`mutate_only_covered_lines` are not read.

### Rust

Every mutant of a pass is built in one switched copy, and each test runs by
its exact name in a process of its own; doctests run through `cargo test
--doc`. Under cargo-nextest each test runs through nextest with the project's
configuration. Each mutant is the program cargo-mutants' replacement text
makes, regrouping included (`a - b * c` with `*` made `+` is `(a - b) + c`).
A mutant the compiler refuses is `unviable`, as cargo-mutants reports it,
those no test reaches included; a mutant in code the compiler evaluates
(`const`, `static`, `const fn`) is built on its own.

- **Time limits** are cargo-mutants': a test's baseline × 5, at least 20 s.
  A test past its limit beside other tests runs again alone, after a run of
  it without the mutant sets its limit, so a busier machine makes no
  timeout.
- **Stopping**: `--budget` and a signal stop the pass at once, its test
  processes with it; nothing a stop cut short is judged, and each candidate a
  mutant already passed is kept for the next pass.
- **After an edit**, a Rust test keeps a verdict where nothing its own
  function can name changed, a trait implementation's methods counting
  where its type or trait is named; a doctest keeps one only where no code
  changed (`docs/design/rust-mutation.md`, After an edit).

Limits: tests built for wasm run against mutants in Node, under WASI and in
Chrome; tests run in Firefox, Safari or Edge have their mutants recorded
`error`. Value probes do not cover `&&`, `||`, struct fields and returns that
are not numbers or booleans: those mutants stay `unknown` where they ran, and
the strong pass decides them. Equivalent mutants are not detected for Rust:
an equivalent mutant survives, as under cargo-mutants, and is accepted in
`supercov.accepted.json` (Critical code and accepted mutants).

A merged run's mutants run under its parts' test command; parts that differ
only in the test names after `--` (`cargo test -- --exact a b`) give it
without those names, since the pass names each test it runs. A test the
measured run skipped (`#[ignore]`, unless the command asked for
`--ignored` or `--include-ignored`) is recorded `skipped`, not failed, and
is never a candidate.

### Java and Kotlin

The mutants are PIT's default mutators, made from source rather than bytecode
(`ConditionalsBoundary`, `NegateConditionals`, `Math`, `Increments`,
`InvertNegs`, `VoidMethodCall` and the return mutators), with PIT's default
filters applied in source terms (`docs/design/jvm-mutation.md`). `supercov
runs latest mutants --format pit` writes them as PIT's XML report, and
`supercov runs latest mutants pit <mutations.xml>` joins a PIT report with
the run's verdicts.

The build compiles one switched copy and starts its test JVM as it always
does (Maven's Surefire, Gradle's `test` task, Ant's `<junit>`,
`<junitlauncher>` or `<testng>`); the pass runs inside it, as PIT's minion
does, one test at a time by the name the measured run recorded.

- **Fresh classes for each mutant**: each mutant runs in a class loader that
  loads the project's classes afresh, so a cache or a static a test filled
  without the mutant does not hide it.
- **A test class's own work** (`@BeforeAll`, `@AfterAll`, a `@MethodSource`,
  a static initializer, JUnit 4's `@BeforeClass`, a Kotest spec's body) is
  credited to every test of its class: a mutant there runs against them.
- **Baselines**: each test runs twice without a mutant before any mutant
  runs against it; one that fails then is no candidate, and a kill is
  confirmed by running the killing test again without the mutant.
- **Time limits**: the longer of the test's measured time and its baseline,
  × 1.5, at least 1 s; a mutant past it waits for a second round with × 3
  and 10 s, past which it is a `timeout`. A mutant whose code loops past a
  million evaluations is stopped there.
- **Several JVMs** (`SUPERCOV_STRONG_JVMS`, by default half the machine's
  processors, at most eight) share the pass; each works in a directory of
  its own made of links to the project's files, so the files one JVM's test
  makes or deletes are not another's.
- **Stopping**: `--budget` stops the pass; each mutant decided is kept for
  the next pass.

Limits: a Kotest test runs through Kotest's own filter
(`kotest.filter.tests`); where its baseline does not run that way, it runs
with its whole spec, which is slower. Code kotlinc writes that has no source
of its own (inlined standard-library code, property accessors, the null
checks it adds) has no mutant here, where PIT mutates it: `mutants pit` names
each such PIT-only mutant's category. Under TestNG, which reports no class
boundary, work outside every test is credited to every test of the run. On
Windows without the right to make links, the JVMs share one directory and
say so.

### Go (preview)

Each package's test binary is built once (`go test -c`); each mutant runs the
packages that hold a candidate, whole, as `go test` runs them, with the
command's own flags (`-tags`, `-race`, `-short` and the rest, read from the
`go test` the command runs, even behind `make test` or a script). A mutant the
type checker or `go vet`'s test-time checks refuse is `unviable`; Gremlins
counts such a mutant killed. A constant declaration's mutant is built on its
own.

Limits: floating-point operations have no value probe. On Windows, the
command's flags are read only from a `go test` the command names itself.

### Ruby (preview)

The pass runs once the suite has loaded: one fork of the loaded process per
mutant, its candidate tests in the suite's order, through RSpec's own
`run_specs`, Minitest's run, test-unit's mediator and Cucumber's filters, so
suite hooks run with the mutant on.

Limits: the test-unit and Cucumber drivers have run under test-unit 3.7 and
Cucumber 12 only. A method holding syntax the port does not convert yet (a
heredoc, a constant pattern) is skipped, and named. A switched file loaded
before Supercov's runtime (a gemspec that requires the library under `bundle
exec`) has its mutants recorded `error`.

## Compared with the established tools

Measured on public projects pinned to a tag, every mutant of each tool joined
by its id. A soundness violation is a mutant one tool kills and the other
calls survived; every disagreement was applied by hand and the suite run as
written. Each tool's settings and every disagreement are in `docs/design/`.

### JavaScript and TypeScript, against StrykerJS 10.1.1

Vitest, against StrykerJS's full kill matrix, whole wall time on 16-CPU Linux
machines (AWS c8a and m8a.4xlarge), 2026-11-23: StrykerJS's set-up, dry run
and mutants against Supercov's measured run and strong pass. tRPC does not fit
one 10-minute job, so both tools ran in 8 jobs, each repeating its dry run or
measured run:

| Project | StrykerJS | Supercov (measure + strong) | Without what each job repeats | Disagreements |
| --- | ---: | ---: | --- | --- |
| hono (5,111 tests) | 258.5 s | 177.1 s (8.6 + 168.5) | the same, one job | 10 |
| tRPC (1,032 tests) | 2,782.2 s | 2,018.1 s (181.3 + 1,836.8) | 2,125.3 s against 1,752.8 s | 2 |

On defu, scule, cookie-es, pathe and ufo the verdicts agree, mutant for
mutant. hono's 10 disagreements are kills of StrykerJS's covering subset (a
test run without the tests it depends on, a fake clock left running) that the
suite as written does not make; tRPC's 2 are a flaky test and a mutant
StrykerJS calls Timeout for passing its hit limit while every test passes.

The other runners, against StrykerJS with `disableBail` (every covering test
against every mutant), one project per 16-CPU machine (AWS c8a and
m8a.4xlarge), both tools with 15 workers, 2026-11-22. Times in seconds:

| Project | Runner | Mutants joined | Disagreements | StrykerJS | Supercov (measure + strong) |
| --- | --- | ---: | ---: | ---: | ---: |
| bytes.js | Mocha | 146 | 0 | 3.0 | 1.1 + 0.4 |
| negotiator | Mocha | 544 | 0 | 3.4 | 1.2 + 1.0 |
| pathval | Mocha (ES modules) | 123 | 13 | 1.6 | 0.9 + 1.3 |
| deep-eql | Mocha (ES modules) | StrykerJS cannot run it | | | 1.1 + 5.9 |
| ms | Jest | 227 | 0 | 4.6 | 1.8 + 3.2 |
| cron-parser | Jest | 460 | 1 (error) | 31.8 | 2.4 + 22.4 |
| classnames | `node:test` | 161 | 0 | 7.8 | 0.6 + 2.8 |
| camelcase | `node:test` | 234 | 0 | 10.1 | 1.2 + 3.9 |
| dot-prop | AVA | 590 | 0 | 46.4 | 2.7 + 26.7 |
| p-map | AVA | 512 | 1 | 354.6 | 25.9 + 118.4 |
| ini | node-tap | 413 | 0 (28 StrykerJS errors) | 13.6 | 1.6 + 6.5 |
| remix-auth | `bun:test` | 625 | 0 | 7.3 | 0.4 + 1.9 |
| mini-throttle | Vitest Browser Mode | 77 | 1 (error) | 32.4 | 9.0 + 28.0 |

Playwright, which StrykerJS does not run, on playwright-test-coverage (a Vite
app served by `npm run dev`, three tests): its 9 mutants run are 7 `timeout`
and 2 `survived`, and each applied by hand to the source, with `npx playwright
test` as written, agrees: the 7 fail, waiting out Playwright's 30 seconds, and
the 2 pass.

pathval's 13 are kills StrykerJS makes because it runs every mutant in one
process: once a mutant left a polluted global behind, one test failed for
every mutant after it; each applied by hand survives `npx mocha`. The two
`error`s are function bodies where no switch could be placed. p-map's one is a
kill by a test that fails run alone and passes in the whole suite. Where
StrykerJS has no runner of its own (`node:test`, AVA, Bun) it runs the whole
command per mutant, and Supercov is 2.5 to 3.5 times faster.

### Python (preview), against mutmut 3.8.1

Against mutmut's full kill matrix (every test against every mutant), 5,438
mutants, on 16-CPU Linux machines (AWS c8a and m8a.4xlarge), 2026-11-22:
every mutant the matrix detects,
Supercov detects. The five Supercov detects alone each fail the suite as
written when applied by hand: mutmut does not switch on a mutant in a module
run with `-m`, or in a decorator applied as the test modules load.

Wall time with mutmut at one child per CPU and Supercov's forks one per CPU,
each tool on a machine of its own, 4-CPU Linux machines (AWS m8a.xlarge),
2026-11-22, parse's pytest-cov off for both:

| Project | Mutants | mutmut | Supercov measure + strong |
| --- | ---: | ---: | ---: |
| python-slugify 9.2.0 | 1,124 | 23.6 s | 1.2 + 17.9 s |
| cachetools 8.0.2 | 1,660 | 44.8 s | 1.6 + 43.6 s |
| parse 1.22.4 | 1,306 | 56.5 s | 1.4 + 12.6 s |
| humanize 4.17.0 | 1,348 | 152.3 s | 4.6 + 19.8 s |

### Rust, against cargo-mutants 27.1.0

Eleven public crates pinned to a tag (`benchmarks/mutation/rust/`), every
mutant of each joined by id: cargo-mutants runs the package's whole suite
for each mutant; Supercov measures the suite and runs its strong pass. Both
on GitHub's `ubuntu-latest` (4 vCPUs), two mutants at a time, 2026-11-27:
cargo-mutants in shards of a crate's mutants, Supercov's `supercov mutants
--shard k/n` (measuring included) with its shards merged. Times are summed
over each tool's jobs (wall / CPU seconds), each job repeating its own build
and baseline, or its own measured run.

| Crate | Shape | Mutants joined | Agree | Soundness violations | Other disagreements | cargo-mutants | Supercov |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: |
| strsim 0.11.1 | small library, doctests | 363 | 363 | 0 | 0 | 195 / 529 s | 97 / 201 s |
| hex 0.4.3 | deny-by-default lints | 104 | 102 | 0 | 2 lint | 131 / 434 s | 88 / 216 s |
| semver 1.0.27 | `const` code, integration tests | 365 | 363 | 0 | 2 lint | 163 / 405 s | 136 / 185 s |
| itoa 1.0.15 | generic over integer types | 46 | 46 | 0 | 0 | 8 / 17 s | 2 / 2 s |
| smallvec 1.15.1 | unsafe collection | 458 | 458 | 0 | 0 | 301 / 876 s | 181 / 368 s |
| unicode-width 0.2.1 | generated tables | 383 | 383 | 0 | 0 | 576 / 1,598 s | 218 / 410 s |
| bitflags 2.9.1 | macro-generated code | 107 | 107 | 0 | 0 | 267 / 577 s | 134 / 309 s |
| anyhow 1.0.98 | build script, `deny(dead_code)` | 201 | 192 | 0 | 9 lint | 218 / 796 s | 74 / 238 s |
| fastrand 2.3.0 | under cargo-nextest | 211 | 203 | 0 | 8 lint | 501 / 1,555 s | 828 / 616 s |
| regex-syntax 0.8.5 | workspace member, parser | 1,301 | 1,301 | 0 | 0 | 13,679 / 50,595 s (39 jobs) | 3,611 / 8,964 s |
| base64 0.22.1 | randomized round trips | 305 of 619 | 305 | 0 | 0 | 15,249 / 50,394 s (48 jobs) | 5,684 / 17,390 s (16 jobs) |

No mutant one tool detects is one the other lets live. The other
disagreements, each applied by hand:

- **Lints** (hex, semver, anyhow, fastrand): cargo-mutants' build fails on a lint the
  crate denies (`arithmetic_overflow` after `+` made `*`, anyhow's
  `#![deny(dead_code, unused_imports)]` once a body is replaced); Supercov
  caps lints as a measured run does, builds the mutant, and its tests kill
  it, or none reaches it.
- **`vec!` in a `no_std` crate** (fastrand, 2): a body replaced by `vec![]`
  where no `vec!` is in scope does not compile, an error rustc gives no code.
  4.29.18 reads that error as the mutant's own and calls them `unviable`, as
  cargo-mutants does: in the fastrand row, measured with it, they agree.
- **Time limits**: a test's limit is cargo-mutants' rule applied to that test,
  5 × its baseline and at least 20 s, where cargo-mutants applies it to the
  whole suite. A mutant of fastrand that makes rejection sampling loop far
  longer ended its one test past 20 s in one run, within 5 × the suite: a
  `timeout` here, missed there; in another run it ended in time, as there.

Mutants the table does not join were left by a job's time limit (6
minutes of mutants per job, to keep each job under ten): base64's tests
are slow under mutation, so cargo-mutants' 48 jobs decided 509 of its 619
mutants and Supercov's 16 left 204 of those without a verdict; under
fastrand, which runs each test through nextest, Supercov left 21. Every
mutant both tools decided is in the table, and none of them is a soundness
violation; joining the rest needs about twice the jobs. Supercov's wall time
is the larger on fastrand: nextest starts a process per test, against
cargo-mutants' one per mutant; its CPU time is two fifths. cargo-mutants
runs under a 10 GiB address-space cap: one of base64's mutants made a test
allocate without end, which took the runner, and every outcome of its job,
down twice. Each earlier gap the benchmark found is fixed and held by a test (`CHANGELOG.md`,
4.29.18): a probe that failed `u128` tests, `const fn` mutants read as never
run, operator mutants that regroup their expression, unviable mutants no
test reaches.

### Java and Kotlin, against PIT 1.17.1

Eight public projects pinned to a tag (`benchmarks/mutation-jvm/`), every
mutant joined by file, line, mutator and PIT's description (`supercov runs
latest mutants pit`): PIT as a team runs it (each mutant until its first
kill) and Supercov's measured run and strong pass, over the same source files
in shards of each project, on GitHub's `ubuntu-latest` (4 vCPUs), 4 PIT
threads and 4 JVMs for the strong pass, 2026-11-27
(`.github/workflows/mutation-jvm.yml`). Times are summed over each tool's
jobs, in seconds; each Supercov job measures the whole suite once.

| Project | Shape | PIT mutants | Supercov mutants | Joined | Agree | Soundness violations | PIT | Supercov measured + strong |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| commons-cli 1.9.0 | Java, JUnit 5, Maven | 882 | 882 | 881 | 879 | 1 | 98 | 29 + 186 |
| commons-csv 1.12.0 | Java, JUnit 5, Maven | 720 | 719 | 717 | 717 | 0 | 147 | 56 + 194 |
| typetools 0.6.3 | Java, TestNG, Maven | 216 | 216 | 216 | 213 | 3 | 28 | 14 + 49 |
| java-diff-utils 4.12 | Java, JUnit 5, Maven, multi-module | 652 | 660 | 645 | 645 | 0 | 166 | 482 + 595 |
| Apache Ant 1.10.15 `util` | Java, JUnit 4 under Ant's `<junit>` | 2,083 | 2,056 | 2,039 | 2,031 | 8 | 258 | 63 + 245 |
| picnic 0.5.0 | Kotlin, JUnit 4, Gradle | 632 | 342 | 318 | 317 | 1 | 56 | 39 + 87 |
| kotlinpoet 1.14.2 | Kotlin, JUnit 4, Gradle, 12 shards | 2,114 | 1,213 | 1,121 | 1,117 | 4 | 1,394 | 968 + 2,235 |
| aedile 3.0.4 | Kotlin, Kotest, Gradle | — | 28 | — | — | — | — | 117 + 324 |

PIT does not run Kotest, so aedile has no join: all 28 of its mutants are
killed. Every soundness violation was checked, the mutant applied to the
source and the project's own suite run with it:

- **Supercov killed, PIT survived or no coverage (13):** code that runs only
  as a class's statics are set up (typetools' `TypeResolver` initializer,
  3; picnic's `textBorder.kt:5`; Ant's cached parser factories and package
  list, `JAXPUtils` and `JavaEnvUtils`, 5; Ant's `SymbolicLinkUtils.java:55`,
  read into a test class's static field): PIT switches a mutant into a JVM
  whose statics are already set, so the mutant never runs there, where
  Supercov loads the project's classes afresh for each mutant. kotlinpoet's
  `CodeWriter.kt:41` and `Taggable.kt:73` are in `inline` functions, whose
  code runs only copied into callers compiled before PIT mutates anything;
  `Taggable.kt:28` and `:63` are reached only by a JUnit 4 parameterized
  test whose names carry identity hashes, which PIT cannot select again.
- **Supercov survived, PIT killed or timed out (5):** with the mutant applied,
  the project's whole suite passes: commons-cli's `OptionBuilder.java:141`
  and Ant's `FileUtils.java:1204` are killed in PIT's single JVM by state an
  earlier test left; Ant's `VectorSet.java:228` makes `retainAll` slower
  (its speed test passes in 5.7 s alone), a timeout in PIT's loaded JVMs and
  within Supercov's second-round limit.

Mutants one tool lists and the other does not: on the Java projects, a few
(PIT's loop filters match some loops of a source shape and not others; a
lambda whose return type the source does not show). On Kotlin, PIT mutates
code kotlinc writes that has no source of its own (property accessors, null
checks it adds, inlined standard-library code, `when` subject comparisons):
`mutants pit` names each such mutant's category, and leaves 52 of
kotlinpoet's and 11 of picnic's as `other`. Supercov's own extra Kotlin
mutants include operators on a type the source does not show that the
compiler refuses (`unviable`). Details in `docs/design/jvm-mutation.md`.

### Go (preview), against Gremlins 0.6.0

Five modules at their tags, Gremlins with every test per mutant
(`--integration`, `-count=1`), each tool on its own machine of the same type.
Supercov's time is the whole of `supercov mutants -- go test ./...`, its
measured run and builds included. 16 CPUs: AWS c8a.4xlarge, 2026-11-22. 4
CPUs: GitHub's `ubuntu-latest`, 2026-11-22, measured before a change that
shortened Supercov's retries:

| Module | Mutants | Killed (Gremlins / Supercov) | Unviable | 16 CPUs: Gremlins / Supercov | 4 CPUs: Gremlins / Supercov |
| --- | ---: | ---: | ---: | ---: | ---: |
| go-humanize 1.0.1 | 332 | 234 / 211 | 84 | 15.5 / 12.3 s | 61.2 / 24.7 s |
| btree 1.1.3 | 367 | 140 / 134 | 21 | 11.3 / 11.4 s | 41.1 / 29.0 s |
| golang-lru 2.0.7 | 165 | 120 / 104 | 11 | 31.7 / 17.9 s | 165.3 / 59.3 s |
| uuid 1.6.0 | 205 | 130 / 145 | 1 | 15.3 / 13.6 s | 90.0 / 43.7 s |
| cast 1.7.1 | 190 | 129 / 122 | 7 | 9.4 / 8.8 s | 59.4 / 22.9 s |

Gremlins counts mutants that do not compile as killed; Supercov counts them
unviable. Supercov kills 16 mutants of go-humanize's and 16 of uuid's constant
declarations, which Gremlins never runs. Gremlins runs one `go test` per
mutant; Supercov builds each package's test binary once, and its measured run
is most of its time on 16 CPUs. The remaining differences are a nested module
and a `//go:build ignore` file Gremlins mutates and `go test ./...` never
builds, time-dependent kills of clock arithmetic, and a flaky Example; none is
a soundness violation.

### Ruby (preview), against mutant 0.17.1

On 16-CPU Linux machines (AWS c8a and m8a.4xlarge, Ruby 3.4.13, 16 jobs for
both tools), 2026-11-22, mutant with every test a candidate for every subject:

| Project | mutant's mutants | Joined | Disagreements | mutant | Supercov measure + strong |
| --- | ---: | ---: | ---: | ---: | ---: |
| dry-inflector 1.3.1 (RSpec) | 2,061 | 2,061 | 0 | 30.6 s | 1.6 + 17.8 s |
| dotenv 3.2.0 (RSpec) | 1,345 | 1,345 | 0 | 21.0 s | 1.0 + 18.2 s |
| erubi 1.13.1 (Minitest) | 1,571 | 1,571 | 0 | 14.2 s | 0.4 + 1.5 s |

mutant has no test-unit or Cucumber integration; on ostruct 0.6.3 (test-unit,
824 mutants run) and json_test_data 1.2.0 (Cucumber, 1,313) every mutant the
pass ran was run again by hand with the whole suite, with no disagreement.

## Cost, and turning it off

Mutation testing costs nothing until it is asked for:

- A plain run (`supercov -- <tests>`) places no probe and lists no mutant.
- The first `runs <id> mutants` lists the run's mutants from its sources and
  keeps the list beside the run. A file changed since the run is left out
  until it is measured again.
- The HTML reports and the run summary show mutants only for a run that
  listed them.

What costs time:

- **The value probes** (`SUPERCOV_MUTANTS=1`, which `supercov mutants` sets)
  record the values that decide a mutant's infection, so more mutants are
  decided without running. Over a measured run without them:
  - Python: 0.98 to 1.15 times (16-CPU AWS c8a and m8a.4xlarge, 2026-11-22);
  - Rust: 4 to 11% more, warm builds (16-CPU AWS c8a and m8a.4xlarge,
    2026-11-22);
  - Go: within the noise, with up to 0.11 s for the type checker's facts
    after an edit (16-CPU AWS c8a.4xlarge, 2026-11-22);
  - Ruby: 1.02 to 1.16 times the whole run (16-CPU AWS c8a and m8a.4xlarge,
    2026-11-22);
  - JavaScript and TypeScript: 1.4 to 1.7% on hono and 2.6% on ufo (Vitest),
    3.6% on cron-parser (Jest) and 4% on negotiator (Mocha), medians of 5
    (16-CPU AWS c8a and m8a.4xlarge, 2026-11-22).
- **The strong pass** reruns tests, so it takes about as long as the
  established tool on the same project: the tables above. Use `--diff` on a
  pull request to run only the change's mutants, and `--shard` to split a
  large pass across jobs.

To turn it off, measure without `SUPERCOV_MUTANTS` and do not run `supercov
mutants` or `mutants test`; a run's coverage is the same either way.
`npx supercov runs clean` removes stored runs with their mutants and verdicts.

See [CLI reference](cli.md#mutants) for every option.
