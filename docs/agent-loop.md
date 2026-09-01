# Agent workflow

Supercov works best as a small, repeatable loop: run the suite, choose one useful
gap, write one test, rerun, and prove what improved.

```text
run the suite  →  choose a gap  →  write one test  →  rerun  →  compare
      ↑                                                            |
      └────────────────────────────────────────────────────────────┘
```

Supercov supplies the coverage signal and evidence. Your coding agent writes
the tests.

## Choose the job

For one careful first pass, ask:

```text
Measure code coverage with `npx supercov` and write the first useful test based
on coverage. Only edit tests. Rerun the complete suite and report what improved.
```

For an overnight run or leftover token budget, ask:

```text
Use `npx supercov` to improve coverage. Only write tests. Keep going while
useful gaps remain. Never weaken assertions or change application code to make
coverage easier. Stop at a measurement limit, unreachable behavior, or the end
of the available time budget. Report the run ids compared and what improved.
```

The second prompt is intentionally open-ended, but 100% is a direction rather
than permission to write meaningless tests or reshape application code.

## One safe pass

```sh
# 1. Establish a baseline.
npx supercov -- npm test

# 2. Ask for a short list of useful targets.
npx supercov runs latest gaps --limit 5

# 3. Inspect one target.
npx supercov runs latest file app/checkout/session.ts
npx supercov runs latest decision app/checkout/session.ts:64
npx supercov runs latest line app/checkout/session.ts:64

# 4. Write one focused test, rerun, and prove the gain.
npx supercov -- npm test
npx supercov diff <previous-run-id> latest
```

For Rust, use `cargo test` or `cargo nextest run` in both runs. Keep the baseline
and verification commands identical.

The `line` query is useful before writing a test because it shows which tests
already reach that line. Extending a nearby test is often better than adding a
duplicate.

## A complete prompt for longer runs

```text
Use `npx supercov` to improve coverage. Only write tests. Keep going while
useful gaps remain.

Run the repository's complete test command through Supercov. Then repeat:
1. Run `npx supercov runs latest gaps --limit 5`.
2. Choose one useful uncovered behavior.
3. Inspect it with the `file`, `decision`, or `line` query.
4. Write one focused test with meaningful assertions.
5. Rerun the same complete suite through Supercov.
6. Run `npx supercov diff <previous-run-id> latest` to prove the gain.

Only edit tests. Never weaken assertions, delete tests, or change application
code to make coverage easier. Stop when no useful gap remains, a path is not
reachable through public behavior, Supercov reports a measurement limit, or
the time budget is exhausted. Report the run ids compared and what improved.
```

## Choose value, not just percentage

`gaps` ranks unresolved obligations, but the largest number is not always the
most valuable test. Prefer behavior around:

- permissions and access control;
- payments and state transitions;
- retries, failures, and recovery;
- user-visible outcomes; and
- public APIs with consequential edge cases.

Before writing a test, ask whether the behavior is reachable, whether an
existing test almost covers it, and whether the new test can make a meaningful
assertion. Dead code is usually something to report for human review, not a
reason to manufacture a test.

If the repository separates test levels, narrow the view:

```sh
npx supercov runs latest gaps --kind e2e --limit 10
```

## Keep the loop efficient

- Begin and end with the complete test command.
- A focused command is fine during iteration, but finish against the full
  denominator before reporting success.
- Write one related test at a time. Large batches make failures and gains hard
  to explain.
- Use immutable run ids when work spans sessions. Use `latest` for an
  interactive loop.
- Treat a run as history after the source or relevant configuration changes.

## Use Supercov in a software factory

Your factory schedules agents; Supercov gives each pass a bounded coverage task
and a durable result. A worker can run the suite, choose a gap, write one test,
and return the before-and-after run ids. The next worker can inspect that result
without relying on a dashboard or the previous agent's memory.

Keep the same safety contract in unattended work: tests only, meaningful
assertions, full-suite verification, and an explicit stop when the remaining
items are measurement limits rather than testable gaps.

## Know when to stop

Stop instead of grinding when:

- no useful uncovered behavior remains;
- the path cannot be reached through supported public behavior;
- source scope is ambiguous and needs `SUPERCOV_SOURCE_ROOTS`;
- the runner can provide only aggregate evidence for the question being asked;
- Supercov reports a measurement limit rather than an ordinary gap; or
- the next test would exist only to move a number.

See [Troubleshooting](troubleshooting.md) when a run appears incomplete or
unexpected.
