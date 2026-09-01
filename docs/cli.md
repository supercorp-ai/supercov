# CLI reference

Supercov has one command for measuring a suite and a small set of commands for
reading the result. Text output is designed for people and coding agents. Add
`--json` only when an integration needs a stable machine-readable response.

```sh
npx supercov --help
```

## Quick reference

| Goal | Command |
| --- | --- |
| Measure a suite | `npx supercov -- <test command>` |
| List recent runs | `npx supercov runs` |
| Read the newest run | `npx supercov runs latest` |
| Find useful gaps | `npx supercov runs latest gaps` |
| Inspect one file | `npx supercov runs latest file <path>` |
| Compare two runs | `npx supercov diff <older> <newer>` |
| Combine shards | `npx supercov merge <id> <id> [...]` |
| Remove local data | `npx supercov clean` |
| Read bundled guides | `npx supercov docs` |

## Measure a test command

```sh
npx supercov -- <test command>
```

Everything after `--` is passed to the test command:

```sh
npx supercov -- npm test
npx supercov -- npx playwright test --project=chromium
npx supercov -- cargo test
npx supercov -- cargo nextest run
```

Use the complete command you rely on before merging or deploying. A coverage run
preserves the wrapped command's exit status, so it can remain a CI gate.

## List and select runs

```sh
npx supercov runs
npx supercov runs --limit 5
npx supercov runs latest
npx supercov runs <run-id>
```

Runs are listed newest first. `latest` is convenient during an interactive
loop. Use the immutable run id in automation, review notes, and work that spans
sessions.

## Query a run

```sh
npx supercov runs <run-id> [query] [options]
```

| Query | Use it to |
| --- | --- |
| no query | Read the overall result, test outcome, completeness, and timings |
| `gaps` | See only files with uncovered behavior or measurement limits |
| `files` | See every included file, including fully covered files |
| `file <path>` | Inspect the open obligations in one file |
| `decision <id \| path:line>` | Understand missing boolean outcomes and MC/DC witnesses |
| `line <path:line>` | See one line's state, obligations, and covering tests |
| `test <id \| name>` | See the coverage attributed to one test |
| `kinds` | Group coverage by test level, such as unit or E2E |
| `runners` | Group coverage by test runner |
| `scope` | Review included, excluded, and ambiguous source files |
| `minimize` | Find a small test subset that preserves a coverage target |

Common examples:

```sh
npx supercov runs latest gaps --limit 10
npx supercov runs latest file app/routes/checkout.ts
npx supercov runs latest decision app/routes/checkout.ts:42
npx supercov runs latest line app/routes/checkout.ts:57
npx supercov runs latest test "checkout retry"
```

Run any query with `--help` to see only the options valid for that query:

```sh
npx supercov runs latest --help
npx supercov runs latest file --help
```

## Narrow a view

| Option | Meaning |
| --- | --- |
| `--filter all \| passed \| failed` | Recalculate the view from all, successful, or failed attempts |
| `--kind <kind>` | Restrict to a test level such as `unit`, `integration`, or `e2e` |
| `--runner <runner>` | Restrict to one runner |
| `--metric all \| lines \| statements \| functions \| branches \| mcdc` | Choose a metric for `files`, `gaps`, `diff`, or `minimize` |
| `--limit N`, `--offset N` | Page through a collection |
| `--json` | Return the machine-readable form |

Collection output includes a copyable command for the next page.

For a large file, group and rank its decisions:

```sh
npx supercov runs latest file app/routes/checkout.ts \
  --group decision --sort missing
```

## Compare runs

```sh
npx supercov diff <older-run> <newer-run>
```

`diff` reports gains and losses. Use it after adding a test to prove that the
expected behavior became covered without an unexplained regression elsewhere.
Neither input run is changed.

The same filters can focus a comparison:

```sh
npx supercov diff <older-run> <newer-run> --kind e2e
```

## Find a smaller test set

```sh
npx supercov runs latest minimize
npx supercov runs latest minimize --metric branches --target 90
```

`minimize` finds a small set of tests that preserves the selected coverage
target. It does not edit, delete, or skip tests for you. Treat the result as an
analysis aid, not permission to remove tests that protect behavior outside the
selected metric.

## Combine shards

```sh
npx supercov merge <shard-a> <shard-b> <shard-c>
```

Merge creates a new run. The inputs must describe the same source,
configuration, toolchain, schema, and coverage denominator. Supercov rejects an
incompatible merge rather than publishing a misleading aggregate.

## Clean local data

```sh
npx supercov clean --dry-run
npx supercov clean --keep 20
npx supercov clean
```

By default, `clean` removes all stored runs and the isolated build cache.
`--keep N` keeps the newest N runs. Cleanup removes only marker-owned Supercov
storage.

## Read bundled documentation

```sh
npx supercov docs
npx supercov docs getting-started
npx supercov docs troubleshooting
```

The guides are installed with the package, so they remain available in a
terminal or offline environment after the package has been downloaded.

## Environment variables

| Variable | Use |
| --- | --- |
| `SUPERCOV_SOURCE_ROOTS` | Set comma-separated first-party source roots when automatic discovery is ambiguous |
| `SUPERCOV_TEST_KIND` | Label the wrapped command as a test level such as `unit` or `e2e` |

Examples:

```sh
SUPERCOV_SOURCE_ROOTS=src,app npx supercov -- npm test
SUPERCOV_TEST_KIND=e2e npx supercov -- npx playwright test
```

## Exit codes

| Code | Meaning |
| --- | --- |
| `0` | The command or query succeeded |
| Wrapped command's code | The test command failed and Supercov preserved its status |
| `2` | Supercov could not complete the request |
