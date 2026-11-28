# Coverage overhead outside JavaScript: measurements and experiment log

Goal: a measured run takes at most 1.1x the plain test run for Rust, Python,
Go, Ruby and Java/Kotlin, with every answer unchanged. This records what was
measured on 2026-11-27/28, what was tried, and what is left.

## Method

`scripts/speed/languages.mjs` on GitHub Actions `ubuntu-24.04` (4 CPUs),
driven by a workflow kept on the experiment branch
(`exp/speed-languages-2`, `.github/workflows/speed-languages.yml`: one build
job per binary, one measuring job per project). For one public project per
job: one cold run of plain and of each binary, then rounds in ABBA order; the
median warm run is reported. A project's `.supercov` is kept between runs, as
an agent's loop keeps it. Each binary measures its own clone. Go runs with
`-count=1` plain as well, since Supercov adds it; Minitest suites run with a
fixed seed, since their order otherwise changes the phase ids.

Accuracy: the first two warm runs of each binary write their answers
(`scripts/speed/answers.mjs --quick`, or in full with
`SPEED_FULL_ANSWERS=1`: every file, line, decision and test).
`scripts/speed/canonical-answers.mjs` names the random attempt and phase ids by
kind and sorts every list, then compares base with base, new with new, and
base with new, with and without the raw evidence. Base against new is judged
against base against base: a suite whose timing decides what runs (gin,
jinja, a Rust test that spawns threads) differs between two runs of one
binary. A JVM parameterized test whose display name holds an object's identity
hash (`HelpFormatter@546621c4`) differs between any two JVMs.

Diagnostics (one extra run per job, outside the timed rounds), patched into
the experiment branch only: `perf record -g` of Supercov's own process with a
frame-pointer build, `SUPERCOV_VERBOSE=1` for Python's evidence phases, py-spy
of the Python processes, stackprof of the Ruby test process (loaded through
`RUBYOPT` past Bundler), and Go's instrumented copy run by plain `go`.

Phases are Supercov's own: initialization, workspace preparation, adapter
setup, instrumented build, test command, evidence publication.

## Results per language (warm medians, seconds)

### Rust

| Project | Plain | main | After | Where the rest goes |
| --- | --- | --- | --- | --- |
| dtolnay/semver 1.0.23 (38 tests) | 0.17 | 0.75 (4.4×) | same | a process per test and per doctest |
| tokio-rs/bytes 1.7.2 (168 doctests) | 3.44–5.41 | 15.9–24.5 (4.5×) | same | doctests: one rustdoc per doctest, so the crate's analysis runs 168 times |
| serde-rs/json 1.0.128 | 3.57 | every run after the first failed | fixed by #363 | a `match` arm behind a `#[cfg]` no build compiles |

### Python

| Project | Plain | main | After | Where the rest goes |
| --- | --- | --- | --- | --- |
| python-hyper/h11 0.14.0 | 0.27–0.50 | 0.44–0.77 (1.55×) | same | test command; fixed ~0.1 s |
| pallets/jinja 3.1.4 | 1.60–2.17 | 4.96–6.36 (2.9–3.1×) | same | evidence 1.7–2.1 s: join 0.6–0.7, archive 0.4–0.5, analysis 1.2–1.4 beside it; publication 0.5 |
| pallets/click 8.1.7 | 0.92 | 2.65 (2.9×) | same | evidence 0.9 s; one test fails only under Supercov (finding 4) |

### Go

| Project | Plain | main | After (#362) | Test command main → after |
| --- | --- | --- | --- | --- |
| spf13/cobra 1.8.1 | 0.46 | 1.71 (3.75×) | 1.03 (2.25×) | 1.18 → 0.48 |
| gin-gonic/gin 1.10.0 | 0.70 | 2.44 (3.5×) | 1.29 (1.84×) | 1.90 → 0.75 |

Left on gin: workspace preparation 0.23–0.25 s (the copy is refreshed and
re-instrumented every run), evidence 0.16–0.20 s, publication about 0.17 s,
probes about 12% (the instrumented copy run by plain `go test`: 0.96 s
against 0.86 s).

### Ruby

| Project | Plain | main | After (#367) | Test command main → after |
| --- | --- | --- | --- | --- |
| rack/rack 3.1.8 (1092 tests) | 3.61 | 8.14 (2.25×) | 7.16 (1.98×) | 6.62 → 5.64 |

Left on rack: evidence 0.7 s and publication 0.7 s; in the test process,
garbage collection is still a third of the samples, and the final
`Coverage.result` of every loaded file a tenth.

### Java and Kotlin

| Project | Plain | main | After | Test command main → after |
| --- | --- | --- | --- | --- |
| apache/commons-cli 1.9.0 | 6.10 | 9.01 (1.48×) | 6.94 (1.14×) with #368 | 8.37 → 6.29 |
| google/gson 2.11.0 (Java 7) | 4.97 | failed to compile | 11.96 (2.37×) with #364 | not profiled yet |

## Experiment log

| # | Idea | Effect | Kept? | Why |
| --- | --- | --- | --- | --- |
| 1 | Go: test packages write evidence to a path stable across runs | test command gin 2.27→1.07, 1.90→0.75 s; cobra 1.16→0.49, 1.64→0.71, 1.18→0.48 s; answers identical on cobra, gin differs as much between two base runs | Kept (#362) | Each run's path was compiled into every test file, so Go rebuilt every package with tests and relinked every test binary |
| 2 | Ruby: a decision evaluation already recorded in the context returns at once | rack test command 6.51→6.26 and 6.68→6.42 s alone | Kept (#367) | |
| 3 | Go: the instrumented copy run by plain `go test` | 0.96 s against 0.86 s plain on gin | Measurement | Probes cost about 12%; the rest is Supercov's phases around the command |
| 4 | Ruby: batch decision vectors with the hits, one record per context and boundary | rack test command 6.54→5.75 s with 2 and 5 | Dropped | A test whose process ends during it (`exit!`) lost its decisions: a vector must be written when it is seen |
| 5 | Ruby: an open condition evaluation as two integers, not a hash and an array | with 2, 6 and 7: rack 6.62→5.64 s, 2.25×→1.98×; full answers identical | Kept (#367) | Garbage collection was a third of the test process's samples |
| 6 | Ruby: arrival states by context | (in 5) | Kept (#367) | The table grew with every phase and was scanned whole at each phase's end |
| 7 | Ruby: a decision vector's record built directly, the same bytes as the encoder's, still written at once | (in 5) | Kept (#367) | The encoder's walk was most of a vector's cost; replaces 4 |
| 8 | JVM: carry each module's build output to the next run; date sources rewritten to the same bytes before it; stable evidence directory | commons-cli 1.48×→1.14× (test command 8.37→6.29 s); answers identical | Kept (#368) | Every run compiled every module again |
| 9 | JVM: the runtime class compiles for Java 7 | gson measurable at all | Kept (#364) | Two lambdas, `Map.merge`, and generic calls Java 7 does not infer in a conditional |

## Bugs found while measuring

1. Ruby: a suite using SimpleCov lost every run (`Coverage.result` raised after SimpleCov stopped Coverage). #354.
2. JVM: the runtime class did not compile for Java 7; gson failed. #364.
3. Rust: every run after the first failed on serde_json ("Rust runtime emitted an unknown obligation"): a match arm behind a `#[cfg]` no build compiled. #363.
4. Python: click's `test_light_imports` fails only under Supercov: a child interpreter lists `supercov_probes` among its imported modules. Not fixed.

## What is left, by measured size

- **Rust doctests before edition 2024** are the largest Rust cost (bytes 4.5×). A design that keeps every name exact: run a batch of doctests in one rustdoc with `--test-threads=1`, read rustdoc's output as it is written, and have the runtool ask the run which test it is. libtest prints and flushes `test NAME ...` before the test starts, and the test's thread waits for the runtool, so once the run has read everything already in the pipe, the last name started is the runtool's. Several batches side by side keep the cores busy. Open question: selecting a batch, since rustdoc splits its harness arguments on whitespace and names hold spaces.
- **Python evidence** (jinja): `create_coverage_view_with_model` is 23% of Supercov's own CPU (the shared analysis), `python_evidence::snapshot` 8% (two hash lookups and a `BTreeSet<&str>` insert per hit per phase; validating each slot id once and ordering slots by a precomputed rank removes both), the archive's deflate 7%, string hashing 5%. The analysis, the archive and publication are shared with every language.
- **Go and Ruby fixed costs**: a Go copy is refreshed and re-instrumented every run (0.23–0.31 s); Go and Ruby evidence and publication are 0.35–1.4 s whatever the suite's size.
- **gson** at 2.37× with the build carried: not profiled yet.
- **Sub-second suites** cannot reach 1.1×: every run pays its own copy, analysis and publication, 0.3–0.5 s at least.
