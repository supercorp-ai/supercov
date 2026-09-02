# Getting started

Supercov turns the test suite you already have into a list of useful tests to
write next. Run the suite once, inspect a coverage gap, add a focused test, and
compare the result.

```sh
npx supercov -- npm test
```

No account, config file, import, custom reporter, or hosted service is required.
Supercov supports JavaScript, TypeScript, Rust, and Python today.

## Before you start

You need:

- Node.js 22 or newer;
- a test command that already works in the repository; and
- for Rust, the Rust 1.95 toolchain;
- for Python, CPython 3.12 or newer with pytest or unittest.

The CLI is distributed through npm, even for Rust projects. The first `npx`
invocation may download Supercov from the npm registry. Supercov itself does not
upload your source or coverage evidence to a Supercov service.

## 1. Run your real test command

Everything after `--` is the command Supercov measures. Start with the same
complete command you trust before merging or deploying:

```sh
# JavaScript or TypeScript
npx supercov -- npm test
npx supercov -- npx playwright test
npx supercov -- pnpm test:e2e

# Rust
npx supercov -- cargo test
npx supercov -- cargo nextest run

# Python
npx supercov -- pytest
```

Supercov runs that command in an isolated, instrumented copy of the project.
The command keeps its normal arguments, environment, output, and exit status.
If one command launches several supported runners, their evidence lands in one
run.

## 2. Read the first result

Open the newest run:

```sh
npx supercov runs latest
```

The summary answers three practical questions:

1. Did the test command pass?
2. How much behavior did the suite cover?
3. Is anything genuinely uncovered, or was some code impossible to measure?

An uncovered gap is a candidate for a test. A measurement limit is different:
it means Supercov cannot honestly account for that code yet. Do not try to test
away a measurement limit.

## 3. Choose one useful gap

Ask for a short list, then inspect one file:

```sh
npx supercov runs latest gaps --limit 10
npx supercov runs latest file app/checkout/session.ts
```

Use the more specific queries when you need them:

```sh
npx supercov runs latest decision app/checkout/session.ts:64
npx supercov runs latest line app/checkout/session.ts:64
```

`file` is usually the best place to start. `decision` explains missing boolean
outcomes and MC/DC witnesses. `line` shows the obligations and tests associated
with one source line.

## 4. Add a test and prove the gain

Write one focused test with a meaningful assertion. Then rerun the same complete
command and compare the two runs:

```sh
npx supercov -- npm test
npx supercov diff <previous-run-id> latest
```

For Rust, rerun the same `cargo test` or `cargo nextest run` command used for the
baseline. A useful change leaves the suite passing and shows the expected gain
without an unexplained loss elsewhere.

## Give the loop to a coding agent

Paste this into any coding agent that can run terminal commands:

```text
Use `npx supercov` to improve coverage. Only write tests. Keep going while
useful gaps remain.

Run the repository's complete test command through Supercov. Use
`npx supercov runs latest gaps --limit 5` to choose one useful target. Inspect
the target, write one focused test with meaningful assertions, rerun the same
complete suite, and verify the gain with
`npx supercov diff <previous-run-id> latest`.

Never weaken assertions or change application code to make coverage easier.
Stop when no useful gap remains or Supercov reports a measurement limit instead
of an ordinary gap.
```

Replace `npm test` with the repository's real complete test command when needed.

## Files and cleanup

Completed runs live under `.supercov/runs/`. Supercov also keeps an isolated
workspace for instrumented builds. These files are local and ignored by Git.
Supercov does not rewrite your source, tests, imports, runner configuration,
dependencies, or ordinary build output.

```sh
npx supercov clean --dry-run   # preview what would be removed
npx supercov clean --keep 20   # keep the 20 newest runs
npx supercov clean             # remove all runs and the build cache
```

If the first run does not look right, go to [Troubleshooting](troubleshooting.md)
before changing the project.

## Next

- [Agent workflow](agent-loop.md) — run a safe, repeatable coverage loop.
- [Supported suites](supported-suites.md) — check languages, runners, and limits.
- [Coverage model](coverage-model.md) — understand gaps, metrics, and measurement limits.
- [CLI reference](cli.md) — find every command and filter.
