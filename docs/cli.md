# CLI reference

Every command is local. Nothing is uploaded, and no command runs your test
suite unless you ask it to.

```sh
supercov --help
```

## Creating a run

```sh
supercov -- <test command>
```

Everything after `--` is executed as written. Supercov propagates coverage
through every Node child process the command launches, then publishes one
immutable run.

```sh
npx supercov -- npm test
npx supercov -- npx playwright test --project=chromium
npx supercov -- npx vitest run app/checkout
```

A per-project lock rejects overlapping runs before either can build.

## Listing runs

```sh
supercov runs [--limit N] [--json]
```

Runs are listed newest first with their id, duration, phase timings and
integrity state. Use the id — not `latest` — when work spans a session.

## Coverage queries

All coverage queries take the form:

```sh
supercov runs <run-id> [query] [options]
```

`<run-id>` is positional because every coverage view belongs to exactly one
immutable run. `latest` selects the newest local run.

| Query | Answers |
| --- | --- |
| no query | Overall completeness for the selected view |
| `kinds` | Completeness split by semantic level (`unit`, `e2e`, …) |
| `runners` | Completeness split by executing runner |
| `scope` | Which source files are included, excluded or ambiguous |
| `files` | Every included source file, ranked |
| `gaps` | Only files with unresolved obligations or measurement limits |
| `file <path>` | Every open obligation in one file |
| `decision <id \| path:line>` | Observed vectors and missing witnesses for one decision |
| `line <path:line>` | Line state, nested obligations, covering tests, and phases |
| `test <id \| name fragment>` | What one test contributes |
| `minimize` | The smallest test subset that preserves coverage |

### Options

| Option | Applies to | Meaning |
| --- | --- | --- |
| `--filter all \| passed \| failed` | most queries | Which attempts contribute. `all` is the default and matches conventional tools. |
| `--kind <kind>` | most queries | Restrict to a semantic level, for example `--kind e2e`. |
| `--runner <runner>` | summary | Restrict to one executing runner, for example `--runner playwright`. |
| `--metric all \| lines \| statements \| functions \| branches \| mcdc` | `minimize` | Which obligations the solver must preserve. |
| `--target 0..100` | `minimize` | Stop once the metric reaches this level. |
| `--limit N`, `--offset N` | collections | Pagination. Collections default to 20 items and print a copyable next-page command. |
| `--json` | every query | The stable machine format. |

### Examples

```sh
# Orient in a few lines.
npx supercov runs latest
npx supercov runs latest --filter passed
npx supercov runs latest kinds

# Find and open one target.
npx supercov runs latest gaps --kind e2e --limit 10
npx supercov runs latest file app/routes/example.ts
npx supercov runs latest decision app/routes/example.ts:42
npx supercov runs latest line app/routes/example.ts:57

# Understand contribution and redundancy.
npx supercov runs latest test "checkout retry"
npx supercov runs latest minimize --filter passed
npx supercov runs latest minimize --filter passed --metric mcdc --target 80
```

With `--kind`, gap and file queries additionally distinguish obligations covered
only by other test levels from obligations uncovered everywhere. On a combined
unit/E2E run, the default summary also prints the line count reached by other
test kinds but not by E2E, followed by the exact `gaps --kind e2e` query.

## Comparing runs

```sh
supercov diff <older-run> <newer-run> [--limit N] [--json]
```

Reports what the newer run covers that the older one did not, and what it lost.
Both runs remain untouched.

## Combining shards

```sh
supercov merge <run-id> <run-id> [...]
```

Accepts only runs with identical source, test, dependency, configuration,
instrumenter, schema and denominator fingerprints. It rewrites the run scope
inside every evidence record, namespaces shard paths, and publishes a new
immutable run atomically. Input runs are never modified. Incompatible shards
fail clearly rather than producing a plausible but invalid aggregate; the
error names each exact fingerprint domain that differs.

## Retention

```sh
supercov clean [--keep N] [--dry-run]
```

`clean` removes all history and the isolated build workspace by default.
`--keep N` preserves the N newest runs. It never runs automatically, takes the
same lock as a coverage run, refuses to race an active run, and deletes only
exactly marker-owned Supercov storage.

## Environment variables

| Variable | Effect |
| --- | --- |
| `SUPERCOV_SOURCE_ROOTS` | Declares the authoritative first-party source scope, resolving ambiguity that would otherwise block a complete verdict. |
| `SUPERCOV_TEST_KIND` | Declares the semantic level of the tests in this command, overriding every inference. |

## Exit codes

| Code | Meaning |
| --- | --- |
| `0` | The run or query succeeded. |
| The test command's own code | A coverage run exits with the status of your command, so `supercov -- npm test` remains usable as a CI gate. |
| `2` | Supercov itself failed: an unknown command, an unreadable run, an incompatible merge, or a lock conflict. |
