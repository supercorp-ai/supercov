# Coverage overhead outside JavaScript: measurements and experiment log

Goal: a measured run takes at most 1.1x the plain test run for Rust, Python,
Go, Ruby and Java/Kotlin, with every answer unchanged.

## Method

`scripts/speed/languages.mjs` on GitHub Actions `ubuntu-24.04` (4 CPUs). For
one public project per job: one cold run of plain and of each binary, then
rounds in ABBA order; the median warm run is reported. A project's
`.supercov` is kept between runs, as an agent's loop keeps it. Each binary
measures its own clone. The first warm run of each binary writes its answers
(`scripts/speed/answers.mjs --quick`); two binaries are compared byte for
byte. Minitest suites run with a fixed seed, since their order otherwise
changes the phase ids.

Phases are Supercov's own timings: initialization, workspace preparation,
adapter setup, instrumented build, test command, evidence publication.

## Baseline (main at ce132bc2)

| Project | Plain warm s | Supercov warm s | Ratio | Where the rest goes |
| --- | --- | --- | --- | --- |
| rust-semver (38 tests) | 0.17 | 0.86 | 5.1 | test command 0.67: a process per test and doctest |
| python-h11 | 0.38 | 0.61 | 1.6 | test command 0.50 |
| go-cobra | 0.54 | 1.90 | 3.5 | workspace 0.31, test command 1.15, publication 0.18 |
| go-gin | 1.00 | 3.49 | 3.5 | workspace 0.30, test command 2.78, evidence 0.20 |
| ruby-rack (1092 tests) | 3.62 | 8.02 | 2.2 | test command 6.5 (Minitest 3.2 s to 5.7 s), evidence 0.7, publication 0.7 |

## Experiment log

| # | Idea | Effect | Kept? | Why |
| --- | --- | --- | --- | --- |
| 1 | Go: test packages write evidence to a path stable across runs | pending | | Each run's path was compiled into every test file, so Go rebuilt every package with tests and relinked every test binary |
| 2 | Ruby: a decision evaluation already recorded in this context returns at once | rack test command 6.51 s to 6.26 s (4%) | pending | |
