![Supercov: Coverage for coding agents working overnight](https://raw.githubusercontent.com/supercorp-ai/supercov/main/supercov.jpg)

**Coverage for coding agents working overnight.**

**Supercov gives the agent the next test to write.** Each run returns the uncovered paths. The agent adds focused tests, reruns the suite, and continues while useful gaps remain.

Supercov wraps the test command you already run, records immutable local evidence, and returns bounded queries about the code paths that remain uncovered. Your agent writes the tests; Supercov tells it where.

No account, config file, import, custom reporter, or hosted service is required. Your source and coverage evidence stay on your machine. Supercov is free, open source, and MIT licensed.

Supported by [Supercorp](https://supercorp.ai). Learn more at [supercov.com](https://supercov.com).

## Installation & Usage

Run your existing test command through Supercov:

```bash
npx supercov -- npm test
```

Everything after `--` is your command, executed exactly as written. If your project uses a different command, pass that instead:

```bash
npx supercov -- npx playwright test
npx supercov -- pnpm test:e2e
npx supercov -- npm run test:unit && npx supercov -- npm run test:e2e
npx supercov -- cargo test
npx supercov -- cargo nextest run
```

Supercov requires Node.js 22 or newer. Rust projects currently use the Rust 1.95 toolchain; cargo-nextest 0.9.138 and 0.9.140 are supported. Package tools such as `npx` may contact the npm registry to download Supercov when it is not already cached; the Supercov CLI itself does not contact a Supercov service during a coverage run.

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

More languages are planned.

## Give it to your coding agent

Paste this into Claude Code, Codex, Cursor, Copilot, Gemini, or any coding agent that can run terminal commands:

```text
Use `npx supercov` to improve coverage. Only write tests. Keep going while
useful gaps remain.

Start with `npx supercov -- npm test`. Then use
`npx supercov runs latest gaps --limit 5` to choose one useful target.
Write one focused test, rerun the same full suite through Supercov, and verify
the gain with `npx supercov diff <previous-run-id> latest`.

Only edit tests. Never weaken assertions or change application code to make
coverage easier. Stop when no useful gaps remain.
```

Replace `npm test` with the repository's complete test command when needed.

## How it works

Each run returns fresh, executable evidence, so coding agents can keep iterating without loading a large HTML report into context.

1. **Run the suite.** Supercov executes the command after `--` inside an isolated workspace without modifying your source, tests, runner configuration, or ordinary build output.
2. **Measure what happened.** It records a fixed coverage denominator and immutable evidence for the run.
3. **Ask what is open.** Short, paginated CLI queries identify uncovered files, lines, branches, decisions, and value paths without loading a large HTML report into an agent's context.
4. **Write and prove one test.** Your coding agent adds a focused test, reruns the suite, and uses `diff` to verify exactly what improved.

Supercov supplies the coverage signal and evidence. It does not host, schedule, or replace your coding agent.

## Use leftover tokens on coverage

Before a reset—or overnight—turn idle agent time into coverage that stays with the repository. Every pass should close a small number of useful gaps and finish with evidence that the tests still pass and coverage improved.

## For software factories

Add Supercov as a repeatable coverage loop in an automated software factory. Every pass runs the real suite, chooses a useful uncovered path, writes one focused test, reruns, and proves the gain. Fresh, executable evidence lets agents keep iterating around the clock while failed tests and regressions stop the loop before they ship.

Your factory schedules the work; Supercov gives each agent a bounded next task and a durable record of what improved.

## Read the result

Start with the summary, then narrow to one useful target:

```bash
# Recent runs and the latest summary
npx supercov runs --limit 5
npx supercov runs latest

# The most useful open coverage obligations
npx supercov runs latest gaps --limit 10

# Details for one file or source location
npx supercov runs latest file app/checkout/session.ts
npx supercov runs latest decision app/checkout/session.ts:64
npx supercov runs latest line app/checkout/session.ts:64

# Prove what changed between two runs
npx supercov diff <previous-run-id> latest
```

Add `--json` to any query for the stable machine-readable format. Collections accept `--limit` and `--offset` and print a copyable next-page command.

## Coverage agents can act on

From lines and branches to MC/DC, every gap becomes the next test to write. Supercov measures more than a line percentage:

- lines, statements, functions, and branches;
- MC/DC independence witnesses;
- optional-chain, default-value, and logical-assignment paths;
- `try`/`catch` and zero-iteration control-flow paths; and
- per-test provenance where the runner exposes exact test boundaries.

The denominator is derived from source structure before the run, so adding or removing tests cannot silently change what 100% means. Ambiguous source scope, uninstrumented code, and missing evidence remain visible as completeness blockers instead of being rounded away.

## Works with your existing test suite

Runner support differs by attribution level. Supercov uses exact per-test attribution where an adapter is available and reports aggregate structural coverage rather than guessing for other runners.

| Runner | Attribution |
| --- | --- |
| Playwright | Exact per test, worker, retry, outcome, action, and assertion phase |
| Vitest | Exact per test, with setup execution kept separate |
| Jest | Exact per test, including concurrent and parameterized tests |
| `node:test` | Exact per test |
| AVA, Mocha, and other Node runners | Aggregate structural coverage |
| Cargo's standard libtest runner | Exact test and attempt identity |
| cargo-nextest | Exact test, attempt, retry, and binary identity |

Supercov works with Vite, Next, Turbopack, Webpack, esbuild, SWC, and projects with no build step. A single command can collect evidence from several supported runners into one run.

See [Supported suites](https://supercov.com/docs/supported-suites) for exact compatibility and attribution boundaries.

## Any coding agent

Claude Code, Codex, Cursor, Gemini CLI, GitHub Copilot—and more. Any coding agent that can run terminal commands can use Supercov's concise text output or optional machine-readable format.

## Local and zero-edit

Every run is stored locally under `.supercov/runs/<run-id>/`. Supercov also maintains a marker-protected isolated build cache under `supercov/workspace/`.

It does not rewrite your source files, tests, imports, reporter list, runner configuration, dependency tree, or ordinary build output. An existing user-created `supercov/` directory is never adopted.

Storage is controlled explicitly:

```bash
npx supercov clean --dry-run   # preview a full cleanup
npx supercov clean --keep 20   # retain the 20 newest runs
npx supercov clean             # remove all runs and the build cache
```

## Documentation

- [Getting started](https://supercov.com/docs/getting-started)
- [Agent loop](https://supercov.com/docs/agent-loop)
- [CLI reference](https://supercov.com/docs/cli)
- [Coverage model](https://supercov.com/docs/coverage-model)
- [Supported suites](https://supercov.com/docs/supported-suites)
- [Evidence and runs](https://supercov.com/docs/evidence)
- [Verification](https://supercov.com/docs/verification)
- [Workspace isolation](https://supercov.com/docs/workspace-isolation)
- [Performance and storage](https://supercov.com/docs/performance)

## Free and open source

[MIT licensed](LICENSE). Inspect, extend, and run it anywhere.
