# Checkout verification

Use Supercov to find a missing test for an expired checkout session. The original
tests have 100% line and branch coverage, but still pass if the expiry check is
removed. The additional test checks that an expired session cannot check out.

[Read the walkthrough](https://supercov.com/docs/code-verification).

## Run it

Requires Node.js 22+ and npm. From this directory:

```sh
npm ci
npm run demo
```

The example uses Supercov 0.0.42 and Node's built-in test runner and assertion
library. The demo runs the original tests, includes the additional test, and
compares their coverage. It then removes the expiry check in a separate copy
and verifies that only the additional test fails.

All of this runs in temporary directories and leaves your files unchanged.
The failing regression test is expected; the demo succeeds when it observes it.

| Measured result | Original tests | With the additional test |
| --- | --- | --- |
| Passing tests | 2 | 3 |
| Lines | 100% (3/3) | 100% (3/3) |
| Branches | 100% (2/2) | 100% (2/2) |
| MC/DC conditions | 50% (1/2) | 100% (2/2) |
| MC/DC conditions linked to passing assertions | 1/2 | 2/2 |
| Detects a removed expiry check | No | Yes |

## Run each stage manually

The additional test is included in a separate file. `coverage:before` runs only
the original two tests; `coverage:after` runs all three. These commands keep
the results in this example's `.supercov/` directory so you can query them later:

```sh
npm run coverage:before
npx supercov runs latest
npx supercov runs latest decision src/session.js:2
# Keep the run ID printed above for the comparison.

npm run coverage:after
npx supercov runs latest
npx supercov runs latest decision src/session.js:2
npx supercov diff <before-run-id> latest
```

The [walkthrough](https://supercov.com/docs/code-verification) explains each
command and its output.

## Files

- `src/session.js`: the checkout function.
- `tests/session.test.js`: the original two tests.
- `tests/expired-session.test.js`: the additional test for an expired session.
- `scripts/reproduce.mjs`: runs both suites, checks the coverage results, and
  tests a separate copy with the expiry check removed.
- [recorded/](recorded/): saved command output and test logs.

To update the recorded output, run `npm run record`. It verifies the results
before rewriting the files in `recorded/`. Use `npm run demo` to check the
example without changing those files.
