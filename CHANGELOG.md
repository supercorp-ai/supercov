# Changelog

## 0.0.52

**Added**

- The map report says when a `watch` entry changes what a flow rests on. Naming a file that already holds the flow's nodes widens it from the declarations holding those nodes to the whole file, so a neighbouring function's body is a review again -- sometimes what the author means, never something that should happen unsaid. Naming a file the flow already depends on whole, such as its own test, is reported as redundant alongside the existing manifest advisory.

**Fixed**

- A reason for a change in a whole-file dependency no longer blames the declaration that changed for holding a node it does not hold. `src/a.js: other (line 4) changed (holds this flow's n:2)` named the neighbour of the node's declaration as its holder; it now reads `... (is watched by this flow; this flow's n:2 sits in work (line 1))`, which says why the change counts and where the claim sits.
- Removing a `watch` entry that Supercov itself reports as redundant no longer restates the claim. A manifest, lockfile or runner configuration named in `watch` is tracked for the whole run, so it never became one of the flow's dependencies -- but it was still part of what the acknowledgement was computed over, so deleting the entry the report advised deleting cost a full re-acknowledgement. Writing one and removing one are now both free. Flows that never named such a file keep their existing tokens; only a flow carrying a redundant entry is re-based, onto the token it would have had without it.

## 0.0.51

**Added**

- `supercov runs <id> tests affected` names the tests of a run that the changes since it could have reached: a change in code the test ran, in its test file, or in the shape of a file it ran code in. A change confined to code the test never ran does not count, nor do comments, blank lines or trailing whitespace. `--names` and `--files` print one line per test for a runner's filter; a dependency, configuration or toolchain change affects every test and says so; a file added since the run is outside every record, and the working-tree check says that too.
- Each run records what every test executed, declaration by declaration, in the run's own state. Assertion change records say which tests ran the changed code and how many flows that exposes (`exposed`), alongside the `knownFlows` the change made stale, so the one assessment a change asks for is asked of the right people -- and a change no selected test ran is not asked about at all.

**Changed**

- An acknowledged assertion flow now goes stale for a change to what its claim rests on and for nothing else: the declaration holding each of its nodes and the top level of that file, the file's set of declarations, the test it applies to, a watched file, its assertion, the run's context. Editing another function in a node's file is a notice on the flow (`notices` in the report), not a review. Editing a comment, a blank line or trailing whitespace is nothing, in every language; a comment the language itself reads -- a Go `//go:` directive, a Ruby magic comment, a Rust doctest -- still counts. Each reason names what moved and where the flow sits: `src/server.js: Server.start (line 12) changed (holds this flow's return:31)`.
- A node keeps its acknowledgement when code is added or removed above it, in another declaration or in comments; pointing it at another statement of the same text does not.
- Acknowledgement tokens are now `scov3:` and pin the code a claim rests on rather than the bytes of whole files. Tokens from earlier releases still parse; each such flow reads as needing acknowledgement, with that as its reason, once. Copy the current `expectedBasis` after rereading the claim.

**Fixed**

- A node whose statement appears twice in its file was reported "changed or ambiguous" whenever the file changed anywhere, though it sat untouched at its recorded line.

## 0.0.50

**Added**

- Supercov installs from Homebrew: `brew install supercorp-ai/tap/supercov`. The same prebuilt binary every other channel ships, at the same version, with nothing compiled on install.
- A Go project can run Supercov with Go and nothing else: `go run github.com/supercorp-ai/supercov/cmd/supercov@latest -- go test ./...`. Like any `go run` with a version suffix it ignores the `go.mod` in the current directory, so it neither needs nor touches your module. Every language Supercov measures now has a way in that does not start by installing a different one.

## 0.0.49

**Added**

- Supercov measures Go, Java and Kotlin: lines, branches, functions and MC/DC, on the same footing as every other language. `npx supercov -- go test ./...`, `npx supercov -- mvn test`, `npx supercov -- ./gradlew test`.
- Go runs on `go test` with exact per-test attribution and one evidence file per test package. `-count=1` is added unless your command says otherwise, so a cached package cannot report as covered without running. A `go.work` workspace gets a runtime per module, and a nested `go.mod` no workspace names is left alone. A test that calls `t.Parallel()` counts run-wide rather than being attributed by guesswork.
- Java and Kotlin attribute through each framework's own lifecycle rather than rewritten test sources: JUnit 5, JUnit 4 through Vintage, Kotest and Spock through one JUnit Platform listener, and TestNG through a listener of its own, each data-provider invocation its own test.
- A JUnit 4 suite reaches the platform through Vintage. Supercov adds the engine to its copy of a Maven module and measures it; a Gradle module is named in the output and left alone instead, because putting the platform on a JUnit 4 classpath makes Gradle pick a provider that finds no engine and fails the suite. Add `junit-vintage-engine` and `useJUnitPlatform()` and Supercov measures it.
- Multi-module Maven and Gradle builds are measured module by module and merged, including builds that fork several JVMs to run tests in parallel — `maxParallelForks`, `forkCount`. Kotlin Multiplatform layouts are measured where the JVM is the only target.
- Supercov instruments an isolated copy and leaves your own build untouched. Conditions the compiler reads are left exactly as written — Java's pattern `instanceof` and record patterns, Kotlin's `is` and null comparisons — so such a branch is measured from its arms and carries no condition vectors. Every surface left unmeasured is named in the run's limitations, including a source file the parser could not read.
- Assertion maps inventory `t.Error`, `t.Fatal` and testify for Go, and `assertSomething`, `assertThat` and `fail` for the JVM, which covers JUnit, TestNG, AssertJ, Hamcrest and kotlin.test.

## 0.0.48

**Added**

- `supercov runs <id> check` fails CI below a coverage floor, reading a recorded run without rerunning tests. Floors are set per metric (`--min-lines`, `--min-branches`, `--min-mcdc` and the rest) and `--per-file` applies them to every file with eligible obligations. They compare the counts, never a rounded percentage, so 9,999 covered lines of 10,000 fails a 100% floor. Insufficient evidence — a failed suite, a stale run, a metric with nothing eligible, one left partly measured, or one the adapter never records — ends with `2` rather than passing.
- `supercov runs <id> patch --base <ref>` reports coverage of the lines a change touches, against the merge base rather than the target branch's tip. The denominator is changed lines the run measured, so comments, blanks and declarations fall out by the adapter's own judgement, and deleted lines are excluded. A change with nothing executable says so instead of claiming 100%, and changed product source missing from the run is named rather than counted as covered. `--annotate github` emits workflow annotations, needing no token.
- `supercov runs <id> report --format lcov|cobertura|html` exports a run. All three read the view the gates read, so a consumer's totals are the ones Supercov enforced. Files are written atomically and kept unless `--force`.
- The HTML report is one self-contained document that opens offline from a CI artifact, with a filterable file table and a source view marking each line in words and a glyph as well as colour. Uncovered, not applicable, partly measured and stale stay distinct rather than collapsing into one score; source is embedded only when the run still matches the checkout.
- Assertion reports carry `advisories`, the first naming a `watch` on a file already tracked run-wide.

**Changed**

- Each invalidation signal now costs what it is worth, so maps need one review after upgrading and then stop being disturbed by changes that alter nothing.

**Fixed**

- Cutting a release costs nothing. Manifests are fingerprinted by what they declare rather than their bytes, and a flow watching one is covered run-wide — together, 62% of supergateway's manifest edits.
- Upgrading Supercov no longer marks maps or stored runs stale; only an instrumenter contract change does. Merging and the build caches still see that digest.
- A dependency upgrade is one change to assess, not staleness on every flow, and credit is retained meanwhile.
- The ambient environment left run identity for Rust, Python and Ruby. Linters, formatters, type checkers and coverage settings are no longer execution context; Babel, tsconfig and pytest still are.
- Published crates carry the project README again.

## 0.0.47

**Added**

- Assertion coverage measures Python and Ruby, alongside JavaScript, TypeScript and Rust. Runs record which assertion each test reached and where it is written, so an agent can explain them and earn statement credit. Covers pytest, unittest, RSpec, Minitest, test-unit and Cucumber.
- Python and Ruby test results name the file a test is defined in, which is what an assertion map's test selector needs.

**Changed**

- Neither runtime reports an assertion's column, so a line is credited only when the inventory holds exactly one assertion on it. Put two assertions on separate lines.
- pytest's assertion-pass hook stays armed for a whole test rather than disarming after the first assertion, so every site is recorded. pytest now builds an explanation per passing assertion.

**Fixed**

- Assertion review tokens no longer depend on the ambient environment. A different directory, terminal or Node install previously marked every flow stale at once. Name variables in `SUPERCOV_ASSERTION_CONTEXT_ENV` when a suite needs them. Maps need one review after upgrading.
- Assertion line reports count only lines a statement can be claimed on. Continuation lines and nested function bodies were wrongly in the denominator. The statement percentage is unaffected.

## 0.0.46

**Fixed**

- JavaScript and TypeScript assertion coverage excludes imports known to disappear during compilation. Value imports, side-effect imports and ambiguous compiler settings remain measured. Reports explain excluded statements; rerun tests to collect the corrected denominator.
- Preserve test ownership through HTTP requests, WebSocket upgrades and child processes. Node test cleanup assertions retain their evidence, while shared setup stays separate from individual tests. Skipped and TODO tests remain visible without receiving passing assertion credit.
- Repeated assertion reports reuse cached calculations while checking current source and map freshness. Corrupt caches rebuild automatically without changing authored maps.

**Added**

- Assertion details explain why each mapped source node receives credit or remains context only. Large flows support paginated node and edge views with compact output.
- Evidence diagnostics distinguish missing execution, setup or background work, unobserved assertions and non-passing test selectors. Test-kind reports recognize common E2E and integration file names and disclose runner defaults.
- Updated CLI reference and assertion guides cover mapping, validation, percentage, gaps and reuse after edits. The same guides ship with the CLI and on supercov.com, with a shared sync command and publication checks to prevent drift.

## 0.0.45

**Added**

- Agent-authored assertion maps for JavaScript and TypeScript. Each test run creates `assertions.json`; an agent records which exact assertions observe which source statements. Start with `supercov docs assertion-agent`.
- The regular coverage report shows agent-assessed assertion percentage beside Lines, Branches and MC/DC. Untouched maps and unresolved changes show explicit status instead of a misleading zero. This score does not prove mutation resistance or mapping completeness.
- New runs reuse compatible mappings. Freshness is tracked per flow; changed files enter an impact queue. Read-only validation supplies acknowledgement tokens for the agent to save in the map.
- Inspect assertions with `runs <run> assertions` and `assertion <id>`. Read matching current code with `source <path>`. File hashes support reuse without archiving the whole codebase.
- Rust-generated JSON Schema, paginated validation, source diagnostics and CI gates for current mappings, passing evidence and percentage. Guides ship with installed packages. Async operands, parameterized tests, CommonJS matchers and Playwright fixtures retain exact assertion evidence.

**Changed**

- Agent-authored maps replace experimental mechanical assertion inference. Ordinary execution-phase links no longer award assertion credit. Assertion analysis requires current source matching the run; rerun tests after edits to inherit mappings.

## 0.0.44

**Fixed**

- Repeated Node test registrations no longer overwrite one another's evidence. Same-name loop entries, nested subtests and worker executions retain distinct attempts. Rerun tests to collect evidence missing from older archives.
- Preserve test provenance in paths containing parentheses and retain passed tests whose source registration cannot be resolved. Unsupported assertion operands report analysis limits instead of misleading missing-test claims.
- Bind awaited native assertions to their own source witnesses. Resolve native assertion imports and distinguish checked operands from diagnostic arguments, self-comparisons and shared-input comparisons.

**Added**

- Query-time analysis of bounded console-mock histories, selected counts and payloads, primitive decisions, direct returns and synchronous exception completion. Inline `observes` hints can guide supported checks against an existing passing assertion; they do not change test outcomes or replace missing assertions.
- Decision evidence distinguishes bounded source-model results from branch-observation heuristics. Unsupported cases remain unresolved; assertion evidence is not a global assertion score or a guarantee that arbitrary edits are safe.

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
