# Workspace isolation

Supercov measures an instrumented copy of the project. It does not rewrite the
source tree you edit or the ordinary build output your project already owns.

## What Supercov may write

| Location | Purpose |
| --- | --- |
| `.supercov/runs/<run-id>/` | Immutable completed runs |
| `.supercov/work/` | In-progress state and evidence staging |
| `.supercov/locks/` | Prevents overlapping run and cleanup operations |
| `supercov/workspace/<project>/` | Marker-protected isolated source and build cache |

Supercov owns these locations only when its exact marker is present. If the
project already contains a user-created `supercov/` directory, Supercov does
not adopt or delete it; it chooses a deterministic fallback location instead.

The managed directories contain their own gitignore rules so run evidence and
instrumented builds do not become normal repository changes.

## What remains untouched

Supercov does not intentionally edit:

- application source or tests;
- imports or dependency declarations;
- test-runner configuration or reporter lists;
- the project's ordinary build output; or
- files outside its marker-owned storage.

The wrapped test command can still create anything it normally creates. The
isolation guarantee applies to Supercov's additional instrumentation, evidence,
and build work—not to side effects authored into the command itself.

## Repeated runs

When source, configuration, dependencies, toolchain, and build mode match,
Supercov can reuse the isolated instrumented build. Test-only changes do not
force an unrelated application rebuild.

Workspace updates are prepared separately and published only when complete, so
a failed refresh does not replace the last complete cache with a partial one.

## Crashes and concurrent commands

One project can have one coverage or cleanup transaction at a time. A second
operation fails clearly instead of racing the first.

In-progress state records allow the next command to recover after interruption,
forced termination, or host restart. Unpublished staging data is discarded;
completed runs are published atomically and remain immutable.

## Cleanup

Preview cleanup before deleting local Supercov data:

```sh
npx supercov clean --dry-run
npx supercov clean --keep 20
npx supercov clean
```

Cleanup follows marker ownership and refuses to race an active run. It does not
scan for similarly named directories or delete paths supplied by run metadata.

## Containers and remote workspaces

When a suite launches a container or VM from a mounted workspace, Supercov uses
the isolated workspace as the source presented to that environment. If an
executor hides its launch or mount boundary, Supercov reports the limitation
instead of claiming that unseen code was measured.
