# Troubleshooting

Start with the run summary. It usually tells you whether the problem is the test
command, source discovery, runner attribution, or a measurement boundary.

```sh
npx supercov runs latest
npx supercov runs latest scope
npx supercov runs latest runners
```

## `npx` cannot start Supercov

The first `npx supercov` invocation may need to reach the npm registry. Check
Node.js first:

```sh
node --version
npx supercov --version
```

Supercov requires Node.js 22 or newer. Registry, proxy, authentication, or
offline-cache errors happen before the Supercov CLI starts; resolve them as you
would for another npm package.

## No application source was found

Run the same command from the repository root. If first-party code lives in an
unusual directory, declare the source roots explicitly:

```sh
SUPERCOV_SOURCE_ROOTS=src,app npx supercov -- npm test
```

Then inspect what Supercov included and excluded:

```sh
npx supercov runs latest scope
```

Do not broaden the roots to dependencies or generated output merely to remove a
warning. The goal is an honest boundary around code the repository owns.

## The tests pass but coverage is missing

First check runner and source scope:

```sh
npx supercov runs latest runners
npx supercov runs latest scope
npx supercov runs latest gaps
```

Some runners expose exact test boundaries; others provide only aggregate
coverage. Processes inside a container, VM, remote executor, or hidden launcher
may also sit beyond the instrumentation boundary. Supercov reports that limit
instead of assigning execution to a test that may not have caused it.

Compare the command with [Supported suites](supported-suites.md). If the runner
should be supported, preserve the summary and exact command when reporting the
problem.

## A run is marked stale

A stored run remains valid history, but it stops describing the current
workspace after relevant source, tests, dependencies, configuration, or
toolchain inputs change. Rerun the same complete command to create a current
baseline:

```sh
npx supercov -- npm test
```

Use immutable run ids in review notes and automation. Use `latest` only when the
newest local run is the one you intend to inspect.

## `gaps` shows a measurement limit

A measurement limit is not an uncovered path. It means Supercov could not
establish a complete measurement boundary—for example, because source scope is
ambiguous, code creates source dynamically, or execution crossed an unsupported
boundary.

Read the reason in the summary, `scope`, or `gaps` output. Fix a configuration
problem when one is named. Otherwise stop the coverage loop and report the
limit; do not change application code or add a meaningless test to chase 100%.

## Coverage is aggregate instead of per test

Aggregate coverage still shows which source ran, but Supercov cannot truthfully
say which individual test caused it. This is expected for Node runners without
an exact adapter and for background work without a reliable test identity.

Use the whole-run `gaps` and `file` views. Per-test queries become useful when
the runner exposes exact test and attempt boundaries.

## A second command says Supercov is busy

One project can publish or clean one coverage store at a time. Let the active
run finish before starting another Supercov command. If a process was
interrupted, the next command recovers its unpublished staging state; completed
runs remain intact.

## The first run is slow

The first run may include the npm download, browser or toolchain startup,
workspace creation, and an instrumented build. Repeated runs can reuse the
isolated build when source, dependencies, configuration, toolchain, and build
mode still match.

Inspect the recorded phases with:

```sh
npx supercov runs latest
```

See [Speed and storage](performance.md) for practical ways to shorten a loop.

## Supercov is using too much disk space

Preview cleanup, then choose how much history to keep:

```sh
npx supercov clean --dry-run
npx supercov clean --keep 20
npx supercov clean
```

The final command removes all stored runs and the isolated build cache. Cleanup
only removes marker-owned Supercov data.

## Ask for help

Open an issue in the [Supercov repository](https://github.com/supercorp-ai/supercov/issues)
with:

- `npx supercov --version`;
- the exact wrapped test command;
- the relevant summary, `scope`, and `runners` output; and
- a small reproduction when the repository can be shared.

Do not include secrets, private source, or raw evidence from a repository you
cannot share.
