# Workspace isolation

Supercov writes generated and temporary files only under the project's
`.supercov/` directory. Application source and ordinary build artifacts are
not write targets, and cleanup does not depend on a signal handler.

## Owned paths

For `supercov -- <command>`, every Supercov-created persistent or temporary path
is below the project's `.supercov/` directory:

| Path | Lifetime |
| --- | --- |
| `locks/active.json` | Exclusive run or cleanup transaction; removed by its owner, stale owners are recovered. |
| `work/<run>/state.json` | In-flight lifecycle record; removed after atomic run publication. |
| `work/<run>/run-publication/` | Incomplete run staging; atomically renamed or removed on recovery. |
| `evidence/<run>/` | Loose in-flight evidence; packed and removed after publication. |
| `runs/<run>/` | Immutable `evidence.raw.gz` (manifest plus raw execution evidence) and `run.json`; retained until explicit `clean`. Derived query views are cached only after their first query. |
| `supercov/workspace/<project>/` | Stable physical fallback and provider snapshot cache. The non-dotted ancestor keeps Express/`send` and similar static-file stacks semantically unchanged. |
| `supercov/workspace/.<project>.staging-*` | Unpublished cache transaction; removed on error or recovery. |
| `supercov/workspace/.<project>.previous-*` | Last complete cache generation during publication; restored or removed on recovery. |
| `supercov/workspace/<project>/.supercov/server-evidence/<run>/` | Server/background transport shared with local or mounted guest processes; archived and removed after publication, interruption, refresh, or cleanup. |

The lower-level runtime retains `/tmp/supercov-server-evidence` only as a
fallback when it is embedded without the Supercov CLI and no owned transport
root is configured. Normal CLI runs always inject an owned root, and VM path
translation maps that same root into the guest mount.

## Current physical fallback

Builds and opaque VM/container mounts currently use a stable physical namespace
at `supercov/workspace/<project>`. The `supercov/` container is owned only when
its exact marker is present; if a project already owns that name, Supercov uses
a deterministic non-dotted fallback and copies the user's directory as ordinary
source rather than adopting or excluding it. Refresh has four states:

1. The last complete source generation remains live while a sibling `staging`
   tree is prepared.
2. The live generation is renamed to a uniquely named `previous` tree.
3. The complete `staging` tree is renamed to the stable name.
4. The obsolete `previous` tree is removed.

All publication renames stay on one filesystem and their parent directory is
fsynced. Recovery treats `staging` as never published. If the stable name is
missing, recovery restores the newest `previous` generation; if the stable name
exists, all `previous` generations are obsolete. This covers a normal error,
SIGKILL, power loss, and host restart at every boundary without trusting a path
read from a state file.

Source files request Node's
[`COPYFILE_FICLONE`](https://nodejs.org/api/fs.html#fspromisescopyfilesrc-dest-mode)
mode. On filesystems such as APFS that support reflinks, file contents are
copy-on-write. Node explicitly falls back to a real copy on unsupported
filesystems, however, and directory entries must always be recreated. This is
why the transaction rules remain necessary.

An exact fingerprint over application source, dependencies, configuration,
build mode, instrumenter runtime, build command, Node, OS, and architecture
allows a complete instrumented output and its manifest to survive a source
snapshot refresh. Test-only edits do not invalidate it. A missing artifact or
any key change forces a new build.

The stable physical path is not accidental. Some VM/container systems include
the host mount path in a snapshot identity. A fresh random path per run would
avoid retention but force a cold machine snapshot every time.

## Why FUSE is not the default

A FUSE overlay could present transformed bytes at the original relative paths,
but it adds a kernel/system extension or privileged mount dependency on common
developer platforms. Mount teardown also becomes another failure boundary. A
zero-install `npx supercov` command cannot assume that dependency, so FUSE may
become an opt-in adapter but is not a safe portable baseline.

## Copy-free target architecture

The intended architecture is capability-based rather than one mechanism for
every runner:

- Node [module customization hooks](https://nodejs.org/api/module.html#customization-hooks)
  can return transformed source for ESM and CommonJS without changing files.
  This is appropriate only when the observed loader chain preserves source
  identity and source maps.
- Vite/Rollup-compatible tools should transform modules in their native plugin
  graph and relocate every Supercov-added cache/output into `.supercov/`.
- A Node coordinator that later exposes an opaque VM/container mount can lazily
  materialize the transactional physical fallback and replace only that mount.
- Native/non-Node compilers and remote control planes that never expose reads or
  mounts need explicit adapters or the physical fallback. Supercov must report
  this boundary instead of silently claiming coverage.

The command itself may create files that the same test command normally creates.
Supercov is responsible for ensuring that instrumentation, generated config,
manifests, evidence, and additional builds do not add writes outside
`.supercov/`.

## Release blockers for a copy-free mode

A copy-free path is not eligible as the default until its regression suite
proves all of the following:

- original and transformed programs remain semantically equivalent;
- ESM, CommonJS, TypeScript/transpiler, worker, and child-process loader chains
  preserve exact source attribution;
- test/build configuration and source fingerprints refer to the original tree;
- output relocation cannot escape `.supercov/`, including plugin-defined
  outputs;
- an opaque remote mount either receives a complete materialized fallback or is
  rejected clearly;
- SIGINT, SIGTERM, SIGKILL simulation, concurrent `clean`, ENOSPC, and failed
  rename/copy injection leave no source changes and recover deterministically;
- retained bytes and startup time are measurably lower than the transactional
  reflink fallback.

Until those gates pass, the transactional physical namespace is the conservative
fallback rather than a temporary-directory mount whose cleanup must succeed.

The filesystem gate runs the transaction suite on Linux, macOS, and Windows,
including reflink/ordinary-copy behavior, internal links or junctions, ENOSPC,
failed renames, and forced-process-termination recovery.
