# Find a missing test

The tests in this example cover every line and branch of a checkout function,
but never check an expired session. Use Supercov to find that missing case,
then check what changes when you add a test for it.

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
test are included, so you can run each stage without editing any files.

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

The example also includes a script that runs both stages and checks what
happens if the expiry check is removed:

```sh
npm run demo
```

The script runs in temporary directories and leaves your files unchanged.
After checking the coverage results, it makes a separate copy with this change:

```diff
-  if (signedIn && !expired) return true;
+  if (signedIn) return true;
```

| Tests run against the changed function | Result |
| --- | --- |
| Original two tests | Both pass. |
| All three tests | The expired-session test fails; the other two pass. |

The new test expects `false`, but the changed function returns `true`. The demo
expects this test to fail and succeeds when it observes that failure.

Removing the guard and checking for this failure is part of the example
script, not something the Supercov coverage command does.

## Next

- [Full example and recorded output](https://github.com/supercorp-ai/supercov/tree/main/examples/checkout-verification) — source, tests, and the complete output excerpted above.
- [Understanding coverage](coverage-model.md) — what each metric measures and what 100% means.
- [Agent workflow](agent-loop.md) — use the same run, inspect, test, and compare steps with a coding agent.
