# Getting started

Supercov gives a coding agent the next test to write. Run the test command you
already use, ask which useful paths remain uncovered, add a focused test, and
repeat.

```sh
npx supercov -- npm test
```

No account, config file, import, custom reporter, or hosted service is required.

## Language support

| Language | Status | Start with |
| --- | --- | --- |
| JavaScript | Available | `npx supercov -- npm test` |
| TypeScript | Available | `npx supercov -- npm test` |
| Rust | Available | `npx supercov -- cargo test` |
| Python | Coming soon | — |
| Zig | Coming soon | — |
| PHP | Coming soon | — |
| C | Coming soon | — |

More languages are planned. See [Supported suites](/docs/supported-suites) for
the runners and attribution available today.

## Requirements

- Node.js 22 or newer. The current CLI is distributed through npm, including
  when it measures a Rust project.
- A working test command for the project.
- For Rust, the Rust 1.95 toolchain. `cargo test` is supported directly;
  `cargo nextest run` is supported with cargo-nextest 0.9.138 or 0.9.140.

Package tools such as `npx` may contact the npm registry to download Supercov
when it is not cached. The Supercov CLI does not contact a Supercov service
during a coverage run, and your source and evidence stay on your machine.

## Run the complete suite

Everything after `--` is your test command. Use the same command you trust
before merging or deploying:

```sh
# JavaScript or TypeScript
npx supercov -- npm test
npx supercov -- npx playwright test
npx supercov -- pnpm test:e2e

# Rust
npx supercov -- cargo test
npx supercov -- cargo nextest run
```

If one command launches several supported runners, Supercov combines their
evidence into one run.

## Find the next test

Start broad, then open one useful target:

```sh
npx supercov runs latest
npx supercov runs latest gaps --limit 10
npx supercov runs latest file app/checkout/session.ts
npx supercov runs latest decision app/checkout/session.ts:64
```

The output is short and paginated so a coding agent can use it directly. Add
`--json` only when a tool specifically needs machine-readable output.

## Add a test and prove the gain

Write one focused test, rerun the same complete command, and compare the two
runs:

```sh
npx supercov -- npm test
npx supercov diff <previous-run-id> latest
```

For Rust, rerun the same `cargo test` or `cargo nextest run` command you used
for the baseline.

## Give the loop to an agent

Paste this into any coding agent that can run terminal commands:

```text
Use `npx supercov` to improve coverage. Only write tests. Keep going while
useful gaps remain.

Run the repository's complete test command through Supercov. Use
`npx supercov runs latest gaps --limit 5` to choose one useful target. Write
one focused test, rerun the complete suite, and verify the gain with
`npx supercov diff <previous-run-id> latest`.

Never weaken assertions or change application code to make coverage easier.
Stop when no useful gaps remain.
```

## Local files and cleanup

Completed runs live under `.supercov/runs/`. Supercov also maintains a
marker-protected isolated workspace for instrumented builds. It does not rewrite
your source, tests, imports, runner configuration, dependency tree, or ordinary
build output.

```sh
npx supercov clean --dry-run   # preview a full cleanup
npx supercov clean --keep 20   # retain the 20 newest runs
npx supercov clean             # remove all runs and the build cache
```

## Next

- [Agent loop](/docs/agent-loop) — a repeatable coverage workflow for coding agents.
- [CLI reference](/docs/cli) — commands and filters.
- [Coverage model](/docs/coverage-model) — what Supercov measures.
- [Supported suites](/docs/supported-suites) — languages, runners, and current limits.
