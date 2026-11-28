# Releasing

## Before pushing

Run `npm run pre-push` (CONTRIBUTING.md): lint and the tests CI's fast tier
picks, on this machine. For macOS and Windows as well, run the quick check
before pushing anything that touches several crates, and before a release
commit:

```sh
gh workflow run quick-check.yml --ref <branch>
gh run watch $(gh run list --workflow quick-check.yml --branch <branch> --limit 1 --json databaseId --jq '.[0].databaseId')
```

It runs `cargo fmt`, clippy and the tests a change most likely affects on Linux
x64, macOS arm64 and Windows x64, in about ten minutes, and prints why each test
was picked (`node scripts/quick-check.mjs` prints the same selection locally).
It is a pre-check: CI and Compatibility still decide.

## What a pull request runs

Two tiers (CONTRIBUTING.md): the fast tier (`ci.yml`) on every push, the slow
tier (`slow-tier.yml`) in the merge queue, and both on the commit the queue
merges. Each runs on Linux alone: the tests its
change can affect, by Supercov's own `tests affected` over the Self Coverage
map of main
(`scripts/ci-affected.mjs`; each workflow's step summary says why each job ran
in full, in part, or not at all). macOS, Windows and ARM, every Node and the
whole Compatibility matrix run on a release pull request (titled
`Release <v>`, or a `release/` branch), each night on main, by hand, and on
a pull request labelled `full-ci`. A pull request that changes only the
compatibility corpus (`tests/compat`) runs the `compat` tests alone.

## A release

1. A release pull request carries the `Release <v>` commit. Release native
   builds (`release-native.yml`) start on it, beside both tiers, which run in
   full on every platform on every push to it.
2. Queue it (`gh pr merge <number> --match-head-commit <sha>`) once release
   native builds have passed
   and the `Fast tier` and `Slow tier` statuses both say "Passed in full".
   The merge queue runs both tiers in full again on its commit and merges
   that commit. Bring the branch up to date with main before queuing it (and
   let release native builds pass on that commit), and queue nothing else
   beside it: the queue's commit then has the tree release native builds
   built, which is the tree a release publishes.
3. A failure of a download or the network (a job that says
   `SUPERCOV_INFRA_FAILURE`, a registry's 429 or 5xx) is rerun, not treated as a
   code failure.
4. After merging, publish (`publish.yml`). It publishes nothing unless the
   tree it publishes passed both tiers in full: on that commit, or on the
   release pull request's head of the same tree, whose results it reuses --
   nothing runs again on main (`scripts/ci-tiers.mjs verify-release`). If
   main moved past the release commit, its tree is a new one: run both tiers
   on it first (`gh workflow run ci.yml --ref main` and
   `gh workflow run slow-tier.yml --ref main`), and pass the release pull
   request's native build run as `reuse-run-id`.
5. Publishing pushes the corpus branch `compat/record-<v>`. Actions cannot open
   the pull request, so open it by hand:
   `gh pr create --head compat/record-<v> --title "Record <v> in the compatibility corpus" --fill`.
