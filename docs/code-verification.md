# 100% coverage. One missing test.

Two tests pass. Every line and both branches are covered. But remove the session
expiry check, and those tests still pass. Here is how Supercov exposes the gap
and what the next test catches.

## The code and the original tests

A signed-in customer can check out only while their session is valid:

```js
export function canCheckout(signedIn, expired) {
  if (signedIn && !expired) return true;
  return false;
}
```

The original suite checks a valid session and a signed-out visitor:

```js
assert.equal(canCheckout(true, false), true);
assert.equal(canCheckout(false, false), false);
```

Both return paths run. Neither test asks what happens when a signed-in
customer's session has expired.

## What Supercov reveals

Run the original tests, then query the result:

```sh
npm run coverage:before
npx supercov runs latest
npx supercov runs latest decision src/session.js:2
```

The summary reports:

```text
Coverage
  Lines      100.00% (3/3)
  Branches   100.00% (2/2)
  MC/DC      50.00% (1/2)
```

The decision query identifies the missing condition:

```text
signedIn && !expired
C1 covered + asserted: signedIn
C2 MISSING: !expired
confidence asserted; asserted MC/DC 1/2
```

MC/DC—Modified Condition/Decision Coverage—asks whether each condition has
been shown to independently change the decision. The existing tests show that
signing out prevents checkout. They do not show that expiry does.

For a coding agent, this is a specific next task: keep the customer signed in,
expire the session, and assert that checkout is denied.

## One additional test

```js
test('an expired session cannot check out', () => {
  assert.equal(canCheckout(true, true), false);
});
```

The application code stays untouched. Rerun with this test included:

```sh
npm run coverage:after
npx supercov runs latest
npx supercov runs latest decision src/session.js:2
```

Now all three tests pass, and the summary reports:

```text
Coverage
  Lines      100.00% (3/3)
  Branches   100.00% (2/2)
  MC/DC      100.00% (2/2)
```

The expiry condition has evidence linked to a passing assertion:

```text
C2 covered + asserted: !expired
confidence asserted; asserted MC/DC 2/2
```

Comparing the two run IDs with `npx supercov diff <before-run-id> <after-run-id>`
confirms the gain:

```text
lines +0pp, branches +0pp, MC/DC +50pp
gained: 0 lines, 0 branches, 1 MC/DC conditions
lost: 0 lines, 0 branches, 0 MC/DC conditions
+ MC/DC src/session.js:2 C2 !expired
```

## Does the new test catch anything?

The reproduction script makes a separate copy and deliberately removes the
expiry guard:

```diff
-  if (signedIn && !expired) return true;
+  if (signedIn) return true;
```

| Tests against the broken copy | Result |
| --- | --- |
| Original two tests | Both pass. The regression escapes. |
| With the expired-session test | The new test fails: checkout was allowed. |

That is the useful result: an assertion that catches a specific regression,
even though line and branch percentages never moved.

The regression check belongs to this example's script, not a Supercov
mutation-testing command. Assertion linkage is evidence, not a proof that every
assertion is meaningful or that the application has no other bugs.

## Run it yourself

Requires Node.js 22+ and npm. The example pins Supercov 0.0.42 and uses Node's
built-in test runner. No account, agent subscription, or test framework install
is needed.

```sh
git clone --depth 1 https://github.com/supercorp-ai/supercov.git
cd supercov/examples/checkout-verification
npm ci
npm run demo
```

The demo runs both suites, verifies the coverage and assertion-linked evidence,
compares the runs, and confirms that the additional test catches the broken
copy. Everything runs in temporary directories; your checkout stays unchanged.

These are excerpts of actual CLI output, not a simulated agent transcript.
The added test is supplied in a separate file so you can reproduce both stages.
Run IDs, paths, and timings will differ; the checked results should match.

[Browse the example](https://github.com/supercorp-ai/supercov/tree/main/examples/checkout-verification)
or [inspect the full recorded output](https://github.com/supercorp-ai/supercov/tree/main/examples/checkout-verification/recorded).
