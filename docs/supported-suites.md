# Supported languages and test suites

Supercov supports JavaScript, TypeScript, Rust, and Python today. Start with
the same test command the repository already uses; Supercov detects supported
runners inside that command.

```sh
npx supercov -- npm test
npx supercov -- npx playwright test
npx supercov -- cargo test
npx supercov -- pytest
```

## Language support

| Language | Status | Start with |
| --- | --- | --- |
| JavaScript | Available | `npx supercov -- npm test` |
| TypeScript | Available | `npx supercov -- npm test` |
| Rust | Available | `npx supercov -- cargo test` |
| Python | Available | `npx supercov -- pytest` |
| Zig | Coming soon | — |
| PHP | Coming soon | — |
| C | Coming soon | — |

The npm-distributed CLI requires Node.js 22 or newer for every language.

## What exact and aggregate mean

**Exact attribution** means Supercov knows which test, attempt, retry, and
runner produced the coverage. Queries such as `test`, `passed`, and `failed`
can use that identity.

**Aggregate coverage** means Supercov knows the source executed but cannot
truthfully assign it to one test. Whole-run `gaps` and `file` queries still
work; per-test questions are limited.

Supercov reports the level it actually observed. It does not guess.

## JavaScript and TypeScript

| Runner | Attribution |
| --- | --- |
| Playwright | Exact per test, worker, retry, outcome, action, and assertion phase |
| Vitest | Exact per test, with setup execution kept separate |
| Jest | Exact per test, including concurrent and parameterized tests |
| `node:test` | Exact per test |
| AVA and Mocha | Aggregate structural coverage |
| Other Node-based runners | Aggregate when their processes remain visible to Supercov |
| Browser component runners without an adapter | Aggregate structural coverage |

One command may launch several runners. Supercov combines their evidence into
one run and preserves runner identity wherever the runner exposes it.

### Builds and source formats

JavaScript and TypeScript projects may use Vite, Next, Turbopack, Webpack,
esbuild, SWC, `tsc`, or no build step. ESM, CommonJS, JavaScript, JSX,
TypeScript, and TSX are supported.

Supercov instruments an isolated copy. It does not ask you to add an import,
reporter, plugin, or alternate build output.

### Browsers, servers, and child processes

Playwright support includes Chromium, Firefox, and WebKit, along with pages,
frames, popups, workers, request contexts, WebSockets, and test-launched child
processes where the runner exposes their identity.

Browsers a suite launches itself are covered too. A fixture that calls
`chromium.launchPersistentContext`, or `launch`/`connect` and hands out its own
contexts and pages in place of Playwright's `page` fixture, is adopted by each
test's collector: its pages are read before the fixture closes them, and a
context kept for the whole worker follows the current test's identity. Actions
on such pages are not recorded as separate phases, so their evidence is
attributed to the test and its assertions rather than to individual clicks.

Node child processes inherit coverage automatically. Long-running servers get
a short drain window after the test command finishes so buffered evidence can
arrive. Work without a reliable test identity is kept as background coverage
instead of being assigned to an arbitrary test.

## Rust

| Runner | Attribution | Current requirement |
| --- | --- | --- |
| Cargo's standard libtest runner | Exact test and attempt identity | Rust 1.95; run with `npx supercov -- cargo test` |
| cargo-nextest | Exact test, attempt, retry, and binary identity | cargo-nextest 0.9.138 or 0.9.140 |

Supercov preserves Cargo's test selection, scheduling, fail-fast behavior,
environment, and exit status. Use the repository's normal flags after the
wrapped command:

```sh
npx supercov -- cargo test --workspace
npx supercov -- cargo nextest run --workspace
```

`cross` is not supported yet. Unsupported command shapes fail with an
explanation instead of silently falling back to plausible but inaccurate
attribution.

## Python

| Runner | Attribution | Current requirement |
| --- | --- | --- |
| pytest | Exact test, worker, retry, and setup/call/teardown phase identity | CPython 3.12 or newer; run with `npx supercov -- pytest` or `python -m pytest` |
| pytest-xdist | Exact per worker | Workers inherit the run through the environment |
| pytest-rerunfailures | Exact per attempt; flaky tests are reported as such | |
| `python -m unittest` | Exact test and setUp/test/tearDown phase identity | Serial in-process; skips and expected failures are recorded; subtest failures roll up to the parent test |

Supercov measures Python through CPython's own monitoring interface. Nothing is
copied, rewritten, or compiled differently: the project runs in place with its
own interpreter and virtual environment, and Supercov only adds a start-up hook
through `PYTHONPATH`, a pytest plugin through `PYTEST_PLUGINS`, and a few
`SUPERCOV_*` variables. Child interpreters started with `subprocess` or
`multiprocessing` inherit the exact test identity; threads and thread pools
carry it through `contextvars`.

Each interpreter writes commit-framed evidence to a process-owned mmap. A hard
kill preserves completed observations and an incomplete tail is ignored; an
exhausted transport or corrupt committed frame fails the run closed.

Measured obligations are statements (including several on one line), function
entry, boolean decisions with MC/DC vectors, `for` and comprehension iteration,
`and`/`or` short-circuiting, `match` case selection, and `try` completion,
handler selection and exception propagation, all derived from CPython's own
instruction positions rather than from exception hooks.

Interpreters launched with `-I`, `-E`, or `-S` ignore `PYTHONPATH` and are not
measured. Code compiled from strings at runtime has no source obligations.

```sh
npx supercov -- pytest
npx supercov -- python -m pytest -n 4
npx supercov -- uv run pytest
npx supercov -- python -m unittest
```

## Containers, VMs, and remote execution

Supercov can collect from supported processes launched through a container, VM,
or remote executor when it can see the launch boundary, carry the instrumented
workspace into that environment, and receive evidence back.

Mounted workspaces and local child-process launchers are the most direct path.
If an executor hides how code is launched or cannot return evidence, Supercov
reports the missing boundary rather than claiming unseen code was measured.

## If your runner is not listed

For a Node-based runner, try the complete command and inspect the result:

```sh
npx supercov -- npm test
npx supercov runs latest runners
npx supercov runs latest scope
```

Aggregate coverage may already be useful even without exact per-test identity.
If a supported runner appears incomplete, see [Troubleshooting](troubleshooting.md)
and include the exact command and runner output when opening an issue.
