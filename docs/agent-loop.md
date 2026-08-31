# Agent loop

Use Supercov in a repeatable agent loop: run the suite, query uncovered
obligations, add a focused test, rerun, and compare results. This page covers
the loop, the recommended prompt, and failure handling.

## The shape of the loop

```text
run the suite  ->  ask what is open  ->  write one test  ->  re-run  ->  diff
      ^                                                                   |
      +-------------------------------------------------------------------+
```

Each pass should close a small number of related obligations and end with
evidence that it did. An agent that writes ten tests before re-running has no
way to attribute the outcome; an agent that re-runs after every trivial edit
spends its budget on test execution instead of thinking.

## One pass, in commands

```sh
# 1. Establish a baseline. Only needed once per session.
npx supercov -- npm test

# 2. Orient without loading a report into context.
npx supercov runs latest --json
npx supercov runs latest gaps --limit 5 --json

# 3. Understand one target.
npx supercov runs latest file app/checkout/session.ts --json
npx supercov runs latest decision app/checkout/session.ts:64 --json

# 4. Check what already exercises that line, to avoid writing a duplicate.
npx supercov runs latest line app/checkout/session.ts:64 --json

# 5. Write one test. Then re-run and prove the gain.
npx supercov -- npm test
npx supercov diff <previous-run-id> latest --json
```

Step 4 is the one agents skip and should not. `line` answers "what already
executes this line", which usually reveals either an existing test to extend or
the exact reason nothing reaches it.

## A prompt you can paste

```text
You are improving test coverage for this repository using Supercov.

Baseline:
  npx supercov -- npm test

Then repeat this loop until coverage completeness stops improving, the target
is met, or you run out of time:

1. npx supercov runs latest gaps --limit 5 --json
2. Pick the file with the highest-value open obligations.
3. npx supercov runs latest file <path> --json
   npx supercov runs latest decision <path>:<line> --json
   npx supercov runs latest line <path>:<line> --json
4. Write ONE focused test that closes the specific obligations you just read.
   The assertion must be meaningful on its own; never assert something trivial
   just to execute a line.
5. npx supercov -- npm test
6. npx supercov diff <previous-run-id> latest --json
   If the diff shows no gain, revert the test rather than keeping it.

Rules:
- Never contort live application source to make coverage easier. Deleting
  provably dead code is the opposite case: coverage found real cruft, and
  removing it (with the project owner's normal review) is the improvement.
- Never weaken or delete an existing assertion.
- If a decision cannot be reached from any public entry point, say so and move
  on instead of exporting internals to reach it.
- Report the run ids you compared and the obligations you closed.
```

The last rule matters more than it looks. An unattended agent that cannot reach
a branch will otherwise start reshaping the code so it can, which is exactly
the failure mode that gives coverage targets a bad name. The first rule cuts
the other way just as deliberately: when an obligation is unreachable because
the code is dead, the honest fix is deletion, not an exclusion that leaves the
cruft sitting behind a clean number.

## Budgeting an overnight session

Test execution dominates the wall clock, so the number of passes is roughly the
time budget divided by suite duration. Two adjustments help:

- Narrow the command while iterating. `npx supercov -- npx vitest run
  app/checkout` produces a valid run over a smaller denominator; use the full
  suite for the baseline and the final verification.
- Let the build cache work. When the source, configuration and toolchain
  fingerprint is unchanged, the instrumented build is reused and that phase
  costs approximately nothing. Changing a dependency or a build config in the
  middle of a session throws that away.

## Choosing what to attack

`gaps` is ordered to be useful, but not every open obligation deserves a test.
For a project that prefers end-to-end evidence, start with the existing
projection rather than inventing a new test taxonomy:

```sh
npx supercov runs latest gaps --kind e2e
```

Each file distinguishes obligations covered by another test kind from those
uncovered everywhere. The former are candidates for stronger E2E coverage;
the latter are gaps in the combined suite. When an error path cannot be reached
through E2E, first check whether the test double can express that failure before
falling back to a narrower unit test.

Two queries help an agent argue about value rather than count:

```sh
# What does the suite prove today, minus redundancy?
npx supercov runs latest minimize --filter passed

# Reach a target with the smallest possible subset.
npx supercov runs latest minimize --filter passed --metric mcdc --target 80
```

`minimize` is an exact branch-and-bound solver, not a greedy approximation: the
subset it returns is a proved minimum. It refuses to answer for a view that
contains background or unattributed evidence, because there is no honest way to
name an exact subset of tests when the runner never exposed test boundaries.

## Reading a run that is not the newest

`latest` is a convenience for interactive use. An agent that resumes work later,
or that compares across a session, should use the immutable run id:

```sh
npx supercov runs --limit 10 --json
npx supercov runs run_0123456789abcdef gaps --json
```

Queries compare the stored fingerprint with the current workspace and mark a run
stale when the code has moved on. Treat a stale run as history, not as a
description of the working tree.

## What to do about honest gaps

Some obligations are open because the tooling says so, not because a test is
missing:

- **Background or unattributed evidence.** An unsupported runner, or work that
  arrived without a carrier, is recorded under a first-class background scope.
  It appears in the default all-attempt view and is excluded from per-test
  passed-only coverage. Writing more tests will not move it; adding runner
  support will.
- **Ambiguous source scope.** A candidate file that Supercov could not
  confidently classify as first-party blocks a complete verdict. Inspect with
  `coverage scope` and set `SUPERCOV_SOURCE_ROOTS` to declare the authoritative
  scope.
- **Semantic-safety blockers.** A function whose source is coerced or reflected
  on at runtime is left uninstrumented on purpose, and direct `eval` cannot have
  a stable denominator at all. Both are recorded with their exact location.

An agent should surface these rather than grind against them.
