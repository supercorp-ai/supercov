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
| Assess source quality with TypeSafe AI | `npx supercov quality src/` |
| Score code by named properties | `npx supercov quality health src/` |
| Review what a change introduced | `npx supercov quality review --since main` |
| Read bundled guides | `npx supercov docs` |

## Assess source quality (experimental)

```sh
supercov quality src/server.ts --dry-run
supercov quality src/ --json
supercov quality src/server.ts --context contracts.md
supercov quality src/server.ts --refresh
supercov quality src/ --review-below 5
```

Set `TYPESAFE_API_KEY` in your environment for uncached assessments. This command
sends the selected source files and optional context to TypeSafe AI. It does not
run tests, instrument code, or require a coverage run. `--dry-run` prints the exact
request bodies as JSON without making API calls or writing cache files.

Whole files receive nine independent Jev scores: maintainability, readability,
correctness, input validation, failure handling, state integrity, cohesion,
changeability, and overall. Each file is sent once as plain source text inside a
named state object, with those nine Score questions and one Noul question about
behavioral context sufficiency. The pinned model is `jev-1.13.0`; the rubric is
`quality-v3`.

Every displayed score, including overall, is Jev's own judgment, scaled from its
rubric levels to 0–10. **A file's headline is its maintainability**, not the
broad `overall` answer, which is the weakest construct in the rubric: against
50 human-rated Java classes it reached 0.46 where maintainability reached 0.74
and file size alone reached 0.60, and on 200 Python functions it reached 0.30
against readability's 0.50. It is still reported, and still Jev's own answer,
but it does not lead. There is no local average, weight, or score penalty.
Maintainability and readability are agreement judgments on four levels, worded
against the human rating instruments they were compared with; the other seven
constructs use five levels with concrete conditions for weak and strong grades.
Against professional maintainability ratings of Java classes, given two files
the raters clearly separate, the ranking picks the harder one 85 to 96 times in
100, where file size alone manages 74 to 88. **It does not beat the static
metrics that are already free**: on 80 classes joined to the ICSME 2024
benchmark it orders clear pairs correctly 90.2% of the time and a count of
statements in the file gets 90.3%, with cyclomatic complexity at 86.5%,
CodeScene Code Health at 84.4% and SonarQube's debt ratio at 57.3%. Treat this
ranking as orientation, not as a reason to prefer it over a parser. On 200 Python functions rated by
three people each it agrees at 0.50 where source length manages 0.32. The nine
constructs are not nine independent judgments: maintainability, readability and
changeability correlate at 0.89 to 0.97 with each other in both languages, and
no construct adds agreement with the human reference once maintainability is
known.

**This ranks code by how hard it is to work with. It does not predict where
bugs are or were**, and that boundary has been measured twice. Across twelve
real upstream fixes, the declaration a maintainer changed ranked weakest 5
times out of 12 against 3.77 by chance. Across four years of one TypeScript
project's history, 1,029 author-declared fix commits over 186 files, the
correlation between a file's grade and the fixes it received disappears once
file size is held constant, falling from −0.41 to +0.02. What people call hard
to maintain and where bugs get fixed are different things. Refactors in that
same history behave differently: holding size constant, poorly graded files
attract more refactoring and optimisation and fewer features, which is what a
maintainability signal should look like and not what a size proxy would
produce.

These judgments are not measured coverage, probabilities of correctness, or a
security audit. The overall answer is independent; a high overall score can
coexist with a weak individual axis.

Files are listed weakest maintainability first, then weakest readability, then
Jev's overall answer. **A difference under 1 point is not one this rubric has
been shown to resolve**, and the listings say so: a reader who could not see
the grades agreed with 10 of 10 blinded pairs separated by more than three
points, and 7 of 10 separated by less than one, where chance is 5. In the
`dimension` view, files within one point of the weakest are marked, because
their order among themselves carries nothing. **Maintainability is the only construct with a review
cutoff**, at 6.9, selected on 20 human-rated Java development classes. It has
since held up on 40 further classes and on 31 that no rubric had graded, where
it caught every negative class, and on 200 Python functions from 128
repositories that humans rated adequate or better it flagged 35, a false-alarm
rate of 17.5% against the 20% this project set as its bar. Every other construct is ranked and never
marked. The readability cutoff of 7.775 was withdrawn on 2026-09-18: on those
31 unused classes it flagged 87% of a sample that was 45% negative, its
agreement fell across all three samples that tested it, and on the 200 Python
functions it would have flagged 26.5% where nothing was rated badly. `--review-below <n>`
replaces every construct's cutoff with `n` (range 0–10). Cutoffs decide markers
only: they never change a score, no marker fails the command, and none of them
is a validated defect boundary.
Scores remain visible at low confidence or when context is missing. Every score
shows its provider confidence. **Confidence says how concentrated an answer
was, which predicts how much it would move if asked again, not whether it is
right.** It correlates with repeat movement at −0.72, and a grade below 0.7
confidence moves about four times as much between identical requests as one
above. It does not separate good code from bad: on 31 Java classes the median
confidence of the human-negative ones was 0.71 against 0.72 for the positives,
and requiring confidence alongside the review cutoff lost ten of twenty-three
real findings while removing no false alarms. Nothing in this command acts on
confidence, deliberately. The report separately shows Jev's 0–1 judgment of
behavioral context sufficiency. High scores with insufficient
context do not establish the behavior of missing dependencies.

Paths must be inside the current working directory. Directory scans respect
`.gitignore` and `.ignore` files and omit hidden files and common build,
dependency and generated directories. Explicit source files may be Git-ignored.
Supported extensions include JS/TS, Rust, Python, Ruby, Go, Java/Kotlin, C/C++, C#,
Swift and PHP. Symlink targets are rejected. Tests are included when selected;
there is no automatic test or dependency retrieval. `--context` supplies a UTF-8
contract document to every selected file, so keep it relevant to that selection.

Source is never truncated. A request must stay within an estimated 30,000 tokens,
counted as one token per three serialized bytes. That estimate is deliberately
low: Rust source measured about 3.8 bytes per token, so most files well over
80 KB still fit. A file over the budget is assessed as **windows** of whole
declarations instead. Windows partition the file at declaration boundaries,
descending into any declaration too large to send on its own, so a file holding
a single large class splits at that class's methods rather than not at all.
Every line belongs to exactly one window and no byte is sent twice; each window
is graded on its own and states which lines it covers, and the file outline is
supplied for orientation. A windowed file has **no whole-file grade**, because
a file-wide construct such as cohesion cannot be judged from one window; it is
ordered in the report by its weakest window, which is ordering rather than a
grade. Window grades are not covered by the cutoff calibration above. A file
over the budget with no declarations to split on, including any file in a
language Supercov cannot parse, is reported as an error, as is a single
declaration too large to send alone; its neighbouring windows are still
assessed. Empty files are errors. Nothing larger than 4 MiB is read.

### The repository judgment

Once every file has been graded, Jev judges each directory and then the
repository. It makes those judgments the same way it makes a file's: as answers
to a rubric. What it reads is an inventory of what the scope holds and, for each
part of it, **the rubric level Jev itself chose for that part, quoted word for
word**. It never receives a score, a cutoff or an average. A repository grade is
therefore Jev's own judgment, not our arithmetic over file grades, and the
report says so in those terms.

Scopes are judged deepest first, so a directory is one verdict by the time the
scope above reads it. That is what lets a large repository be judged at all: a
parent reads one verdict per child, not every file beneath it. A directory
holding more children than fit in one request is judged in parts first, the way
an oversized file is judged in windows. A directory that holds only one thing is
not judged separately, since its verdict would simply be that thing's.

Jev is also asked whether the supplied evidence is enough to judge the scope as
a whole. **A grade is shown only when that answer is at least 0.5.** Below it,
the report says the judgment was withheld and gives the number, and the grades
stay in the JSON view rather than being deleted or quietly displayed. This gate
exists because a bare inventory still produces a confident-looking number: asked
to judge a repository from file names alone, Jev graded it 6.65 out of 10 while
answering 0.13 to whether it had the basis to judge at all.

Wider scopes carry **no review markers**. The file cutoff was selected on
human-rated Java classes; nothing at directory or repository scope is calibrated
against anything, so nothing there is marked.

The repository judgment has been tested once, against five Java projects whose
classes carry professional ratings. It ordered the best-rated and worst-rated
projects correctly on both constructs, and a repository seeded with the dataset's
worst classes fell on all three, so it reads its evidence. Two limits came out of
the same test. Its range is compressed: across projects the raters placed far
apart, its grades spanned about 1.6 points on maintainability and 0.9 on
readability. And an offline average of the same file grades ordered all five
projects correctly where the judgment misplaced one. Averaging is not what this
command does, by design, because an average over file grades is arithmetic rather
than a judgment; but the extra request has not been shown to order repositories
better than one, and on five projects it could not be. Jev is additionally asked where
behavioral risk sits, but only when a scope has between two and twelve children:
a Choice ranks one option first however weak the evidence, and over 22 options
that answer was measured at 0.25 confidence with "no clear one" tied for first.

A single assessed file has no wider scope and produces no repository judgment.

### Browse a saved assessment

```sh
supercov quality snapshots
supercov quality show
supercov quality dimension maintainability
supercov quality file src/server.ts q_1a2b3c4d5e6f7890
```

Every scan saves an immutable snapshot and prints its id. The read commands
above work entirely from what is on disk: no API call, no key, no new grade.
A snapshot argument is optional and defaults to the most recent scan made in
this directory; a scan never revises an earlier snapshot, so an id keeps
showing what it showed.

`show` gives the scan's header, the repository judgment or the reason it was
withheld, each judged directory, each construct with how many files its cutoff
marked and which file is weakest on it, and the files ranked weakest
maintainability first. `dimension <construct>` ranks every file on one
construct. `file <path>` opens one file: each construct's grade and confidence,
**the rubric level Jev actually chose, in its own words**, the behavioral
context and window judgments, and the file's top-level declarations, which are
listed precisely because none of them was graded on its own yet.

A windowed file has no whole-file grade, so a listing represents it by its
weakest window and names that window. That is how the row is ordered and
labelled, not a grade for the file.

`--limit <n>` sets how many rows a listing shows, defaulting to 20, with `0`
for all of them. `--json` prints the same view as JSON. The first argument is
read as a subcommand only when it is one of the reserved words, so a directory
called `show` is still assessable as `supercov quality scan show`.

### Drill into one file's functions

```sh
supercov quality functions src/server.ts
supercov quality functions src/server.ts --deepen
supercov quality function src/server.ts::createServer --source
```

A file's grade says the file deserves attention; it does not say where in the
file to look. Deepening asks Jev about each function and method on its own,
with the whole file supplied once as shared context, so a declaration is read
where it actually lives rather than as a snippet. Every scan already records
what a file declares, so `functions <path>` lists them with no API call: each
one either carries its grades or says it has none and gives the command that
would grade it. **A file's grade is never copied down onto its parts.**

What counts as a declaration is every function and method the file declares,
excluding closures nested inside them: a callback is part of the code that
installs it, and grading it separately reports noise rather than a place to
look. **Two constructs are graded here, readability and maintainability**, and
Jev is separately asked whether the declaration is substantial enough for a
separate judgment to say anything, which is what keeps a one-line accessor
scoring well from reading as news. Every other construct stays a file-scope
question.

**This view ranks the declarations of a file by how hard they are to read and
change. It does not say where a bug is.** Tested on twelve real upstream fixes
from four npm packages, the declaration a maintainer changed ranked weakest on
the relevant construct 5 times out of 12, against 3.77 expected by chance. A
subtle defect in an otherwise careful function does not make that function read
worse than a gnarlier neighbour.

Correctness and failure handling were graded here until 2026-09-18 and were
withdrawn by the same test. Reformatting a file without changing its parsed
syntax tree moved those two by a median of 0.16 and 0.15 per declaration, while
the real fixes moved the declaration they repaired by a median of 0.13: a grade
that answers more to whitespace than to the defect is not evidence about the
defect. The same reformatting moved readability and maintainability by a median
of 0.03. Both withdrawn constructs remain file-scope questions, where the
relevant one was measured rising for three of three independently reproduced
fixes.

`--deepen` saves a **child snapshot**, with the parent untouched and named in
the child's manifest. Deepening a second file starts from the child, so the
grades accumulate. It refuses to run when the file no longer holds the bytes
that were graded, and `--source` refuses to print a changed file under a grade
that describes different source. If the snapshot was assessed with a
`--context` document, supply the same one to deepen it; only its hash is
stored, so it cannot be assumed.

Declarations carry no review markers either, for the same reason wider scopes
do not: the cutoff was selected on whole human-rated classes. The Python
transfer that came closest to this scope was explicitly exploratory, and the
two constructs graded here have passed a stability test rather than an accuracy
one.

### Compare two snapshots

```sh
supercov quality diff q_332dd8d1bfd149f1 q_c0e9a3aba4f77eb8
```

Only snapshots of the same rubric, policy version and model can be compared;
anything else would compare two different questions and call the difference a
change in the code. The comparison sorts files into four groups, because they
mean different things:

- **Changed source.** The file's bytes differ, so a movement may be the edit.
- **Same source, different grade.** An identical request is answered from
  cache, so this only happens after `--refresh`: it is one question asked
  twice, not a change in the code, and it is labelled that way.
- **Newly assessed declarations.** A deepened child snapshot, where no grade
  changed and grades were added.
- **Unchanged.** Counted, not listed.

Every movement carries its own size, and one no larger than **0.7** is marked
as within the repeat variation measured for this rubric. That number is the
largest difference seen between two identical requests when this rubric family
was measured on real files, across 28 source snapshots and 8 constructs, where
the median difference was 0.075. It is an observed maximum rather than a
statistical threshold, and it is attached to a movement rather than hiding it.

A real example: renaming a function's locals to single letters and deleting its
error log moved readability by -2.67 and failure handling by -1.67, while five
other constructs moved less than the repeat variation and were marked as such.
The repository grade above that file moved -0.03, which is what one file out of
twenty-two should do.

Declarations are compared by name, so a declaration added or removed is
reported, and one that was renamed reads as one of each. Nothing here detects a
rename, because a snapshot stores the hash of its source rather than the source.

Validated raw responses are cached in `.supercov/quality/requests/`, keyed by
the exact request hash, including source, comments, context, model and question
wording. Responses cached by an earlier version in `.supercov/quality/` itself
are still read. Snapshots live in `.supercov/quality/snapshots/` and hold grades
and identities, not copies of the provider's answers: they point into the
response cache by request hash. A file view degrades gracefully when a cached
answer has been deleted, losing the level wording but keeping the grades.
Cache hits work without an API key. `--refresh` requests a new assessment.
JSON reports contain source/request/context hashes, model and rubric versions,
the snapshot id, the token budget, review policy with each construct's cutoff
and its basis, raw answer distributions, per-axis scores and confidence,
context sufficiency, review flags, window spans and declarations, the original
assessment duration and token usage. `usage_this_run` counts successful fresh responses, excluding
cache hits; it cannot account for provider billing on failed attempts. Full
source and API credentials are not written to the cache.

A scan prepares every request first and then sends eight at a time, so its
speed is set by the provider rather than by the file count: Supercov's own 141
source files, about 5 MB, take 24 seconds and $0.085 cold, and 7 seconds and
nothing when every answer is already cached. Wider scopes are judged after the
files and in order, since each one reads the verdicts below it.

Exit status is 0 for a completed report (including review flags and uncertain judgments), or 2 for
invalid input or any file/API error. Partial results remain in the report. Low
grades do not fail the command. HTTP 429 and 5xx responses receive at most two
retries with bounded backoff; long retry delays are returned to the caller.
Line locations within a declaration, changed-file selection and failing CI
grade gates are not implemented yet.

## Score code by named properties (experimental)

```sh
npx supercov quality health src/
npx supercov quality health --json src/
```

A different instrument from the rubric above. Rather than asking the model to
place a file on a scale, this asks twelve yes/no questions about specific named
properties, and the arithmetic that turns twelve answers into one number is done
here rather than by the model. Every part of the score is therefore a claim you
can check against the file in seconds.

The checks are drawn from Fowler and Beck's refactoring smells and the
class-scope smells CodeScene's Code Health is built from: god class, long
method, deep nesting, complex conditional, long parameter list, duplicated
logic, primitive obsession, dead code, feature envy, temporary field, message
chains and magic values. `--json` carries the full catalog, including what is
known about each check's accuracy.

Health is `10` when nothing fires and `0` when everything does. It is the mean
of the twelve answers, so the scale means the same thing if the catalog changes.
Directory and repository health weight files by size, because a plain average
would let a directory of one-line re-exports outvote the file everything depends
on. **That weighting is arithmetic done by this command, not a judgment the
model was asked for.** The per-file numbers it combines are model judgments.

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

There is no cutoff. No threshold in this project has survived calibration, and
this command does not fail a build.

## Review what a change introduced (experimental)

```sh
npx supercov quality review                    # unstaged, the default
npx supercov quality review --staged           # what a commit would contain
npx supercov quality review --since main       # a branch, tag or commit
npx supercov quality review --since main src/  # only changes under src/
```

The same twelve checks, asked about a change rather than a file: does the new
version show this property where the old one did not. Both whole versions go
into one request. A hunk alone cannot separate a property the change introduced
from one the file already had, which is the entire question, so the extra tokens
are the point.

Output lists only the files where something appeared, and only the checks that
fired, strongest first. A change that introduces nothing prints one line saying
so. The three ranges exist because they are three different moments: before a
commit, before a push, and on a pull request.

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

What is untested is a real pull request where a property arrived incidentally
among unrelated edits. The eight edits above were constructed, and a deliberate
four-level nest is a cleaner signal than nesting that grew over three years.

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
