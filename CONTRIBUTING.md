# Contributing

## Before you push

```sh
npm run pre-push                  # lint and the affected tests, as CI's fast tier picks them
npm run pre-push -- --coverage    # and the changed lines held to 100% coverage, as the slow tier holds them
```

`scripts/pre-push.sh` checks what is committed, against the merge base with
`origin/main`:

1. `npm run check:lint`: the fault-site check, `cargo fmt --check` and clippy
   with warnings denied.
2. The tests CI's plan picks for the change: Supercov's own `tests affected`
   over the map main's Self Coverage keeps, then `scripts/ci-affected.mjs`,
   then `cargo nextest run` of them. The map is downloaded once per map into
   `target/pre-push` (about 1.5 GB, with `gh`); without `gh` or a map, the
   quick check's selection (`scripts/quick-check.mjs`) stands in, and the
   script says so. It runs the workspace's tests alone: the plan's table says
   which of CI's other jobs (engine integrations, wasm, workers) also run.
3. With `--coverage`, the slow tier's changed-line gate: the tests Self
   Coverage would measure for the change, measured by this commit's Supercov,
   and every line the change adds or changes held to 100% lines, branches and
   MC/DC. It builds the instrumented workspace, so it takes longer. macOS
   measures a handful of platform lines Linux does not, and the reverse.

It works on macOS and Linux; it needs `cargo-nextest`
(`cargo install cargo-nextest --locked`).

## What CI runs: two tiers

| | Fast tier | Slow tier |
| --- | --- | --- |
| Workflow | `ci.yml` | `slow-tier.yml`, calling `self-coverage.yml`, `compatibility.yml`, `react-compatibility.yml`, `next-compatibility.yml` and `release-gates.yml` |
| What | build, `cargo fmt`, clippy, the tests the change can affect, engine integrations, wasm and worker gates | Self Coverage with its 100% gate on the changed lines, Compatibility, React, Next.js, the release gates (release build, packed install, benchmark) |
| When | every push to every pull request, drafts included; the merge queue's commit; each night on main | the merge queue's commit; every push to a release pull request; `full-ci`; each night on main |
| How long | about ten minutes | about twenty to thirty minutes |
| Platforms | Linux; every platform on a release pull request, at night and with `full-ci` | Linux; every platform on a release pull request, at night and with `full-ci` |
| Answer | the `Fast tier` commit status | the `Slow tier` commit status |

Both statuses are required before a merge, on the merge queue's commit. A
status, not a job's check: a run that decided to run nothing (a label's)
leaves its jobs skipped, and a skipped check counts as passed.

### Running the slow tier

The slow tier runs in the merge queue, once, on the commit that merges (see
below). On a pull request it says so: its `Slow tier` status passes as "Runs
in the merge queue, on the commit it merges", and nothing runs. There is no
label to add.

It runs on the pull request as well, in full on every platform:

- on every push to a release pull request (titled `Release <v>`, or on a
  `release/` branch);
- when `full-ci` is added: `gh pr edit <number> --add-label full-ci`.

By hand, in full, on any branch: `gh workflow run slow-tier.yml --ref
<branch>`.

### Merging: the merge queue

A pull request whose `Fast tier` passed goes into the merge queue:
`gh pr merge <number> --match-head-commit <sha>` (the queue merges with a
merge commit; there is no method to choose). The queue makes a commit of main
as it is now with the pull request merged in (and the pull requests queued
before it), runs both tiers on it as a `merge_group` event, and merges that
commit once its `Fast tier` and `Slow tier` pass. The tiers run there on what
the group changes since main, or in full for a release pull request or
`full-ci` (scripts/ci-tiers.mjs, scripts/ci-affected.mjs). If they fail, the
pull request leaves the queue and main is untouched: fix it, push, and queue
it again.

There is no need to keep a pull request up to date with main by hand: the
queue tests it on the newest main, so a merge never waits on a rebase and a
second run of both tiers.

On a pull request, Self Coverage measures the affected tests and the tests
written in the files the change touches, and holds the lines the change adds or
changes to 100%. `gh workflow run self-coverage.yml --ref <branch> -f
tests="<names>"` measures named tests alone, to see what they cover.

### Waiting for CI

`scripts/wait-ci.sh <pull request number or URL | run id or URL>` waits for a
pull request's checks, or for one run. A number means the repository of the
current directory, and the first line names the repository it waits on. On a
pull request, the required checks (`Fast tier` and `Slow tier`) decide. It
sends one GraphQL query every three minutes (one per hundred checks) and
prints a line only when something finished, failed or stalled (nothing
finished for ten minutes). It exits 0 when they passed and 1 when anything
failed. Use it instead of `gh run
watch` or a `gh run view` loop: those use the REST API, which allows 5,000
requests an hour shared by everyone using the token, and several agents
polling that way used it up in minutes.

### Branch protection

The required checks are the two statuses, from GitHub Actions:

```sh
gh api -X POST repos/supercorp-ai/supercov/rulesets --input - <<'JSON'
{
  "name": "Both tiers before a merge",
  "target": "branch",
  "enforcement": "active",
  "conditions": { "ref_name": { "include": ["~DEFAULT_BRANCH"], "exclude": [] } },
  "rules": [
    {
      "type": "required_status_checks",
      "parameters": {
        "strict_required_status_checks_policy": false,
        "required_status_checks": [
          { "context": "Fast tier", "integration_id": 15368 },
          { "context": "Slow tier", "integration_id": 15368 }
        ]
      }
    },
    {
      "type": "merge_queue",
      "parameters": {
        "merge_method": "MERGE",
        "grouping_strategy": "ALLGREEN",
        "max_entries_to_build": 3,
        "min_entries_to_merge": 1,
        "max_entries_to_merge": 5,
        "min_entries_to_merge_wait_minutes": 0,
        "check_response_timeout_minutes": 90
      }
    }
  ]
}
JSON
```

The merge queue merges the commit both tiers passed on, main as it was then
with the pull request merged in: a release publishes that tree without running
anything again (RELEASING.md). So the branch need not be up to date with main
(`strict` is off): the queue's commit always is. `max_entries_to_build` is how
many queue commits run at once, each holding the pull requests before it;
`check_response_timeout_minutes` covers the slow tier's longest run.
