# Mutation reports: the HTML report and the formats other tools read

Every report reads a mutant the same way, from `mutants`' own entries
(`mutants_query::MutantEntry`, its `strong` verdict included), through
`mutation_report.rs`: each mutant's strong outcome, why it has it, whether the
tests leave it alive, and the counts and scores of some mutants. Nothing in
it knows a language; a mutant's language is read from its file's extension,
as the mutation testing report schema names languages.

## Outcomes

A saved verdict's status is the outcome: `killed`, `timeout`, `survived`,
`no-coverage`, `equivalent`, `unviable` and `error`. An `error` whose reason is
the one `mutants test` saves for a language without a driver (`no strong
mutation driver for <language> yet`) is `not-run`, so a report can count
those apart and say `not run: no driver for <language>`. A mutant with no
verdict is `pending`.

A mutant is **left alive** when a pass saw it survive or no test cover it, or,
with no verdict, when weak mutation shows no test can kill it (not infected
or not reached): the strong plan decides those two `survived` and
`no-coverage` without running them (`strong_plan::Decided`), so a report says
the same before and after a pass. An infected or unknown mutant with no
verdict is not counted alive: no test has been run against it.

Why, in plain words: the verdict's own reason where the driver gave one;
otherwise a sentence per outcome (`every test that reached it passed with it
in place (2 tests ran)`, `<test> failed with it in place`); for a pending
mutant the weak reason, and for an infected or unknown one that no strong
verdict exists yet, worded as left out by the pass when a pass gave other
mutants verdicts.

The mutation score is StrykerJS's, `(killed + timeout) / (killed + timeout +
survived + no-coverage)`, as `mutants test` reports it.

## The HTML report

`supercov report` builds each run's mutants from the same query `mutants`
answers, with every mutant (not a page) and the run's saved verdicts, and
with `--diff <base>` the change's (`mutant_diff`, as `mutants --diff`). The
payload holds the weak summary, a tally (weak counts, strong counts,
survivors, both scores) of the run, of each file and of each language, the
diff when given, and every mutant with its change, weak status and reason,
outcome, killer, tests run, duration, why and whether it is left alive. The
page then draws:

- the Overview's Mutants figure: the mutation score once verdicts exist, the
  weak score before;
- a Mutation testing card: both levels' scores and counts, the reasons for
  no verdict grouped and counted, a table by language when there are several,
  and the survivors grouped by file, five files of three, with the rest in a
  searchable list;
- the Files list's Mutants column: each file's score, sorted lowest first;
- the source view: a mark on each line with mutants (orange where one is
  left alive), and in the line's evidence each mutant's two levels, killer
  (opening the test where the run recorded it) and id;
- below a file's source, its survivors.

A plain run's mutants are in the report once they are listed: the report
reads, and never lists them itself.

## StrykerJS's `mutation.json`

The mutation testing report schema, version 2, as published in
`mutation-testing-report-schema` 3.9.0. Each file needs its full source,
read from the checkout; a file it no longer holds is left out and named.
Outcomes map to the schema's statuses: unviable is `CompileError`, error
`RuntimeError` (both left out of the score there too), not run and pending
`Pending`, equivalent `Ignored`. A pending mutant not reached is `NoCoverage`
and one not infected `Survived`, as the plan decides them. `statusReason` is
the plain why; `killedBy`, `testsCompleted` and `duration` come from the
verdict. Locations are the mutant's span, 1-based, as StrykerJS's.

Checked by `scripts/mutation-report-formats.mjs` (an end-to-end gate): a run
of two languages with every outcome validates against the published schema
with Ajv, and mutation-testing-elements 3.9.0 renders it in Chromium, its
files' table and a file's source with its mutants.

## PIT's `mutations.xml`

PIT's statuses map one to one for killed, timed out, survived, no coverage
and unviable (`NON_VIABLE`). An error and a mutant not run are `NOT_STARTED`:
PIT's `RUN_ERROR` counts as detected, which would raise the score of a
mutant that never ran. Equivalent mutants are left out, as PIT has no status
for them. PIT names a mutant by class, method, descriptor and instruction
index; Supercov mutates source, so the class is the file's path after a
`java` or `kotlin` source root, dotted, the method and descriptor are empty,
and the index is the mutant's place in its file, which keeps each mutant's
identity distinct. Checked by the same gate with PIT 1.17.1's own reader,
`MutationResultDataLoader`, which PIT's report aggregator reads
`mutations.xml` with: every mutant is read back, with every status.

## Markdown

Unchanged before a pass. Once verdicts exist, a second line gives the
mutation score and each verdict's count, and the table lists the survivors,
survived and uncovered first, with both levels and why; `--status` still
picks by weak status.

## Coverage formats

`runs report --format lcov|cobertura|istanbul|clover|jacoco|sonar|html` stay
coverage only: none of those formats has a place for a mutant.
