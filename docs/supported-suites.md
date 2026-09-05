# Supported languages and test suites

Supercov supports JavaScript, TypeScript, Rust, Python, and Ruby today. Start with
the same test command the repository already uses; Supercov detects supported
runners inside that command.

```sh
npx supercov -- npm test
npx supercov -- npx playwright test
npx supercov -- cargo test
npx supercov -- pytest
npx supercov -- rspec
```

## Language support

| Language | Status | Start with |
| --- | --- | --- |
| JavaScript | Available | `npx supercov -- npm test` |
| TypeScript | Available | `npx supercov -- npm test` |
| Rust | Available | `npx supercov -- cargo test` |
| Python | Available | `npx supercov -- pytest` |
| Ruby | Available | `npx supercov -- rspec` |
| Zig | Coming soon | — |
| PHP | Coming soon | — |
| C | Coming soon | — |

The npm-distributed CLI requires Node.js 22 or newer for every language.

## What exact and aggregate mean

**Exact attribution** means Supercov knows which test, attempt, retry, and
runner produced the coverage. Queries such as `test`, `passed`, and `failed`
can use that identity.

**Aggregate coverage** means Supercov knows the source executed but cannot
truthfully assign it to one test. Whole-run `gaps` and `file` queries still
work; per-test questions are limited.

Supercov reports the level it actually observed. It does not guess.

## JavaScript and TypeScript

| Runner | Attribution |
| --- | --- |
| Playwright | Exact per test, worker, retry, outcome, action, and assertion phase |
| Vitest | Exact per test, with setup execution kept separate |
| Jest | Exact per test, including concurrent and parameterized tests |
| `node:test` | Exact per test |
| AVA and Mocha | Aggregate structural coverage |
| Other Node-based runners | Aggregate when their processes remain visible to Supercov |
| Browser component runners without an adapter | Aggregate structural coverage |

One command may launch several runners. Supercov combines their evidence into
one run and preserves runner identity wherever the runner exposes it.

### Builds and source formats

JavaScript and TypeScript projects may use Vite, Next, Turbopack, Webpack,
esbuild, SWC, `tsc`, or no build step. ESM, CommonJS, JavaScript, JSX,
TypeScript, and TSX are supported.

Supercov instruments an isolated copy. It does not ask you to add an import,
reporter, plugin, or alternate build output.

A suite that runs its own compiled output is built before the tests start.
Supercov reads the tests: one that imports from `dist/`, or launches a package
script that does—`spawn("npm", ["run", "start"])`, `execSync("npm run
start")`, or any launch handed to the shell as one string—means the build is
part of the run, and a `build` script is run inside the isolated copy first.
Without it the server under test would never exist. A launch that only names a
build subcommand, such as `vite build`, is not taken as consuming a build.

Instrumented TypeScript is exempt from the host's type policy whether the
compile is Supercov's own build step or the test command's own `tsc`.
Instrumentation necessarily rewrites control-flow expressions in ways a type
checker cannot narrow through, so a project that compiles inside its test
command builds under measurement exactly as it does without it.

### Browsers, servers, and child processes

Playwright support includes Chromium, Firefox, and WebKit, along with pages,
frames, popups, workers, request contexts, WebSockets, and test-launched child
processes where the runner exposes their identity.

Browsers a suite launches itself are covered too. A fixture that calls
`chromium.launchPersistentContext`, or `launch`/`connect` and hands out its own
contexts and pages in place of Playwright's `page` fixture, is adopted by each
test's collector: its pages are read before the fixture closes them, and a
context kept for the whole worker follows the current test's identity. Actions
on such pages are not recorded as separate phases, so their evidence is
attributed to the test and its assertions rather than to individual clicks.

Node child processes inherit coverage automatically. Long-running servers get
a short drain window after the test command finishes so buffered evidence can
arrive. Work without a reliable test identity is kept as background coverage
instead of being assigned to an arbitrary test.

A child a test stops in teardown keeps the coverage it produced. Buffered
evidence is written when a terminating signal arrives—`SIGTERM`, `SIGINT`,
`SIGHUP`—not only when the process exits on its own, so killing a gateway
after the request it served does not lose the request. The program's own
signal handling is untouched: a process with no handler still dies from the
signal exactly as it would unmeasured, and one with its own handler keeps
it. `SIGKILL` cannot be caught by anything and is the one stop that loses
whatever was still buffered.

## Rust

| Runner | Attribution | Current requirement |
| --- | --- | --- |
| Cargo's standard libtest runner | Exact test and attempt identity | Rust 1.95; run with `npx supercov -- cargo test` |
| rustdoc doctests | Exact doctest identity; every doctest runs in a process of its own | Rust 1.95; part of `npx supercov -- cargo test` |
| cargo-nextest | Exact test, attempt, retry, and binary identity | cargo-nextest 0.9.138 or 0.9.140 |

Supercov preserves Cargo's test selection, scheduling, fail-fast behavior,
environment, and exit status. Doctests are measured like any other test:
Supercov stands in for rustdoc during `cargo test --doc`, runs each doctest
in its own process, and attributes what it executed to that doctest by name.
With nextest, Supercov is nextest's target runner, so nextest's own
scheduling, retries and output stay as they are and each attempt is recorded
separately. The measured source is what rustc compiles: every crate root and
the modules it reaches through `mod` declarations, `#[path]` attributes and
literal `include!` calls; a `.rs` file nothing declares as a module, such as
one embedded with `include_str!`, is left untouched. Use the repository's
normal flags after the wrapped command:

```sh
npx supercov -- cargo test --workspace
npx supercov -- cargo nextest run --workspace
```

`cross` is not supported yet. Unsupported command shapes fail with an
explanation instead of silently falling back to plausible but inaccurate
attribution.

## Python

| Runner | Attribution | Current requirement |
| --- | --- | --- |
| pytest | Exact test, worker, retry, and setup/call/teardown phase identity | CPython 3.12 or newer; run with `npx supercov -- pytest` or `python -m pytest` |
| pytest-xdist | Exact per worker | Workers inherit the run through the environment |
| pytest-rerunfailures | Exact per attempt; flaky tests are reported as such | |
| `python -m unittest` | Exact test and setUp/test/tearDown phase identity | Serial in-process; skips and expected failures are recorded; subtest failures roll up to the parent test |

Supercov measures Python through CPython's own monitoring interface. Nothing is
copied, rewritten, or compiled differently: the project runs in place with its
own interpreter and virtual environment, and Supercov only adds a start-up hook
through `PYTHONPATH`, a pytest plugin through `PYTEST_PLUGINS`, and a few
`SUPERCOV_*` variables. Child interpreters started with `subprocess` or
`multiprocessing` inherit the exact test identity; threads and thread pools
carry it through `contextvars`.

Each interpreter writes commit-framed evidence to a process-owned mmap. A hard
kill preserves completed observations and an incomplete tail is ignored; an
exhausted transport or corrupt committed frame fails the run closed.

Measured obligations are statements (including several on one line), function
entry, boolean decisions with MC/DC vectors, `for` and comprehension iteration,
`and`/`or` short-circuiting, `match` case selection, and `try` completion,
handler selection and exception propagation, all derived from CPython's own
instruction positions rather than from exception hooks.

Interpreters launched with `-I`, `-E`, or `-S` ignore `PYTHONPATH` and are not
measured. Code compiled from strings at runtime has no source obligations.

```sh
npx supercov -- pytest
npx supercov -- python -m pytest -n 4
npx supercov -- uv run pytest
npx supercov -- python -m unittest
```

## Ruby

| Runner | Attribution | Current requirement |
| --- | --- | --- |
| RSpec | Exact example and before/example/after phase identity | Ruby 3.4 or newer for full measurement; run with `npx supercov -- rspec` or `bundle exec rspec` |
| Minitest (including Minitest::Spec and ActiveSupport::TestCase) | Exact test and setup/test/teardown identity; skips recorded | `ruby -Itest ...`, `rake test`, `rails test` |
| test-unit | Exact test and setup/test/teardown identity; omissions and pendings recorded | `ruby -Itest ...`, `rake test` |
| parallel_tests, Rails process workers | Exact per worker process | Workers inherit the run through `RUBYOPT`; verified on a Rails app with bootsnap, Zeitwerk and two forked workers |
| Thread-parallel Minitest (`parallelize_me!`, `parallelize(with: :threads)`) | Probe observations exact per test; line, method and simple-branch observations made while phases overlapped go to the run, declared | |
| Cucumber | Exact scenario identity (`features/x.feature:LINE`), hook steps as setup/teardown | `cucumber`, `bundle exec cucumber` |

Supercov measures Ruby with Ruby's own `Coverage` module plus probe calls it
splices into application files in memory as they load. Nothing on disk is
rewritten or copied; the project runs with its own interpreter and bundle, and
Supercov only adds a `-r` entry to `RUBYOPT` and a few `SUPERCOV_*` variables.
No insertion adds a line, so backtraces keep their line numbers.

Measured obligations are statements, method definitions, `if`/`unless`/
ternary/`while`/`until` decisions with MC/DC vectors over `&&`/`||` operands,
`while`/`until`/`for` iteration and the idiomatic iterator blocks (`each`,
`map`, `times`, `select`, ...), `&&`/`||`/`||=`/`&&=` short-circuiting,
`case`/`when`, `case`/`in` and `&.` selection, and `begin`/`rescue` completion,
handler selection and propagation. `||=` and `&&=` on method-call, index and
constant targets are exact too: an arrival probe and a right-side probe count
the skipped side without re-reading the target. Blocks and lambdas are
statements inside their methods, not function entry points, and that includes
a `define_method` block: Ruby's own method coverage reports one entry per
method it defines, but the block's body is measured statement by statement
instead of as a definition.

A statement on a line Ruby's own line table never counts (`x = case`, a
multi-line literal assignment, a bare `begin`, `if false`) gets a probe at load
time instead.
`if true`/`if false`/`if nil` and other literal predicates are folded the way
Ruby folds them: no branch, and the dead arm is not an obligation. A Spring
preloader started before the run has no hook and fails closed; JRuby and
TruffleRuby are not supported.

The runtime loads through `RUBYOPT` before Bundler and requires only
`coverage`, so it never activates a gem an application's Gemfile pins
differently. Insertions are checked against Ruby itself by a sweep
(`scripts/ruby-position-sweep.rb`) over Ruby's whole standard library and the
Rails, Rack, RSpec, Minitest and Cucumber gems, about 3,000 files: every file
is transformed and compiled, and every position Supercov expects is compared
with what Ruby reports. With `--load` each file also runs twice, untouched and
transformed, so the probes are proven to preserve behaviour and every method
position is checked.

A `begin` whose body ends in an expression that can `return` from inside
itself has its handlers and propagation measured as usual, but its normal
completion is declared instead of probed unless every branch of that
expression can carry the probe: Ruby cannot pass such an expression as an
argument, which is what a probe wrapper does.

Measuring never breaks the program being measured. If a file cannot be
compiled with its probes, it loads unmodified: Ruby's `Coverage` still
measures its lines, methods and own branches, and only the obligations a
probe would have proven are declared for that file. Setting
`SUPERCOV_RUBY_SKIP_PROBES` to a comma-separated list of path fragments puts
chosen files on that same path deliberately, which is the escape hatch if
instrumentation ever disagrees with one of yours.

Ruby 3.3 does not apply its `Coverage` module to code compiled by a load hook,
so on 3.3 Supercov measures through `Coverage` alone: lines, methods and the
branches Ruby reports itself. Everything that needs a probe (multi-condition
decisions, `||=`, loops, `rescue` flow, a second statement on a line) is
declared unmeasured on that interpreter rather than shown as a gap. Ruby 3.4
and newer measure everything.

Ruby reads its own coverage as the interpreter exits, and that shapes what a
stopped process keeps. A process ended by a signal it can catch—`SIGTERM`
from a test's teardown, `SIGINT`—unwinds through that exit and reports
everything it measured. A process killed with `SIGKILL`, or one that leaves
through `exit!`, never gets there and takes with it whatever it observed since
its last test boundary. Supercov cannot recover that or say which lines it
would have been, so the run declares that a process did not report, which
blocks completeness, rather than counting those lines against the code. Stop a
Ruby server with `SIGTERM`, or wait for it to exit, and it reports.

```sh
npx supercov -- rspec
npx supercov -- bundle exec rspec
npx supercov -- ruby -Itest test/shapes_test.rb
npx supercov -- bin/rails test
```

## Containers, VMs, and remote execution

Supercov can collect from supported processes launched through a container, VM,
or remote executor when it can see the launch boundary, carry the instrumented
workspace into that environment, and receive evidence back.

Mounted workspaces and local child-process launchers are the most direct path.
If an executor hides how code is launched or cannot return evidence, Supercov
reports the missing boundary rather than claiming unseen code was measured.

## If your runner is not listed

For a Node-based runner, try the complete command and inspect the result:

```sh
npx supercov -- npm test
npx supercov runs latest runners
npx supercov runs latest scope
```

Aggregate coverage may already be useful even without exact per-test identity.
If a supported runner appears incomplete, see [Troubleshooting](troubleshooting.md)
and include the exact command and runner output when opening an issue.
