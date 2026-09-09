# Tutorial

In this tutorial, you'll open a small JavaScript project in your coding agent,
ask it to add one test, and review the result. Supercov measures coverage; the
agent writes the test.

## 1. Open the starter project

You need Node.js 22 or newer, npm, and a coding agent that can edit files and
run terminal commands.

[Download the starter project](https://supercov.com/downloads/supercov-tutorial.zip),
extract it, and open the `supercov-tutorial` folder in your agent. Install its
dependencies from that folder:

```sh
npm ci
```

The project uses Node's built-in test runner and Supercov 0.0.42. It contains
one function and two tests. The completed test is not included in the download.

`src/session.js` allows checkout only when the customer is signed in and their
session has not expired:

```js
export function canCheckout(signedIn, expired) {
  if (signedIn && !expired) return true;
  return false;
}
```

The tests cover a valid session and a signed-out visitor:

```js
assert.equal(canCheckout(true, false), true);
assert.equal(canCheckout(false, false), false);
```

## 2. Paste the prompt

Start a conversation with your agent in the starter folder and paste:

```text
Measure code coverage with npx supercov and write one missing test.
Only change tests. Rerun the full test suite and show me the test you
added and the before-and-after coverage.
```

Let the agent run the commands and edit the tests. Approve those actions if
your agent asks for permission.

The test and output below come from a recorded Codex run with this starter and
prompt. Your agent may choose different commands or name the test differently.

## 3. Read what the agent found

In the recorded run, the agent measured the full suite and opened the summary:

```sh
npx supercov -- npm test
npx supercov runs latest
```

Everything after `--` is the project's test command. Here, `npm test` runs
`node --test`, which picks up the test files in the project.

Both tests passed. The summary showed:

```text
Coverage
  Lines      100.00% (3/3)
  Branches   100.00% (2/2)
  MC/DC      50.00% (1/2)
```

The agent then listed the gaps and inspected `src/session.js`. You can open
the same views with:

```sh
npx supercov runs latest gaps
npx supercov runs latest file src/session.js
```

The file query explained what was missing:

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

## 4. Review the test it wrote

The agent added this test to `tests/session.test.js`:

```js
test('a signed-in visitor with an expired session cannot check out', () => {
  assert.equal(canCheckout(true, true), false);
});
```

The customer is still signed in, but the session has expired. The assertion
checks that checkout is denied. The original tests and application code were
left unchanged.

## 5. Check the result

The agent reran the same full test command:

```sh
npx supercov -- npm test
```

All three tests passed. The new run's summary showed:

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

You now have a test-file change to review and a coverage comparison in the
conversation. Ask your agent separately if you want it to commit the change or
open a pull request.

## Check the test yourself

To check that the new test catches a regression, temporarily remove
`&& !expired` from `src/session.js` in this starter project:

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
- [Agent workflow](agent-loop.md) — use this process with your own test suite.
- [Understanding coverage](coverage-model.md) — what each metric measures and what 100% means.
