# Understanding assertions

Coverage tells you which code ran. Assertion evidence helps you investigate
what the tests checked about that code, and where that relationship is still
unknown.

Use it after a coverage run to choose a useful assertion to add or to review
the tests around a proposed change. It is not a percentage of code that is safe
to change.

## Start with the suite you already have

```sh supercov
npx supercov -- npm test
npx supercov runs latest assertions --limit 5
npx supercov runs latest assertions --file src/core.mjs --limit 5 --json
```

Use your project's complete test command and actual source path. Supercov
analyzes the stored run and its matching source after the tests finish. The
query does not execute mutants or rerun application code. It uses the existing
statement and assertion-phase instrumentation; querying adds no test-time probes.

This source-to-assertion analysis is available for JavaScript and TypeScript
through the npm launcher, with Node's test runner and Vitest exercised. It is
separate from structural coverage and assertion-phase linkage in other runners
and languages.

The project must provide a compatible TypeScript compiler API, even when its
source is JavaScript. TypeScript 5.8.3 and native 7.0.2 are tested; native 7.0.2
requires Node 22.12 or newer and its platform-specific compiler package. If you
add a compiler dependency, rerun the suite before querying. Do not downgrade an
application's compiler to make a report work. See the
[requirements](assertion-evidence.md#requirements) for the complete boundary.

## Executed is not the same as checked

Consider this function in `src/core.mjs`:

```js
export function exact() {
  return 4;
}
```

Each of these assertions calls it, but they check different things. Here,
`assert` is imported from `node:assert/strict`:

```js
assert.equal(exact(), 4);
assert.ok(exact());
assert.equal((exact(), 7), 7);
```

| Assertion | What it checks about the returned value |
| --- | --- |
| `assert.equal(exact(), 4)` | The result equals the number `4`. Returning `5` would fail this assertion. |
| `assert.ok(exact())` | The result is truthy. Returning `5` would still pass. |
| `assert.equal((exact(), 7), 7)` | Nothing: the comma expression discards the result and compares `7` with `7`. |

Those conclusions concern the returned value. A call can also throw, change
state, or produce output. Discarding its return does not mean removing the call
is safe, and checking one return does not test every input or side effect.

Likewise, a log-count assertion checks how many calls occurred, not their
arguments. A substring assertion checks for that substring, not the entire
message. Reaching a passing assertion later on the same thread does not, by
itself, establish either relationship.

## Read a result as an investigation

The report inventories source sites, such as decisions, returns and effects,
and attaches candidate results and supporting evidence. These are not a count
of assertion statements or a line-by-line safety certificate.

| Candidate | How to use it |
| --- | --- |
| `evident` | The analyzer found supporting observations under its rules. Inspect the predicate and source path before relying on them. |
| `presence` | Evidence supports presence or truthiness, not the value distinctions you might care about. |
| `partial` | Some relevant evidence is present, but the site's modeled obligations are not all satisfied. Read the remaining reason. |
| `unresolved` | A test gap or analysis limit remains. The reason tells you which. |

Reasons beginning with `gap:` describe missing execution or checks under the
analyzer's model. Reasons beginning with `limit:` mean it could not establish
the relationship. A limit is not proof that the test checks nothing; a candidate
gap still deserves source review before you write a test.

Open a site and follow its evidence:

```sh supercov-example
npx supercov runs '<run-id>' assertions --site '<site-id>' --json
npx supercov runs '<run-id>' assertions --evidence '<returned-pointer>' \
  --analysis '<analysisId>' --json
```

Look for the exact passing assertion, its owning test attempt, the value or
effect it observes, and any transformation between that observation and the
source site. A test passing overall is not a substitute for that assertion's
own passing witness.

Use the run id and `analysisId` returned by the first query while paging.
Follow `pagination.nextOffset` and returned evidence pointers; a short page or
`detailOnly: true` does not mean evidence was dropped. The
[evidence reference](assertion-evidence.md#pagination-and-oversized-records)
explains how to inspect large records.

## Choose a test, a hint, or a limit

After reading the code and evidence, choose the next step:

1. **The behavior never ran.** Add a realistic scenario that reaches it and
   checks its result. An assertion hint cannot create an MC/DC execution pair.
2. **The behavior ran, but its relevant result was not checked.** Add or
   strengthen a real assertion. Prefer a public outcome over internal details.
3. **An existing assertion checks it, but the analyzer cannot follow the path.**
   Try a supported inline hint. Keep the assertion itself unchanged.
4. **The relationship is still unsupported.** Keep the analysis limit visible.
   Do not rewrite a good test solely to earn a stronger classification.

Subprocesses are a useful example of the fourth case. A helper can collect
output from the wrong child, select only a history window, transform a buffer,
or finish before a pipe is fully captured. Recognizing a method called `ready`
or a passing substring check does not establish which production behavior it
observed. Some of these relationships remain unresolved even with a hint.

## Point to an existing assertion

An optional `observes` comment names the behavior you believe the next assertion
checks:

```js
// observes: src/core.mjs#exact return 4
assert.equal(exact(), 4);
```

Use a project-relative source path, the exact function or owner name after `#`,
and, when needed, a literal source substring to select one inventory site. Put
the comment immediately before a statement containing one recognized assertion.
No separate contract file is needed.

After editing the test, rerun and inspect the hints:

```sh supercov
npx supercov -- npm test
npx supercov runs latest assertions --pragmas --json
```

The hint's origin remains `user-suggested`. Its separate validation result is
`analyzer-supported`, `unresolved`, or `invalid`. Support requires target
validation, the selected assertion's own passing witness, and a connection the
analyzer supports. Naming a function cannot make a discarded result observed.

Hints are comments: invalid or unsupported guidance affects the analysis, not
whether the test passes. They cannot supply a missing assertion, borrow another
assertion's witness, remove sites from the denominator, or override a limit by
declaration. Supported hints retain the analysis model's assumptions.

## Ask a specific change question

Some hints also select a bounded source check using a suffix such as
`; check missing call`, `; check count`, `; check value`, or `; check completion`.
Each recipe supports particular source shapes and precisely defined changes;
the suffix is not a general request to prove the named code safe.

For example, `check missing call` can ask whether omitting a supported console
callback's body would make an existing call-count assertion reject the result.
It does not ask whether changing the message would be detected.

Read the selected change, model, assumptions and per-variant result together:

- `rejected` means the modeled change would be rejected by the selected
  assertion within that model.
- `not-rejected` means that assertion would accept it. Another assertion, later
  test, or side effect might still cause the suite to fail.
- An unresolved variant remains unknown even when another variant is checked.

These source models are bounded and trusted, not formally verified interpreters
for arbitrary JavaScript. Checked hints are reported separately; they do not
promote ordinary candidates or manufacture coverage. See the
[hint reference](assertion-evidence.md#optional-assertion-hints) for supported
shapes, supported changes and each recipe's assumptions.

## What about an assertion percentage?

Supercov does not currently report a verified global assertion percentage.
`assertionScore` is `null`, not zero. Candidate counts summarize the analysis
inventory; dividing `evident` sites by all sites does not measure the fraction
of the library that is safe to change. Candidate `analysisCertainty` remains
`unverified`, including when separate bounded checks provide useful evidence.

MC/DC measures whether conditions independently affected decisions in the run.
It does not establish that assertions distinguish the resulting behavior.
Conversely, a passing value assertion does not supply missing MC/DC cases.
Use the two views together rather than substituting one percentage for another.

## Keep the evidence current

Assertion queries require source, tests, dependencies and configuration matching
the run. Editing a pragma changes test source too. Rerun the complete suite after
changes; do not apply an old conclusion to a new checkout. The query checks
freshness and records compiler and analyzer identity.

For a proposed code change, the useful result is specific: which existing
assertions check the behavior you intend to preserve, which distinctions they
could miss, and what remains unknown. Review those checks, add a meaningful test
where needed, then run the full suite. An `evident` label is not permission to
change behavior.

See [Agent workflow](agent-loop.md) for the test-writing loop and
[Assertion evidence reference](assertion-evidence.md) for the detailed models.
