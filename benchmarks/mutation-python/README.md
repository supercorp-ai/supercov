# Python mutation: validation against mutmut

The corpus and scripts that check Supercov's Python mutants against mutmut
3.8.1 (`docs/design/python-mutants.md`).

## Projects

| Project | Tag | Source | Suite |
| --- | --- | --- | --- |
| python-slugify | v9.2.0 | `slugify/` | pytest, unittest-style classes, CLI tests that run `python -m slugify` |
| cachetools | v8.0.2 | `src/cachetools/` | `unittest.TestCase` classes, run by pytest and by `python -m unittest` |
| parse | 1.22.4 | `parse/` | pytest with `--doctest-modules`, README doctests and pytest-cov |
| humanize | 4.17.0 | `src/humanize/` | pytest with doctests, freezegun, pytest-benchmark and translations |

`setup.sh <project> <dir>` clones a project at its tag and installs it with
its test dependencies (uv, Python 3.12). `mutmut-config.sh` adds mutmut's
configuration: the source path and what the tests read beside the source
(python-slugify's `tools/` and README, parse's README, humanize's
`scripts/`), and humanize's `forkserver` isolation (one of its tests changes
its own parameter lists, so the suite cannot run twice in one process).

## Scripts

- `run-supercov.sh <project> <dir> <pytest|unittest> <supercov> <out>`: one
  measured run and its strong pass, timed; copies the verdicts.
- `run-mutmut.sh <project> <dir> <out>`: `mutmut run` with one child per CPU,
  timed; writes mutmut's verdicts.
- `generate.py <src>`: mutmut's mutated tree without running anything.
- `kill_matrix.py <dir> --shard k/n --out FILE`: the full kill matrix: every
  test against every mutant. pytest collects once in mutmut's mutated tree;
  each mutant runs in a fork with mutmut's trampoline switched to it, every
  test in collection order, none skipped at the first failure.
- `compare.py <ids.jsonl> <kill-matrix.jsonl> <strong-mutation.json>
  [mutmut-results.json]`: joins the matrix to Supercov's verdicts through
  `scripts/mutmut-parity.py`'s ids, and lists every disagreement.
- `part.sh <parts.tsv> <part> <dir> <supercov> <out>`: one numbered part of
  a validation run, as `parts.tsv` lists it (project, scenario, shard), into
  the directory `compare-all.sh` reads; `part.sh <parts.tsv> list` prints the
  parts, for a CI matrix whose jobs carry only their numbers.
- `compare-all.sh <results> <dir> <out>`: every part's results joined into
  one summary.

The matrix forks after collection, so code a test module's import runs
(decorators applied at import) runs there without the mutant; mutmut's own
runs, and Supercov's, import the test modules with the mutant on. Each
disagreement is checked by applying the mutant to the source by hand and
running the suite as written.
