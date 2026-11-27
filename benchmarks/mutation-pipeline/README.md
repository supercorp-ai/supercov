# Mutation pipeline benchmark

One workflow (`.github/workflows/mutation-pipeline.yml`) that measures the
strong pass on every language's mutation corpus with one binary, so a change
to the language-independent pipeline (generation, the plan's weak decisions,
candidate selection, ordering, sharding) can be compared with its base:

- Go: go-humanize, btree, golang-lru, uuid, cast (`benchmarks/mutation-go`),
  `supercov -- go test ./...` then `runs latest mutants test`;
- Python: python-slugify, cachetools, parse, humanize
  (`benchmarks/mutation-python/run-supercov.sh`, pytest);
- JavaScript: defu, scule, cookie-es, pathe, ufo
  (`benchmarks/mutation/run-job.mjs --scenario supercov-cold`).

It runs on a push to a branch named `exp/mutation-pipeline*`, or dispatched.
The `summary` job prints, per project and language, the mutants, those
decided without running a test, those run, the test executions and the
strong pass's seconds, and uploads every mutant's verdict
(`summary/verdicts.jsonl`).

`compare.mjs <base.jsonl> <new.jsonl>` lists every mutant whose verdict
moved between two runs, and exits 1 when a decided verdict (killed,
survived, timeout, no-coverage) moved to another.

The experiment log is `docs/design/mutation-pipeline-speed.md`.
