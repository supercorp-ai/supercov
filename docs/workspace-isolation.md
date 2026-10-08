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

## Tools that judge the code

Some tools a test command runs read or measure the code instead of running it,
and the instrumented copy is not what you wrote:

- Linters, formatters and `tsc --noEmit` (ESLint, Prettier, standard, xo and
  the like) read each file as you wrote it, and don't see `.supercov`.
- The type check `next build` runs reads each file as you wrote it too, while
  the build itself compiles the instrumented copy.
- Biome, oxlint, dprint, cspell and knip run on your source as you wrote it:
  the copy's `node_modules/.bin` starts each of them in a view of the copy
  that holds every file in its original text. A file the tool writes there
  (a report, a cache) is the command's output like any other. To have another
  tool that reads source as text run the same way, name it:
  `SUPERCOV_SOURCE_TOOLS=typos,stylelint`.
- A change one of these tools makes to a source file (`prettier --write`,
  `eslint --fix`, `biome check --write`, `dprint fmt`) is made in your
  project when the command ends, and a tool later in the same command reads
  the changed text. The tests ran the copy instrumented before the change, so
  the run measured the file as it was, says so, and reads as stale; the next
  run measures the changed file. A file you edited yourself while the command
  ran keeps your edit, and the run names it.
- Coverage tools the command runs itself (tap, c8, nyc, Jest's and Vitest's
  `--coverage`) still collect and report coverage. They measure the
  instrumented copy, and report close to what they report without Supercov,
  but not always exactly, so their thresholds are not checked, and the run
  prints a line saying so. Supercov's own report has the run's coverage.

Files the wrapped command creates or changes inside the isolated workspace are
synced back to the project after the run, so `supercov -- npm test -- -u`
updates snapshots in the repository exactly as `npm test -- -u` would. Three
exceptions are reported instead of applied: changes the command makes to
instrumented source files (the instrumented copies must never overwrite your
sources), a build the command made from them, and deletions (never propagated
automatically). Such a build stays behind whole: when a file in `.next/`,
`dist/` or another directory that git ignores or the project does not have was
built from instrumented source, nothing in that directory is copied, so the
project never holds an instrumented build. Changes inside any `node_modules`
directory are neither applied nor reported: dependency trees are not command
outputs.

The run prints where the synced files went, what stayed behind and what the
command deleted, by top-level directory (`test-results/ 6`, `.next/ 2547`).

The copy starts without build output: `dist/`, `build/`, `.next/` and the
like are not copied, because they hold code that was never instrumented.
Supercov runs the command you give it and no build of its own, so a suite that
needs build output has the build in its command:

```bash
npx supercov -- sh -c "npm run build && npm test"
```

A `pretest` script, or a Playwright `webServer` that builds, does the same.
When a run fails in a project that has a `build` script the command does not
reach, it says this. `SUPERCOV_KEEP_WORKSPACE=1` leaves the instrumented copy
in `.supercov/workspaces/` after a run, to inspect.

## Files Supercov creates

| Location | What it is for |
| --- | --- |
| `.supercov/runs/<run-id>/` | Completed immutable runs |
| `.supercov/work/` | Temporary state while a run is being prepared |
| `.supercov/locks/` | Prevents two operations from racing |
| `.supercov/workspaces/` | Isolated source mirror and reusable instrumented build cache (safe to delete) |

Managed directories include Git ignore rules so run evidence and instrumented
builds do not become ordinary repository changes.

Supercov owns a directory only when its exact marker is present. If the project
already has a user-created `supercov/` directory, the CLI chooses a deterministic
fallback instead of adopting or deleting it.

## Repeated runs and the build cache

For a compiled language, when source, dependencies, configuration, toolchain,
and build mode still match, Supercov can reuse the isolated instrumented build,
so test-only changes do not force an unrelated rebuild. A JavaScript project's
build is part of its command and runs every time.

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

```sh supercov
npx supercov runs clean --dry-run
npx supercov runs clean --keep 20
npx supercov runs clean
```

The final command removes all runs and the isolated build cache. `--keep 20`
retains the 20 newest runs. Cleanup follows marker ownership, waits for the
project lock, and does not scan for similarly named directories.

## Containers and remote workspaces

When a suite launches a container or VM from a mounted workspace, Supercov uses
the isolated workspace as the source presented to that environment. Where it
sees the launch call -- a command and its environment passed as an `env`,
`environment` or `envs` option, as a plain map after the command, or to a
builder method such as Testcontainers' `withEnvironment` -- it hands the guest
its settings with paths moved to the guest's mount, and the guest's coverage is
credited to the test that launched it. A guest started any other way, with none
of those settings, still runs the instrumented code: it reads the run's
settings from the mounted workspace itself, and its coverage counts for the run
as background coverage, credited to no test. The run summary's `Guests` line
says how many processes did that.

Dependencies stay out of the instrumented copy. The root `node_modules` is
linked entry by entry to the project's own, so an environment that mounts the
workspace must bring its own root dependencies (the supported launchers do).
Nested `node_modules` inside packages and extensions are materialised as real
directories: cloned copy-on-write where the filesystem allows it (APFS), and
hard-linked file by file elsewhere on Unix, so they resolve inside the mount
either way. Only when neither is possible, such as a dependency tree on another
volume, are they linked entry by entry like the root.

If a remote executor hides the launch or mount boundary, Supercov reports the
limitation instead of claiming unseen code was measured. See
[Supported suites](supported-suites.md) for the current boundary.
