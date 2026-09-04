![Supercov: Coverage for coding agents working overnight](https://raw.githubusercontent.com/supercorp-ai/supercov/main/supercov.jpg)

**Coverage for coding agents working overnight.**

**Supercov gives your coding agent the next useful test to write.** It runs the test command you already use, records local coverage evidence, and turns uncovered paths into small, actionable queries. Your agent writes a focused test, reruns the suite, proves what improved, and keeps going while useful gaps remain.

No account, config file, import, custom reporter, or hosted service is required. Supercov is local, free, open source, and MIT licensed.

[Website](https://supercov.com) · [Documentation](https://supercov.com/docs) · [npm](https://www.npmjs.com/package/supercov) · [GitHub](https://github.com/supercorp-ai/supercov)

Supported by [Supercorp](https://supercorp.ai).

## Start with the suite you already have

```bash
npx supercov -- npm test
```

Everything after `--` is your test command. Supercov runs it without changing your source, tests, runner configuration, or normal build output.

Then ask what is still uncovered:

```bash
npx supercov runs latest gaps --limit 10
```

After your agent adds a test, rerun the complete suite and prove the gain:

```bash
npx supercov -- npm test
npx supercov diff <previous-run-id> latest
```

Use whichever complete test command the repository already trusts:

```bash
npx supercov -- npx playwright test
npx supercov -- pnpm test:e2e
npx supercov -- cargo test
npx supercov -- cargo nextest run
npx supercov -- pytest
npx supercov -- python -m unittest
npx supercov -- bundle exec rspec
```

## Give Supercov a job

Paste one of these prompts into Claude Code, Codex, Cursor, Gemini CLI, GitHub Copilot, or any coding agent that can run terminal commands.

### Write the first useful test

```text
Measure code coverage with `npx supercov`. Use the coverage evidence to choose
one useful missing test. Only edit tests. Run the repository's complete test
suite through Supercov again and report what improved.
```

### Use leftover tokens on coverage

```text
Measure code coverage with `npx supercov` and write tests til 100%. Only edit
tests. Keep going while useful gaps remain.

Run the repository's complete test suite through Supercov. Use
`npx supercov runs latest gaps --limit 5` to choose one useful target at a
time. Write a focused test, rerun the same complete suite, and use
`npx supercov diff <previous-run-id> latest` to verify the gain.

Never weaken assertions or change application code to make coverage easier.
Stop if the suite fails, the evidence is incomplete, or no useful gaps remain.
```

## The agent loop

1. **Run the real suite.** Supercov executes the command after `--` in an isolated workspace.
2. **Find one useful gap.** Short, paginated queries show uncovered files, lines, branches, decisions, and value paths without loading a large HTML report into context.
3. **Write one focused test.** The coding agent changes tests—not application code or coverage configuration.
4. **Rerun and prove the gain.** `diff` shows exactly what the new test covered.
5. **Repeat while useful gaps remain.** Failed tests, incomplete evidence, or ambiguous scope stay visible instead of being rounded away.

Supercov supplies the coverage signal and evidence. It does not host, schedule, or replace your coding agent.

## Use leftover tokens on coverage

Before a reset—or overnight—turn idle agent time into coverage that stays with the repository. Each pass closes a small number of useful gaps and finishes with evidence that the tests still pass and coverage improved.

## Use it in a software factory

Add Supercov as a repeatable quality loop in an automated software factory. Your factory schedules the work; Supercov gives each agent a bounded next task and an immutable record of the result.

Every pass runs the real suite, chooses an uncovered path, writes a focused test, reruns, and proves the gain. Fresh executable evidence lets agents keep iterating around the clock while failed tests and regressions stop the loop before they ship.

## Coverage agents can act on

From lines and branches to MC/DC, every gap becomes a concrete test target. Supercov measures:

- lines, statements, functions, and branches;
- MC/DC independence witnesses;
- optional-chain, default-value, and logical-assignment paths;
- `try`/`catch` and zero-iteration control-flow paths; and
- per-test provenance where the runner exposes exact test boundaries.

The denominator comes from source structure before the run, so adding or removing tests cannot silently change what 100% means. Ambiguous source scope, uninstrumented code, and missing evidence remain visible as completeness blockers.

## Supported languages

| Language | Status | Start with |
| --- | --- | --- |
| JavaScript | Available | `npx supercov -- npm test` |
| TypeScript | Available | `npx supercov -- npm test` |
| Rust | Available | `npx supercov -- cargo test` |
| Python | Available | `npx supercov -- pytest` |
| Ruby | Available | `npx supercov -- rspec` |
| Zig | Coming soon | — |
| PHP | Coming soon | — |
| C | Coming soon | — |

Supercov requires Node.js 22 or newer. Rust support currently uses Rust 1.95; cargo-nextest 0.9.138 and 0.9.140 are supported. Python support requires CPython 3.12 or newer and measures pytest and unittest runs. Ruby support requires Ruby 3.3 or newer (3.4 or newer for full measurement) and measures RSpec, Minitest, test-unit and Cucumber runs.

## Supported test suites

Supercov uses exact per-test attribution where an adapter is available. For other supported runners, it reports aggregate structural coverage instead of guessing which test covered a path.

| Runner | Coverage attribution |
| --- | --- |
| Playwright | Exact per test, worker, retry, outcome, action, and assertion phase |
| Vitest | Exact per test, with setup execution kept separate |
| Jest | Exact per test, including concurrent and parameterized tests |
| `node:test` | Exact per test |
| AVA and Mocha | Aggregate structural coverage |
| Cargo's standard libtest runner | Exact test and attempt identity |
| cargo-nextest | Exact test, attempt, retry, and binary identity |
| RSpec | Exact example and before/example/after phase identity |
| Minitest and test-unit | Exact test and setup/test/teardown identity |
| Cucumber | Exact scenario and hook-phase identity |

Supercov works with Vite, Next, Turbopack, Webpack, esbuild, SWC, and projects with no build step. One command can collect evidence from several supported runners into a single run.

See [Supported languages and test suites](https://supercov.com/docs/supported-suites) for exact compatibility and attribution boundaries.

## Read the result

```bash
# Recent runs and the latest summary
npx supercov runs --limit 5
npx supercov runs latest

# The most useful open coverage obligations
npx supercov runs latest gaps --limit 10

# Details for a file or source location
npx supercov runs latest file app/checkout/session.ts
npx supercov runs latest decision app/checkout/session.ts:64
npx supercov runs latest line app/checkout/session.ts:64

# What changed between two runs
npx supercov diff <previous-run-id> latest
```

Collections accept `--limit` and `--offset` and print a copyable next-page command. Machine-readable output is available with `--json` when an integration needs it.

## Local, private, and zero-edit

Everything Supercov writes lives under one hidden `.supercov/` directory: run evidence in `.supercov/runs/<run-id>/` and the isolated build cache in `.supercov/workspaces/`. It ignores itself in Git, so there is nothing to add to your `.gitignore`.

The Supercov CLI does not contact a Supercov service during a coverage run. Package tools such as `npx` may contact the npm registry to download Supercov when it is not already cached.

Supercov does not rewrite your source files, tests, imports, reporter list, runner configuration, dependency tree, or normal build output. An existing user-created `supercov/` directory is never adopted.

```bash
npx supercov clean --dry-run   # preview cleanup
npx supercov clean --keep 20   # keep the 20 newest runs
npx supercov clean             # remove all runs and the build cache
```

## Documentation

- [Getting started](https://supercov.com/docs/getting-started)
- [Agent workflow](https://supercov.com/docs/agent-loop)
- [Troubleshooting](https://supercov.com/docs/troubleshooting)
- [CLI reference](https://supercov.com/docs/cli)
- [Supported languages and test suites](https://supercov.com/docs/supported-suites)
- [Understanding coverage](https://supercov.com/docs/coverage-model)
- [Runs and evidence](https://supercov.com/docs/evidence)
- [Files, privacy, and cleanup](https://supercov.com/docs/workspace-isolation)
- [Trusting a result](https://supercov.com/docs/verification)
- [Speed and storage](https://supercov.com/docs/performance)

## Free and open source

[MIT licensed](LICENSE). Inspect, extend, and run it anywhere.
