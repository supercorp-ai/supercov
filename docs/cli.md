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
| See how much executed code the tests assert | `npx supercov runs latest assertions` |
| Assess it with Jev | `npx supercov runs latest assertions assess` |
| Find what is not asserted in one file | `npx supercov runs latest assertions <path>` |
| See each test's answer for one statement | `npx supercov runs latest assertions <path>:<line>` |
| See what one test is judged to catch | `npx supercov runs latest assertions --test <name>` |
| Read matching current source code | `npx supercov runs latest source <path>` |
| Compare two runs | `npx supercov diff <older> <newer>` |
| Open an interactive report | `npx supercov report` |
| Find the tests a change affects | `npx supercov runs latest tests affected` |
| Combine shards | `npx supercov merge <id> <id> [...]` |
| Remove stored runs | `npx supercov runs clean` |
| Assess code quality with Jev | `npx supercov quality` |
| See only files with findings | `npx supercov quality gaps` |
| Review what a change introduced | `npx supercov quality patch` |
| Remove saved assessments | `npx supercov quality clean` |
| Find security surface with Jev | `npx supercov security` |
| Review what a change introduced, security only | `npx supercov security patch` |
| Read bundled guides | `npx supercov docs` |

## Assess source quality

```sh supercov-example
npx supercov quality                 # this repository
npx supercov quality src/            # one directory
npx supercov quality gaps            # only files something fired on
npx supercov quality file src/a.ts   # one file, every check
npx supercov quality scope           # which files are assessed, and why
npx supercov quality snapshots       # saved assessments
npx supercov quality diff <older> <newer>
npx supercov security                # twelve security checks, every file
npx supercov security gaps           # only files something fired on
npx supercov security patch --base origin/main
npx supercov security --run latest   # flagged files no test exercises
```

With no argument the subject is the repository you are standing in. Every
assessment saves a snapshot, so the reading commands work afterwards with no key
and no network.

Twelve yes/no questions about named code properties go to [Jev](https://typesafe.ai);
the score is arithmetic this command does over the answers. Text reports a band,
`good`, `fair` or `weak`; `--json` carries the number and every check with what
is known about it. `--all` includes test files, generated output and anything
outside a source root, all of which are left out by default. `--dry-run` prints
the exact requests and contacts nothing.

`quality diff` reports what changed between two assessments: which files lost
or gained health, each check that moved in them with its value before and after,
and which files entered or left the scope.

Assessing needs a TypeSafe AI API key in `TYPESAFE_API_KEY`; reading a saved
assessment does not. `TYPESAFE_BASE_URL` and `TYPESAFE_DEFAULT_MODEL` use Jev
through another provider, such as OpenRouter. The command prints a cost estimate before sending anything
and caches answers by content, so a second run pays only for what changed.

See [Understanding quality](https://supercov.com/docs/quality) for what the
number is worth and which files get assessed.

## Review what a change introduced

```sh supercov-example
npx supercov quality patch
npx supercov quality patch --base origin/main
npx supercov quality patch --base origin/main --annotate github --run latest
```

The same twelve properties asked of a change, plus six risk checks that only
apply to one: a credential in source, untrusted input in a query, a change to
who may do what, a test that now checks less, a schema migration, and debugging
left behind.

With no range it reviews uncommitted work when the tree is dirty and everything
since this branch left its default branch when it is clean. `--unstaged`,
`--staged` and `--base <ref>` say so explicitly; `--base` uses the merge base,
like `runs patch`, so commits other people landed after you branched are not
your change.

`--annotate github` prints workflow annotations on stdout, needing no token and
posting no comment. `--run <id>` reads a saved coverage run and marks any file
where a property appeared and the run left lines uncovered.

Output lists only files where something appeared. A change that introduces
nothing prints one line saying so. About $0.0005 per changed file.

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
| `files` | See every included file, including fully covered files; `--group dir` shows coverage by directory, split by test kind |
| `file <path>` | Inspect the open obligations in one file |
| `decision <id \| path:line>` | Understand missing boolean outcomes and MC/DC witnesses |
| `line <path:line>` | See one line's state, obligations, and covering tests |
| `test <id \| name>` | See the coverage attributed to one test |
| `tests without-evidence` | List the tests that made assertions and recorded no coverage, which the summary counts in a warning |
| `kinds` | Group coverage by test level, such as unit or E2E |
| `runners` | Group coverage by test runner |
| `scope` | See what is source and what is not, by directory and reason; `--files` lists every file |
| `assertions` | Read the share of executed statements the tests assert, and the ones they do not; `assertions assess` works it out |
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

To see where the coverage is, read it by directory. Each row is a directory,
with what it holds and how much of it each kind of test covers; `--depth`
keeps more levels, and `--metric` picks branches, functions, statements or
MC/DC instead of lines:

```sh supercov
npx supercov runs latest files --group dir
npx supercov runs latest files --group dir --depth 2 --metric branches
```

```
Directory   Files  Lines      e2e    unit  no test      All
app             5     23   86.96%   0.00%    8.70%   95.65%
lib             1     10   90.00%  40.00%    0.00%   90.00%
.               2      8   75.00%   0.00%   25.00%  100.00%
components      1      2  100.00%   0.00%    0.00%  100.00%
```

`no test` is what ran only while no test was running, such as a server
starting before the first test. The column is there when a directory has any.

In `--json`, every file of `files` and `gaps` carries `totals` and `covered`
for each metric beside what is missing, so a percentage can be worked out for
any file or set of files.

Run any query with `--help` to see only the options valid for that query:

```sh supercov
npx supercov runs latest --help
npx supercov runs latest file --help
npx supercov runs latest assertions --help
```

`source <path>` reads the matching current file directly. It prints source code
with line numbers, preserving indentation; add `--json` only when you want
structured `{line, text}` items. `--offset` is zero-based and `--limit` controls
the number of source lines. The file must still match the run; rerun the suite
after source changes.

### Assertion coverage

```sh supercov-example
npx supercov runs <run-id> assertions assess --dry-run
npx supercov runs <run-id> assertions assess
npx supercov runs <run-id> assertions assess --refresh   # ask again instead of reusing saved answers
npx supercov runs <run-id> assertions
npx supercov runs <run-id> assertions --all --json
```

`assertions assess` asks Jev, test by test, whether each statement the test ran
would make it fail if changed, and saves the result to the run; it needs
`TYPESAFE_API_KEY` for questions not answered before. `assertions` reads the
saved result without the network. The regular run summary shows the same share,
and JSON reports expose it under `data.assertionCoverage`. See
[Assertion coverage](assertions.md) for what counts as asserted and how answers
are reused across runs.

## Fail CI below a coverage floor

```sh supercov-example
npx supercov runs check --min-lines 90 --min-branches 80 --min-mcdc 80
npx supercov runs check --min-lines 100 --per-file --json
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

## Check the lines a change touches

```sh supercov-example
npx supercov runs patch --base origin/main --min-lines 100
npx supercov runs patch --base origin/main --annotate github
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
npx supercov runs report --format lcov --output coverage/lcov.info
npx supercov runs report --format cobertura --output coverage/cobertura.xml
npx supercov runs report --format html --output coverage/report
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

## Generate a portable HTML report

```sh
npx supercov report
npx supercov report <run-id>
npx supercov report latest --compare <older-run-id>
```

`report` turns stored local evidence into one self-contained interactive HTML
file and opens it in the default browser. It does not rerun tests, start a
server, load external assets, or upload anything. The default report includes
up to ten recent runs. Selecting a run compares it with the previous saved run
automatically.

```sh
npx supercov report --runs 10
npx supercov report --output artifacts/supercov-report.html --no-open
```

A report includes the five newest runs; `--runs N` includes up to 20. A large
project's report can pass GitHub's 25 MB attachment limit; `--runs 1` makes
the smallest. See [Portable HTML reports](reports.md) for
the privacy and source-integrity rules.

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
per line, for a runner's filter. A Vitest, Playwright or node:test test inside
`describe` blocks is printed as the blocks and the test joined by spaces
(`small charges`), which is what `vitest -t`, `--grep` and
`--test-name-pattern` match. A dependency, lockfile, configuration or
toolchain change affects every test and is reported as such. A source file
added since the run is outside every test's record; the working-tree check
says so, and the suite should run in full.

### Which affected tests check the change

When the run has an [assertion assessment](assertions.md), `tests affected`
also says, for each affected test, whether it was judged to catch a change to
the statements that changed in code it ran, and lists those tests first. A run
without an assessment answers from coverage alone, as above.

```sh supercov
npx supercov runs latest assertions assess              # once, on the run
# ... edit code ...
npx supercov runs latest assertions assess --changed    # ask every test that ran the changed code
npx supercov runs latest tests affected --names --ran-changed
```

The assessment stops asking about a statement once one test catches it, so on
its own it knows only some tests' answers. `assess --changed` asks, for the
statements in code changed since the run, every test that ran them: typically
a few requests per commit. After it, `--ran-changed` leaves out the affected
tests that ran the changed declaration but none of the changed statements, and
`--asserting` also those judged to catch no change to them. Tests whose own
file changed or that did not pass stay, and so does any test never asked. With
an assessment or without, `--names` and `--files` list first the tests whose
own file or name shares words with a changed file (`test_receivebuffer.py` for
`_receivebuffer.py`), the verdicts breaking ties.

Measured by applying random changes and running the suite:

| Project | Breaking changes (failing tests) | Coverage's set | `--ran-changed` | `--asserting` | A failing test in the first three |
| --- | --- | --- | --- | --- | --- |
| h11 (Python, 78 tests) | 23 (153) | all 153 in 28 tests | all 153 in 23.5 | 141 in 19.9 | 18 of 23 |
| go-version (Go, 31 tests) | 36 (140) | all 140 in 10.1 | all 140 in 9.8 | 133 in 9.4 | 32 of 36 |

`--ran-changed` never dropped a failing test; `--asserting` dropped 5 to 8% of
them. `assess --changed` cost about a tenth of a cent per change.

Measured on real history: at each of a project's recent commits, the source
the commit replaced was put back -- someone reintroducing what it fixed -- and
the commit's own tests run against it:

| Project | Commits (breaking) | Failing tests | Kept by coverage's set | Kept by `--asserting` | Tests selected (coverage / `--asserting`) of all |
| --- | --- | --- | --- | --- | --- |
| h11 (Python) | 60 (13) | 27 | 27 | 27 | 25.2 / 24.9 of 75 |
| go-version (Go) | 24 (2) | 3 | 3 | 3 | 15.2 / 14.1 of 25 |
| dry-inflector (Ruby) | 12 (7) | 26 | 26 | 26 | 813 / 718 of 1,109 |
| uuid (TypeScript, node:test) | 4 (3) | 6 | 6 | 6 | 13.2 / 11.5 of 64 |
| dtolnay/semver (Rust) | 20 (0) | 0 | -- | -- | 27.9 / 25.9 of 38 |
| commons-cli (Java) | 15 (7) | 12 | 12 | 12 | 159 / 143 of 985 |

No failing test was left out of the set to run. On real commits the
assessment narrows little -- Jev judges most tests that ran a changed line to
catch the change -- and its verdicts alone ranked a failing test in the first
three for only 6 of 32 breaking commits. Ranking by name, with the verdicts
breaking ties, put one there for 10 of those 32, and for 15 of 25 breaking
commits in four projects it was not designed on (hashie, Masterminds/semver,
itsdangerous, java-classmate), where file order managed 11; without an
assessment, name alone managed 8 and 12. After a project's first
assessment, each commit cost under a cent.

## Combine shards

```sh supercov
npx supercov merge <shard-a> <shard-b> <shard-c>
```

Merge creates a new run. The inputs must describe the same source,
configuration, toolchain, schema, and coverage denominator. Supercov rejects an
incompatible merge rather than publishing a misleading aggregate.

## Clean local data

```sh supercov
npx supercov runs clean --dry-run
npx supercov runs clean --keep 20
npx supercov runs clean
```

By default, `runs clean` removes all stored runs and the isolated build cache.
`--keep N` keeps the newest N runs, by when they started rather than by their
identifiers, which carry no order. Cleanup removes only marker-owned Supercov
storage.

Saved quality assessments live in their own lane and are never removed here: an
assessment costs money and cannot be reproduced from the repository. Remove
those deliberately, with the same options:

```sh supercov
npx supercov quality clean --dry-run
npx supercov quality clean --keep 5
npx supercov quality clean
```
If Supercov itself fails to publish a run the tests already paid for, it keeps
that run's raw evidence in `.supercov/failed-evidence/<run id>` and says so in
the error. Report the failure with that directory: it is what diagnoses it. Only
a full `runs clean` reclaims it -- `--keep N` leaves it alone -- and the summary says
when it goes.

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
| `SUPERCOV_SOURCE_ROOTS` | Comma-separated directories or files that hold your own code, in any language; everything else is left out |
| `SUPERCOV_TEST_KIND` | Label the wrapped command as a test level such as `unit` or `e2e` |
| `SUPERCOV_SOURCE_TOOLS` | Comma-separated names of tools the test command runs that read source as text, such as a linter or a spell checker, besides Biome, oxlint, dprint, cspell and knip. Each is run on your source as you wrote it instead of on the instrumented copy |
| `SUPERCOV_KEEP_WORKSPACE` | Set to `1` to leave the instrumented copy of the project in `.supercov/workspaces/` after the run, to inspect what the command ran |

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

`quality` and `security` exit `0` for a complete report in which some files
could not be assessed, such as a file larger than the model accepts; each is
listed in the report with its reason, and stderr says how many. They exit `2`
when no file could be assessed.
