# CLI reference

Supercov runs locally. No command uploads source or coverage evidence.

```sh
npx supercov --help
```

## Measure a test command

```sh
npx supercov -- <test command>
```

Everything after `--` is the command Supercov measures.

```sh
npx supercov -- npm test
npx supercov -- npx playwright test --project=chromium
npx supercov -- cargo test
npx supercov -- cargo nextest run
```

A coverage run exits with the test command's own status, so the wrapped command
can remain a CI gate.

## List runs

```sh
npx supercov runs [--limit N] [--json]
```

Runs are listed newest first. Use an immutable run id when work spans a session;
use `latest` for interactive work.

## Query one run

```sh
npx supercov runs <run-id> [query] [options]
```

| Query | What it answers |
| --- | --- |
| no query | Overall completeness and measurement limits |
| `kinds` | Coverage by semantic level, such as unit or E2E |
| `runners` | Coverage by test runner |
| `scope` | Included, excluded, and ambiguous source files |
| `files` | All included files, ranked |
| `gaps` | Files with useful open obligations or measurement limits |
| `file <path>` | Open obligations in one file |
| `decision <id \| path:line>` | Observed decision vectors and missing witnesses |
| `line <path:line>` | Line state, nested obligations, and covering tests |
| `test <id \| name>` | What one test contributes |
| `minimize` | The smallest test subset that preserves selected coverage |

Common examples:

```sh
npx supercov runs latest
npx supercov runs latest gaps --limit 10
npx supercov runs latest file app/routes/checkout.ts
npx supercov runs latest decision app/routes/checkout.ts:42
npx supercov runs latest line app/routes/checkout.ts:57
npx supercov runs latest test "checkout retry"
```

## Query options

| Option | Meaning |
| --- | --- |
| `--filter all \| passed \| failed` | Choose which test attempts contribute. `all` is the default. |
| `--kind <kind>` | Restrict to a test level such as `unit`, `integration`, or `e2e`. |
| `--runner <runner>` | Restrict a summary to one runner. |
| `--limit N`, `--offset N` | Page through collection results. |
| `--metric all \| lines \| statements \| functions \| branches \| mcdc` | Choose the obligations preserved by `minimize`. |
| `--target 0..100` | Stop `minimize` when the selected metric reaches the target. |
| `--json` | Return the stable machine-readable form when an integration needs it. |

Collections print a copyable next-page command. Ordinary text output is intended
to work well for both people and coding agents.

## Compare runs

```sh
npx supercov diff <older-run> <newer-run> [--limit N] [--json]
```

`diff` shows both gains and losses. Neither input run is changed.

## Merge shards

```sh
npx supercov merge <run-id> <run-id> [...]
```

`merge` creates a new run from compatible shards. If source, configuration,
toolchain, schema, or denominator fingerprints differ, it fails clearly rather
than producing an invalid aggregate.

## Clean local data

```sh
npx supercov clean --dry-run
npx supercov clean --keep 20
npx supercov clean
```

By default, `clean` removes all stored runs and the isolated build cache.
`--keep N` preserves the newest N runs. Cleanup only removes marker-owned
Supercov storage.

## Read bundled documentation

```sh
npx supercov docs
npx supercov docs getting-started
```

## Environment variables

| Variable | Use |
| --- | --- |
| `SUPERCOV_SOURCE_ROOTS` | Declare the authoritative first-party source roots when automatic scope is ambiguous. |
| `SUPERCOV_TEST_KIND` | Declare the semantic level of the tests in the wrapped command. |

## Exit codes

| Code | Meaning |
| --- | --- |
| `0` | The run or query succeeded. |
| Test command's status | A coverage run preserves the wrapped command's exit status. |
| `2` | Supercov itself could not complete the request. |
