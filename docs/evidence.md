# Evidence and runs

Every completed Supercov run is an immutable local record of what the suite
executed. Queries, comparisons, and filtered views are derived from that record.

## What a run contains

A run records:

- the fixed coverage denominator for the measured source;
- observed lines, statements, functions, branches, and decision vectors;
- test, attempt, runner, outcome, and phase identity where the runner exposes it;
- source, test, dependency, configuration, toolchain, and schema fingerprints;
- completeness blockers and unattributed background execution; and
- phase timings and integrity information.

Completed runs live under `.supercov/runs/<run-id>/`. The original evidence is
not rewritten when you query it. Supercov may build a disposable local index to
answer later queries faster; that index is derived data and can always be
recreated from the immutable run.

## Read a run

```sh
npx supercov runs --limit 10
npx supercov runs latest
npx supercov runs latest gaps --limit 10
npx supercov runs latest file app/checkout/session.ts
```

Use `latest` while working interactively. Use the run id printed by `runs` for
automation, review notes, and work that spans sessions.

Queries compare the stored fingerprint with the current workspace. A stale run
remains valid history, but it is no longer presented as a description of the
current source.

## Filter attempts

The same run can answer different questions without rerunning the suite:

```sh
npx supercov runs latest --filter all
npx supercov runs latest --filter passed
npx supercov runs latest --filter failed
```

- `all` includes every executed attempt and matches conventional coverage tools.
- `passed` includes successful attempts of tests that ultimately passed.
- `failed` isolates failed attempts, including failed retries of flaky tests.

Filtered views are recomputed from the stored evidence. They are not separate
report files that can drift apart.

## Compare two runs

```sh
npx supercov diff <older-run> <newer-run>
```

The diff shows newly covered and newly uncovered obligations. It is the easiest
way to prove that a focused test changed coverage without losing behavior
elsewhere.

## Combine shards

```sh
npx supercov merge <shard-a> <shard-b> [...]
```

Merge creates a new immutable run. Shards must have matching source,
configuration, toolchain, schema, and denominator fingerprints. Input runs are
never changed.

## Integrity and incomplete evidence

Supercov validates evidence before publishing a run. Corrupt, truncated,
duplicated, or contradictory input is rejected or surfaced as an explicit
measurement limit. It is never silently converted into a clean percentage.

Likewise, ambiguous source scope, uninstrumented code, and execution without a
reliable test identity remain visible. See [Coverage model](/docs/coverage-model)
for how these states affect completeness.

## Retention

Runs remain until you remove them:

```sh
npx supercov clean --dry-run
npx supercov clean --keep 20
npx supercov clean
```

Cleanup takes the same project lock as a coverage run and removes only
marker-owned Supercov data.
