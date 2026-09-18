# CLI reference

Supercov has one command for measuring a suite and a small set of commands for
reading the result. Text output is designed for people and coding agents. Add
`--json` only when an integration needs a stable machine-readable response.

```sh supercov
npx supercov --help
```

## Quick reference

| Goal | Command |
| --- | --- |
| Measure a suite | `npx supercov -- <test command>` |
| List recent runs | `npx supercov runs` |
| Read the newest run | `npx supercov runs latest` |
| Find useful gaps | `npx supercov runs latest gaps` |
| Inspect one file | `npx supercov runs latest file <path>` |
| List assertions and their status | `npx supercov runs latest assertions` |
| Inspect one assertion and its flows | `npx supercov runs latest assertion <id>` |
| Read matching current source code | `npx supercov runs latest source <path>` |
| Compare two runs | `npx supercov diff <older> <newer>` |
| Find the tests a change affects | `npx supercov runs latest tests affected` |
| Combine shards | `npx supercov merge <id> <id> [...]` |
| Remove local data | `npx supercov clean` |
| Assess source quality with TypeSafe AI | `npx supercov quality` |
| See only files with findings | `npx supercov quality gaps` |
| Review what a change introduced | `npx supercov quality patch --base origin/main` |
| Read bundled guides | `npx supercov docs` |

## Assess source quality (experimental)

```sh supercov-example
supercov quality                 # this repository
supercov quality src/            # one directory
supercov quality gaps            # only files something fired on
supercov quality file src/a.ts   # one file, every check
supercov quality snapshots       # saved assessments
```

With no argument the subject is the repository you are standing in, the way
`supercov runs` needs no argument. Every assessment saves a snapshot, so the
reading commands work afterwards with no key and no network.

Twelve yes/no questions about specific named properties, drawn from Fowler and
Beck's refactoring smells and the class-scope smells CodeScene's Code Health is
built from: god class, long method, deep nesting, complex conditional, long
parameter list, duplicated logic, primitive obsession, dead code, feature envy,
temporary field, message chains and magic values. **The arithmetic that turns
twelve answers into one number is done by this command, not by the model**, so
every part of the score is a claim you can check against the file in seconds.
`quality file <path>` shows all twelve with what is known about each.

Text output reports a **band**, not a decimal, because the measured resolution
of these judgments is about one point and `4.4/10` would claim precision nobody
observed. `good` is 8 and above, `fair` is 5 to 8, `weak` is below 5. The number
is in `--json`, where something has to sort.

Directory and repository health weight files by size, because a plain average
would let a directory of one-line re-exports outvote the file everything depends
on. A file too large to send whole is split into windows at declaration
boundaries and the strongest answer to each question wins, since every question
is existential: "does this file contain a method that…" is true if any window
has one. A windowed file says so, because a property of the whole file, such as
logic duplicated across two windows, is weaker there.

### Which files are assessed

```sh supercov-example
supercov quality scope
SUPERCOV_SOURCE_ROOTS=src,app supercov quality
```

The same shape as the coverage side's source scope, and the same vocabulary, so
a reviewer reading `runs patch` and `quality patch` together is looking at one
set of files. Every file is **included**, **excluded** or **ambiguous**.

**Included** means under a source root. Roots are found by looking for package
manifests, `package.json`, `Cargo.toml`, `pyproject.toml`, `go.mod`, `Gemfile`,
`pom.xml`, `Package.swift` and the rest, at the repository root or under a
conventional package parent (`packages`, `apps`, `crates`, `services`, `workspaces`), plus whatever a
root manifest declares in `workspaces` or `[workspace].members`. Inside each
package it takes the conventional source directories, matched without regard to
case: `src`, `app`, `lib`, `server`, `client`, `api`, `functions`, Go's `cmd`,
`internal` and `pkg`, C and C++'s `include`, and SwiftPM's `Sources`. A Python
package is any directory holding an `__init__.py`, which is the flat layout PyPA
documents beside the `src` one. A Go module root is a source root **for Go files
only**, because Go compiles every package beneath it wherever it sits, and
scoping that by extension keeps a `go.mod` at the top of a polyglot repository
from claiming the rest of the tree. A declared package with no conventional
layout is measured whole.

Two manifests do not have fixed names. Gradle lets a module call its build file
after itself, as JUnit's `junit-jupiter-api.gradle.kts` does, so any `.gradle`
or `.gradle.kts` file counts. And a .NET project file names itself after the
project, so any `.csproj`, `.fsproj` or `.vbproj` marks a package **at any
depth**, with its own directory as a root, because .NET nests projects and keeps
sources beside the project file.

**It also reads what the manifest itself declares**, which is how a project says
where its code is without anyone guessing: `main`, `module`, `browser`, `bin`
and `exports` in a `package.json`, `path` entries and a `build.rs` in a
`Cargo.toml`. Supercov's own package declares `bin/supercov.js` and
`./runtime/javascript/*.mjs`; neither is a conventional source directory and
both ship. A declared file in a directory of its own brings that directory, so
the files it loads travel with it, while a declared file at the package root,
such as Cargo's `build.rs`, is only itself rather than swallowing the package.

Five conventional locations hold real code that is not the product, and are
named rather than left unclassified because there is nothing to declare: a
`scripts` directory is a `tool script`, matching the coverage scope's own rule,
`examples` is an `example`, `benches` is a `benchmark`, `docs` is
`documentation`, and anything spelled
`<tool>.config.<ext>` or `.<tool>rc.<ext>` is `build or tool configuration`. A
manifest entry pointing into build output, such as `"bin": "dist/index.js"`, is
ignored for the same reason: it names the artifact, not the code you would
change.

A test directory is recognised by suffix as well as by name, so Hono's
`runtime-tests` and .NET's `Shop.Tests` count. A separator is required before
the suffix, which keeps `contest` and `latest` as source.

**Excluded** means test code or generated output, each carrying its reason.
This is not a claim that test code does not matter: it costs a request each, and
several checks encode assumptions that are wrong for it, since duplication
between two test cases is often deliberate and a literal in a fixture is the
fixture. Recognition is by directory, by separator convention
(`login.test.ts`, `auth_test.go`, `test_login.py`, `user_spec.rb`), and by the
CamelCase suffix Java, Kotlin, C#, Swift and PHP use instead (`OrderTest.java`,
`OrderSpec.kt`, `OrderIT.java`). Generated output is recognised by name
(`.d.ts`, `.min.js`, `.pb.go`, `_pb2.py`, `.designer.cs`) and by the marker a
generator leaves in the file, which is the one signal needing no convention.

**Ambiguous** is the state that matters. First-party source under no recognised
root is never silently dropped: it is counted, reported as a limitation next to
the score, and listed by `quality scope`. Declare your roots to resolve
it:

```sh supercov-example
SUPERCOV_SOURCE_ROOTS=crates,runtime,bin supercov quality
```

That switches to **explicit** mode, where anything outside the named roots is
excluded as a decision rather than left as a question, and nothing is ambiguous.

Two details. **Matching is on whole separator-delimited parts, never on
substrings**, so `latest.ts`, `contest.rs`, `manifest.rs`, `attestation.go` and
`AUDIT.java` are source. And a file you name directly is always assessed,
whatever the scope decided, since `supercov quality tests/auth_test.ts` is an
unambiguous request for it.

`--all` assesses everything discovered, whatever its status. `quality patch`
applies the same scope for the same reasons.

### What is known about this number

On 272 Java classes carrying professional maintainability ratings, the composite
orders size-matched pairs 78% the way the raters did, against CodeScene Code
Health's 67%, a statement count's 58% and the Microsoft Maintainability Index's
52%. Asked twice, a single check moves by a median of 0.01 and the composite by
0.5% of its range.

Three limits belong next to it and are not hidden:

- The composite **correlates 0.92 with a statement count.** It is largely a size
  measure whose residual is right, which is a weaker claim than the table above
  sounds.
- Its margin over Code Health is **somewhere between 7 and 11 points** and moves
  by several points depending on which fifth of the corpus is dropped. The
  margins over a statement count and the Maintainability Index are large and
  stable.
- **Six checks score as well as twelve.** These are not twelve measurements but
  one measurement taken twelve times, so rewording checks does not improve the
  number. Four were reworded after a blind reader found them failing, and the
  composite moved by 0.4 points with an interval that includes zero.

Three checks fire on more than half of a real repository, so a full list of what
fired buries the finding that is news under the ones that are true of
everything. Text output shows the three strongest per file and `quality file`
shows the rest.

A property is reported as present at **0.60**, chosen against three references
rather than taken as a midpoint: it agrees with a blind reader slightly more
often than 0.50, reports 2.2 properties per file across 272 classes rather than
2.8, and still catches every deliberately introduced smell while co-firing on
unrelated checks falls from 12% to 7%. **Health does not use it.** The score is
the mean of the raw answers, so this decides what is shown and never what is
scored. No assessment fails a build.

### Compare two assessments

```sh supercov-example
supercov quality snapshots
supercov quality diff q_332dd8d1bfd149f1 q_c0e9a3aba4f77eb8
```

This is the decline gate. It reports which files lost health, which gained,
which properties appeared that were not there before, and which files entered or
left the scope. Only snapshots of the same catalog version and model can be
compared, because the questions would otherwise differ and the difference would
be read as a change in the code.

**A file whose bytes are identical in both snapshots is marked as such.** The
same question was asked twice, so any movement is the model's own variation
rather than an edit, and a reader should not go looking for a change that never
happened.

## Review what a change introduced (experimental)

```sh supercov-example
supercov quality patch                          # unstaged, the default
supercov quality patch --staged                 # what a commit would contain
supercov quality patch --base origin/main       # a branch, tag or commit
supercov quality patch --base origin/main src/  # only changes under src/
supercov quality patch --base origin/main --annotate github
```

The twelve checks, asked about a change rather than a file, plus **seven risk
checks that exist only in this form** because each is about what a change did:
a credential written into source, untrusted input interpolated into a query,
a change to who may do what, a test that now checks less, a change callers
outside the file would have to follow, a schema or data migration, and
debugging left behind.

The two sets come from different evidence and are worth reading differently. It mirrors
`supercov runs patch`, which asks whether the lines a change touched are tested,
and takes the same `--base` and `--annotate` options for the same reasons.

`--base` compares against the **merge base**, the point this branch left that
ref, not the ref's current tip. Commits other people landed after you branched
are not your change. A shallow checkout has no merge base; fetch with full
history (`actions/checkout` takes `fetch-depth: 0`).

`--unstaged` and `--staged` have no equivalent on the coverage side, because
coverage of uncommitted work means nothing without a run. Here they are the
pre-commit and pre-push moments. Untracked files are reviewed as additions under
`--unstaged` and `--base`, since you wrote them; under `--staged` they are not,
because an untracked file is by definition not in the index.

Both whole versions go into one request. A hunk alone cannot separate a property
the change introduced from one the file already had, which is the entire
question, so the extra tokens are the point.

Output lists only the files where something appeared, and only the checks that
fired, strongest first. A change that introduces nothing prints one line saying
so. `--annotate github` prints workflow annotations on stdout, needing no token
and posting no comment. A named property is a fact about a file rather than
about one line, so each annotation is anchored at the first line the change adds
rather than guessing which line caused it.

### What is known about this

Eight real files were each given one deliberately introduced smell. All eight
were detected, seven of eight ranked the introduced property first, and there
was **one false alarm across 312 control questions**: reformatting, playing the
change backwards, and comparing a file with itself produced nothing at or above
0.5, and nothing above 0.25. Cost is about **$0.0005 per changed file** for all
twelve checks, since they share one copy of the two versions.

This matters more than it sounds. Asked about a file, several of these checks
fire on two thirds of everything and are useless as flags. Asked about a change,
they stay quiet unless something changed.

**But a benchmark of 173 comments real reviewers wrote found the complexity
catalog has a word for 8% of them and fired on 1%.** Fifty-four percent of what
reviewers write about is bugs, and nothing here asks about correctness. Treat
the complexity half as a structural regression check, which is what CodeScene's
decline gate is, and not as a review.

The seven risk checks were built for that gap and are cleaner. On constructed
positives with matched safe changes they separate by 0.91 or more, and across
the same 50 real pull requests they fire once per pull request. On the seven
pull requests where a reviewer raised a security concern, one of them fired on
four, against nine of the forty-three without one. `breaks_api` is the loose
one: twenty of fifty with a median of 0.45, which needs its own ground truth
before it is worth relying on.

What is untested is a real pull request where a property arrived incidentally
among unrelated edits. The eight edits above were constructed, and a deliberate
four-level nest is a cleaner signal than nesting that grew over three years.

Nothing here fails a build. No threshold in this project has survived
calibration, so the command reports and exits successfully.

### Crossing a change with coverage

```sh supercov-example
supercov quality patch --base origin/main --run latest
```

Reads a run that already happened and marks any changed file where a property
appeared **and** the run left measured lines uncovered.

Neither half justifies stopping anyone on its own. A structural property is a
judgment, and an uncovered line is normal in code nobody has tested yet. Both at
once describes a change that made code harder to follow in a place no test
exercises, which is the one claim this tool can make that a coverage tool and a
quality tool cannot make separately.

This never starts a run, and **nothing about quality appears in a run's own
output**. An assessment costs money and needs a credential, so it stays
something you ask for.

### When both versions do not fit

A file whose two versions exceed the request budget cannot be split into windows
the way one file can, so the unified diff is sent instead and the report says
`unified diff only` for that file. On one real change the diff alone scored 0.83
where both versions scored 0.82, so it is a usable second choice, but it is one
measurement and it is labelled rather than silently substituted.

## Measure a test command

```sh supercov
npx supercov -- <test command>
```

Everything after `--` is passed to the test command:

```sh supercov
npx supercov -- npm test
npx supercov -- npx playwright test --project=chromium
npx supercov -- cargo test
npx supercov -- cargo nextest run
```

Use the complete command you rely on before merging or deploying. A coverage run
preserves the wrapped command's exit status, so it can remain a CI gate.

## List and select runs

```sh supercov
npx supercov runs
npx supercov runs --limit 5
npx supercov runs latest
npx supercov runs <run-id>
```

Runs are listed newest first. `latest` is convenient during an interactive
loop. Use the immutable run id in automation, review notes, and work that spans
sessions.

## Query a run

```sh supercov
npx supercov runs <run-id> [query] [options]
```

| Query | Use it to |
| --- | --- |
| no query | Read the overall result, test outcome, completeness, and timings |
| `gaps` | See only files with uncovered behavior or measurement limits |
| `files` | See every included file, including fully covered files |
| `file <path>` | Inspect the open obligations in one file |
| `decision <id \| path:line>` | Understand missing boolean outcomes and MC/DC witnesses |
| `line <path:line>` | See one line's state, obligations, and covering tests |
| `test <id \| name>` | See the coverage attributed to one test |
| `kinds` | Group coverage by test level, such as unit or E2E |
| `runners` | Group coverage by test runner |
| `scope` | Review included, excluded, and ambiguous source files |
| `assertions` | List assertions, including sites without flows, with freshness and execution status |
| `assertion <id>` | Inspect one assertion and its authored flows |
| `source <path>` | Read matching current project source with line numbers |
| `minimize` | Find a small test subset that preserves a coverage target |

Common examples:

```sh supercov
npx supercov runs latest gaps --limit 10
npx supercov runs latest file app/routes/checkout.ts
npx supercov runs latest decision app/routes/checkout.ts:42
npx supercov runs latest line app/routes/checkout.ts:57
npx supercov runs latest test "checkout retry"
```

Run any query with `--help` to see only the options valid for that query:

```sh supercov
npx supercov runs latest --help
npx supercov runs latest file --help
npx supercov runs latest assertions --help
```

Assertion queries read the run-owned map. `source <path>` reads the matching current
file directly. It prints source code with line numbers, preserving indentation;
add `--json` only when you want structured `{line, text}` items. `--offset` is
zero-based and `--limit` controls the number of source lines. Source and assertion
investigation require current files that match the run. Rerun the suite after
source changes to inherit the map into a new run.

### Assertion coverage

The regular run summary includes assertion coverage when a map has been
assessed. JSON reports expose it under `data.assertionCoverage`. Start with
[Understanding assertion coverage](assertions.md), or use these commands to
inspect and check a map:

```sh supercov-example
npx supercov runs <run-id> assertions --needs-attention
npx supercov runs <run-id> assertion <assertion-id>
npx supercov runs <run-id> assertions report --view statements --file src/shipping.js
npx supercov runs <run-id> assertions report --view excludedStatements
npx supercov runs <run-id> assertions validate --json
npx supercov runs <run-id> assertions check --require-mappings
```

Edit the file shown by `assertions`. Validation returns `expectedBasis` tokens;
after examining a flow, save its token in the map before running `check`.
`--require-mappings` requires explanations for recognized assertions observed
passing. Add `--require-observed` when every mapped site and selector should have
passing evidence, or `--min <percentage>` for a chosen target.

To inspect one large flow, add `--flow <flow-id> --view nodes` or `--view edges`
to the assertion detail command. `--compact` omits repeated source text from the
report. Follow the printed next-page command or JSON `pagination.nextOffset`.
Validation supports `--view flows`, `--view changes` and `--view errors` for large
maps. The [map reference](assertion-maps.md) describes all fields and gates.

## Fail CI below a coverage floor

```sh supercov-example
supercov runs check --min-lines 90 --min-branches 80 --min-mcdc 80
supercov runs check --min-lines 100 --per-file --json
```

`check` reads a recorded run; it never runs tests again. Give a floor per metric
with `--min-lines`, `--min-statements`, `--min-functions`, `--min-branches` or
`--min-mcdc`. `--per-file` applies the same floors to every file that has
eligible obligations, in addition to the whole run. Both report the counts
behind the percentage and, for lines, where the gaps are.

Floors are compared against the counts, never a rounded percentage: 9,999
covered lines out of 10,000 displays as 99.99% and fails a 100% floor, and a
run that displayed `100.00%` could never pass one while something is uncovered.

A check answers only when the run can answer. These end the command with `2`
rather than a pass or a failure:

- the wrapped test command did not pass, so a gate over it would turn a red CI
  run green
- the run no longer matches the current checkout
- a requested metric has nothing eligible, which is not the same as complete
- a requested metric left obligations unmeasured, so no exact judgement exists
- a requested metric is not recorded by the language adapter

Assertion coverage keeps its own check. Whether a test *examines* what it
executes is a different question from whether a line ran, and
`runs <id> assertions check` carries the freshness and acknowledgement rules
that answer needs.

## Check the lines a change touches

```sh supercov-example
supercov runs patch --base origin/main --min-lines 100
supercov runs patch --base origin/main --annotate github
```

`patch` answers whether the lines this change added or modified are tested. It
compares against the **merge base** with `--base`, not that branch's tip, so
commits other people landed after you branched are not counted as your
obligation. A shallow checkout has no merge base; fetch with full history
(`actions/checkout` takes `fetch-depth: 0`).

The denominator is the changed lines the run measured. Comments, blank lines and
declarations fall out because the language adapter already decided they are not
executable, not because `patch` guesses at syntax. Deleted lines are excluded:
there is nothing left to cover. Untracked new source counts as entirely added.

A change with nothing executable in it reports **No executable changes** and
passes, rather than claiming 100% for a patch that changed only comments. A
changed file that looks like product source but is absent from the run is named
separately, because treating it as zero uncovered lines would report success for
code nothing ran.

`--annotate github` prints workflow-command annotations on stdout, combining
adjacent misses into one range and capping the total (`--max-annotations`). It
needs no token and posts no comment.

## Export for other tools

```sh supercov-example
supercov runs report --format lcov --output coverage/lcov.info
supercov runs report --format cobertura --output coverage/cobertura.xml
supercov runs report --format html --output coverage/report
```

Both are written from the same view `check` and `patch` read, so a viewer,
hosted service or CI integration sees the totals Supercov enforced. Paths are
repository-relative with forward slashes, ordering is stable, and the file is
written atomically; an existing file is kept unless you pass `--force`. Without
`--output` the report goes to stdout and diagnostics to stderr, so a redirect
captures only the report.

Supercov records that a line ran, not how many times, so `DA:` and `hits` state
`1` or `0`. They are not execution frequencies, and Supercov will not invent
one to fill a field.

MC/DC conditions are not exported as ordinary branches. A consumer would then
show condition obligations as branch coverage, which is a different
measurement; that evidence stays in the JSON view and the HTML report. A report
from a failed or stale run is still written, with a warning on stderr — only
`check` refuses to pass on one.

### The HTML report

`--format html` writes one self-contained document. It opens from a copied CI
artifact with no server, no network and no login, and nothing is fetched from a
CDN. Because a source path is never used as an output path, a filename cannot
write outside the directory you named, and no directory is ever cleared to
regenerate a report.

It has three levels: the run's metric counts, a filterable and sortable file
table, and a source view marking each line covered or not covered in words and
a glyph as well as colour. Every line links as `#<file>:<line>`, so a CI summary
can point someone at the obligation rather than at the report.

Four states stay distinct, because collapsing them into one score is how a
report starts to mislead: **uncovered** (measured, nothing reached it), **not
applicable** (nothing eligible), **partly measured** (Supercov declined some
obligations, which are excluded from every count) and **stale** (the run no
longer matches the checkout). A failed suite says so beside its numbers.

Source text is embedded only when the run still matches the checkout; otherwise
the report shows line numbers and explains why. Embedding makes a report
portable and also means it contains your code — worth knowing before uploading
one as a public artifact.

## Narrow a view

| Option | Meaning |
| --- | --- |
| `--filter all \| passed \| failed` | Recalculate the view from all, successful, or failed attempts |
| `--kind <kind>` | Restrict to a test level such as `unit`, `integration`, or `e2e` |
| `--runner <runner>` | Restrict to one runner |
| `--metric all \| lines \| statements \| functions \| branches \| mcdc` | Choose a metric for `files`, `gaps`, `diff`, or `minimize` |
| `--limit N`, `--offset N` | Page through a collection |
| `--json` | Return the machine-readable form |

Collection output includes a copyable command for the next page.

For a large file, group and rank its decisions:

```sh supercov
npx supercov runs latest file app/routes/checkout.ts \
  --group decision --sort missing
```

## Compare runs

```sh supercov
npx supercov diff <older-run> <newer-run>
```

`diff` reports gains and losses. Use it after adding a test to prove that the
expected behavior became covered without an unexplained regression elsewhere.
Neither input run is changed.

The same filters can focus a comparison:

```sh supercov
npx supercov diff <older-run> <newer-run> --kind e2e
```

## Find a smaller test set

```sh supercov
npx supercov runs latest minimize
npx supercov runs latest minimize --metric branches --target 90
```

`minimize` finds a small set of tests that preserves the selected coverage
target. It does not edit, delete, or skip tests for you. Treat the result as an
analysis aid, not permission to remove tests that protect behavior outside the
selected metric.

## Find the tests a change affects

```sh supercov
npx supercov runs latest tests affected
npx supercov runs latest tests affected --files
npx supercov runs latest tests affected --json
```

`tests affected` names the tests of a run whose recorded execution the changes
since that run could have reached: a change in code the test ran, in its test
file, or in the shape of a file it ran code in -- a declaration added, removed
or renamed. A change confined to code the test never ran does not count, and
neither do comments, blank lines or trailing whitespace. A test that did not
pass in the run is listed regardless.

`--names` prints one affected test name per line and `--files` one test file
per line, for a runner's filter. A dependency, lockfile, configuration or
toolchain change affects every test and is reported as such. A source file
added since the run is outside every test's record; the working-tree check
says so, and the suite should run in full.

## Combine shards

```sh supercov
npx supercov merge <shard-a> <shard-b> <shard-c>
```

Merge creates a new run. The inputs must describe the same source,
configuration, toolchain, schema, and coverage denominator. Supercov rejects an
incompatible merge rather than publishing a misleading aggregate.

## Clean local data

```sh supercov
npx supercov clean --dry-run
npx supercov clean --keep 20
npx supercov clean
```

By default, `clean` removes all stored runs and the isolated build cache.
`--keep N` keeps the newest N runs. Cleanup removes only marker-owned Supercov
storage.

## Read bundled documentation

```sh supercov
npx supercov docs
npx supercov docs getting-started
npx supercov docs troubleshooting
```

The guides are installed with the package, so they remain available in a
terminal or offline environment after the package has been downloaded.

## Environment variables

| Variable | Use |
| --- | --- |
| `SUPERCOV_SOURCE_ROOTS` | Set comma-separated first-party source roots when automatic discovery is ambiguous |
| `SUPERCOV_TEST_KIND` | Label the wrapped command as a test level such as `unit` or `e2e` |

Examples:

```sh supercov
SUPERCOV_SOURCE_ROOTS=src,app npx supercov -- npm test
SUPERCOV_TEST_KIND=e2e npx supercov -- npx playwright test
```

## Exit codes

| Code | Meaning |
| --- | --- |
| `0` | The command or query succeeded |
| Wrapped command's code | The test command failed and Supercov preserved its status |
| `1` | A valid measurement failed a policy you set, such as a coverage floor |
| `2` | Supercov could not complete the request, or the evidence cannot answer it |
