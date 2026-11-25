# Releasing

## Before pushing

Run the quick check before pushing anything that touches several crates, and
before a release commit:

```sh
gh workflow run quick-check.yml --ref <branch>
gh run watch $(gh run list --workflow quick-check.yml --branch <branch> --limit 1 --json databaseId --jq '.[0].databaseId')
```

It runs `cargo fmt`, clippy and the tests a change most likely affects on Linux
x64, macOS arm64 and Windows x64, in about ten minutes, and prints why each test
was picked (`node scripts/quick-check.mjs` prints the same selection locally).
It is a pre-check: CI and Compatibility still decide.

## What a pull request runs

A pull request runs on Linux alone: the tests its change can affect, by
Supercov's own `tests affected` over the Self Coverage map of main
(`scripts/ci-affected.mjs`; each workflow's step summary says why each job ran
in full, in part, or not at all). macOS, Windows and ARM, every Node and the
whole Compatibility matrix run on a release pull request (titled
`Release <v>`, or a `release/` branch), on every push to main, by hand, and on
a pull request labelled `full-ci`. A pull request that changes only the
compatibility corpus (`tests/compat`) runs the `compat` tests alone.

## A release

1. A release pull request carries the `Release <v>` commit. Release native
   builds (`release-native.yml`) start on it, beside CI and Compatibility.
2. Merge and publish once release native builds have passed and CI shows no
   code failures. Slow macOS jobs may still be running: let them finish on
   main, and fix forward if one fails.
3. A failure of a download or the network (a job that says
   `SUPERCOV_INFRA_FAILURE`, a registry's 429 or 5xx) is rerun, not treated as a
   code failure.
4. After merging, publish (`publish.yml`). If main moved past the release
   commit, pass the release pull request's native build run as `reuse-run-id`.
5. Publishing pushes the corpus branch `compat/record-<v>`. Actions cannot open
   the pull request, so open it by hand:
   `gh pr create --head compat/record-<v> --title "Record <v> in the compatibility corpus" --fill`.
