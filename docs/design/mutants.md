# Mutants: weak mutation now, strong mutation later

Supercov should report, for every mutant StrykerJS would make, how far the
run's tests got toward killing it, from the one ordinary run it already
measures. Today that is weak mutation (Howden 1982): did a test reach the
mutated code, and did the mutant's value differ there. The report and its JSON
are shaped so that strong mutation (running the tests against mutants, as
StrykerJS does) can later fill in the last step without changing either.

This record covers JavaScript and TypeScript.

## The model: reach, infection, propagation

Each mutant moves through the steps of the RIPR model (Ammann and Offutt,
*Introduction to Software Testing*; DeMillo and Offutt 1991; Voas's PIE, 1992):

| Step | Question | Who answers it |
| --- | --- | --- |
| Reach | Did a test run the mutated code? | this run (statement and branch hits) |
| Infection | Did the mutant's value differ from the original's there? | this run (probes record the original's values; Supercov evaluates the mutant's) |
| Propagation and revealing | Did the difference reach the output, and did an assertion fail? | strong mutation only: running the tests against the mutant |

Weak mutation answers the first two exactly or says it cannot. A mutant that no
test infects cannot be killed by these tests (weak live implies strong live), so
"not infected" is a certain survivor, with a reason that says which input is
missing. "Infected" is a candidate kill, not a kill: on the app it was validated on, 17% of
infected mutants still survived StrykerJS.

## Which mutants

Supercov enumerates the mutants itself while it instruments, with the mutators,
replacements and skip rules of a pinned StrykerJS version (10.1.1), so counts
compare with StrykerJS and can be checked against its reports. The version is
named in every report (`mutantSet: "stryker-js@10.1.1"`). A script diffs the
enumeration against StrykerJS's own instrumenter, and the release keeps that
diff empty.

A mutant's id is stable across runs: the file, its span, the mutator and the
replacement. A later strong pass can reuse a result while the file's hash and
the tests that reach it are unchanged.

### On by default

Every JavaScript and TypeScript run places the probes and lists its mutants;
`SUPERCOV_MUTANTS=0` turns both off. The research switches they replace,
`SUPERCOV_COMPARISONS=1` and `SUPERCOV_INFECTION_PROBES=1`, are gone rather
than kept as aliases: each turned on one group of probes, which is now on,
and no switch turns one group off alone. The probes measured about 0% of run
time on a large Next.js app's unit, integration and browser e2e suites. Instrumenting
its 511 source files took 0.56 s of one core with them, against 0.42 s
without, and wrote 2% more code; its `mutants.json` lists 58,000 mutants in
24 MB (2.5 MB compressed in the archive).

Each probe's runtime helper (`compareV2`, `arithV2`, `assignV2`, `methodV2`,
`valueV2`, `regexV2`) is named only once the file has a site of that kind,
after every other name the file's allocator gives. A file with no site of a
kind imports nothing for it, and the names of everything else in it are those
it has with the probes off: a file with no site at all is instrumented byte
for byte as with `SUPERCOV_MUTANTS=0`.

The regex probe stays off unless `SUPERCOV_REGEX_INPUTS=1`. It keeps up to 200
characters of every string a regular expression is matched against, which is
user input (emails, names, URLs a test typed) written into the run's evidence
and archived with it. No regex mutant is enumerated yet, so nothing uses
those inputs; a later step can compare the original and the mutated regex in
the page or process instead of storing what they matched.

### Ids and descriptors

An id is `file:startLine:startCol-endLine:endCol:Mutator:descriptor`, with
1-based lines and 1-based columns in UTF-16 code units, as StrykerJS's
`mutation.json` reports them. The descriptor names the mutant within its span
and mutator without printing code, so StrykerJS's mutants can be joined by id
from their replacement code:

| Mutator | Descriptor |
| --- | --- |
| ArithmeticOperator, EqualityOperator, LogicalOperator, AssignmentOperator, UpdateOperator | the new operator (`<=`, `&&`, `-=`, `--`) |
| UnaryOperator | the new operator (`+`, `-`), or `removed` for `~x` to `x` |
| BooleanLiteral | the new literal (`true`, `false`), or `removed` for `!x` to `x` |
| ConditionalExpression | `true` or `false` (a test, or a `for(;;)` given a false test), or `empty` for a switch case emptied |
| StringLiteral | `empty`, or `Stryker was here!` for an empty string or template |
| MethodExpression | the new method name (`startsWith`), or `removed` for `x.trim()` to `x` |
| OptionalChaining | the link that loses its `?`: `.name`, `[]` for a computed member, `()` for a call |
| ArrowFunction | `undefined` |
| BlockStatement, ObjectLiteral | `{}` |
| ArrayDeclaration | the replacement: `[]`, `["Stryker was here"]`, `Array()`, `Array([])`, `new Array()`, `new Array([])` |
| CallExpression | `empty` (the statement removed) |

Regex mutants (weapon-regex's) are not enumerated yet and have no descriptor.
The grammar is implemented twice: in `scripts/stryker-parity.mjs`, which
prints StrykerJS's own mutants with their descriptors and writes the goldens,
and in the engine's `descriptor_from_stryker`. The engine's enumeration is
tested against those goldens: same mutants, order, spans and descriptors.

## Status

Each mutant has one `status`, the furthest step known, and the per-step facts
behind it:

| status | meaning | set by |
| --- | --- | --- |
| `not-reached` | no test ran the mutated code | weak |
| `not-infected` | reached, but every evaluation gave the mutant the original's value | weak |
| `infected` | some evaluation gave the mutant another value | weak |
| `unknown` | reached, but the run cannot tell (no probe at that site, values it cannot compare, code that ran in a server process whose records carry no values) | weak |
| `killed` | a test failed with the mutant in place | strong (reserved) |
| `survived` | every test that reached it passed with it in place | strong (reserved) |
| `timeout` | a test ran past its time with the mutant in place | strong (reserved) |

Strong statuses replace a weak one only when a strong pass ran for that mutant.
A weak status is never shown as `killed` or `survived`.

## Scores

- **Weak mutation score:** `infected / (infected + not-infected + not-reached)`.
  Unknown mutants are left out and counted beside it. Also shown over reached
  mutants only: `infected / (infected + not-infected)`, as StrykerJS shows a
  score over covered code.
- **Mutation score** (reserved for strong): `killed / (killed + survived + not-reached)`,
  with timeouts counted as killed, as StrykerJS does. It appears only once a
  strong pass ran, and is never estimated from weak results.

Both are shown beside coverage and never change a coverage figure.

## Command line

Reading never runs anything, as with `assertions`:

```sh
npx supercov runs latest mutants                      # per-file table and the scores
npx supercov runs latest mutants app/checkout.ts      # that file's mutants, by line
npx supercov runs latest mutants app/checkout.ts:42   # one line's mutants and their reasons
npx supercov runs latest mutants --status not-infected,not-reached
npx supercov runs latest mutants --mutator EqualityOperator
npx supercov runs latest mutants --json
npx supercov runs latest mutants --stryker reports/mutation/mutation.json   # join StrykerJS results
npx supercov runs latest mutants --stryker reports/mutation                 # every report in a directory
npx supercov runs latest mutants --format jsonl --output mutants.jsonl      # every mutant, one per line
```

The run summary gets one line, `Mutants  2,955 · weak score 85.0% · 211 not
infected · 240 not reached · 206 unknown`, and its JSON `data.mutants` (the
`summary` below with `mutantSet`). A later step has `gaps` point to `mutants
--status not-infected` when a file has some. Narrowing options (`--tag`,
`--test-file`, `--title`, `--kind`, `--runner`) narrow mutants as they narrow
every query: a mutant only other tests infect is not infected there. The
verdicts are worked out when a run's index is built, from every test it keeps,
so a kind or a runner picks its tests the way a tag does (a filtered index).
`--filter passed` and `--filter failed` are refused: the verdicts are of every
attempt, and an outcome view of them is not built.

Without a file, people see a table of files, a page of files at a time, ordered
by how many mutants a test reaches and does not infect, then by how many no
test reaches, then by name. Not-infected mutants are what only this report
shows; a not-reached mutant is a coverage gap `gaps` names already. A file no
test ran at all (every mutant not reached) comes after the files a test ran,
however many mutants it holds, and one line counts those files: listed by size,
the files a suite never loads buried the files with mutants not infected.
`--json`, a file, `--status` and `--mutator` page through mutants, which are
always listed by file path and then by position, whatever the table's order.
`--format jsonl` writes all of them, one per line in that order, each as
`--json` lists it, as `gaps --format jsonl` does for gaps: a reply stops at 64
KB, about 128 mutants, and a large app has 61,000. The scores and counts are of
the mutants asked about: the run, a file or a line, and one mutator with
`--mutator`; `--status` picks from them without changing the scores.

`--stryker` reads one or more StrykerJS `mutation.json` reports and joins them
by id. One `--stryker` takes every report after it (the words that are a
`.json` file or a directory, as a glob gives them), and a directory stands for
every `.json` file under it. The commands a reply suggests repeat what was
given, up to three reports or directories, and otherwise write `<56 reports>`
with a line saying what it stands for: users with chunked reports pass dozens,
and every suggested command repeated them all. Each mutant then shows
StrykerJS's status beside Supercov's (`stryker: Killed`), and the summary
counts disagreements. A not-infected or not-reached mutant StrykerJS killed (or
timed out) is a soundness violation, and the command exits 1. This is how
releases are validated, and how a team that already runs StrykerJS gets
Supercov's reasons for its survivors.

A StrykerJS mutant's id is built from its file (relative to the project root,
or to `--stryker-root`), its location, and the descriptor
`descriptor_from_stryker` reads from its replacement and the original code the
report's `source` holds. Several reports merge by status: Killed or Timeout
over Survived over NoCoverage; Ignored, CompileError, RuntimeError and Pending
are no result and are dropped. The join is of the mutants asked about (a file,
a line, a mutator).

Reserved for strong mutation, not built now:

```sh
npx supercov runs latest mutants test                 # run tests against infected mutants, save statuses
npx supercov runs latest mutants test --changed       # only mutants on changed lines
```

`mutants test` will save results to the run, as `assertions assess` does, and
`mutants` will then show strong statuses. It can be much cheaper than StrykerJS
because the weak pass already knows:

- which mutants need running at all: only infected ones, since not-reached and
  not-infected mutants cannot be killed (Just, Ernst and Fraser, ISSTA 2014,
  prune the same way);
- which tests to run for each mutant: only those that infected it;
- in what order: the tests that infected it, cheapest first, stopping at the
  first failure.

## JSON

`mutants --json` is the agent envelope's `coverage.mutants` command:

```json
{
  "mutantSet": "stryker-js@10.1.1",
  "levels": { "weak": true, "strong": false },
  "summary": {
    "mutants": 2955,
    "status": { "not-reached": 240, "not-infected": 211, "infected": 2297, "unknown": 207 },
    "weakScore": 0.835,
    "weakScoreReached": 0.916,
    "mutationScore": null
  },
  "mutants": [
    {
      "id": "server/services/billing.ts:42:9-42:14:EqualityOperator:<=",
      "file": "server/services/billing.ts",
      "span": { "start": [42, 9], "end": [42, 14] },
      "mutator": "EqualityOperator",
      "original": "a < b",
      "replacement": "a <= b",
      "status": "not-infected",
      "weak": { "reach": "reached", "infection": "not-infected", "code": "operands-never-equal", "reason": "a never equal to b" },
      "strong": null,
      "stryker": null
    }
  ],
  "stryker": null
}
```

Beside this shape the reply has `run`; `file` and `line` when one was asked;
and `note` when there are no mutants, saying why and how to get them (the run
did not list them: measure it again without `SUPERCOV_MUTANTS=0`). `weak.code` is a
stable code for the reason, and `weak.proxy` is `true` for an infection that is
only that the code around it ran (a block or a call removed). `reach` is
`unknown` where no statement or function point contains the mutant;
`infection` is null for a mutant not reached. `original` and `replacement` are
on one line, cut at 120 characters; `replacement` is the code with the mutant
in place where it is simply written from the original and the descriptor
(operators, literals, method names, optional links), and the descriptor
otherwise.

With `--stryker`, the top-level `stryker` is the join:

```json
"stryker": {
  "reports": ["reports/mutation/mutation.json"],
  "matched": 2700,
  "onlySupercov": { "total": 3, "byMutator": { "UnaryOperator": 3 } },
  "onlyStryker": { "total": 12, "byMutator": { "Regex": 12 } },
  "soundnessViolations": { "total": 0, "mutants": [] },
  "survivorsExplained": { "survivors": 420, "explained": 300 }
}
```

`soundnessViolations.mutants` lists up to 50, each with `id`, `status`,
`stryker` and `reason`. `survivorsExplained` counts StrykerJS's matched
survivors (Survived, NoCoverage) and those this run says no test infects or
reaches.

`strong` stays `null` until a strong pass fills it with `{ status, tests,
durationMs }`. `stryker` holds the joined StrykerJS status when `--stryker`
was given. A reader that knows only `status` keeps working when strong results
arrive.

## Values from server processes

A test process of Vitest, Jest or Mocha, and a Playwright page, hands over a
snapshot per test: its hits, decision vectors, and the outcomes its
comparisons, selections and probes saw. A process outside the test process
-- an app server an end-to-end test drives -- and the test process of
node:test, AVA and tap write records instead, one per first hit or decision
vector in each context (a test's attempt and phase, or the process's
background). They write the values the same way:

```json
{"type":"values"}
{"type":"comparison","id":"src/a.ts:3:9","outcome":"lt","timestampMs":1793741378313}
{"type":"logical","id":"0f3c…","outcome":"RT","timestampMs":1793741378313}
```

A `comparison` record is one outcome of a comparison or infection probe site
(`lt`, `eq`, `same`, `differs`, ...), a `logical` one a selection's: `S` or
`R` for the short or the right side, `F` or `T` for a falsy or truthy value.
Each is written once per distinct outcome per context, as a hit is, and is
credited to a test as the hits around it are: by the attempt a request
names, or by time. Reading a run folds a test's value records into one
snapshot of its values, so the verdicts read them as a test process's.

`{"type":"values"}` is each writer's first record: the first line it writes
to an attempt's file (with the attempt's scope), and the first of its
background records. It says the writer records values, so its silence about
one is a snapshot's: a comparison with no record there never ran there. It
goes with the writer's work to each test that work is credited to. A test's
hits from records without it were written by a runtime older than value
records, and a not-reached or not-infected verdict that reads values, at a
mutant whose statement or function such a test ran, stays `unknown`
(`server-process`).

The note is per test, not per process: a test credited with the work of two
processes, one of them measured by an older Supercov (a server that outlived
an upgrade and was reused), is read as if both recorded values.

## Merged runs

A run's mutants are archived as one `mutants.json`. `supercov merge` writes
exactly one for the merged run, as it writes one `manifest.json`, instead of
keeping each run's under `shards/<n>/` (which the reader, looking for the one
at the top, did not see: merged runs had no mutants). The list is the union by
file of the runs that listed mutants, each file's from the run whose manifest
the merge takes (the latest, where the tests grew) or else the first that
listed it. Runs that recorded a file with the same bytes enumerate the same
mutants of it, so where two such runs listed different mutant ids for it, or
one listed it and the other did not, the merge is refused; the same mutant set
(`stryker-js@10.1.1`) is required too. A file recorded with other bytes keeps
the first run's list.

A run that listed no mutants (measured with `SUPERCOV_MUTANTS=0`, or before mutants
were listed) adds no files, and its tests still count: their hits say what ran.
They recorded no comparison or probe values, though, so their silence is not
evidence. Their ids go in the merged list's `testsWithoutValues`, and a verdict
of not-reached or not-infected that reads values, at a mutant whose statement
or function those tests ran, is `unknown` (`values-not-recorded`), as for code
that ran in a server process. A test id that is in both kinds of run is counted
as one without values.

`statement-exclusions.json` is still kept once per run under `shards/<n>/`;
the reader requires every copy to be the same, which runs of one source are.

## What stays out

- Regex mutants are not enumerated. Recording the regex inputs stores user
  strings in the evidence (only with `SUPERCOV_REGEX_INPUTS=1`), and comparing
  JavaScript regexes needs a JavaScript engine. A later step compares the
  original and mutated regex in the page or process instead.
- Code that ran in a server process measured by a runtime older than value
  records (its records carry no `values` note): its outcome-based verdicts stay
  `unknown`. A server process, and node:test, AVA and tap's test processes,
  record values now (see Values from server processes).
- A server process killed in the middle of an event-loop turn loses that
  turn's records, values among them, as it loses its hits. A statement whose
  hit was written in an earlier turn and whose comparison ran in the lost one
  reads as if the comparison never ran there.
- Equivalent mutants are not detected. They show up as `not-infected` with
  their reason, which is often what makes them recognisable.
