# Coverage overhead outside JavaScript: measurements and experiment log

Goal: a measured run takes at most 1.1x the plain test run for Rust, Python,
Go, Ruby and Java/Kotlin, with every answer unchanged.

## Method

`scripts/speed/languages.mjs` on GitHub Actions `ubuntu-24.04` (4 CPUs). For
one public project per job: one cold run of plain and of each binary, then
rounds in ABBA order; the median warm run is reported. A project's
`.supercov` is kept between runs, as an agent's loop keeps it. Each binary
measures its own clone.

Accuracy: the first two warm runs of each binary write their answers
(`scripts/speed/answers.mjs --quick`). `scripts/speed/canonical-answers.mjs`
names the random attempt and phase ids by kind and sorts every list, then
compares base with base, new with new, and base with new. Base against new is
judged against base against base: a suite whose timing decides what runs
(gin, jinja) differs between two runs of one binary. Minitest suites run
with a fixed seed.

Diagnostics (one extra run per job, outside the timed rounds): `perf` of
Supercov's own process with call graphs, py-spy of the Python processes,
stackprof of the Ruby test process, Go's instrumented copy run by plain `go`.

Phases are Supercov's own timings: initialization, workspace preparation,
adapter setup, instrumented build, test command, evidence publication.

## Baseline (main at ce132bc2 and 1d5505c1), warm medians

| Project | Plain s | Supercov s | Ratio | Where the rest goes |
| --- | --- | --- | --- | --- |
| rust-semver (38 tests) | 0.17 | 0.75 | 4.4 | test command 0.57: a process per test and per doctest |
| rust-serde-json (238 tests, 101 doctests) | 3.57 | cold 32.8 | n/a | warm runs failed (bug 3); cold: doctests 16.8 s against 3.4 s plain, one rustdoc per doctest |
| python-h11 | 0.27–0.50 | 0.44–0.77 | 1.5–1.6 | test command |
| python-jinja | 1.60–2.17 | 4.96–6.36 | 2.9–3.1 | test command +1.1–1.5 s, evidence 1.7–2.1 s (Supercov's own CPU is 53% of the run's) |
| python-click | 0.92 | 2.65 | 2.9 | evidence 0.87 s; one test fails only under Supercov (finding 4) |
| go-cobra | 0.46–0.68 | 1.69–2.31 | 3.4–3.6 | test command 1.16–1.64 s: rebuilds (experiment 1) |
| go-gin | 0.70–1.00 | 2.44–3.49 | 3.3–3.5 | test command 1.9–2.8 s: rebuilds (experiment 1) |
| ruby-rack (1092 tests) | 3.62 | 8.02–8.26 | 2.2 | Minitest 3.2 s to 5.7 s, of which tests' own time 3.1 to 3.9 s: the rest per test, outside test bodies; evidence 0.7 s, publication 0.7 s |
| jvm-commons-cli | 5.84 | 8.35 | 1.43 | Maven compiles the whole instrumented copy every run (target/ is not carried over, every source is rewritten) |
| jvm-gson | 5.06 | failed | n/a | the runtime class did not compile for Java 7 (bug 2) |

## Experiment log

| # | Idea | Effect | Kept? | Why |
| --- | --- | --- | --- | --- |
| 1 | Go: test packages write evidence to a path stable across runs | test command gin 2.27→1.07 s and 1.90→0.75 s, cobra 1.16→0.49 s and 1.64→0.71 s; answers identical on cobra; gin differs as much between two base runs | Kept (PR #362) | Each run's path was compiled into every test file, so Go rebuilt every package with tests and relinked every test binary |
| 2 | Ruby: a decision evaluation already recorded in the context returns at once | rack test command 6.51→6.26 s and 6.68→6.42 s (4%); answers identical | Kept, small | The rest of rack's cost is per test, not per probe |
| 3 | Go: instrumented copy run by plain `go test` | 0.96 s against 0.86 s plain on gin | Measurement | Probes cost about 12%; the other ~0.8 s is Supercov's phases around the command |

## Bugs found while measuring

1. Ruby: a suite using SimpleCov lost every run (`Coverage.result` raised after SimpleCov stopped Coverage). PR #354.
2. JVM: the runtime class used lambdas; gson (`<release>7`) failed to compile in the copy. Fix on `fix/jvm-runtime-java-7`.
3. Rust: every run after the first failed on serde_json ("Rust runtime emitted an unknown obligation"): a match arm behind a `#[cfg]` no build compiled. PR #363.
4. Python: click's `test_light_imports` fails only under Supercov: a child interpreter shows `supercov_probes` among its imported modules. Not fixed.

## Next candidates, by measured size

- Python evidence (1.7–2.1 s on jinja, more than the test command adds): call graph pending.
- Rust doctests before edition 2024 are the largest Rust cost. tokio-rs/bytes v1.7.2: plain 5.4 s, Supercov 24.5 s (4.5x), almost all of it the 168 doctests, each in a rustdoc invocation of its own, so the crate's analysis runs 168 times. A design that keeps every name exact: run a batch of doctests in one rustdoc with `--test-threads=1`, read rustdoc's output as it is written, and have the runtool ask the run which test it is. libtest prints and flushes `test NAME ...` before the test starts and the test's thread waits for the runtool, so once the run has read everything already in the pipe, the last name started is the runtool's. Several batches run side by side to keep the cores busy. Not tried yet: it needs a way to select a batch, since rustdoc splits its harness arguments on whitespace and names hold spaces.
- JVM: carry the copy's build output between runs, and keep the time of a source the instrumentation rewrote to the same bytes, so Maven and Gradle compile only what changed.
- Ruby: per-test cost outside test bodies (four phase switches and three outcome records per test).
- Go: workspace preparation re-copies and re-instruments every run (0.2–0.3 s); evidence and publication about 0.2 s each.
