# Portable HTML reports

Supercov's terminal queries are deliberately small for coding agents. The HTML
report is the human view over the same immutable local evidence.

```sh
npx supercov report
```

This writes `.supercov/reports/supercov-report.html` and opens it in your
default browser. The store carries its own `.gitignore`, so the report cannot
be committed by accident; `--output` puts it anywhere else, relative to the
directory you run the command from. The file contains its application, styles, and compressed
evidence. It works after the file is moved, attached to a pull request, or
opened on a machine that does not have Supercov installed. It does not need a
server, extraction step, file picker, external font, CDN, or network request.

## What the report answers

The first screen is ordered around four questions:

1. Did the measured command pass?
2. Is the measurement complete and still current?
3. What changed from the comparison run?
4. What is the next concrete line, branch, or MC/DC obligation to test?

Sidebar entries pair the recorded time with compact Coverage, Assertions, and
Quality charts, plus a Security shield showing the flagged-file count. Missing metrics keep neutral gray charts and show a dash rather
than a zero, including in file lists and detail headers.

The **Overview** groups coverage and assertion evidence in one full-width card,
followed by Quality and Security. All four measures use the same small heading.
The run header shows its recorded branch (when available), short commit, local
change state, and date. The inline **Test command** action opens the recorded command in a
modal.
Older runs without a branch keep their recorded commit; the current checkout's
branch is never substituted for historical metadata.

**Coverage** counts obligations and never averages them. The ring summarizes
lines, statements, functions, branches and MC/DC conditions. **View breakdown**
opens a modal with a circle chart and percentage for each type; a category with navigable gaps opens the files that owe
it. Empty coverage shows no percentage. The test outcome counts, source-file count and
duration appear beneath the coverage and assertion summaries. Failed tests,
measurement limits and stale source remain explicit.

**Assertions** shows the share of measured statements credited by recorded
assertion links. Its arc retains a gap because the map's completeness is unknown.
Missing links do not prove that code has no assertions. Unavailable or stale
maps show an unavailable state rather than a percentage. Below the summaries,
the report lists files with coverage gaps first, or the lowest assertion-credit
percentages when coverage is complete. File, Coverage and Assertions headers
change the order; **All files** keeps that ordering in the complete list. Each
row opens the file's evidence.

**Quality** shows its score, band, assessment scope and files, initially sorted
by lowest score. Its File and Quality headers change the sort order. Main
concerns appear as compact labeled pills with distinct icons. Recorded 0–1
values remain available on hover and in the file assessment. Higher values
receive a stronger warm tint; these values are not severity grades. The quality gauge colors its existing weak, fair and good bands,
and the band label sits beside the score. It is a ranking,
not a target. **Security** reads saved audits from `.supercov/security/`. It shows
flagged files, file-level and line-confirmed findings, CWE references, and
cross-file paths. These are patterns to review, not a security score or proof
of exploitability. Audits pair with coverage only when recorded source digests
agree; other audits get their own timeline entry. Repeated security audits of identical
files and source bytes share one entry showing the newest result; older snapshots
remain stored. Missing or failed assessments
remain explicit.

**Files** includes measured and security-assessed files with four marks in order:
Coverage, Assertions, Quality, and Security. Each column is sortable. Security
shows a finding count with an amber shield when findings exist, green for zero,
and gray when unassessed. Its **All files** action opens this list with the most
findings first. Cross-file findings appear below the list with links to both
endpoints, and each path contributes to the count on each involved file. The list is one
card with spacious rows and rounded hover highlights, without divider rules or
redundant file-status dots. Narrow lists stack paths above labeled metrics and
wrap concerns instead of scrolling sideways. On wide screens, paths appear on one line,
with the directory muted and the basename emphasized. Below about 40 pixels a
coverage ring drops its arc and carries state alone, because a missing sliver
thinner than a pixel would read as complete. Arriving from a kind on the
overview narrows the list to the files owing it, behind a scope the reader can
see and dismiss. The file view explains each unreached obligation's evidence
beside the source.
When the run has been assessed with `assertions assess`, the source view also
marks each assessed statement as asserted or not, and the file list shows how
many of a file's statements are asserted. One compact gutter button contains
both coverage and assertion symbols and opens an inline card with both kinds of
evidence: attributed tests, open obligations, measurement limits and decision
evidence for coverage; for assertions, whether a test is judged to catch a
change to the statement, which test, and a button to open it. A file with
statements that are not asserted lists them. An unassessed run says how to
assess it.

The **Improve** buttons open copyable, metric-specific prompts for Codex, Claude
Code, Gemini CLI, or another coding agent. The dialog shares supercov.com's
numbered steps and copy controls, with two options for each action. The native
command is detected from the recorded test command and source extensions.
Prompts contain the native Supercov command, project, saved run or assessment,
optional file path, current metric value, and next action. File prompts use the
file's value; overview prompts use the project value. Unavailable metrics are
labeled unmeasured or unassessed. This context appears in the prompt paragraph.
Coverage breakdowns are available for both the project and individual files,
with a circle chart and percentage for each coverage type.
The **New run** button beside
the view tabs opens a prompt to measure the current project, reusing the saved
test command when available. Neither button executes commands or requires report
generation. Security prompts include the saved audit ID and current findings,
and use the `npx supercov security` command.

File headers show Coverage, Assertions, Quality, and Security without repeating
the detailed counts. Below the source, Tests comes before Assertions and Quality,
with Security last. Unavailable security results remain labeled Not assessed.

A file's **Assertions** card previews three linked assertions and opens a searchable modal for all
of them. Selecting one jumps to its source evidence. The **Quality** card pairs
the gauge with descriptions from the assessment's property catalog and shows
each recorded property value beside its concern. **Tests covering this file** previews the three
tests reaching the most lines, with an **All tests** action into the filtered test
browser. These lists share the same flat row treatment.

A statement's coverage is recorded on the line where it starts. Lines that
continue it, such as the remaining arguments of a call or a block's closing
brace, take that line's state in the source view rather than reading as
non-executable; comments and blank lines inside a statement do not.
**Tests** uses inset, padded lists inside cards at the file and individual-test
levels. Evidence lists use explicit buttons and searchable modals rather than
disclosure dropdowns. It shows runner, kind, outcome, retry, and attributed
source information, and opens from any assertion that credits a file.

The address bar follows the reader: tab, timeline entry, file and line each
get a history entry, so the browser's back button retraces the path.

## Mutation testing

For a run whose mutants are listed (JavaScript and TypeScript, and in
preview Python, Rust, Go and Ruby; see [Mutation testing](mutation.md)), the
Overview's first card gains a **Mutants** figure: the mutation score once
`mutants test` gave verdicts, the weak mutation score before, and how many
mutants the tests leave alive. Below it, a **Mutation testing** card holds the
rest:

- **Weak mutation**: its score, the share of mutants a test infects, with how
  many mutants it is over and how many unknown or equivalent ones it leaves
  out, and the
  mutants by weak status (infected, not infected with any at a clock or random
  boundary, not reached, unknown, equivalent), with the score over the
  mutants a test reaches.
- **Strong mutation**: the mutation score, killed and timed-out over those and
  the survivors and uncovered, with how many mutants it is over (`of 12 of 40
  mutants`): a score of a few mutants is not the run's. Where no mutant got a
  verdict the score counts, the card says so and what they got instead (`No
  mutation score: none of the 28 mutants was killed, timed out, survived or
  left uncovered (28 errors)`). Every verdict is counted: killed, timed out,
  survived, no coverage, equivalent, unviable (does not compile), error, and
  not run for a language no driver runs yet (`not run: no driver for cobol`).
  Mutants with no verdict, after a pass narrowed by `--diff`, `--file` or
  `--shard` or one that stopped early, are counted too, and each reason a
  mutant has none is said once in plain words with how many share it. Before
  any pass the card says so and names the command that runs one.
- **By language**, when the run's mutants are of several: each language's
  mutants, weak score, mutation score and survivors.
- **Left alive**: the mutants a strong pass saw survive or no test cover, and,
  without a verdict, those weak mutation shows no test can kill (not infected
  or not reached), as the pass itself decides them. They are grouped by file,
  the files with the most first, five files of three mutants each, with
  **and N more** opening a searchable list of all of them. Each opens its line.

The **Files** list's **Mutants** column shows each file's score (the
mutation score where verdicts exist, the weak score otherwise) beside a mark
that warns when mutants are left alive; it sorts lowest score first, more
survivors first on a tie. In a file's source, a line with a mutant left alive
is marked with an orange ✱ and a line whose mutants are all killed or
settled with a green one. Its evidence lists each of the line's mutants: what
it changes, its weak status and reason, its strong verdict and why, the test
that killed it (a button to that test where the run recorded it), how many
tests ran against it and for how long, and its id. A survivor is set apart
with an orange rule. Below the source, **Mutants left alive** lists the file's
survivors and its counts and scores.

```sh
npx supercov report --diff origin/main
```

`--diff <base>` narrows the mutation testing to a change, as `runs <id>
mutants --diff` does: only the mutants on lines it added or changed since its
merge base with `<base>`, and the card says so, naming the base, the merge
base and how many changed lines hold a mutant. Coverage is not narrowed. A
change with no mutant on its lines says that.

A run that listed no mutants shows nothing of them. A plain run lists its
mutants the first time `mutants`, `mutants test` or `supercov mutants` asks.

## Choose runs and output

Up to ten recent runs are included by default. The selected run is compared with
the previous saved run automatically:

```sh
npx supercov report
npx supercov report <run-id>
npx supercov report latest --compare <older-run-id>
```

Include more local history or choose a destination with:

```sh
npx supercov report --runs 10
npx supercov report --output artifacts/coverage.html --no-open
```

A report includes the five newest runs; `--runs` accepts 1 through 20. Each
run adds to the file's size, and a large project's runs can take the report
past GitHub's 25 MB attachment limit; `--runs 1` makes the smallest one.

## Source integrity

Stored evidence always supplies immutable source locations and snippets. Full
authored source is added only when its current source fingerprint is identical
to the selected run. A test-only change can therefore mark a run stale while
its source remains safe to show. If authored source changed, the report labels
that run as stale and uses the stored snippets instead of displaying newer code
beside older coverage.

Individual source files over 1 MB and total source beyond 16 MB are omitted.
The report names its source mode and any omitted paths rather than silently
substituting content.

## Other formats

The HTML report and `runs report --format html` are the files that show coverage and
mutants together. For other tools:

- `runs <id> mutants --format stryker` writes StrykerJS's `mutation.json`,
  which mutation-testing-elements, the Stryker Dashboard and CI integrations
  read, and `--format pit` PIT's `mutations.xml`, for PIT's report aggregator
  and Sonar's PIT plugin.
- `runs <id> mutants --format markdown` writes a pull request's comment: the
  scores, the verdicts, and the survivors with `file:line:column`, the change
  and why.
- `runs report --format lcov|cobertura|istanbul|istanbul-summary|clover|jacoco|sonar`
  write coverage only: those formats have no place for a mutant.
- `runs report --format html` writes a static page, without the interactive
  report's application, that shows the run's mutation testing as this report
  does: the Mutation testing card with both levels, the reasons for no
  verdict, each language and the survivors by file (each linking to its
  line, the rest folded under "and N more"); a Mutants column with each
  file's score and survivors, sortable; and each line's mutants in the source
  view with both levels and the test that killed one. `--diff <base>` narrows
  its mutants to a change and the card says so.

See the [CLI reference](cli.md#mutation-reports-other-tools-read) for each.

## Privacy and sharing

The report includes source, test names, commands, and coverage evidence. Treat
it like any other development artifact and share it only with intended
reviewers. Supercov does not include environment variables or raw test output,
and the report has a content-security policy that blocks outbound connections.

For an ordinary pull request, attach the report file from
`.supercov/reports/` to the description or a comment. GitHub serves HTML attachments as downloads. The reviewer clicks
the attachment, downloads the single file, and opens it locally.
