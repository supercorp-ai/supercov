# Files, privacy, and cleanup

Supercov measures an instrumented copy of the project. It does not rewrite the
source tree you edit or send your source and coverage evidence to a hosted
Supercov service.

## What uses the network

The first `npx supercov` invocation may contact the npm registry to download the
package. Your wrapped test command may also use the network if it normally does.

The Supercov CLI does not need a Supercov account or upload a coverage run to
Supercov. Run evidence and query indexes stay on the machine.

## What stays untouched

Supercov does not intentionally edit:

- application source or tests;
- imports or dependency declarations;
- test-runner configuration or reporter lists;
- the project's ordinary build output; or
- files outside marker-owned Supercov storage.

The wrapped command still has its normal side effects. If `npm test` writes
snapshots, calls a service, or changes a database, wrapping it does not remove
that behavior. The isolation guarantee applies to Supercov's instrumentation
and evidence work.

Files the wrapped command creates or changes inside the isolated workspace are
synced back to the project after the run, so `supercov -- npm test -- -u`
updates snapshots in the repository exactly as `npm test -- -u` would. Two
exceptions are reported instead of applied: changes the command makes to
instrumented source files (the instrumented copies must never overwrite your
sources) and deletions (never propagated automatically).

## Files Supercov creates

| Location | What it is for |
| --- | --- |
| `.supercov/runs/<run-id>/` | Completed immutable runs |
| `.supercov/work/` | Temporary state while a run is being prepared |
| `.supercov/locks/` | Prevents two operations from racing |
| `.supercov-workspace/workspace/<project>/` | Isolated source and reusable instrumented build cache (safe to delete) |

Managed directories include Git ignore rules so run evidence and instrumented
builds do not become ordinary repository changes.

Supercov owns a directory only when its exact marker is present. If the project
already has a user-created `supercov/` directory, the CLI chooses a deterministic
fallback instead of adopting or deleting it.

## Repeated runs and the build cache

When source, dependencies, configuration, toolchain, and build mode still
match, Supercov can reuse the isolated instrumented build. Test-only changes do
not force an unrelated application rebuild.

Workspace refreshes are prepared separately and become active only when
complete. An interrupted refresh does not replace the last complete cache with
partial output.

## Interrupted and overlapping commands

One project can run one coverage or cleanup transaction at a time. A second
operation fails clearly instead of racing the first.

After interruption or host restart, the next command recovers unpublished
staging state. Completed runs remain immutable.

## Clean up local data

Preview cleanup before removing anything:

```sh
npx supercov clean --dry-run
npx supercov clean --keep 20
npx supercov clean
```

The final command removes all runs and the isolated build cache. `--keep 20`
retains the 20 newest runs. Cleanup follows marker ownership, waits for the
project lock, and does not scan for similarly named directories.

## Containers and remote workspaces

When a suite launches a container or VM from a mounted workspace, Supercov uses
the isolated workspace as the source presented to that environment. The runtime
must be able to cross the launch boundary and return evidence.

If a remote executor hides the launch or mount boundary, Supercov reports the
limitation instead of claiming unseen code was measured. See
[Supported suites](supported-suites.md) for the current boundary.
