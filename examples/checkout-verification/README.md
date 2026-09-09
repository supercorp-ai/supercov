# Checkout verification

Use Supercov to find a missing test for an expired checkout session. The original
tests have 100% line and branch coverage, but still pass if the expiry check is
removed. The additional test checks that an expired session cannot check out.

[Read the walkthrough](https://supercov.com/docs/code-verification).

## Run it

Requires Node.js 22+ and npm. From this directory:

```sh
npm ci
```

The example uses Supercov 0.0.42 and Node's built-in test runner and assertion
library. The additional test is included in a separate file, so you can compare
the two stages without editing any files.

Run only the original two tests, then inspect the result:

```sh
npx supercov -- node --test tests/session.test.js
npx supercov runs latest
npx supercov runs latest decision src/session.js:2
```

Keep the run ID printed in the summary. Now include the expired-session test:

```sh
npx supercov -- node --test tests/session.test.js tests/expired-session.test.js
npx supercov runs latest
npx supercov runs latest decision src/session.js:2
```

Compare the runs, replacing `<before-run-id>` with the ID you saved:

```sh
npx supercov diff <before-run-id> latest
```

The results stay in this example's `.supercov/` directory so you can query them
later.

| Measured result | Original tests | With the additional test |
| --- | --- | --- |
| Passing tests | 2 | 3 |
| Lines | 100% (3/3) | 100% (3/3) |
| Branches | 100% (2/2) | 100% (2/2) |
| MC/DC conditions | 50% (1/2) | 100% (2/2) |
| MC/DC conditions linked to passing assertions | 1/2 | 2/2 |

The [walkthrough](https://supercov.com/docs/code-verification) explains each
command and shows how to check that the new test fails if the expiry check is
removed.

## Files

- `src/session.js`: the checkout function.
- `tests/session.test.js`: the original two tests.
- `tests/expired-session.test.js`: the additional test for an expired session.
- `scripts/reproduce.mjs`: runs both suites, checks the coverage results, and
  tests a separate copy with the expiry check removed.
- [recorded/](recorded/): saved command output and test logs.

## Maintaining the example

This repository also includes `npm run demo`, which automates the coverage and
regression checks in temporary directories. It leaves your files unchanged and
succeeds when the expired-session test catches the removed expiry check.

To update the recorded output, run `npm run record`. It verifies the same
results before rewriting the files in `recorded/`.
