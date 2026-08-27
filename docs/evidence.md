# Evidence and runs

A coverage number is only as trustworthy as the thing it was computed from.
Supercov keeps exactly one artifact per run and derives every view from it on
demand, so a report can never quietly disagree with the evidence it claims to
summarise.

## What a run is

```text
.supercov/runs/2026-08-24T01-25-11Z/
  evidence.raw.gz   exact denominator manifest + raw per-worker and background evidence
  run.json          fingerprints, phase timings, schema version, integrity state
```

Two files. No HTML, no derived report, no query cache. Loose evidence written
during the run is removed only after the whole run directory is atomically
visible, so a run is either complete or absent.

Run ids are UTC timestamps, which makes them sort chronologically and makes
retention deterministic.

## Derived, never stored

Every coverage view — the summary, per-file rankings, gap lists, decision
detail, per-test contribution, the minimizer, and the passed and failed filters
— is reconstructed from the archive when you ask for it. Nothing is written back.

This is why `--filter passed` and `--filter all` can never contradict each
other, and why a query added in a future version can answer questions about a
run recorded today: the stored schema is the raw evidence, not a rendering of it.

Fresh-process summary, files and gaps queries take roughly two tenths of a
second on the reference run described in [Performance](/docs/performance).

## Integrity and staleness

Each run stores SHA-256 fingerprints for:

- first-party source
- test files
- dependency lockfiles
- test and build configuration
- the instrumenter itself

plus the evidence schema version and the Git revision and dirty state at the
time of the run.

Queries compare the stored fingerprint against the current workspace and
visibly mark a stale run. Evidence carrying a different run scope is rejected
outright rather than merged in.

## Comparing two runs

```sh
npx supercov diff <older-run> <newer-run>
npx supercov diff <older-run> <newer-run> --json
```

`diff` reports what the newer run covers that the older one did not, and what
it lost. Both inputs are immutable and untouched, which is what makes the
comparison meaningful: neither side can have been rewritten by the act of
comparing them.

## Merging shards

```sh
npx supercov merge <first-run-id> <second-run-id> [...]
```

`merge` accepts only runs whose source, test, dependency, configuration,
instrumenter, schema and denominator fingerprints are identical. It rewrites
the run scope inside every evidence record, namespaces shard paths, and
publishes a new immutable run atomically. Input runs are never modified or
deleted.

This is the distributed and multi-host primitive. Incompatible shards fail with
the exact differing fingerprint domains rather than producing a plausible but
invalid aggregate — two shards built from different source trees do not have a
common denominator, and no amount of arithmetic creates one.

## Durability

Everything that can be interrupted is written to survive it.

- Evidence archive, metadata and state writes use sibling temporary files,
  `fsync`, and atomic rename.
- Lock acquisition uses exclusive creation followed by `fsync`.
- Run state is written durably through the preparing, building, testing and
  publishing phases.
- `SIGINT`, `SIGTERM` and `SIGHUP` are forwarded to the entire child process
  group.
- If the process is killed without a cleanup opportunity, the next invocation
  marks the dead PID's run abandoned and refreshes the isolated namespace
  before reusing it.

The published `run.json` is the durable terminal record, so terminal work state
is not retained after publication.

## Retention

```sh
npx supercov clean
npx supercov clean --keep 20 --dry-run
npx supercov clean --keep 20
```

Cleanup never runs automatically. `clean` removes explicit history, orphaned
and terminal transient data, and the marker-owned build workspace; `--keep N`
preserves the N newest runs. It acquires the same lock as a coverage run,
refuses to race an active run, and never touches unowned paths.

## Phase timings

Every run records monotonic durations for initialization, workspace
preparation, adapter setup, the instrumented build, your unchanged test command,
and evidence publication. They are stored in `run.json` and returned by
`supercov runs --json`.

These are timings, not an overhead claim. A test script that performs its own
build may overlap work with the instrumented-build phase, and true end-to-end
overhead requires an explicit control run — which Supercov never performs
automatically, because an arbitrary test command can write data or cost money.
[Performance](/docs/performance) documents the comparison methodology.
