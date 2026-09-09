# Tutorial

Ask your coding agent to find a gap in your project's tests and cover it.
Supercov measures coverage; the agent writes the test. We'll use a small
checkout example to show what that looks like as you follow along in your
own project.

## 1. Open your project

Open your repository in the coding agent you normally use. It needs to be able
to edit files and run your existing test suite. Set up any dependencies,
environment variables, or local services the tests normally need.

The prompt below uses `npx`, so you need Node.js 22 or newer and npm. See
[Getting started](getting-started.md) for other installation options and
language requirements. You don't need to copy the example files into your
project.

## 2. Paste the prompt

In your project's agent conversation, paste:

```text
Measure code coverage with npx supercov and write one missing test.
Only change tests. Rerun the full test suite and show me the test you
added and the before-and-after coverage.
```

Let the agent run the commands and edit the tests. Approve those actions if
your agent asks for permission. If your project has several test commands,
tell it which full suite to use.

The agent should measure your existing tests, inspect a gap, add a test, and
rerun the same suite. The rest of this page shows those steps using the files
in our tutorial project. The test and output below come from a recorded Codex
run with the same prompt and Supercov 0.0.42; your files, results, and output
format may differ.

## 3. Review the gap

Look for the behavior your agent says is missing from your tests. Here's the
gap it found in our example.

[`src/session.js`](https://github.com/supercorp-ai/supercov/blob/main/examples/checkout-verification/starter/src/session.js)
allows checkout only when the customer is signed in and their session has
not expired:

```js
export function canCheckout(signedIn, expired) {
  if (signedIn && !expired) return true;
  return false;
}
```

The two tests in
[`tests/session.test.js`](https://github.com/supercorp-ai/supercov/blob/main/examples/checkout-verification/starter/tests/session.test.js)
check a valid session and a signed-out visitor:

```js
assert.equal(canCheckout(true, false), true);
assert.equal(canCheckout(false, false), false);
```

The agent measured this suite and opened the summary:

```sh
npx supercov -- npm test
npx supercov runs latest
```

Everything after `--` is the project's test command. This example's `npm test`
runs `node --test`. In your project, the agent should use your actual test
command instead.

Both tests passed. The summary showed:

```text
Coverage
  Lines      100.00% (3/3)
  Branches   100.00% (2/2)
  MC/DC      50.00% (1/2)
```

The agent then listed the gaps and inspected the example's `src/session.js`:

```sh
npx supercov runs latest gaps
npx supercov runs latest file src/session.js
```

In your project, the file query takes the path of the file your agent is
investigating. For this example, it explained:

```text
 LINE  STATUS        SOURCE
    2  PARTIAL       signedIn && !expired
       Unobserved: no witness pair shows `!expired` independently changing the decision result
```

Both return paths had run, so line and branch coverage were 100%. But neither
test checked an expired session. MC/DC checks whether each condition has been
shown to affect the decision independently. Here, `signedIn` had; `!expired`
had not.

If your agent stops at 100% line coverage, ask it to inspect the MC/DC gaps.
If Supercov reports a measurement limit instead, check
[Troubleshooting](troubleshooting.md) before treating it as a missing test.

## 4. Review the test it wrote

Review your agent's change: does the assertion check the behavior it identified,
and did it leave your application code and existing tests alone?

In our example, it added this test to `tests/session.test.js`:

```js
test('a signed-in visitor with an expired session cannot check out', () => {
  assert.equal(canCheckout(true, true), false);
});
```

The customer is still signed in, but the session has expired. The assertion
checks that checkout is denied. The original tests and application code were
left unchanged.

## 5. Check the result

Your agent should rerun the same full suite it measured at the start, not just
the new test. In the example, that was:

```sh
npx supercov -- npm test
```

All three example tests passed. The new run's summary showed:

```text
Coverage
  Lines      100.00% (3/3)
  Branches   100.00% (2/2)
  MC/DC      100.00% (2/2)
```

It also compared the two runs. To inspect that comparison yourself, use the
first run ID from your agent's output:

```sh
npx supercov diff <before-run-id> latest
```

The recorded comparison showed:

```text
lines +0pp, branches +0pp, MC/DC +50pp
gained: 0 lines, 0 branches, 1 MC/DC conditions
lost: 0 lines, 0 branches, 0 MC/DC conditions
+ MC/DC src/session.js:2 C2 !expired
```

If the diff says the older run is stale because tests changed, that is expected.
It still records the state before the edit.

In your project, look for a passing suite and evidence that the test covers the
gap your agent chose. One test won't necessarily take coverage to 100%.

The result is a change to your test files and a coverage comparison in the
agent conversation. Ask separately if you want a commit or pull request.

## Try the example yourself

If you'd rather follow along with the exact files shown here,
[download the starter project](https://supercov.com/downloads/supercov-tutorial.zip),
extract it, and open the `supercov-tutorial` folder in your agent. From that
folder, install its dependencies:

```sh
npm ci
```

Then paste the same prompt from step 2. The starter contains the original
function and two tests, pinned to Supercov 0.0.42. The completed test is not
included in the download.

### Check that the new test catches a regression

After your agent adds the expiry test, you can check it in the downloaded
starter. Temporarily remove `&& !expired` from its `src/session.js`:

```diff
-  if (signedIn && !expired) return true;
+  if (signedIn) return true;
```

Run the tests again:

```sh
npx supercov -- npm test
```

The new test should fail: the function now allows checkout when it should
return `false`. Restore `&& !expired` and rerun the same command. All three
tests should pass again.

## Next

- [Recorded agent run and completed test](https://github.com/supercorp-ai/supercov/tree/main/examples/checkout-verification/agent-run) — the prompt, command output, and file change used here.
- [Agent workflow](agent-loop.md) — prompts for continuing beyond one test.
- [Understanding coverage](coverage-model.md) — what each metric measures and what 100% means.
