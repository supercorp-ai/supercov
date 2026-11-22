# Mutation testing

Coverage says which code the tests ran. It does not say whether they would
notice that code doing the wrong thing. Mutation testing asks that directly:
it makes small changes to the source, one at a time (`a < b` becomes
`a <= b`, a `return` gives `0`, a call is removed), and checks whether a test
fails. Each change is a **mutant**. A mutant a test fails on is **killed**; one
every test passes with **survived**, and points at a check no test makes.

Supercov does this for every language and runner it measures, with the
mutants of each language's established tool, so its counts and verdicts can be
compared with that tool's.

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

| Status | Meaning |
| --- | --- |
| `killed` | A test failed with the mutant in place |
| `survived` | Every test that could kill it passed |
| `timeout` | A test ran past its time limit with the mutant in place |
| `no-coverage` | No test ran the mutated code |
| `unviable` | The mutant does not compile (Rust, Go, Java and Kotlin); counted apart and left out of the score |
| `error` | The pass could not give it a verdict, with the reason |

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

Each of these shortcuts is exact. Where the run cannot prove a test is
irrelevant, the test runs.

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
the strong pass. While the checkout is unchanged, the next `supercov mutants`
reads that run again and runs only what has no verdict yet. Without `--` it
uses the latest run's command.

The steps can also be taken one at a time:

```sh supercov-example
npx supercov -- npm test                   # a plain run: no cost for mutants
npx supercov runs latest mutants           # weak mutation, from the run
npx supercov runs latest mutants test      # strong mutation, saved into the run
npx supercov runs latest mutants --json    # each mutant's strong verdict, and the mutation score
```

`mutants test` and `supercov mutants` print the counts by strong status and
the mutation score. The text listings of `runs latest mutants` show each
mutant's weak status and its reason; after a strong pass, `--json` gives each
mutant's verdict as `strong` (`status`, `killedBy`, `tests`, `reason`) and the
summary's `mutationScore`.

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

The table of files puts first the files with the most mutants a test reaches
and does not infect, which only this report shows. A listed mutant shows its
change as `original → replacement`, or by the part it changes where the code
is long (`….slice(0, maxUsers) → …`). `--kind`, `--runner`, `--tag`,
`--title` and `--test-file` read the verdicts of those tests alone.

### Gate a pull request

`--diff <base>` keeps the mutants on lines the change added or modified since
it left `<base>`, and lists first the ones its tests leave alive:

```sh supercov-example
npx supercov runs latest mutants --diff origin/main
npx supercov runs latest mutants --diff origin/main --format markdown --output mutants.md
npx supercov runs latest mutants --diff origin/main --fail-on not-infected,not-reached
npx supercov mutants --diff origin/main --fail-on survived
```

- `--format markdown` writes a table for a pull request's comment: the counts,
  then up to 25 surviving mutants with where they are, what they change and
  why, and the command that lists the rest.
- `--fail-on` exits `1` when a mutant asked about has one of the statuses
  given. On `runs latest mutants` they are weak statuses, from the run alone;
  on `mutants` and `mutants test` they are `survived`, `timeout`,
  `no-coverage` and `error`, after the strong pass.
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
- `--shard k/n` runs one of n shares of the mutants, for parallel jobs.
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

### Join another tool's report

A team that already runs the established tool can read Supercov's reasons
beside that tool's verdicts:

```sh supercov-example
npx supercov runs latest mutants --stryker reports/mutation/mutation.json
npx supercov runs latest mutants --stryker reports/mutation
```

`--stryker` reads StrykerJS's `mutation.json` reports, or a directory of
them, and mutant's session results for Ruby (`.mutant/results`). Each mutant
then shows the other tool's status beside Supercov's, and the reply counts the
mutants each tool made alone. A mutant Supercov calls not reached, not
infected or equivalent that the other tool killed is a soundness violation:
it is listed, and the command exits `1`. For Java and Kotlin,
`supercov runs latest mutants pit <mutations.xml>` joins PIT's report the same
way and names the PIT mutants that come from code the compiler wrote.

## The HTML report

`npx supercov report` shows a run's mutants where it shows its coverage, for a
run that listed them:

- **The Mutants card**, beside Coverage and Assertions, holds the weak
  mutation score and the mutants by status: not infected (with how many sit
  at a clock or random boundary), not reached, infected, unknown and
  equivalent. The first two are the ones the tests leave alive and are
  highlighted when there are any.
- **The file list** gets a Mutants column: each file's not-infected and
  not-reached mutants, sortable.
- **A file's source** marks with `✱` each line that holds such a mutant.
  Selecting the line lists each one with its status, its mutator, its change
  and its reason.

A run that listed no mutants shows none of this. The report shows the weak
verdicts; strong verdicts are read with `runs latest mutants --json`.

## Languages and runners

Each language uses its established tool's mutants, by the same rules, and its
reports name that set (`mutantSet`). Every strong verdict was compared with
that tool's on public projects: a mutant one tool kills and the other calls
survived is a soundness violation, and every disagreement was applied by hand
and the suite run as written.

| Language | Mutants of | Weak report | Strong pass |
| --- | --- | --- | --- |
| JavaScript, TypeScript | StrykerJS 10.1.1 | Yes | Vitest, Vitest Browser Mode, Playwright, Jest, `node:test`, Mocha, AVA, node-tap, `bun:test` |
| Python | mutmut 3.8.1 | Yes | pytest, `python -m unittest` |
| Rust | cargo-mutants 27.1.0 | Yes | `cargo test` (libtest and doctests), cargo-nextest, wasm in Node, WASI and Chrome |
| Go | Gremlins 0.6.0, every mutator | Yes | `go test` |
| Ruby | mutant 0.17.1, light operators | Yes | RSpec, Minitest, test-unit, Cucumber |
| Java, Kotlin | PIT 1.17.1, `DEFAULTS` | Yes | JUnit 5, JUnit 4 (through Vintage), Kotest, Spock and TestNG under Maven or Gradle; Ant's `<junit>`, `<junitlauncher>` and `<testng>` |

### JavaScript and TypeScript

- **Vitest** runs many mutants at once in one resident Vitest: each test file
  can carry its own mutant, a mutant is switched on before the file's imports,
  and it stops at its first failing test.
- **Playwright** carries each test's mutant with its requests, so one server
  serves different mutants to different workers. A mutant of a server's
  start-up runs with the server started again for it. StrykerJS has no
  Playwright runner, so these verdicts have no reference tool to be compared
  with.
- **Jest and CommonJS Mocha** run in resident processes, as StrykerJS's
  runners do; ES module Mocha suites, `node:test`, AVA, node-tap, `bun:test`
  and Vitest Browser Mode run one test file per start of the command, with the
  mutant on from its start.
- Rust compiled to wasm and loaded by these tests is mutated too: the command
  builds the wasm with the mutants switched in, and each wasm instance, in
  Node or in a Playwright page, runs the test's mutant.
- Limits: wasm loaded in Vitest Browser Mode's pages is not measured, so its
  mutants are not planned.
  A function body StrykerJS empties where no switch can be placed is `error`.
  A runner with no driver (another Node-based runner) is refused rather than
  reported as a score of nothing killed.

### Python

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
configuration. A mutant the compiler refuses is `unviable`, as cargo-mutants
reports it; a mutant in code the compiler evaluates (`const`, `static`,
`const fn`) is built on its own.

Limits: tests built for wasm run against mutants in Node, under WASI and in
Chrome; tests run in Firefox, Safari or Edge have their mutants recorded
`error`. A crate loaded by JavaScript tests is mutated through them (see
JavaScript and TypeScript above). Value probes do not cover `&&`, `||`, struct fields and returns that
are not numbers or booleans: those mutants stay `unknown` where they ran, and
the strong pass decides them.

### Go

Each package's test binary is built once (`go test -c`); each mutant runs the
packages that hold a candidate, whole, as `go test` runs them, with the
command's own flags (`-tags`, `-race`, `-short` and the rest, read from the
`go test` the command runs, even behind `make test` or a script). A mutant the
type checker or `go vet`'s test-time checks refuse is `unviable`; Gremlins
counts such a mutant killed. A constant declaration's mutant is built on its
own.

Limits: floating-point operations have no value probe. On Windows, the
command's flags are read only from a `go test` the command names itself.

### Ruby

The pass runs once the suite has loaded: one fork of the loaded process per
mutant, its candidate tests in the suite's order, through RSpec's own
`run_specs`, Minitest's run, test-unit's mediator and Cucumber's filters, so
suite hooks run with the mutant on.

Limits: the test-unit and Cucumber drivers have run under test-unit 3.7 and
Cucumber 12 only. A method holding syntax the port does not convert yet (a
heredoc, a constant pattern) is skipped, and named. A switched file loaded
before Supercov's runtime (a gemspec that requires the library under `bundle
exec`) has its mutants recorded `error`.

### Java and Kotlin

The build compiles the switched copy once and starts its test JVM as it always
does; the pass runs inside it, one test at a time by the name the measured run
recorded, each mutant in a class loader of its own. `SUPERCOV_STRONG_JVMS`
sets how many JVMs run mutants.

Limits: PIT mutates bytecode and Supercov mutates source, so Kotlin code
kotlinc writes itself (a property's generated getter, a range loop's bounds
check, the null checks of `?.`, `?:` and `!!`) has PIT mutants and no
Supercov mutant; `mutants pit` names each.

## Compared with the established tools

Measured on public projects pinned to a tag, every mutant of each tool joined
by its id. Supercov's time is its measured run plus its strong pass; each
tool's settings and every disagreement are in `docs/design/`.

### JavaScript and TypeScript, against StrykerJS 10.1.1

On 16-CPU CI machines (AWS c8a and m8a.4xlarge), one project a machine, both
tools with 15 workers, 2026-11-22. StrykerJS ran with `disableBail`, so every
covering test ran against every mutant. Times in seconds:

| Project | Runner | Mutants joined | Disagreements | StrykerJS | Supercov (measure + strong) |
| --- | --- | ---: | ---: | ---: | ---: |
| bytes.js | Mocha | 146 | 0 | 3.0 | 1.1 + 0.4 |
| negotiator | Mocha | 544 | 0 | 3.4 | 1.2 + 1.0 |
| ms | Jest | 227 | 0 | 4.6 | 1.8 + 3.2 |
| cron-parser | Jest | 460 | 1 (error) | 31.8 | 2.4 + 22.4 |
| classnames | `node:test` | 161 | 0 | 7.8 | 0.6 + 2.8 |
| camelcase | `node:test` | 234 | 0 | 10.1 | 1.2 + 3.9 |
| dot-prop | AVA | 590 | 0 | 46.4 | 2.7 + 26.7 |
| p-map | AVA | 512 | 1 | 354.6 | 25.9 + 118.4 |
| ini | node-tap | 413 | 0 (28 StrykerJS errors) | 13.6 | 1.6 + 6.5 |
| remix-auth | `bun:test` | 625 | 0 | 7.3 | 0.4 + 1.9 |
| mini-throttle | Vitest Browser Mode | 77 | 1 (error) | 32.4 | 9.0 + 28.0 |

The two `error`s are function bodies where no switch could be placed. p-map's
one disagreement is a kill by a test that fails run alone and passes in the
whole suite. Where StrykerJS has no runner of its own (`node:test`, AVA, Bun)
it runs the whole command per mutant, and Supercov is 2.5 to 3.5 times faster.

Vitest, on 16-vCPU AWS machines (c8a.4xlarge, AMD EPYC 9R45, the working
directory on a RAM disk), 2026-11-22, time per mutant run against StrykerJS's
mutant phase alone:

| Project | Supercov, ms a mutant | StrykerJS, ms a mutant | Disagreements with StrykerJS's full kill matrix |
| --- | ---: | ---: | --- |
| defu, scule, cookie-es, pathe, ufo | 14-25 | 12-17 | 0 |
| hono (5,111 tests) | 69 | 51 | 10, each a kill of StrykerJS's covering subset (a test run without the tests it depends on, a fake clock left running) that the suite as written does not make |
| tRPC (1,032 tests) | 1,173 | 755 | 2: a flaky test, and a mutant StrykerJS calls Timeout for passing its hit limit while every test passes |

On Vitest, StrykerJS is faster per mutant today: it runs a mutant's test files
one after another and stops at the first that fails, where Supercov's pass ran
them in batches. TBD(Vitest per-mutant times with the continuous pass on
hono, tRPC and the five libraries, machine and date).

### Python, against mutmut 3.8.1

On 16-CPU CI machines, 2026-11-22, mutmut with one child per CPU and
Supercov's forks one per CPU, against mutmut's full kill matrix (every test
against every mutant):

| Project | Mutants | Matrix detects, Supercov not | Supercov detects, matrix not | mutmut | Supercov measure + strong |
| --- | ---: | ---: | ---: | ---: | ---: |
| python-slugify 9.2.0 | 1,124 | 0 | 2 | 10.4 s | 1.3 + 9.7 s |
| cachetools 8.0.2 (pytest) | 1,660 | 0 | 3 | 27.0 s | 1.5 + 18.4 s |
| cachetools 8.0.2 (unittest) | 1,660 | 0 | 3 | — | 1.0 + 13.3 s |
| parse 1.22.4 | 1,306 | 0 | 0 | 269.2 s | 1.4 + 14.1 s |
| humanize 4.17.0 | 1,348 | 0 | 0 | 65.8 s | 5.2 + 14.6 s |

The five Supercov detects and the matrix does not each fail the suite as
written when applied by hand: mutmut does not switch on a mutant in a module
run with `-m`, or in a decorator applied as the test modules load. parse's
mutmut time includes the coverage report its configuration has pytest-cov
build in every worker, which a strong pass turns off. TBD(parse's mutmut time
with pytest-cov off).

### Rust, against cargo-mutants 27.1.0

On 16-CPU machines, one a job, 2026-11-22, cargo-mutants with `-j 8` in
shards, Supercov's strong pass with its default 8 workers:

| Crate | Mutants | Agree | Soundness violations | Viability disagreements | cargo-mutants (shards summed) | Supercov strong pass |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| strsim-rs 0.11.1 | 363 | 363 | 0 | 0 | 60.3 s | 15.7 s |
| rust-hex 0.4.3 | 104 | 102 | 0 | 2 | 74.2 s | 22.5 s |
| semver 1.0.27 | 365 | 362 | 0 | 3 | 43.0 s | 15.8 s |

The five viability disagreements are mutants cargo-mutants' build refuses on a
deny-by-default lint, which Supercov caps as a measured run does (the mutant
then builds and is killed), and one in a `const` no test reaches, which
cargo-mutants' build refuses and Supercov records as no coverage. None is a mutant
one tool kills and the other calls survived.

### Go, against Gremlins 0.6.0

Five modules at their tags, Gremlins with every test per mutant
(`--integration`, `-count=1`), each tool on its own machine of the same type,
2026-11-22: 16 CPUs (AWS c8a.4xlarge) and 4 CPUs (GitHub's `ubuntu-latest`).
Supercov's time is the whole of `supercov mutants -- go test ./...`, its
measured run and builds included:

| Module | Mutants | Killed (Gremlins / Supercov) | Unviable | 16 CPUs: Gremlins / Supercov | 4 CPUs: Gremlins / Supercov |
| --- | ---: | ---: | ---: | ---: | ---: |
| go-humanize 1.0.1 | 332 | 234 / 211 | 84 | 14.6 / 19.1 s | 61.2 / 24.7 s |
| btree 1.1.3 | 367 | 140 / 134 | 21 | 11.1 / 10.2 s | 41.1 / 29.0 s |
| golang-lru 2.0.7 | 165 | 119 / 107 | 11 | 31.9 / 21.2 s | 165.3 / 59.3 s |
| uuid 1.6.0 | 205 | 127 / 143 | 1 | 15.0 / 21.0 s | 90.0 / 43.7 s |
| cast 1.7.1 | 190 | 129 / 122 | 7 | 9.0 / 8.4 s | 59.4 / 22.9 s |

Gremlins counts mutants that do not compile as killed; Supercov counts them
unviable. Supercov kills 16 mutants of go-humanize's and 16 of uuid's constant
declarations, which Gremlins never runs. Gremlins runs one `go test` per
mutant; Supercov builds each package's test binary once, so it gains most on
fewer CPUs, and on 16 its fixed costs are most of its time. The remaining
differences are a nested module and a `//go:build ignore` file Gremlins
mutates and `go test ./...` never builds, time-dependent kills of clock
arithmetic, and a flaky Example; none is a soundness violation.

### Ruby, against mutant 0.17.1

On a 16-CPU Linux machine (AWS c8a.4xlarge, Ruby 3.4.13, 16 jobs for both
tools), 2026-11-22, mutant with every test a candidate for every subject:

| Project | mutant's mutants | Joined | Disagreements | mutant | Supercov measure + strong |
| --- | ---: | ---: | ---: | ---: | ---: |
| dry-inflector 1.3.1 (RSpec) | 2,061 | 2,061 | 0 | 32.2 s | 1.5 + 51.7 s |
| dotenv 3.2.0 (RSpec) | 1,345 | 1,345 | 0 | 21.4 s | 1.0 + 29.6 s |
| erubi 1.13.1 (Minitest) | 1,571 | 1,571 | 0 | 13.4 s | 0.5 + 4.1 s |

Per mutant, the strong pass took 25 ms on dry-inflector against mutant's 16,
14 ms on dotenv against 16, and 2.6 ms on erubi against 8.4. mutant has no
test-unit or Cucumber integration; those drivers were checked by running each
mutant's whole suite by hand.

### Java and Kotlin, against PIT 1.17.1

Against PIT with `fullMutationMatrix`, on commons-cli 1.9.0, commons-csv
1.12.0, typetools 0.6.3, kotlinpoet 1.14.2, picnic 0.5.0 and Ant 1.10.15's
`util` package. TBD(JVM results: mutants joined, disagreements, PIT and
Supercov times, machine and date).

## Cost, and turning it off

Mutation testing costs nothing until it is asked for:

- A plain run (`supercov -- <tests>`) places no probe and lists no mutant.
- The first `runs <id> mutants` lists the run's mutants from its sources and
  keeps the list beside the run (`mutants-listed.json`). A file changed since
  the run is left out until it is measured again.
- The HTML report and the run summary show mutants only for a run that listed
  them.

What costs time:

- **The value probes** (`SUPERCOV_MUTANTS=1`, which `supercov mutants` sets)
  record the values that decide a mutant's infection, so more mutants are
  decided without running. Measured on Rust crates (16-CPU machines,
  2026-11-22, warm builds): 4-11% over a measured run without them. On a Ruby
  suite (dry-inflector): 1.1 times a measured run without them
  (TBD(machine and date of the Ruby probe measurement)). On Go modules
  (16 CPUs, AWS c8a.4xlarge, 2026-11-22): about a quarter of a second a
  measured run, most of it the type checker's facts the probes need.
  TBD(value-probe cost for JavaScript, Python, Java and Kotlin runs).
- **The strong pass** reruns tests, so it takes about as long as the
  established tool on the same project: the tables above. Use `--diff` on a
  pull request to run only the change's mutants, and `--shard` to split a
  large pass across jobs.

To turn it off, measure without `SUPERCOV_MUTANTS` and do not run `supercov
mutants` or `mutants test`; a run's coverage is the same either way.
`npx supercov runs clean` removes stored runs with their mutants and verdicts.

See [CLI reference](cli.md#mutants) for every option.
