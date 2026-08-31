# Agent loop

Use Supercov in a simple loop: run the suite, choose one useful gap, write one
test, rerun, and prove what improved.

```text
run the suite  →  choose a gap  →  write one test  →  rerun  →  compare
      ↑                                                            |
      └────────────────────────────────────────────────────────────┘
```

## One pass

```sh
# 1. Establish a baseline.
npx supercov -- npm test

# 2. Choose a useful target without loading a large report.
npx supercov runs latest gaps --limit 5

# 3. Understand the target and what already reaches it.
npx supercov runs latest file app/checkout/session.ts
npx supercov runs latest decision app/checkout/session.ts:64
npx supercov runs latest line app/checkout/session.ts:64

# 4. Write one focused test, then rerun and prove the gain.
npx supercov -- npm test
npx supercov diff <previous-run-id> latest
```

For Rust, replace `npm test` with `cargo test` or `cargo nextest run` in both
runs. Keep the command identical between the baseline and comparison.

The `line` query is useful before writing a test: it shows what already
executes that line, which can reveal an existing test to extend instead of a
duplicate to add.

## Prompt for a coding agent

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

Only edit tests. Never weaken assertions or change application code to make
coverage easier. Stop when no useful gaps remain, a gap is not reachable
through a public behavior, or the time budget is exhausted. Report the run ids
you compared and what improved.
```

## Choose valuable gaps

`gaps` ranks unresolved obligations, but coverage count is not the same as
product value. Prefer code that protects user-facing behavior, permissions,
payments, state transitions, error recovery, and other high-consequence paths.

Useful checks before writing a test:

- Is this behavior reachable through a public API or user action?
- Does an existing test almost cover it?
- Can the test make a meaningful assertion rather than merely execute a line?
- Is the path actually dead code that should be reported for human review?

If the project separates test levels, focus the view:

```sh
npx supercov runs latest gaps --kind e2e --limit 10
```

## Keep the loop efficient

- Begin and end with the complete test command.
- While iterating, a narrower test command is fine if its smaller denominator
  is understood.
- Write one related test at a time, then rerun. Large batches make failures and
  coverage gains harder to attribute.
- Use immutable run ids when work spans several sessions. `latest` is a
  convenience for interactive use.
- Treat a stale run as history when the source has changed since it was made.

## Know when to stop

Stop instead of grinding when:

- no useful uncovered behavior remains;
- the open path cannot be reached through a supported public behavior;
- source scope is ambiguous and needs `SUPERCOV_SOURCE_ROOTS`;
- execution belongs to an unsupported or unattributed runner; or
- Supercov reports a completeness blocker rather than an ordinary test gap.

These states are reported explicitly so an agent does not reshape application
code merely to reach a number.
