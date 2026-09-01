# Runs and evidence

Every completed Supercov run is a local snapshot of what the suite executed.
You can return to it, ask different questions, or compare it with a later run
without rerunning the tests.

## Find the run you want

```sh
npx supercov runs
npx supercov runs --limit 10
npx supercov runs latest
```

Use `latest` during an interactive loop. Use the printed run id when:

- a task spans more than one session;
- an automated worker must not race a newer run;
- you are recording evidence in a pull request; or
- you need a reproducible comparison later.

A run id is immutable. `latest` is only a convenient selector.

## Ask the same run different questions

```sh
npx supercov runs latest gaps --limit 10
npx supercov runs latest file app/checkout/session.ts
npx supercov runs latest line app/checkout/session.ts:64
```

These queries read stored evidence. They do not run the test suite again or
rewrite the original run. Supercov may create a disposable local index to make
later queries faster; it can rebuild that index from the run.

## Understand stale runs

Supercov compares a stored run with the current workspace. If relevant source,
tests, dependencies, configuration, or toolchain inputs changed, the run is
marked stale.

A stale run is still valid history. It simply should not be presented as the
current state of the repository. Run the complete suite again before choosing
new work from it.

## Focus on passed or failed attempts

```sh
npx supercov runs latest --filter all
npx supercov runs latest --filter passed
npx supercov runs latest --filter failed
```

- `all` includes every executed attempt and matches the normal whole-run view.
- `passed` uses successful attempts from tests that ultimately passed.
- `failed` isolates failed attempts, including failed retries of flaky tests.

The views are recalculated from the same stored evidence. They are not separate
reports that can drift apart.

## Compare before and after

```sh
npx supercov diff <older-run> <newer-run>
```

The diff shows both newly covered and newly uncovered obligations. Use it after
a focused test to answer:

1. Did the expected behavior become covered?
2. Did anything unexpectedly become uncovered?
3. Did the source boundary or measurement completeness change?

Keep the compared run ids in the agent summary or pull request when someone may
need to reproduce the result.

## Combine distributed shards

```sh
npx supercov merge <shard-a> <shard-b> <shard-c>
```

Merge creates a new run and leaves the inputs unchanged. Shards must describe
the same source, configuration, toolchain, schema, and coverage denominator.
Supercov rejects incompatible inputs rather than producing a misleading total.

## What a run remembers

A run contains the coverage denominator, observed obligations, test and runner
identity where available, test outcomes and attempts, source and configuration
identity, measurement limits, and phase timings.

That is enough to answer later queries while keeping the original evidence
immutable. Corrupt, truncated, stale, or incompatible data is surfaced as such;
it is not opened as a plausible clean report.

## Storage and retention

Completed runs live under `.supercov/runs/<run-id>/`. The isolated workspace and
instrumented build cache may use more space than the compressed run itself.
Nothing is pruned in the background.

```sh
npx supercov clean --dry-run
npx supercov clean --keep 20
npx supercov clean
```

Preview cleanup first. The final command removes all runs and the isolated build
cache; `--keep 20` preserves the 20 newest runs. Cleanup removes only
marker-owned Supercov data.
