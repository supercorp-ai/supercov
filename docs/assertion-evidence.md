# Assertion evidence (JS/TS)

Use `assertions` to inspect what your JavaScript and TypeScript tests check.
For a step-by-step example, start with [Understanding assertions](assertions.md).

## Requirements

- Run Supercov through `npx supercov`. The Ruby gem, Python package, and standalone
  binary do not include the JS/TS assertion analyzer.
- Use Node's test runner or Vitest. Other runners and languages may support
  coverage without supporting this assertion query.
- Install a compatible TypeScript compiler in your project, even for JavaScript.
  TypeScript 5.8.3 and native 7.0.2 are tested. Native 7.0.2 requires Node 22.12
  or newer and its platform-specific compiler package.
- Keep source, tests, dependencies, and configuration unchanged between the run
  and the query. Rerun the suite after any changes, including assertion hints.

If you need to add TypeScript, use your project's package manager and a version
compatible with the project. Do not downgrade an application's compiler just to
get an assertion report.

## Inspect assertion evidence

Run your complete suite, then ask for a short report:

```sh
npx supercov -- npm test
npx supercov runs latest assertions --limit 5
```

The query reads the saved run and source. It does not rerun the suite, execute
mutated code, or edit files in your project.

| To inspect | Command |
| --- | --- |
| One file | `npx supercov runs latest assertions --file src/shipping.js` |
| One result | `npx supercov runs latest assertions --site '<site-id>' --json` |
| Assertion hints | `npx supercov runs latest assertions --pragmas --json` |
| Tests used by the report | `npx supercov runs latest assertions --evidence /tests --limit 5 --json` |
| Analysis problems | `npx supercov runs latest assertions --evidence /diagnostics --json` |
| Available options | `npx supercov runs latest assertions --help` |

Use your own file paths and the site ids returned by the report. Text output
works for people and agents; add `--json` when you need structured results.

Each source result includes an `evidence.pointer`. Follow it to inspect the
assertions behind that result:

```sh
npx supercov runs '<run-id>' assertions --evidence '<returned-pointer>' \
  --analysis '<analysisId>' --json
```

Use the run id, pointer, and `analysisId` from the report. Check which test and
assertion the evidence refers to, what value or effect it checks, and why any
part remains unresolved.

## Read the result

The [guide](assertions.md#read-the-result) explains `evident`, `presence`,
`partial`, and `unresolved`. These describe source behaviors, not a count of
assertion statements.

- `gap:` reasons point to missing execution or checks. Review the code before
  deciding which test to add.
- `limit:` reasons mean Supercov could not establish the connection. They do
  not prove that the test checks nothing.
- `assertionScore` is `null`: there is no overall assertion percentage.
  `analysisCertainty: unverified` means the result is evidence for review, not
  a correctness guarantee.

## Pagination and oversized records

For JSON responses, follow `pagination.nextOffset` until `hasMore` is `false`.
Do not calculate the next offset by adding `--limit`; a page can contain fewer
items than requested.

Use a fixed run id and the first response's `analysisId` for subsequent pages:

```sh
npx supercov runs '<run-id>' assertions --offset '<nextOffset>' \
  --limit 5 --analysis '<analysisId>' --json
```

`detailOnly: true` means a record was too large to include on the current page.
Follow its `evidence.pointer` to read it. Evidence pages can return more pointers
for nested records or text chunks for a long string; keep following the returned
offsets and pointers until complete.

```sh
npx supercov runs '<run-id>' assertions --evidence '<returned-pointer>' \
  --offset '<nextOffset>' --limit 4000 --analysis '<analysisId>' --json
```

Use the pointers returned by Supercov rather than building them from row numbers.
The `--analysis` check prevents pages from different analyses being combined.

## Optional assertion hints

If a test already checks the right behavior but Supercov cannot connect it to
the source, add an `observes` comment immediately before the assertion:

```js
// observes: src/shipping.js#shippingCost return 4
assert.equal(shippingCost(), 4);
```

The comment contains:

- the project-relative source path;
- the exact function name after `#`; and
- an optional piece of source text to select one result within that function.

Place it before a statement containing one assertion. Use the names and source
text from your own project, then rerun the suite:

```sh
npx supercov -- npm test
npx supercov runs latest assertions --pragmas --json
```

| Validation | What to do |
| --- | --- |
| `analyzer-supported` | Supercov connected the hint to a passing assertion. Read the evidence to check that it describes what you intended. |
| `unresolved` | The connection is not supported or some evidence is missing. Read the reason; a more specific comment may not solve it. |
| `invalid` | The hint could not be used as written. Check its placement, path, function name, and selected source text. |

A hint points to an existing check; it cannot replace an assertion or make
untested behavior covered. Hints are comments, so an invalid hint does not make
your test fail.

### Check whether a specific change would be caught

Some hints accept a `; check ...` suffix. For example:

```js
// observes: src/validate.js#validate throw Error('invalid'); check completion
assert.throws(() => validate(badInput));
```

This asks whether omitting the selected `throw` statement would be caught by
that assertion. It does not ask whether every change to `validate` would be caught.

| Suffix | Question |
| --- | --- |
| `check missing call` | Would replacing a supported console callback's body with an empty body be caught by the call-count assertion? |
| `check count` | Would forcing a selected condition true, false, or inverted be caught by the call-count assertion? |
| `check value` | Would one of the supported changes to a condition, boolean return, or map callback be caught by the value assertion? |
| `check completion` | Would omitting the selected `throw` or `return` statement be caught by `throws` or `doesNotThrow`? |

These checks support a limited set of synchronous Node tests. Async code, custom
assertion helpers, shared state, and setup hooks can leave them unresolved.
Read the exact change reported for your result rather than assuming the suffix
supports every use of that construct.

`rejected` means the selected assertion would catch the reported change within
the supported check. `not-rejected` means it would not; another assertion could
still fail. An unresolved change remains unknown. These results do not change
MC/DC coverage or create an overall assertion score.

## When a query cannot finish

| Problem | Next step |
| --- | --- |
| Missing analyzer | Use `npx supercov`, not the standalone CLI or another language's package. |
| Missing or incompatible compiler | Check the project's TypeScript installation and the requirements above, then rerun the suite. |
| Source no longer matches the run | Run the complete suite again before querying. |
| A test cannot be linked to its source | Check the runner and any custom test registration or stack formatting, then record a new run. |
| The report leaves a relationship unresolved | Inspect the reason and the test. Custom helpers, subprocess output, browser work, and ambiguous retries can exceed what Supercov can follow. |

Ordinary coverage queries remain useful when assertion analysis is unavailable.
See [Troubleshooting](troubleshooting.md) for problems with the coverage run,
or [Agent workflow](agent-loop.md) to continue improving tests.
