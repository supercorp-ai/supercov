# Example

This example measures coverage for a small checkout function. It includes the
original tests, an additional test for an expired session, and commands for
comparing the results.

## Before you start

You need Node.js 22 or newer and npm. Clone the repository and install the
example's dependencies:

```sh
git clone --depth 1 https://github.com/supercorp-ai/supercov.git
cd supercov/examples/checkout-verification
npm ci
```

Run the commands below from this directory. The example uses Supercov 0.0.42
and Node's built-in test runner. Both the original tests and the additional
test are included. The first three steps do not require any file edits.

## 1. Run the original tests

In `src/session.js`, checkout is allowed only if the customer is signed in and
their session has not expired:

```js
export function canCheckout(signedIn, expired) {
  if (signedIn && !expired) return true;
  return false;
}
```

The two tests in `tests/session.test.js` check a valid session and a signed-out
visitor:

```js
assert.equal(canCheckout(true, false), true);
assert.equal(canCheckout(false, false), false);
```

Run those tests through Supercov, then open the summary:

```sh
npx supercov -- node --test tests/session.test.js
npx supercov runs latest
```

Everything after `--` is the test command Supercov runs. In your own project,
use your existing test command there.

Both tests pass. The coverage section shows:

```text
Coverage
  Lines      100.00% (3/3)
  Branches   100.00% (2/2)
  MC/DC      50.00% (1/2)
```

Line and branch coverage are 100% because the tests reach both `return true`
and `return false`. The MC/DC result shows there is still a condition to test.

Keep the run ID printed at the top of the summary. You'll use it to compare
this run with the next one.

## 2. Inspect the missing condition

Ask about the decision on line 2:

```sh
npx supercov runs latest decision src/session.js:2
```

```text
signedIn && !expired
C1 covered + asserted: signedIn
C2 MISSING: !expired
confidence asserted; asserted MC/DC 1/2
```

MC/DC stands for Modified Condition/Decision Coverage. It checks whether each
condition has independently affected the decision. The original tests show
that changing `signedIn` changes the result, but neither test changes `expired`.
That leaves one of two conditions covered: 50%.

`C2 MISSING: !expired` points to the case to test: a customer who is still
signed in, but whose session has expired. Checkout should be denied.

## 3. Include the expired-session test

`tests/expired-session.test.js` contains that test:

```js
test('an expired session cannot check out', () => {
  assert.equal(canCheckout(true, true), false);
});
```

Run both test files and open the new summary:

```sh
npx supercov -- node --test tests/session.test.js tests/expired-session.test.js
npx supercov runs latest
```

All three tests pass:

```text
Coverage
  Lines      100.00% (3/3)
  Branches   100.00% (2/2)
  MC/DC      100.00% (2/2)
```

Query the same decision again:

```sh
npx supercov runs latest decision src/session.js:2
```

The expiry condition is now covered. `asserted MC/DC 2/2` means both conditions
have coverage evidence linked to passing assertions:

```text
C2 covered + asserted: !expired
confidence asserted; asserted MC/DC 2/2
```

Compare the runs, replacing `<before-run-id>` with the ID you saved in step 1:

```sh
npx supercov diff <before-run-id> latest
```

```text
lines +0pp, branches +0pp, MC/DC +50pp
gained: 0 lines, 0 branches, 1 MC/DC conditions
lost: 0 lines, 0 branches, 0 MC/DC conditions
+ MC/DC src/session.js:2 C2 !expired
```

Line and branch coverage have not changed. The new test covers the missing
expiry condition without changing application code.

## 4. Check that the test catches a regression

In this example only, temporarily remove the expiry check from `src/session.js`:

```diff
-  if (signedIn && !expired) return true;
+  if (signedIn) return true;
```

Run the original two tests against the changed function:

```sh
npx supercov -- node --test tests/session.test.js
```

Then include the expired-session test:

```sh
npx supercov -- node --test tests/session.test.js tests/expired-session.test.js
```

| Tests run against the changed function | Result |
| --- | --- |
| Original two tests | Both pass. |
| All three tests | The expired-session test fails; the other two pass. |

The new test expects `false`, but the changed function returns `true`. The
second command should fail: that is the test catching the removed expiry check.

Restore `&& !expired` in `src/session.js` when you finish, then rerun all three
tests with the same command. They should pass again.

## Next

- [Full example and recorded output](https://github.com/supercorp-ai/supercov/tree/main/examples/checkout-verification) — source, tests, and the complete output excerpted above.
- [Understanding coverage](coverage-model.md) — what each metric measures and what 100% means.
- [Agent workflow](agent-loop.md) — use the same run, inspect, test, and compare steps with a coding agent.
