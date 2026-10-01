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
| `if` (and `elif`, `elsif`, `unless`, `else if`) | the condition inverted, each way separately (asserted if either is caught); a Rust `if let` is made not to match |
| `return x`, and a block's value in Ruby and Rust | returns a stand-in for nothing, without evaluating `x` |
| declaration or assignment with a value | the value becomes that stand-in, without being evaluated |
| JSX expression | the value becomes undefined |
| anything else | skipped: the statement does not run |

The stand-in is `undefined` in JavaScript and TypeScript, `None` in Python and
`nil` in Ruby. Go and the JVM languages replace a value after it is computed,
as their mutation testers do: the zero value in Go, and `null`, `0` or `false`
in Java and Kotlin. Rust has no value that fits every type, so it takes a
different value of the same type, from the function's signature or the
`let`'s annotation where there is one: a boolean negated, an `Ok` turned into
an `Err` and back, a literal swapped (`false` for `true`, `1` for `0`), an
`Ordering` reversed, `None`, `""` or `0`; `Default::default()` only where no
type is known.

Imports, declarations without a value, and function, class and type
declarations are not assessed; in Python neither are `pass`, `global`,
`nonlocal` and docstrings, and in Ruby neither are the calls that declare
(`require`, `attr_reader`, `private`, `include` ...). **Assertions** is the
share of the executed statements that are asserted.

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

## Which tests check a change

`npx supercov runs latest tests affected` uses the assessment to say which of
the tests a change reaches were judged to catch it, and lists them first;
`assertions assess --changed` asks every test that ran the changed code. See
[Find the tests a change affects](cli.md#find-the-tests-a-change-affects).

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
- **Python:** on h11 (532 executed statements, pytest), every statement checked
  the same way: 502 right; of the 67 not asserted it found 45, and 45 of the 53
  it flagged were truly not asserted. The share it reported was 90.0% against a
  true 87.4%, for about $0.04.
- **Go, Rust and Java**, checked the same way:

  | Project | Checked | Right | Flagged (truly not asserted) | Share reported / true |
  | --- | --- | --- | --- | --- |
  | google/uuid (Go) | 339 | 294 | 34 (26) | 90.0% / 81.4% |
  | dtolnay/semver (Rust) | 150 of a sample of 200 | 144 | 5 (1) | 96.7% / 98.0% |
  | apache/commons-cli (Java) | 196 of a sample of 200 | 167 | 49 (33) | 75.0% / 76.5% |
  | dry-rb/dry-inflector (Ruby 3.4) | 157 | 141 | 20 (8) | 87.3% / 92.4% |
  | hashicorp/go-version (Go, held out) | 257 | 245 | 29 (19) | 88.7% / 91.8% |
  | sporkmonger/addressable (Ruby, held out) | 198 of a sample of 200 | 181 | 28 (15) | 85.9% / 90.4% |
  | square/moshi (Kotlin, held out) | 54 of a sample of 60 | 47 | 9 (6) | 83.3% / 81.5% |

  The first five were used to shape how tests are shown to Jev; the held-out
  ones were assessed before their ground truth was taken and not tuned on.
  A Rust change that falls back to `Default::default()` for a type without
  one cannot be made, so it cannot be checked this way; that was a quarter of
  semver's sample. Ruby 3.3 credits each line to the first test that runs it
  (3.4 and newer credit every test), so on 3.3 most statements are asked of a
  single test and the share reads low.

The share is a good measure of a project; a single statement's verdict is a
judgment worth checking before acting on it. Supercov does not run mutated code
for this: nothing is executed, and your tests are never slowed.

## Requirements

Runs of any language Supercov measures: JavaScript and TypeScript, Python
(pytest, unittest), Ruby (RSpec, Minitest, test-unit, Cucumber), Go, Rust
(libtest, nextest, doctests), and Java and Kotlin (JUnit, TestNG); and
`TYPESAFE_API_KEY` for `assess`. Reading a saved result needs neither.
