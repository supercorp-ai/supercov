# Understanding assertions

Coverage tells you which code ran. Assertion evidence helps you see what the
tests checked and where a stronger assertion may be useful.

This is available for JavaScript and TypeScript through the npm package, with
Node's test runner and Vitest. See the [requirements](assertion-evidence.md#requirements)
before the first run.

## Run your suite

Ask your agent to run your project's complete test command, then read the
assertion report:

```sh supercov
npx supercov -- npm test
npx supercov runs latest assertions --limit 5
```

Replace `npm test` with your actual test command. The query reads the stored run
and matching source; it does not run the tests again.

## Check what the test asserts

Suppose `src/shipping.js` contains:

```js
export function shippingCost() {
  return 4;
}
```

These checks all call the function, but they test different things. Here,
`assert` comes from `node:assert/strict`:

| Assertion | What it checks |
| --- | --- |
| `assert.equal(shippingCost(), 4)` | The cost is exactly `4`. Returning `5` would fail. |
| `assert.ok(shippingCost())` | The cost is truthy. Returning `5` would still pass. |
| `shippingCost(); assert.equal(7, 7)` | Nothing about the returned cost. The assertion compares two constants. |

Line coverage can be the same in all three cases. The useful test is the one
that checks the behavior you need to preserve.

The same applies to other results: checking the number of log calls does not
check their messages, and checking one substring does not check the whole response.

## Read the result

Each result refers to a source location, such as a decision, return, or call.
It includes a classification and the evidence behind it:

| Result | Meaning |
| --- | --- |
| `evident` | Supercov found evidence that an assertion checks this behavior. Read the assertion to see exactly what it checks. |
| `presence` | An assertion checks presence or truthiness, but not an exact value. |
| `partial` | Some checks were found, but part of the behavior remains unresolved. |
| `unresolved` | Supercov could not establish a check. Read the reason to find out why. |

A reason starting with `gap:` points to missing execution or a missing check.
A reason starting with `limit:` means Supercov could not analyze the connection.
An analysis limit does not mean your test is wrong or that an assertion is missing.

To focus on one file:

```sh supercov
npx supercov runs latest assertions --file src/shipping.js --limit 5
```

Use your own source path. See the [reference](assertion-evidence.md#inspect-assertion-evidence)
to inspect an individual result and its supporting assertions.

## Improve one test

1. Open the reported source and the test that reaches it.
2. If the behavior never ran, add a scenario that reaches it. If it ran without
   a useful check, add or strengthen an assertion on its result.
3. Rerun the same complete suite and read the updated report.

```sh supercov
npx supercov -- npm test
npx supercov runs latest assertions --file src/shipping.js --limit 5
```

Review the test change, not just the classification. Do not weaken assertions
or change application code to improve the report.

If an existing assertion already checks the behavior but Supercov cannot follow
it, you can try an [optional assertion hint](assertion-evidence.md#optional-assertion-hints).
If the connection is still unsupported, leave the limit visible rather than
rewriting a good test for the analyzer.

## Use coverage and assertions together

MC/DC helps find missing condition cases. Assertion evidence helps you review
what the tests check about the resulting behavior. Neither replaces the other.

There is no overall assertion percentage. An `evident` result is useful for
reviewing a test, not proof that every change to that code would be caught.

See [Agent workflow](agent-loop.md) for the full test-writing loop, or the
[assertion reference](assertion-evidence.md) for commands and options.
