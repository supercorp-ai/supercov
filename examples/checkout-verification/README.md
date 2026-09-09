# Checkout verification

Two passing tests. **100% line and branch coverage.** One missing expiry check.

This small JavaScript example shows Supercov identifying an untested condition,
the additional assertion that closes it, and a deliberate regression that only
the new test catches.

[Read the walkthrough](https://supercov.com/docs/code-verification).

## Run it

Requires Node.js 22+ and npm. From this directory:

```sh
npm ci
npm run demo
```

The example pins Supercov 0.0.42 in its lockfile. It uses Node's built-in test
runner and assertion library. The demo runs in temporary directories, checks
every result, and leaves your source and tests unchanged. No account is needed.

| Measured result | Original tests | With the additional test |
| --- | --- | --- |
| Passing tests | 2 | 3 |
| Lines | 100% (3/3) | 100% (3/3) |
| Branches | 100% (2/2) | 100% (2/2) |
| MC/DC conditions | 50% (1/2) | 100% (2/2) |
| MC/DC conditions linked to passing assertions | 1/2 | 2/2 |
| Detects a removed expiry check | No | Yes |

## Files

- `src/session.js`: the unchanged application function.
- `tests/session.test.js`: the original two tests.
- `tests/expired-session.test.js`: the one additional test.
- `scripts/reproduce.mjs`: runs both suites through Supercov, checks their JSON results,
  and checks a deliberately broken copy with the expiry guard removed.
- `recorded/`: actual output from a successful run, including the regression's
  passing and failing test logs. Run IDs, timestamps, paths, and timings vary.

The additional test is kept in a separate file so both stages can be reproduced
without editing anything. This is a worked example, not a recording of an
independent agent or a claim about a bug found in someone else's project.

## Inspect the evidence yourself

These commands keep runs in this example's `.supercov/` directory:

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

`npm run record` refreshes the committed transcripts after verifying all results.
Only run it when intentionally updating the example's recorded evidence.

## What this establishes

MC/DC asks whether each condition can independently change a decision. The
original tests exercise both return paths, but never a signed-in user whose
session has expired. The new test checks exactly that case and fails if the
expiry check is removed.

The assertion-linked fields above are taken from Supercov's real CLI output;
they are not a separate assertion-coverage percentage. The regression check is
performed by this example's script, not by a Supercov mutation-testing command.
One caught regression does not prove that the function is correct for every
input, or that the application is safe to ship without other checks.
