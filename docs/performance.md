# Performance and storage

Test execution usually dominates a Supercov run. Supercov records the other
phases separately so you can see whether time is going into workspace setup,
instrumentation, the test command, or evidence publication.

## Read run timings

```sh
npx supercov runs --limit 5
npx supercov runs latest
```

Each run records:

| Phase | Includes |
| --- | --- |
| Initialization | Recovery, locking, project discovery, and fingerprints |
| Workspace preparation | Refreshing the isolated project workspace |
| Adapter setup | Preparing runner integration and runtime files |
| Instrumented build | Building instrumented source, or near-zero on an exact cache hit |
| Test command | The wrapped command, including runner and remote latency |
| Evidence publication | Validation, archive creation, summary analysis, and atomic publication |

The same fields are available in `run.json` and in `runs --json` when an
integration needs machine-readable timings.

## Keep repeated runs fast

- Use the same complete command for the baseline and final verification.
- Let the isolated build cache survive between passes.
- Avoid changing dependencies, build configuration, or toolchains in the
  middle of a coverage loop unless the test requires it.
- Use a focused test command while iterating, then finish with the full suite.
- Query the stored run instead of rerunning merely to inspect a different view.

Supercov reuses an instrumented build only when the relevant source,
configuration, dependencies, toolchain, build mode, and instrumenter identity
match exactly. A mismatch causes a fresh build rather than risking stale
coverage.

## Measure end-to-end overhead

Supercov never runs the test command a second time automatically because tests
may write data, call paid services, or be intentionally non-repeatable. To
measure overhead, compare the original and wrapped command under the same cache
state:

```sh
/usr/bin/time -p npm test
/usr/bin/time -p npx supercov -- npm test
```

Use several alternating pairs and compare medians. Do not compare a cold
package, browser, build, or VM cache with a warm one. A first `npx` download is
package-acquisition time, not coverage-engine time.

## Storage

Each completed run stores compressed raw evidence and a small metadata file
under `.supercov/runs/<run-id>/`. Query views are derived from that evidence;
Supercov does not retain a separate full report for every filter.

The isolated workspace can be larger than a run because it may contain an
instrumented build cache. Control retention explicitly:

```sh
npx supercov clean --dry-run
npx supercov clean --keep 20
npx supercov clean
```

Supercov never prunes runs in the background. Cleanup is explicit so historical
evidence does not disappear during an unattended agent session.
