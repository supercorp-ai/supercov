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
| When | every push to every pull request, drafts included; each night on main | a pull request ready to merge; every push to a release pull request; each night on main |
| How long | about ten minutes | about twenty to thirty minutes |
| Platforms | Linux; every platform on a release pull request, at night and with `full-ci` | Linux; every platform on a release pull request, at night and with `full-ci` |
| Answer | the `Fast tier` commit status | the `Slow tier` commit status |

Both statuses are required before a merge. A status, not a job's check: a run
that decided to run nothing (a label's) leaves its jobs skipped, and a skipped
check counts as passed.

### Running the slow tier

The slow tier runs on a pull request that is not a draft when you:

- add the `ready` label: `gh pr edit <number> --add-label ready`;
- mark a draft ready for review: `gh pr ready <number>`;
- add `full-ci`, which also runs every platform;
- open it with the label: `gh pr create --label ready ...`.

A release pull request (titled `Release <v>`, or on a `release/` branch) runs
it on every push, in full. By hand, in full, on any branch:
`gh workflow run slow-tier.yml --ref <branch>`.

A push after the slow tier ran takes the `ready` label off and marks `Slow
tier` pending on the new commit: it has not passed. Add the label again to run
it there. Push the fixes a review asks for first, then add `ready` once.

On a pull request, Self Coverage measures the affected tests and the tests
written in the files the change touches, and holds the lines the change adds or
changes to 100%. `gh workflow run self-coverage.yml --ref <branch> -f
tests="<names>"` measures named tests alone, to see what they cover.

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
        "strict_required_status_checks_policy": true,
        "required_status_checks": [
          { "context": "Fast tier", "integration_id": 15368 },
          { "context": "Slow tier", "integration_id": 15368 }
        ]
      }
    }
  ]
}
JSON
```

`strict` asks for the branch to be up to date with main: a squash merge then
has the tree that passed, and a release publishes that tree without running
anything again (RELEASING.md).
