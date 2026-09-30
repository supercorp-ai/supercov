# Assertion coverage

Line coverage shows which code ran. Assertion coverage shows which of that code
the tests would catch breaking.

## A passing test can miss a wrong result

This function confirms an order and calculates its total:

```js
export function checkout(price, quantity) {
  const total = price * quantity;
  return { status: 'confirmed', total };
}
```

The test checks the status, but not the total:

```js
import assert from 'node:assert/strict';
import test from 'node:test';
import { checkout } from './checkout.js';

test('confirms an order', () => {
  const order = checkout(25, 2);
  assert.equal(order.status, 'confirmed');
});
```

Every line runs and the test passes. It would also pass if `total` were
`undefined`, so the line computing it is covered but not asserted.

## What counts as asserted

A statement is asserted when changing it would make at least one passing test
that runs it fail: an assertion fails, test or helper code throws, the test
times out, or the process crashes. The change depends on the statement:

| Statement | Change |
| --- | --- |
| `if` | the condition inverted, each way separately (asserted if either is caught) |
| `return x` | returns undefined without evaluating `x` |
| declaration with a value | the value becomes undefined |
| JSX expression | the value becomes undefined |
| anything else | skipped |

Imports, declarations without a value, and function, class and type
declarations are not assessed. **Assertions** is the share of the executed
statements that are asserted.

## Assess and read

```bash
npx supercov -- npm test
npx supercov runs latest assertions assess
npx supercov runs latest assertions
```

`assertions assess` works it out and saves the result to the run. For each test,
Supercov renders what the test ran -- its code, its helpers, and the source it
executed -- and asks [Jev](https://typesafe.ai) whether the test would fail for
each statement's change. It prints an estimate first; `--dry-run` sends nothing.
It needs a TypeSafe AI API key in `TYPESAFE_API_KEY`.

`assertions` reads the saved result: the share asserted and the statements that
are not, with no network and no key. `npx supercov runs latest` and the HTML
report show the same number.

```text
Run run_4f2a: 93.8% asserted (2731 of 2913 executed statements)

182 not asserted: no test that runs them was judged to fail if they changed.

  src/adapters.ts
      100  res.removeListener("close", onResClose);  (skipped)
```

A statement that is not asserted is a place to add a check: a test that runs it
exists, but nothing it asserts depends on what the statement does.

## Later runs ask only about what changed

Answers are kept in `.supercov/assertions/`. Each is stored under the exact text
Jev saw for that statement and test, and reused only when a new run would ask
the same question with the same text. Nothing decides whether a change could
matter: a change to the statement, to code around it that the test ran, to the
test or its helpers, or to which tests run it, asks again. A dependency or
configuration change asks everything again. After a typical commit only a few
dozen questions are asked; the rest are reused.

## Cost, speed and accuracy

Measured on four JavaScript and TypeScript projects (7,393 executed statements):

- **Cost:** about 0.007 cents per statement for a first assessment. A project of
  3,000 statements costs about $0.25; a later commit usually well under a cent.
- **Speed:** about a minute for 3,000 statements; `--workers` raises the
  requests in flight (default 8) for a faster, slightly costlier pass.
- **Accuracy:** against ground truth from applying each change and running the
  tests, on a fresh sample of 78 statements: 69 right; of the 13 not asserted it
  found 8, and 8 of the 12 it flagged were truly not asserted. The share it
  reported was 84.6% against a true 83.3%.

The share is a good measure of a project; a single statement's verdict is a
judgment worth checking before acting on it. Supercov does not run mutated code
for this: nothing is executed, and your tests are never slowed.

## Requirements

JavaScript and TypeScript runs, and `TYPESAFE_API_KEY` for `assess`. Reading a
saved result needs neither.
