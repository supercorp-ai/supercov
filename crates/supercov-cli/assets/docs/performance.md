# Speed and storage

A Supercov run includes your test command and evidence publication, and for a
compiled language an instrumented build, which repeated passes can reuse when
the relevant inputs have not changed. A JavaScript project has no such build:
the command runs as given, and a build inside it is part of the command's time.

## See where the time went

```sh supercov
npx supercov runs latest
```

The summary separates:

| Phase | What it includes |
| --- | --- |
| Initialization | Recovery, project discovery, and input checks |
| Workspace preparation | Refreshing the isolated project copy |
| Adapter setup | Preparing the runner integration |
| Instrumented build | Building measured source for a compiled language, or almost nothing on a cache hit; 0 for JavaScript |
| Test command | The wrapped command, including browser, VM, or remote latency |
| Evidence publication | Validating and storing the completed run |

The line Supercov prints when a run ends also reports publication: analysing
the evidence once for the stored query views, the summary and the assertion
map. That is why the first query after a run opens at once rather than
analysing the evidence itself.

The first `npx` invocation may also download the package. That download happens
before Supercov starts and is not coverage-engine overhead.

## Keep an agent loop fast

- Keep the isolated build cache between passes.
- Avoid changing dependencies, build configuration, or toolchains during the
  loop unless the test requires it.
- Query the stored run instead of rerunning merely to open another view.
- Write one related test at a time, then rerun.
- Use a focused test command while iterating when appropriate, but finish with
  the same complete command used for the baseline.

Supercov reuses an instrumented build only when source, dependencies,
configuration, toolchain, build mode, and instrumenter identity match. A
possible mismatch triggers a fresh build rather than risking stale coverage. A
build inside a JavaScript project's command is the command's own and runs every
time, as it does without Supercov.

## Use focused runs carefully

A narrow test command can shorten the inner loop:

```sh supercov
npx supercov -- npx vitest run app/checkout/session.test.ts
```

That run has a narrower evidence set than the complete suite. Before reporting
success, rerun the repository's full command and compare against a full-suite
baseline.

## Measure overhead in your project

Compare the original and wrapped command under similar cache conditions:

```sh supercov
/usr/bin/time -p npm test
/usr/bin/time -p npx supercov -- npm test
```

Alternate the two commands several times and compare typical runs. Do not
compare a cold package, browser, build, or VM cache with a warm one. Supercov
never runs the test command a second time automatically because suites may write
data, call paid services, or be intentionally non-repeatable.

### Where a browser suite's time goes

A Next.js app's Playwright suite (170 tests on one worker, against a
production build the suite builds itself, on a 4-CPU runner) takes 12 to 13%
longer measured than plain when run whole. Each part was timed plain and
measured on the same machine, an eighth of the suite at a time, which adds up
to 8%; the rest is the open contexts below:

| Part | Plain | Measured | Why |
| --- | --- | --- | --- |
| Supercov's start, before the build | 0.7 s | 1.5 s | Copying the project and instrumenting its sources |
| `next build` | 52 s | 57 s (+9.5%) | The build compiles the instrumented sources: probes, and source maps that keep stack traces on the author's lines |
| Server start to the first test | 3.6 s | 5.0 s | The runtime's preload in each process, and a larger server bundle |
| Tests | 255 s | 272 s (+6.4%) | See below |
| After the tests | 0.3 s | 1.6 s | Reading the evidence (0.25 s) and publishing the run (0.35 s), and closing the collector |

Most of what the tests add is waiting, not Supercov's own code:

- **Each action tells the browser its phase first.** A click, a fill or a
  navigation waits for one round trip to every page of the test, about 6 ms
  on such a runner (822 actions: about 5 s). The page has to know the phase
  before the action's effects reach it, or the work it does would be credited
  to the step before. An assertion sends the same message without waiting.
- **Pages run instrumented code.** The browser does more work for each page
  the app renders.
- **A context the suite leaves open is the next tests' as well.** Its pages
  are told each phase and read at the end of each test, in case they still
  run code: a suite that opens contexts and never closes them pays a little
  more with each one. On the suite above, the same test cost about 120 ms more
  late in a whole run than at the start of a run of an eighth of it.

On a generated app with heavy pages and a new context per test
(`benchmarks/large-app`, sixteenths of its 300 tests), the measured run uses
about 22% more CPU than plain. Most of that is there with almost nothing
instrumented (one file measured: +12%): the collector's calls, the runtime's
preload in every Node process, and the build, each a little:

| Process, CPU s per sixteenth | Plain | Measured | One file measured |
| --- | --- | --- | --- |
| `next build` | 23.1 | 26.5 | 24.1 |
| `next start` | 5.5 | 6.6 | 6.5 |
| Playwright's runner and worker | 5.3 | 6.9 | 6.9 |
| Chrome's pages | 4.5 | 5.9 | 6.0 |
| Chrome's browser and network processes | 4.0 | 4.8 | 4.7 |

Switching parts off one at a time moves the total by about a point each:
telling pages each phase (2), the phase cookies (1.3), recording in pages
(0.9), and the runtime's prelude in each document, whether or not minified
(0.6). The source maps of instrumented files, Node's `--enable-source-maps`,
its compile cache and the async hook that keeps a probe's context cost nothing
measurable. Reading what each loaded module exports, for container SDKs, takes
about 0.1 s of CPU in such a part, and about 4.5 s over a large Vitest suite's
151 processes.

#### Why each action still waits for the page

Telling a page a new phase is one round trip that the action waits for:
about 4 to 6 ms, almost all of it the page taking up the message while it
renders, so sending it through Chrome's protocol directly is no faster. Not
waiting was considered and declined. An action that checks its element first
(a click or a fill without `force`) sends those checks on the same protocol
session as the phase, and Chrome runs a session's messages in order, so it
would be enough to wait until the phase was sent rather than answered.
Keyboard and mouse calls, navigation, `force` and the phase cookies have no
such ordering and would still wait. Knowing when Playwright has sent a message
means hooking its internals, which change between versions, and an ordering
assumption that is wrong once credits a page's work to the wrong step. That
would lose accuracy for about 3% of a browser suite's test time, so every
action still waits.

The server records every test's first hits in each phase as lines of JSON. It
writes them through a file kept open, and builds each line directly, which
takes about a third of the time it did before. The collector reads each page's
evidence in the columns it stores, which takes about a quarter of the time.

### Where a unit suite's time goes

Excalidraw's Vitest suite (7,052 test records, jsdom, on a 4-CPU runner)
retires about 25% more instructions measured than plain, and takes 190 to
205 s against 150 to 160 s. Almost none of that is the instrumented code
running: with every probe still called but recording nothing, the suite
retires 2% more than plain. It is the evidence:

- **In the test workers**, each statement's first hit in each phase of a
  test (each assertion is a phase), each decision vector and each selection
  is recorded, and the test's record is written when it ends: 779 MB of
  JSON for the suite, mostly hit ids (190 MB), decisions (138 MB), assertion
  phases (87 MB) and the events that say which phase first hit each point.
- **After the tests**, Supercov reads those records (1.4 s), names each
  probe by its point (1.9 s), builds the views (6 s) and publishes the run
  (3.8 s): about 13 s on the run's critical path, beside its compression.

Writing the records smaller does not make it faster: numbered ids and
vectors as text cut the evidence by 46% and the run retired 2.4% more
instructions, because each item costs about the same whichever way it is
written. What a unit suite pays is per item recorded, not per byte.

`scripts/speed/evidence-composition.mjs` says what a run's evidence is made
of, field by field.

### Prove a speed-up changes nothing

A change meant to make measurement faster must leave every answer as it was.
`scripts/speed/answers.mjs` writes down every answer a run gives: its summary
and every view of it, every file, line (with its tests and phases) and
decision (with MC/DC vectors and witness pairs), every test, the mutants and
their candidates, and every record of the evidence. It replaces what differs
from run to run by its nature (run, attempt and worker ids, times, process
ids, paths) with stable names. Two runs of a suite that runs the same way each
time give files equal byte for byte:

```sh
node scripts/speed/answers.mjs ./old/supercov . "$OLD_RUN" old.json
node scripts/speed/answers.mjs ./new/supercov . "$NEW_RUN" new.json
diff old.json new.json
```

A suite whose timing decides what each phase runs (a page that renders after
a fetch, a server that answers while the next step starts) differs even
between two runs of one binary. The `speed` label's benchmark therefore
compares each binary's two rounds as well as the binaries
(`--quick` for its large apps), and the counts are read against each other.

### Python subprocesses

Interpreter count matters as well as test count. Each child initializes the
Python runtime before executing user code, so a suite that launches thousands
of short-lived interpreters can have more overhead than a single-process suite
with the same number of tests. Tight child-startup deadlines can also expire
before the child produces its first output.

Supercov prepares the run's probe index once and lets later interpreters load
file and decision data as needed. The compiled-module cache is separate: it
avoids compiling unchanged measured source, while the prepared index avoids
repeatedly parsing and indexing the entire coverage plan.

Optional unittest and concurrency adapters install when their libraries are
imported, rather than importing those libraries into every helper process.
Libraries already loaded when measurement starts are patched immediately.

For development, `python3 scripts/python-startup-benchmark.py` measures cold
initialization and repeated child startup against a synthetic plan. Use
`--runtime /path/to/checkout/runtime/python` to compare implementations. This
isolates startup cost; it does not predict a complete suite's slowdown, which
also includes measured execution, concurrency and evidence publication.

For an end-to-end subprocess workload, build a release binary and run:

```sh
cargo build --release -p supercov
python3 scripts/python-subprocess-benchmark.py \
  --output /tmp/supercov-subprocess-benchmark \
  --children 1000 --tests 400 \
  --binary candidate="$PWD/target/release/supercov"
```

Use a new output directory for each experiment. The fixture defaults to 321
source files and saves plain/measured timings, CLI phase timings, child latency,
test outcomes and coverage summaries. Add another `--binary label=/path` to
compare builds; measured coverage and outcomes must agree. Vary `--calls` for
hot execution, `--statements` for plan size, `--decision-width` for wider MC/DC
regions, or `--workers` for concurrent children. `--mode noop` isolates helpers
that run no measured source; `--mode script` exercises native entry scripts and
allows the corrected unmeasured-file denominator to differ between versions.

## Understand disk usage

Each completed run stores compressed evidence and metadata under
`.supercov/runs/<run-id>/`. Query views are derived from that evidence rather
than stored as a full report for every filter.

Most of a large run is its query index, `query-index.v1.bin`: on a suite of
about 400 tests with per-test evidence, 160 MB of a 216 MB run, beside 58 MB of
evidence. The index is a cache. Supercov builds it again from the evidence the
first time the run is queried without it, so a CI job that keeps runs as an
artifact can leave it out:

```sh supercov-example
tar czf supercov-runs.tgz --exclude 'query-index.v1.bin' .supercov/runs
```

A run also keeps the view its reports read (`run-view.json.gz`, a few
megabytes), so an export or the HTML report does not analyse the evidence
again. Like the index, it is a cache, made again when it is missing.

A merged run holds a copy of each run's evidence and an index of its own, so
merging adds about as much again. To hand a run to a reviewer, the HTML report
(`npx supercov report`) is usually the smaller artifact.

The isolated workspace may be larger because it can contain an instrumented
build cache. Supercov does not delete history in the background.

```sh supercov
npx supercov runs clean --dry-run
npx supercov runs clean --keep 20
npx supercov runs clean
```

Use `--dry-run` to preview cleanup. Keep enough run history for active reviews
and automation; remove the cache only when reclaiming space matters more than a
faster next run.

## Keep assertion assessments fast and cheap

`assertions assess` asks only questions it has not answered before; answers live
in `.supercov/assertions/` and are reused across runs whenever the question is
the same. `--workers N` raises the requests in flight (default 8) for a faster,
slightly costlier pass. `--dry-run` prints the estimate without sending
anything.
