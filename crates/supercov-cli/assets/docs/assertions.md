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
| `return x`, and a block's value in Ruby, Rust and Kotlin | returns a stand-in for nothing, without evaluating `x` |
| a `when`, `switch`, `try`, `loop` or Kotlin `throw` a function ends on | every value it returns becomes that stand-in (skipping it would leave the function nothing to return) |
| declaration or assignment with a value | the value becomes that stand-in, without being evaluated |
| JSX expression | the value becomes undefined |
| `return true`/`false`, or a declaration of `true`/`false` (JavaScript, TypeScript, Python) | the boolean becomes the other one: `false` becoming `undefined` changes nothing a truthiness check reads |
| `return undefined` or `return None` | skipped, since returning the stand-in would be the same code |
| `throw` in TypeScript | throws a bare `new Error()` instead: deleting a throw seldom compiles, because it usually guards the narrowing the next line relies on |
| anything else | skipped: the statement does not run |

A constructor's `super(...)` is not assessed: skipping it does not compile.

The stand-in is `undefined` in JavaScript and TypeScript, `None` in Python and
`nil` in Ruby. Go and the JVM languages replace a value after it is computed,
as their mutation testers do: the zero value in Go, and `null`, `0` or `false`
in Java and Kotlin. Rust has no value that fits every type, so it takes a
different value of the same type, from the function's signature or the
`let`'s annotation where there is one: a boolean negated, an `Ok` turned into
an `Err` and back, a literal swapped (`false` for `true`, `1` for `0`), an
`Ordering` reversed, `None`, `""` or `0`. Where nothing is annotated, the type
comes from the crate's own signatures, the parameters and `let`s in scope, or
a `match`'s arms (`let (major, text) = numeric(text)?;` takes `numeric`'s
return type): a tuple gets one element changed, an enum another of its
variants, a struct literal one field, a closure its result, integer
arithmetic its lowest bit flipped, a comparison its order reversed, a slice
or split the empty one, and `let Some(x) = .. else` gets `None`. A value of a
type the crate defines is built from its own constants, constructors and
enums (`Err(Error::new(ErrorKind::Empty))`, `Ok(Version::new(0, 0, 0))`).
`Default::default()` is left only where nothing says what the type is.

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

`assertions` reads the saved result with no network and no key, a page at a
time, from the run down to one statement:

```bash
npx supercov runs latest assertions                       # the share, and the files by statements not asserted
npx supercov runs latest assertions src/adapters.ts       # that file's statements not asserted
npx supercov runs latest assertions src/adapters.ts:100   # one statement: each asked test's answer
npx supercov runs latest assertions --test "closes the stream"  # what one test catches, and what it misses
```

```text
Run run_4f2a: 93.8% asserted (2731 of 2913 executed statements)

Files, most statements not asserted first: a statement is not asserted when none of the tests asked about it was judged to fail if it changed. Up to 15 of the tests that run a statement are asked, so a test never asked can still catch it.
 NOT ASSERTED  ASSERTED  FILE
           41   388/429  src/adapters.ts
           ...
showing 1-20 of 57
next page: npx supercov runs 'run_4f2a' assertions --offset 20
inspect a file: npx supercov runs 'run_4f2a' assertions 'src/adapters.ts'
```

Each listing ends with the command for its next page and for the next step
down, so an agent reads only as much as it needs. `runs latest file` and
`runs latest line` show the same verdicts beside the coverage, and
`npx supercov runs latest` and the HTML report show the same share.

A statement that is not asserted is a place to add a check: tests run it, but
nothing the asked tests assert depends on what the statement does. Five of the
tests that run a statement are asked first, and up to 15 when none of those
catches it: tests whose file quotes text the statement writes (a log line, a
message), then tests named like the source file, then one per test file and
describe block. A statement many tests run can still be caught by one that was
never asked; the statement view (`assertions <file>:<line>`) says how many were.

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

A reused answer is the one Jev gave before, right or wrong. When you have shown
an answer wrong, `assess --refresh` asks every question again and keeps the new
answers.

## Cost, speed and accuracy

- **Cost:** about 0.006 cents per statement for a first assessment: 14 projects
  in six languages (8,356 statements) cost $0.48 together. A project of 3,000
  statements costs about $0.18; a later commit usually well under a cent.
- **Speed:** about 20 seconds for 1,500 statements with 24 requests in flight
  (`--workers`, default 24; it changes how long a pass takes, not what it
  costs).

Measured on four JavaScript and TypeScript projects (7,393 executed statements):

- **Accuracy:** against ground truth from applying each change and running the
  tests, on a fresh sample of 78 statements: 69 right; of the 13 not asserted it
  found 8, and 8 of the 12 it flagged were truly not asserted. The share it
  reported was 84.6% against a true 83.3%.
- **Python:** on h11 (532 executed statements, pytest), every statement checked
  the same way: 502 right; of the 67 not asserted it found 45, and 45 of the 53
  it flagged were truly not asserted. The share it reported was 90.0% against a
  true 87.4%, for about $0.04.
- **Go, Rust, Java, Ruby and Kotlin**, checked the same way. These were used
  to shape how changes and tests are shown to Jev, so they are flattering:

  | Project | Checked | Right | Flagged (truly not asserted) | Share reported / true |
  | --- | --- | --- | --- | --- |
  | google/uuid (Go) | 339 | 298 | 34 (28) | 90.0% / 81.4% |
  | hashicorp/go-version (Go) | 257 | 253 | 21 (19) | 91.8% / 91.8% |
  | dtolnay/semver (Rust) | 200 of a sample of 200 | 190 | 7 (2) | 96.5% / 96.5% |
  | apache/commons-cli (Java) | 196 of a sample of 200 | 167 | 49 (33) | 75.0% / 76.5% |
  | dry-rb/dry-inflector (Ruby 3.4) | 157 | 148 | 13 (8) | 91.7% / 92.4% |
  | sporkmonger/addressable (Ruby) | 198 of a sample of 200 | 182 | 25 (14) | 87.4% / 90.4% |
  | square/moshi (Kotlin) | 60 of a sample of 60 | 52 | 9 (6) | 85.0% / 81.7% |

- **Held out:** five projects nothing was tuned on, each assessed once by the
  shipped version and its verdicts frozen before any change was run, scored on
  a pre-registered sample of 60 statements (and on every statement where the
  suite is fast enough):

  | Project | Checked | Right | Flagged (truly not asserted) | Share reported / true |
  | --- | --- | --- | --- | --- |
  | pallets/itsdangerous (Python) | 60 of 60 | 56 | 3 (2) | 95.0% / 91.7% |
  | — every statement | 238 of 238 | 224 | 13 (9) | 94.5% / 92.0% |
  | Masterminds/semver (Go) | 60 of 60 | 55 | 7 (7) | 88.3% / 80.0% |
  | — every statement | 497 of 498 | 472 | 46 (44) | 90.7% / 86.5% |
  | hashie/hashie (Ruby 3.4) | 60 of 60 | 57 | 3 (1) | 95.0% / 96.7% |
  | — every statement | 700 of 701 | 655 | 26 (10) | 96.3% / 94.4% |
  | FasterXML/java-classmate (Java) | 60 of 60 | 58 | 9 (7) | 85.0% / 88.3% |
  | bluss/arrayvec (Rust) | 54 of 60 | 48 | 1 (1) | 98.1% / 87.0% |

  Assessing all five (2,582 statements) cost $0.15, about 0.006 cents per
  statement. Go reads high: Jev takes a dropped error (`return false,
  fmt.Errorf(..)` becoming `return false, nil`) as caught where the tests check
  only the boolean. Rust reads high too on arrayvec, whose code is generic over
  its element type; six of its sampled changes cannot be made, because no value
  of an unknown `T` can be written.

The share is a good measure of a project; a single statement's verdict is a
judgment worth checking before acting on it. Supercov does not run mutated code
for this: nothing is executed, and your tests are never slowed.

## Requirements

Runs of any language Supercov measures: JavaScript and TypeScript, Python
(pytest, unittest), Ruby (RSpec, Minitest, test-unit, Cucumber), Go, Rust
(libtest, nextest, doctests), and Java and Kotlin (JUnit, TestNG); and
`TYPESAFE_API_KEY` for `assess`. Reading a saved result needs neither.
