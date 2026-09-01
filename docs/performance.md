# Speed and storage

A Supercov run includes your test command, an instrumented build, and evidence
publication. The first pass is usually the slowest; repeated passes can reuse
the isolated build when the relevant inputs have not changed.

## See where the time went

```sh
npx supercov runs latest
```

The summary separates:

| Phase | What it includes |
| --- | --- |
| Initialization | Recovery, project discovery, and input checks |
| Workspace preparation | Refreshing the isolated project copy |
| Adapter setup | Preparing the runner integration |
| Instrumented build | Building measured source, or almost nothing on a cache hit |
| Test command | The wrapped command, including browser, VM, or remote latency |
| Evidence publication | Validating and storing the completed run |

The first `npx` invocation may also download the package. That download happens
before Supercov starts and is not coverage-engine overhead.

## Keep an agent loop fast

- Keep the isolated build cache between passes.
- Avoid changing dependencies, build configuration, or toolchains during the
  loop unless the test requires it.
- Query the stored run instead of rerunning merely to open another view.
- Write one related test at a time, then rerun.
- Use a focused test command while iterating when appropriate, but finish with
  the same complete command used for the baseline.

Supercov reuses an instrumented build only when source, dependencies,
configuration, toolchain, build mode, and instrumenter identity match. A
possible mismatch triggers a fresh build rather than risking stale coverage.

## Use focused runs carefully

A narrow test command can shorten the inner loop:

```sh
npx supercov -- npx vitest run app/checkout/session.test.ts
```

That run has a narrower evidence set than the complete suite. Before reporting
success, rerun the repository's full command and compare against a full-suite
baseline.

## Measure overhead in your project

Compare the original and wrapped command under similar cache conditions:

```sh
/usr/bin/time -p npm test
/usr/bin/time -p npx supercov -- npm test
```

Alternate the two commands several times and compare typical runs. Do not
compare a cold package, browser, build, or VM cache with a warm one. Supercov
never runs the test command a second time automatically because suites may write
data, call paid services, or be intentionally non-repeatable.

## Understand disk usage

Each completed run stores compressed evidence and metadata under
`.supercov/runs/<run-id>/`. Query views are derived from that evidence rather
than stored as a full report for every filter.

The isolated workspace may be larger because it can contain an instrumented
build cache. Supercov does not delete history in the background.

```sh
npx supercov clean --dry-run
npx supercov clean --keep 20
npx supercov clean
```

Use `--dry-run` to preview cleanup. Keep enough run history for active reviews
and automation; remove the cache only when reclaiming space matters more than a
faster next run.
