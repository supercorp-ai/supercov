# Getting started

Supercov measures coverage completeness for a JavaScript or TypeScript test
suite without changing the suite. There is no config file to add, no import to
insert, and no reporter to register: you prefix the command you already run.

```sh
npx supercov -- npm test
```

Everything after `--` is your command, executed exactly as written.

## Requirements

| Requirement | Detail |
| --- | --- |
| Node.js | 22 or newer |
| Project | JavaScript or TypeScript, with a runnable test command |
| Disk | A `.supercov/` directory in the project root, which Supercov creates |

Nothing else is required. Supercov never contacts a network service, and no
part of your source or evidence leaves the machine.

## Your first run

From the project root:

```sh
npx supercov -- npm test
```

The run prints its phases as it goes — initialization, workspace preparation,
adapter setup, the instrumented build, your unchanged test command, and
evidence publication — and finishes by publishing one immutable run under
`.supercov/runs/<run-id>/`. The run id is a UTC timestamp, so run ids sort
chronologically.

If the command you normally use is not `npm test`, use that instead:

```sh
npx supercov -- npx playwright test
npx supercov -- pnpm test:e2e
npx supercov -- npm run test:unit && npx supercov -- npm run test:e2e
```

A single Supercov run can collect several runners. Coverage from a command that
launches Vitest and Playwright ends up in one run, with each test labelled by
the runner that executed it.

## Read the result

Start with the summary, then narrow. Every query names one run; `latest`
selects the newest local run.

```sh
# What runs exist?
npx supercov runs --limit 5

# How complete is the newest one?
npx supercov runs latest

# Which files hold the most open obligations?
npx supercov runs latest gaps --limit 10

# What exactly is open in one file?
npx supercov runs latest file app/checkout/session.ts
```

Output is written for an agent reading a terminal: short, paginated, and
carrying a copyable next-page command. Add `--json` to any query for the stable
machine format.

## Add a test and prove it landed

Write a test the normal way, then re-run and compare:

```sh
npx supercov -- npm test
npx supercov diff <previous-run-id> latest
```

`diff` reports what the newer run covers that the older one did not. To check
one specific test's contribution rather than the whole run:

```sh
npx supercov runs latest test "rejects a locked order"
```

## What Supercov writes

Supercov owns two marker-protected locations inside your project:

```text
.supercov/
  runs/<run-id>/evidence.raw.gz   exact denominator manifest + raw evidence
  runs/<run-id>/run.json          fingerprints, phase timings, integrity
supercov/
  workspace/<project>/            isolated build namespace, reused between runs
```

Your source files, test files, runner configuration and ordinary build output
are never modified, overwritten or rebuilt. Both owned locations carry their
own gitignore; an existing user `supercov/` directory is never adopted.

Storage is bounded by you, not by a background process:

```sh
npx supercov clean                      # remove every stored run and build cache
npx supercov clean --keep 20            # retain the 20 newest runs
npx supercov clean --keep 20 --dry-run  # show what would be removed
```

## Choosing what counts

Two options change the meaning of a number rather than its presentation, so
they are worth knowing early.

`--filter` selects which attempts contribute:

- `all` (default) counts every executed attempt, including attempts that later
  failed. This matches what conventional coverage tools report.
- `passed` counts only successful attempts of tests that ultimately passed —
  verified coverage.
- `failed` counts only failed attempts, which is useful when diagnosing a flaky
  test's real execution path.

`--kind` selects a semantic test level such as `e2e`, `integration`,
`component` or `unit`. Kind is resolved from an explicit `SUPERCOV_TEST_KIND`,
then the Playwright project name, then the test path, then the runner default.
Queries record how the label was established, so an inferred kind is never
presented as one you declared.

Filtered queries recompute every obligation from the selected tests instead of
filtering an already-computed percentage. This matters most for MC/DC, where a
witness pair assembled from one unit vector and one end-to-end vector counts for
the combined suite but not for either level alone.

## Where to go next

- [Agent loop](/docs/agent-loop) — the unattended workflow this is designed for.
- [CLI reference](/docs/cli) — every command and flag.
- [Coverage model](/docs/coverage-model) — what an obligation is, and why the
  denominator is larger than lines and branches.
- [Supported suites](/docs/supported-suites) — where attribution is exact and
  where it is aggregate.
