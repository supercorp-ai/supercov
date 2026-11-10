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
without, and wrote 2% more code; its `mutants.json` listed 58,000 mutants in
24 MB (2.5 MB compressed in the archive) as version 1 stored them, about
two thirds of that as version 2 does (see The stored list).

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
| `equivalent` | no input can make it differ; shown by a static rule (see Equivalent mutants) | static, before weak |
| `killed` | a test failed with the mutant in place | strong (reserved) |
| `survived` | every test that reached it passed with it in place | strong (reserved) |
| `timeout` | a test ran past its time with the mutant in place | strong (reserved) |

Strong statuses replace a weak one only when a strong pass ran for that mutant.
A weak status is never shown as `killed` or `survived`. `equivalent` is decided
when the mutants are listed, from the program alone, before reach and
infection, and wins over them: whatever the tests did, they could not have
made it differ. A strong pass has no need to run it.

## Scores

- **Weak mutation score:** `infected / (infected + not-infected + not-reached)`.
  Unknown and equivalent mutants are left out and counted beside it: an
  equivalent mutant is out of the denominator, as the mutation score has
  always left equivalent mutants out (DeMillo, Lipton and Sayward 1978;
  Offutt and Untch 2001). Also shown over reached
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
infected (12 at a clock or random boundary) · 240 not reached · 206 unknown ·
3 equivalent`, and its JSON `data.mutants` (the `summary` below with
`mutantSet`). The part in parentheses is there only when some are. A later step has `gaps` point to `mutants
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
The table has a column of equivalent mutants after the unknown ones.
`--json`, a file, `--status` and `--mutator` page through mutants, which are
always listed by file path and then by position, whatever the table's order.

A listed mutant shows its code as `original → replacement`. Where the
original is on several lines, or the two together pass 60 characters, the
start of the original says little: a MethodExpression mutant of
`candidates.filter(…).slice(0, maxUsers)` that removes the `slice` and one
that removes the `filter` both start `candidates .filter((c) =>…`. Such a
mutant is shown by the part it changes instead: the original and the
replacement without their longest shared start and end, each cut back to where
no word or operator is split, with `…` where the shared part is left out:
`….slice(0, maxUsers) → …` and `….filter((c) => c.ready) → …`. Only mutants
whose replacement is the original with one part changed have one (operators,
method names and removed calls, `!` and unary operators); an optional link's
part would be its `?` alone, and a block's or a literal's replacement is
written whole. A one-line listing (`file:line`) shows the original after it
(`in:`).
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
    "status": { "not-reached": 240, "not-infected": 211, "infected": 2297, "unknown": 204, "equivalent": 3 },
    "untestable": 12,
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
    },
    {
      "id": "app/route.ts:687:7-689:25:MethodExpression:removed",
      "original": "candidates .filter((c) => c.ready) .slice(0, maxUsers)",
      "replacement": "candidates .filter((c) => c.ready)",
      "changed": { "from": "….slice(0, maxUsers)", "to": "…" },
      "status": "not-infected",
      "weak": { "reach": "reached", "infection": "not-infected", "code": "replacement-same", "reason": "…" }
    },
    {
      "id": "lib/quota.ts:12:21-12:26:EqualityOperator:>=",
      "original": "n > 0",
      "replacement": "n >= 0",
      "status": "equivalent",
      "weak": { "reach": "unknown", "infection": null, "code": "zero-guarded", "reason": "`>` and `>=` differ only where `n` is 0, …", "rule": "zero-guarded" }
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
`unknown` where no statement or function point contains the mutant, and for
an equivalent mutant, which is decided before reach; `infection` is null for a
mutant not reached or equivalent. An equivalent mutant's `weak.rule` is the
rule's code, which is also its `weak.code`. `weak.untestable` (`clock` or
`random`) and `weak.untestableReason` are a hint beside the status (see
Equivalent mutants). `summary.untestable` counts the not-infected mutants with
that hint. `changed` is there only where it is what a reader is shown
(above). `original` and `replacement` are
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
`stryker` and `reason`. An equivalent mutant StrykerJS killed is one too: the
rule that called it equivalent is wrong. `survivorsExplained` counts StrykerJS's matched
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

## The stored list

A run's mutants are archived as `mutants.json`: each file's mutants with the
evidence each is linked to. Version 2 leaves out what reading it works out
again. A mutant's `id` is its file, span, mutator and descriptor, and is
written only where it is not exactly those. Of a long `original` (a block
statement's is a whole function body) it keeps what is shown of it: the
start of the whole, or of each operand around an operator, up to the first
121 characters once whitespace is flattened, with `operator.at` moved to
where the operator is in what is kept. A mutant's line, its replacement's and
every reason that quotes its code are cut from no more than that. The writer
keeps the whole original wherever a shorter one would not show exactly the
same, as where the replacement is written by parsing all of it (a method
call's) or from its end (an optional link's). The run's sources are not in
the archive, which keeps only their hashes, so nothing is read back from
them. Version 1, with every id and original whole, is read as it was.

A probe or comparison site in the mutant's own file is written from its line
too (`:3:9` for `src/a.ts:3:9`). On the engine's fixtures (the StrykerJS
parity files and the runtime's own modules, 12,008 mutants) version 1 took
4.55 MB (471 KB gzipped) and version 2 takes 2.92 MB (329 KB): 243 bytes a
mutant, of 379. At that ratio a large app's 61,000 mutants, 414 bytes each in
version 1, come to about 16 MB (1.8 MB compressed), and less where its longer
originals (JSX under `&&`) are cut.

Version 3 adds to a mutant what the program as written says of it:
`equivalent` (`{ rule, reason }`) where a static rule shows it equivalent, and
`untestable` (`{ rule: "clock" | "random", reason }`). A version 2 file has
neither, and its mutants are read as having none.

## Equivalent mutants

A mutant no input can tell from the original is equivalent (DeMillo, Lipton
and Sayward 1978); no test can kill it, and the mutation score leaves it out.
Deciding equivalence is undecidable in general, but some cases are shown
cheaply from the code: Offutt and Pan (1997) by constraints, Papadakis et al.
("Trivial Compiler Equivalence", ICSE 2015) by comparing compiled code.
Supercov calls a mutant `equivalent` only by a rule that holds for **every**
input under JavaScript's semantics. A rule that holds for the inputs a program
usually sees, or that relies on TypeScript's types (which nothing checks at
run time: JSON, `any`, a cast), is not one. Each rule has a stable code, set
when the mutants are listed and saved in `mutants.json`.

### `zero-guarded`

An EqualityOperator mutant that moves a boundary at 0 (`n > 0` to `n >= 0`,
`n <= 0` to `n < 0`, and `0 < n` to `0 <= n` and the like) where an earlier
operand of the same `&&` chain is `n` itself (`n && … n > 0`), or of an `||`
chain is `!n` (`!n || … n > 0`), and `n` is a `const` whose initializer is
always a number or a bigint.

Why it is sound: the two comparisons differ only where `n` compares as 0. The
earlier operand ran first and found `n` truthy, and a `const` has the same
value whenever it is read. A number is truthy exactly where it is neither 0,
-0 nor NaN, and a bigint where it is not 0n, so `n` is not 0 there. The chain
may hold only `&&`, `||`, parentheses and type-only wrappers between the
operand and the comparison; anything else (a call, an arrow) ends it.

Why it is narrower than "`x && x > 0`": a truthy value can compare as 0. The
strings `"0"`, `" "` and `"0x0"`, an empty array, `new Date(0)` and an object
whose `valueOf` returns 0 are all truthy, and `"0" > 0` is false while
`"0" >= 0` is true. So the guarded identifier must hold a number or a bigint
for every input, which the code shows only for a `const` initialized by a
numeric literal, unary `-`, `+` or `~`, an arithmetic or bitwise operator, a
`+` of two such, or a conditional or sequence of them. A parameter, a `let`, a
member path (whose getter may answer differently on each read, and whose value
nothing types at run time), a call (`Number(s)` can be shadowed) or a `+` with
a string are not. And the `const` must be the file's only binding of its name,
the comparison inside the block (or function body: a parameter's default does
not see it) that declares it, and the file must hold no `with` and no
reference to `eval`, which can bind a name no declaration shows. On the validation app,
the guards the triage found (`data?.count && data.count > 0`)
read a member path of data loaded from the database: equivalent in practice,
not by this rule.

### Considered and left out: `email-input-trim`

The idea: a MethodExpression mutant that removes `.trim()` from the value of a
controlled `<input type="email">`, whose state only `e.target.value` sets. The
HTML Living Standard's value sanitization algorithm for the E-mail state strips
newlines and leading and trailing **ASCII whitespace** (tab, line feed, form
feed, carriage return, space). `String.prototype.trim` strips every
ECMAScript WhiteSpace and LineTerminator: also U+00A0 (no-break space), U+FEFF,
U+1680, U+2000 to U+200A, U+2028, U+2029, U+202F, U+205F, U+3000 and U+000B.
The sanitizer keeps those, and a user can type or paste them (Option-Space
types U+00A0 on a Mac; text copied from a web page often carries one): the
input's value is then `"a@b.co\u00a0"`, which `trim` changes and the mutant
does not. The rule is not sound for every input, and there is no narrower
claim of the same rule that is, so it is not implemented. Of the validation app's 61
equivalent mutants triaged by hand, the six `type="email"` ones are practically
equivalent, not equivalent.

### Untestable boundaries: a hint

Some mutants are not equivalent but no test can practically tell them apart. A
boundary mutant (`<` and `<=`, `>` and `>=`) differs from the original only
where its operands are equal; where an operand reads the clock or a random
number, that is one exact reading. Such a mutant carries `untestable`:

- `clock`: an operand is a clock reading. A reading is `Date.now()`,
  `performance.now()` or `new Date()` with no arguments; arithmetic (`+`, `-`,
  `*`, `/`) of readings with readings, number literals, names or member paths;
  `-x` or `+x` of one; `.getTime()`, `.valueOf()`, `Number()` or `new Date()` of
  one; a date-fns `add*`, `sub*`, `differenceIn*`, `startOf*` or `endOf*` of
  one; a `??` or `||` fallback to one (`params.now ?? new Date()`); and, up to
  two hops, a name whose `const` in a function around the comparison is
  initialized by one, or a call of a function the file declares that returns
  one (`msRemaining()` with `() => budget - (Date.now() - startedAt)`).
- `random`: the same with `Math.random()` as the reading.

The operand must be time-valued, not merely worked out from the clock: a
count, a found element or a property read of something that depends on the
time (`today && today.items.length > 0`, where `today` is
`groups.find((g) => g.isToday)`) is not a reading, and its boundary is
testable with the right data. A first version followed any use of the clock
in an operand's code and marked that one wrongly on the validation app.

It is a hint, not a status: the mutant keeps its status and counts in both
scores, since a test that fakes the clock (or random) can reach the boundary.
The listing shows `(clock boundary)` or `(random boundary)` after a
not-infected mutant's reason, and the summary counts them in its not-infected
part.

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
- Equivalent mutants beyond the static rules above are not detected. They show
  up as `not-infected` with their reason, which is often what makes them
  recognisable.
