# Supported suites

Supercov supports JavaScript, TypeScript, and Rust today. Support is exact when
Supercov can identify individual test attempts; otherwise it reports aggregate
coverage without guessing which test caused a hit.

## Languages

| Language | Status | Supported commands |
| --- | --- | --- |
| JavaScript | Available | Existing Node-based test commands |
| TypeScript | Available | Existing Node-based test commands and build pipelines |
| Rust | Available | `cargo test`, `cargo nextest run` |
| Python | Coming soon | — |
| Zig | Coming soon | — |
| PHP | Coming soon | — |
| C | Coming soon | — |

More languages will follow. The current npm-distributed CLI requires Node.js 22
or newer for every language.

## JavaScript and TypeScript runners

| Runner | Attribution |
| --- | --- |
| Playwright | Exact per test, worker, retry, outcome, action, and assertion phase |
| Vitest | Exact per test, with setup execution kept separate |
| Jest | Exact per test, including concurrent and parameterized tests |
| `node:test` | Exact per test |
| AVA, Mocha, and other Node runners | Aggregate structural coverage |
| Browser component runners without an adapter | Aggregate structural coverage |

Aggregate evidence is still included in the full-run view. It is labelled as
background rather than being assigned to a test that may not have caused it.

A single command may launch several runners. Supercov combines their evidence
into one run and keeps the runner identity where exact attribution is available.

## Rust runners

| Runner | Attribution | Current boundary |
| --- | --- | --- |
| Cargo's standard libtest runner | Exact test and attempt identity | Run with `npx supercov -- cargo test` |
| cargo-nextest | Exact test, attempt, retry, and binary identity | Run with cargo-nextest 0.9.138 or 0.9.140 |

Rust support currently follows the Rust 1.95 toolchain and preserves Cargo's
test selection, scheduling, fail-fast behavior, environment, and exit status.
`cross` is not supported yet. Unsupported command shapes fail clearly rather
than falling back to plausible but inaccurate attribution.

## Builds and source formats

JavaScript and TypeScript projects can use Vite, Vitest, Next, Turbopack,
Webpack, esbuild, SWC, `tsc`, or no build step. ESM and CommonJS are supported,
along with modern JavaScript, JSX, TypeScript, and TSX syntax.

Supercov instruments an isolated workspace. It does not add imports, reporters,
or plugins to the authored project, and it does not overwrite the project's
ordinary build output.

## Browsers

Playwright coverage supports Chromium, Firefox, and WebKit. It follows pages,
frames, popups, workers, request contexts, WebSockets, and test-spawned child
processes where the runner exposes the required identity.

## Background processes and servers

Node child processes inherit coverage automatically. Long-running servers are
given a short drain window after the test command finishes so buffered evidence
can arrive before the run is published.

If work arrives without a reliable test identity, Supercov records it as
background evidence. The default whole-run view includes it; passed-only and
per-test views do not pretend it belongs to a particular test.

## Containers, VMs, and remote execution

Supercov can collect from supported processes launched through a container, VM,
or remote executor when the command exposes a discoverable launch boundary and
the Supercov runtime can be carried into that environment. Mounted workspaces
and local child-process launchers are the most direct path.

If the remote boundary hides how code is launched or cannot return evidence,
Supercov reports the missing coverage boundary. It does not silently treat
remote execution as measured.

## Distributed suites

Run shards separately, then merge compatible run ids:

```sh
npx supercov merge <shard-a> <shard-b> <shard-c>
```

All shards must describe the same source, configuration, toolchain, schema, and
coverage denominator. Incompatible shards are rejected with the mismatched
domains listed.
