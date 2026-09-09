# Changelog

## Unreleased

## 0.0.43

**Added**

- npm JS/TS assertion evidence: `runs <run> assertions`, pageable `--evidence`, and validated `--pragmas` with passing-assertion witnesses. Supports TypeScript 5.8.3 and native 7.0.2 across supported platforms, including Alpine and Windows. Source/compiler freshness is checked. Existing probes are retained; results are candidates, not a verified safety percentage.
- Python and Ruby link pre-assertion execution evidence to passing assertions: pytest, unittest, Minitest, RSpec, Cucumber and test-unit.
- A reproducible checkout walkthrough demonstrates coverage gaps and stronger assertions. Public guides ship with installed packages.

**Changed**

- Ruby 3.4+ avoids repeated branch/method-table sampling without changing coverage results; Ruby 3.3 retains its existing path.

**Fixed**

- Rust test filters are honored, function locations exclude doc comments, and interrupted runs clean up test processes, including through Windows Job Objects.
- Ruby test-unit outcomes, class-variable assignments, UTF-8 source, guarded pattern matching and constant boolean arms are handled correctly. Ractor blocks retain line coverage with explicit probe limitations.
- Jest preserves user configuration and records exact per-test identities, parameterized tests, retries, final outcomes and assertion phases.

## 0.0.42

**Added**

- Rust coverage says whether a test checked what it ran. Evidence a thread recorded before passing an `assert!`, `assert_eq!` or `assert_ne!` links to that assertion, so a line reads "linked to a passing assertion", not "execution only". On hyper: 6,204 of 6,707 covered lines.
- `runs <run> file <path> --json` reports `totalLines` and `coveredLines`; `SUPERCOV_PHASE_TIMING=1` breaks the Rust workspace phase down.

**Fixed**

- Found by checking Rust coverage per file against `cargo llvm-cov` over 26 crates: code that could never be covered was counted as uncovered. A module behind an off `#[cfg]` (memchr's 816 lines for other architectures), a `const fn` body, a `GlobalAlloc` impl and a proc-macro crate's own code are reported as unmeasured now. memchr reads 84.4%, was 63.2%.
- A module declared inside `cfg_if!` or `cfg_select!` is measured; hashbrown's NEON group was invisible.
- Instrumenting a crate that turns the prelude off, or an attributed macro statement, no longer breaks the build (tracing, hyper, tokio).
- A limitation is reported at its own site and real severity: a boundary of the denominator no longer reads as "Instrumentation Incomplete".

**Changed**

- Preparing a Rust workspace parses each file once instead of twice.

## 0.0.41

**Added**

- Arguments of the std expression macros (`assert!`, `assert_eq!`, `println!`, `format!`, `write!`, `vec!`, `dbg!`, `panic!`, `matches!`, ...) are measured. An `assert!` condition is a decision with its own condition vectors; `matches!` is a boolean decision unless it already serves as an `if` condition.

**Fixed**

- Found by running the Rust frontend over 21 real crates (bytes, serde_json, regex, serde, tokio, ...) under `cargo test` and `cargo nextest run`: `if let`/`while let` before edition 2024 became a let chain; a message-less `assert!` lost its panic text; `#[path = "../src/..."]` and symlinked modules were rejected; a word a newer edition reserves failed to parse; `should_panic` doctests went unmatched; proc-macro crate tests lacked the dynamic library path; a trailing attributed macro became an unstable attributed expression; `assert!` over a `&bool` failed to compile; tests ran in the workspace root, not their package directory. All match plain Cargo.
- Deep recursion no longer overflows: probe frames shrank and test processes get a 16 MiB stack via `RUST_MIN_STACK`.
- `SUPERCOV_RUST_DUMP_FAILED_INSTRUMENTATION=<dir>` dumps failed transforms.

**Changed**

- Rust probes cost less in hot loops: repeated hits and decisions dedupe without locks (a decision from about 190ns to 70ns).

## 0.0.40

**Added**

- Doctests are measured: `cargo test` runs them with Supercov standing in for rustdoc, each in its own process, attributed by name, merged and standalone alike.
- `cargo nextest run` works on the public path. Supercov is nextest's target runner: scheduling, retries, filters and exit status are untouched, each attempt is recorded, and a pass on retry is flaky.
- Every Rust branch obligation has a probe: match arms, `&&` and `||`, `for` and `while` loops, `?`, let chains (pattern outcomes derived exactly from where the chain stopped) and attributed statements. Only const contexts and macro expansions remain declared.

**Fixed**

- A doctest-only crate reported zero tests; the test count names tests, not attempts.
- Only files rustc compiles are instrumented: crate roots and the modules reached through `mod`, `#[path]` and literal `include!`. A `.rs` file embedded with `include_str!` stays as written.
- Evidence files name their instrumentation, so a program a test builds and runs with its own probes no longer breaks the run.
- The last arm of an exhaustive match no longer demands an impossible "not selected" outcome.

## 0.0.39

**Added**

- `cargo binstall supercov` downloads the prebuilt binary from the GitHub release instead of compiling from source.
- Rust suites run on Windows. The probe runtime maps its evidence file through a Windows file mapping and hooks thread and process creation through the executable's import table, so threads and children started inside a test stay attributed to it.
- Ruby 4.0 is supported and verified.

**Fixed**

- On Windows, a Python run measured nothing: the project root reached the runtime with a `\\?\` prefix its files lacked. A Ruby run could not start `rspec`, a batch shim. Both work now, and both runtimes warn when a run executes files but none under the root.
- A plain `npm ci` installed native binaries from an older release: the lockfile named one version beside a tarball URL of another.

**Notes**

- Python 3.12 to 3.14, Ruby 3.3 to 4.0 and Rust suites are verified on Linux, macOS and Windows; the Rust frontend runs in CI for the first time.

## 0.0.38

**Fixed**

- The engine crate packages the runtime shims it embeds, so the published crates build from their source again. 0.0.37 reached crates.io only as `supercov-contracts`; `supercov-engine` and `supercov` resume here.

## 0.0.37

**Added**

- PyPI (`supercov-cli`), RubyGems (`supercov`) and crates.io (`supercov`) are published with every release at the same version as npm: a wheel for each of the eight platforms, a gem for the seven Ruby has a platform for, and the source crates. The release also attaches every file to its GitHub release.

**Fixed**

- Linux builds require glibc 2.28 rather than 2.39, so they run on Debian 12, Ubuntu 22.04, RHEL 9 and Amazon Linux 2023 -- the base of most Node container images -- instead of only on distributions as new as Ubuntu 24.04.

**Changed**

- Preparing a run no longer forces each generated file to disk. Those files are rebuilt, or restored from a digest-verified cache, on every run, so the wait bought nothing: setup drops from 290-310 ms to about 50 ms on Windows, and from 400 ms to 14 ms on macOS.

## 0.0.36

**Added**

- Windows builds for x64 and arm64. `npx supercov` selects them automatically. JavaScript and TypeScript suites are verified on Windows; Python, Ruby, and Rust suites are not yet.

**Fixed**

- Navigations inside a Playwright context launched outside the fixtures — a persistent profile, or a `newContext` from test code — now carry the test's identity, so a cross-site iframe's document requests are attributed to the test instead of the run. Headers the suite configured on that context are kept, and restored when the test ends.
- A test that starts a server with `execSync("npm run start")`, or any launch handed to the shell as one string, now gets the project built first, the same as `spawn("npm", ["run", "start"])` did. The string form is how most suites start the server they test against, and it was still reaching a gateway that had never been built.

**Changed**

- The Python package now states what it supports: CPython 3.12 or newer, which is what `sys.monitoring` requires, instead of the 3.8 its metadata claimed. Package homepages point at supercov.com, the npm description lists Ruby, and the README documents the supported operating systems and architectures.

## 0.0.35

**Fixed**

- Coverage a process buffered is no longer lost when a signal ends it, so a server or gateway a test kills in teardown keeps the coverage it produced.
- Instrumented TypeScript carries the generated-source exemption under a direct test command, not only a Supercov-orchestrated build, so a project that compiles inside its own test command builds under measurement.
- A project whose tests launch a package script that runs compiled output is built before the runner, instead of the run reaching a gateway that was never built.
- A Ruby process killed before it could report no longer leaves its lines reading as uncovered. Ruby reads its coverage as the interpreter exits, so a process that never gets there takes with it whatever it observed since the last test boundary; the run now declares that gap, which blocks completeness, rather than counting it against the code.

## 0.0.34

**Fixed**

- Lines no frontend could measure are no longer counted in the line total, where they previously skewed the ratio.
- The instrumentation banner no longer displaces a `#!` line, which broke builds of projects with an executable entry point.
- Instrumented build output no longer overwrites the project's own build output.
- Generated runtime modules are now `.mjs`, so loaders that treat `node_modules` as CommonJS, such as `ts-node/esm`, link them correctly.

## 0.0.33

**Added**

- Ruby coverage for RSpec, Minitest, test-unit and Cucumber. Runs use the project's own interpreter and bundle; Supercov only adds a `-r` entry to `RUBYOPT`.
- Exact MC/DC, loop and iterator iteration, short-circuit assignment (`||=`, `&&=`), case and safe-navigation selection, and rescue handling, on top of Ruby's `Coverage` module.

**Fixed**

- A Python run's stale check no longer compares it against JavaScript inputs.

**Notes**

- Ruby 3.4 and newer measure everything. Ruby 3.3 measures through `Coverage` alone and declares the rest.

## 0.0.32

**Added**

- Playwright browsers and contexts launched by project fixtures or test code are measured, including persistent contexts, remote and standalone launches, raw `Browser.newContext` pages, and pages closed before teardown.

**Fixed**

- Collector fixtures apply to every test-shaped facade export while preserving the facade's own overrides.
- Browser phase ids are scoped to the attempt that minted them, so a shared persistent context no longer leaks a prior test's phase into later evidence.

## 0.0.31

**Added**

- CPython 3.12–3.14 coverage for pytest and unittest through `sys.monitoring`, measuring the project in place with no source rewriting and no copied workspace.
- Exact line, branch, decision and MC/DC obligations, with attribution for pytest workers, retries, phases, threads, subprocesses and multiprocessing, and kill-resilient mmap evidence.
- Python setup documentation and a CPython compatibility matrix.

## 0.0.30

**Fixed**

- A comment leading a parenthesised `return` argument was restored on its own line, letting automatic semicolon insertion read `return;` and change program behaviour under measurement. Restored comments now land only where a line break is inert.
- The instrumented workspace is self-contained when mounted into a VM or container: nested `node_modules` are cloned copy-on-write on APFS or hard-linked elsewhere, and dependency trees never sync back to the project.
- Regenerated bundler output no longer makes a run read as stale as soon as it finishes.

## 0.0.29

**Fixed**

- Source discovery no longer counts tooling as source: nested checkouts, root-level tool directories, generated trees and hashed bundler output are skipped, packages declared in `workspaces` are found wherever they live, and functions passed to compile-time macros are left as written.
- Evidence written by clones of one process, such as VM pools restored from a shared snapshot, no longer corrupts a run. Writers carry per-instance tokens and rotate on collision, and a torn line costs one record instead of the whole run.

## 0.0.28

**Changed**

- Everything Supercov writes now lives under a single `.supercov` directory — runs, locks and the instrumented workspace cache — which writes its own Git ignore rule.

## 0.0.27

**Added**

- Files the wrapped command creates or changes are synced back to the project, so updated snapshots, generated fixtures and test reports land where the plain command would have put them. Changes to instrumented sources and deletions are reported rather than applied.
- Every npm release now also publishes a GitHub release.

**Changed**

- The workspace container moved from `supercov/` to a hidden `.supercov-workspace/`, migrating existing caches automatically.

## 0.0.26

**Changed**

- A test run in an ecosystem Supercov does not support is recognised from the command or manifests and reported by name instead of producing an empty measurement.

**Fixed**

- The test command is authoritative, so a stray `package.json` can no longer route a `go test` run into JavaScript measurement.

## 0.0.25

**Fixed**

- Instrumented JavaScript suites run at baseline speed again. Evidence transports batch per event-loop turn and per macrotask instead of writing per record, taking a latency-sensitive UI flow from 2.4x baseline to 1.08x with byte-equivalent evidence.

**Added**

- The phase-timing benchmark harness that found it.

## 0.0.24

**Fixed**

- Suites that re-export Playwright's `test` and `expect` through their own fixture package link again. On 0.0.23 every spec importing a facade helper failed to link and Playwright discovered zero tests.

## 0.0.23

**Fixed**

- Interactive terminals no longer hang when a workspace phase outlives the quiet period. The spinner is gone; long silent phases print one static status line.
- Instrumented sources reach browser bundles through a dependency-free capability seam, so Vite builds no longer fail on Node builtins.

**Changed**

- Symlinks escaping the project are omitted with a diagnostic instead of refusing the run.
- The canonical contract no longer advertises the removed waivers surface.
