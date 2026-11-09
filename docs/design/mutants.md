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
| `unknown` | reached, but the run cannot tell (no probe at that site, values it cannot compare, code that ran in a server process) | weak |
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
```

The run summary gets one line, `Mutants  2,955 · weak score 85.0% · 211 not
infected · 240 not reached · 206 unknown`. `gaps` points to `mutants --status
not-infected` when a file has some. Narrowing options (`--tag`, `--test-file`,
`--title`, `--kind`) narrow mutants as they narrow every query: a mutant only
other tests infect is not infected there.

`--stryker` reads one or more StrykerJS `mutation.json` reports and joins them
by id. Each mutant then shows StrykerJS's status beside Supercov's (`stryker:
Killed`), and the summary counts disagreements. A not-infected mutant StrykerJS
killed is a soundness violation, and the command exits non-zero. This is how
releases are validated, and how a team that already runs StrykerJS gets
Supercov's reasons for its survivors.

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
      "weak": { "reach": "reached", "infection": "not-infected", "reason": "a never equal to b" },
      "strong": null,
      "stryker": null
    }
  ]
}
```

`strong` stays `null` until a strong pass fills it with `{ status, tests,
durationMs }`. `stryker` holds the joined StrykerJS status when `--stryker`
was given. A reader that knows only `status` keeps working when strong results
arrive.

## What stays out

- Regex mutants are listed but stay `unknown`. Recording the regex inputs
  stored user strings in the evidence, and comparing JavaScript regexes needs
  a JavaScript engine. A later step compares the original and mutated regex in
  the page or process instead.
- Code that ran in a server process records hits but no values until those
  processes report outcomes too, so its outcome-based verdicts stay `unknown`.
- Equivalent mutants are not detected. They show up as `not-infected` with
  their reason, which is often what makes them recognisable.
