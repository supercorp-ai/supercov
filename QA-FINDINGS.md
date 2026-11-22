# Supercov 4.29.0 mutation testing: release QA findings

What a new user meets when they run the release's mutation testing, in every
language, on public projects they have not tuned for it.

## How it was tested

- The release as users get it: `npm pack` of this branch with the version
  bumped to 4.29.0, the native package for `x86_64-unknown-linux-gnu` packed by
  `scripts/package-native.mjs`, both run with
  `npx --package=<supercov-4.29.0.tgz> --package=<native tgz> supercov` in a
  fresh shallow clone. Nothing else of Supercov was installed.
- On Linux CI machines (8-CPU AWS c8a.2xlarge, Ubuntu, Node 24, Python 3.12,
  Ruby 3.4.13, Go 1.24, Rust 1.95, Temurin 11/17/21, Maven 3.9.9), one build and
  one machine a journey, 2026-11-22. Runs 7563657785657160, 5569032582904862
  and 6528316615500289.
- Each project was checked out at the latest commit that changed its sources,
  so `HEAD~1` is a real change, and its test command was first run without
  Supercov (the baseline).
- Branch heads merged into `release/release-qa` and tested:

  | Branch | Commit |
  | --- | --- |
  | `release/4.29.0` (base) | `56a09548` |
  | `release/mutation-core` | `8ba44edf` |
  | `release/vitest-speed` | `45803f06` |
  | `release/mutation-python` | `0fdceebb` |
  | `release/mutation-rust` | `0d9246ab` |
  | `release/mutation-go` | `278dcedc` |
  | `release/mutation-ruby` (local only, not on origin) | `951c39a4` |
  | `release/mutation-jvm` | `d8dbd832` |
  | `release/release-docs` (docs read, not merged) | `c27de66f` |

  The merges needed hand resolution in `js_weak_mutation.rs`,
  `mutant_list.rs`, `strong_driver.rs`, `mutants_test_command.rs`,
  `mutation-drivers.md`, `cli.rs` (fault-pin line numbers, and two test
  functions git interleaved), and three semantic fixes after the textual
  merge: `CandidateMutant.effects` missing in `jvm_strong.rs` and doubled in
  `rust_mutants.rs`, and `StrongRustRequest.workers` missing in two Rust tests.
  `pit_mutator` and the Rust rules both added functions at the same place in
  `js_weak_mutation.rs`; the merge cut `rust_valued`'s closing brace.

## Projects and what happened

| Project (runner) | Baseline | `supercov -- tests` | `supercov mutants` (first) | Again | After an edit |
| --- | --- | --- | --- | --- | --- |
| destr 2.0.5 (Vitest) | passes | 3.5 s | 141 mutants, 78.7 %, 11.8 s | 1.1 s | 11.4 s, all rerun |
| luxon 3.4.4 (Jest 29) | 18 own failures (time zones) | 10.5 s | capped at 240 s: 68 of 4,876 mutants, "about 265m left" | — | — |
| uuid 9.0.1 (Jest 28) | 1 own failure | 7.3 s | capped at 240 s: 180 of 567, 114 errors | capped | — |
| yup 1.4.0 (Jest 27) | passes | no tests found | "0.0 %", exit 0 | — | — |
| content-type 1.0.5 (Mocha) | passes | no sources found; with `SUPERCOV_SOURCE_ROOTS=index.js` 4.6 s | 165 mutants, 91.6 %, 7.8 s | 1.0 s | 7.9 s |
| nanoid 5.1.5 (`node:test`) | passes | 5.3 s | 162 mutants, 23.3 s | 1.4 s | 23.4 s |
| playwright-test-coverage (Playwright) | 3 pass | "No tests found" | "0.0 %", 11 no coverage, exit 0 | — | — |
| qs 6.13.0 (tape: no driver) | passes | 6.5 s | 1,292 survived, 0.0 %, exit 0 | — | — |
| itsdangerous 2.2.0 (pytest) | passes | 5.0 s | 521 run, 73.9 %, 8.4 s | 1.3 s | 8.5 s |
| pastel 0.8.0 (RSpec) | 3 own failures on Ruby 3.4 | 5.6 s | 1,678 run, 83.6 %, 7.9 s | 1.0 s | 8.0 s |
| rack-test 2.1.0 (Minitest) | 2 own failures on Ruby 3.4 | 5.5 s | 1,016 run, 83.1 %, 3.7 s | 1.1 s | 1.1 s |
| google/uuid 1.6.0 (Go) | passes | 5.5 s | 189 run, 73.5 %, 16.4 s | 1.0 s | 14.6 s |
| rust-hex 0.4.3 (cargo test) | passes | 10.7 s | 96 run, 76.2 %, 26.0 s | 1.1 s | 14.2 s |
| java-diff-utils 4.12 (Maven, JUnit 5) | passes | fails (Checkstyle); with `-Dcheckstyle.skip` 33.6 s | capped at 240 s: 315 of 809, estimate growing | capped | — |
| aedile 3.0.4 (Gradle, Kotlin, Kotest) | passes | 32.9 s | 28 mutants, all `error`, "mutation score -" | 1.0 s | 19.4 s |

What works well and should stay: a second `supercov mutants` over an unchanged
checkout answers in about a second; Rust's and Go's progress lines (what is
being built, `N of M mutants`, an estimate) are clear; a run recorded by
`npx supercov@4.28.0` is read without complaint and `supercov mutants` measures
a new one with a clear reason; `runs latest mutants --diff HEAD~1` is accurate
and its text output is good; the Vitest and Mocha passes on small projects are
fast; SIGINT stops every language's pass within a second (exit 130).

## Findings

Severity: **blocker** (wrong answer or broken release promise), **high**
(a journey fails or misleads for a common setup), **medium**, **low**
(cosmetic). Owner is a guess.

### F1. A runner with no driver reports every mutant survived, not a refusal — blocker, core/JS

- Steps: qs 6.13.0, `npx supercov -- npx tape 'test/**/*.js'`, then
  `npx supercov mutants`.
- Expected (`docs/mutation.md`: "A runner with no driver (another Node-based
  runner) is refused rather than reported as a score of nothing killed"): a
  refusal, exit 2.
- Actual: `mutation score 0.0%`, `survived 1292`, `ran 0 mutant(s)`, exit 0.
  Every verdict is `survived` with reason `no-passing-candidate`, while the weak
  report of the same run says 1,220 of them are infected. tape's tests kill
  most of them: these are soundness violations, and `--fail-on survived`
  would fail every pull request.

### F2. `supercov mutants` gives a score and exits 0 when the measured command failed or ran no test — high, core

- Steps: any project whose tests fail under Supercov or in general: Fuse.js
  without its build (7 suites fail to load, 0 tests), yup on Jest 27 (no tests
  found), Playwright (F6), or any project with a failing test added (destr,
  itsdangerous, pastel, uuid, hex, aedile).
- Expected: the same stance as `runs check` and `runs patch`, which exit 2 over
  a run whose command failed; at least a warning that failing tests kill
  nothing; a refusal when the run recorded no test.
- Actual: a full summary (`mutation score 0.0%`, `no coverage 1148`) and exit 0
  for a run with no test at all; for a failing suite a normal-looking score with
  no word that the command failed. `runs latest` over the same run says
  "coverage below is diagnostic and cannot gate"; `supercov mutants` does not.
  With no test, three mutants of module-level code were also called `survived`
  (`no-passing-candidate`) rather than `no-coverage`.

### F3. Kotest suites: every mutant is `error` — high, jvm

- Steps: sksamuel/aedile 3.0.4 (Gradle, Kotlin, Kotest on the JUnit
  Platform), `npx supercov mutants -- ./gradlew test`.
- Expected (`docs/mutation.md` lists Kotest): killed and survived verdicts.
- Actual: all 28 mutants `error`, "its candidate test
  com.sksamuel.aedile.core.AsCacheTest#Cache should support getOrNull was not
  found in the build's test JVM", and the summary prints `mutation score -`.
  Kotest's test names (spaces, nested contexts) are not matched to the names
  the measured run recorded.

### F4. Jest strong pass: seconds a mutant, hours for a real suite, many errors — high, core/JS

- Steps: luxon 3.4.4 (1,094 tests in 2.3 s), `npx supercov mutants -- npx jest`;
  uuid 9.0.1 (50 tests in 0.6 s), `supercov mutants -- sh -c
  "BABEL_ENV=commonjsNode npx jest test/unit/"`.
- Expected: the speed `docs/mutation.md`'s Jest rows suggest ("Jest ... run in
  resident processes, as StrykerJS's runners do").
- Actual: luxon reached 68 of 4,876 mutants in 3 min 45 s and estimated
  "about 265m left"; `--diff HEAD~1` ran its one mutant in 86 s. uuid reached
  180 of 567 in 3 min 45 s with 114 `error`s and dozens of timeouts, and an
  estimate that started at "about 20m00s". The progress estimate swings widely
  (608 min, 748 min, 258 min within two minutes).

### F5. Jest 27 and 28: the project's Jest configuration is ignored — high, core/JS

- Steps: yup 1.4.0 (Jest 27) or uuid 9.0.1 (Jest 28.1.3), `npx supercov -- npx jest`.
- Actual: `Warning: [supercov] the project's Jest configuration was not read
  (readInitialOptions is not a function); Jest's defaults apply`, twice, with a
  Node `--trace-warnings` hint. yup then finds no tests (its config names
  them); uuid runs on defaults.
- Expected: the configuration read on the Jest versions still in wide use, or a
  clear statement of the minimum Jest version in `docs/supported-suites.md`
  (it gives none).

### F6. Playwright: "No tests found" under Supercov — high, core/JS (pre-existing, also 4.28.0)

- Steps: mxschmitt/playwright-test-coverage at `910da570` (Vite app, tests in
  `e2e/*.test.ts`, `webServer: npm run dev`), `npx supercov -- npx playwright test`.
- Expected: the 3 tests the baseline runs.
- Actual: `Error: No tests found`, then the hint about `npm run build`, which
  does not apply. The Playwright journey could not be tested further.

### F7. Ctrl-C leaves test processes running — high, core/JS and go

- Steps: start `npx supercov mutants -- <tests>` in its own process group,
  send SIGINT to the group (as a terminal's Ctrl-C does) once the strong pass
  has started.
- Expected: Supercov exits and so does everything it started.
- Actual: Supercov exits within a second with 130, but 8 `sh -c ... npx jest` /
  `npm exec jest` processes (uuid 9.0.1) and 6 Go test binaries
  (`.supercov/strong/.../bin/*.test -test.v ...`) keep running afterwards.
  Vitest, Mocha, `node:test`, pytest, RSpec, Minitest, cargo and Maven left
  none. (Gradle's and Kotlin's daemons stay, as Gradle intends.)

### F8. An interrupted or killed pass keeps none of its verdicts — medium, core

- Steps: interrupt a pass (F7), or let a long one be killed; run
  `supercov mutants` again.
- Expected: per `docs/cli.md`, a pass that stops early keeps its verdicts and
  the next runs the rest.
- Actual: every rerun starts at mutant 1 (destr 141 of 141, nanoid 162,
  itsdangerous 521, Go 189, Maven from "161 of 809" again). On luxon (F4) an
  interruption after an hour would lose the hour. (The docs' promise is for a
  worker that never ends, but a user reads it as resumable.)

### F9. `supercov mutants --diff` prints the whole run's verdicts, not the change's — high, core

- Steps: itsdangerous, `npx supercov mutants`, then
  `npx supercov mutants --diff HEAD~1`.
- Expected: the change's mutants and their score, as
  `runs latest mutants --diff` gives (2 mutants here).
- Actual: `mutation score 73.9%`, `killed 419 ... ran 0 mutant(s)`: the run's
  saved verdicts. With `--fail-on survived` the exit code is about the change
  (destr: exit 1 and the change's 9 survivors listed), but the score above it
  is not. Nothing says which mutants were asked about.

### F10. The PR comment and the gate disagree after a strong pass — high, core/docs

- Steps: destr, `npx supercov mutants`, then
  `npx supercov runs latest mutants --diff HEAD~1 --format markdown` and
  `npx supercov mutants --diff HEAD~1 --fail-on survived`.
- Expected (`docs/mutation.md`: "--format markdown ... up to 25 surviving
  mutants"): the survivors.
- Actual: the comment says "No mutant is not infected or not reached." while
  the gate fails on 9 survived mutants of the same change. The markdown only
  ever shows weak statuses. "No mutant is not infected or not reached" is also
  hard to read.

### F11. `supercov mutants` has no `--format markdown`, and checks options only after measuring — medium, core

- Steps: `npx supercov mutants --diff HEAD~1 --format markdown`.
- Actual: `Unknown option: --format` followed by the usage of
  `supercov runs <run-id> mutants test`, a command the user did not type,
  exit 2. In `mutants_command` the options are parsed only after the
  measured run, so on a fresh checkout a typo costs a full measured run first.
- Expected: options checked before anything runs, and the refusal showing
  `supercov mutants`'s own usage. A markdown summary of strong verdicts for a
  PR would make the strong gate usable in a comment.

### F12. After a strong pass the text views show only weak results — medium, core

- `runs latest mutants` (table, file, line) and `runs latest` show weak scores
  only; strong verdicts are in `--json` alone. A user who has just been told
  "mutation score 73.9%, survived 112" has no text command to list those 112.
  `supercov mutants` does not say how to see them. The HTML report's Mutants
  card likewise shows only "89.9 % weak mutation score" next to the 73.9 %
  the terminal printed, with no strong figure (documented, but confusing).

### F13. pytest's own output floods the terminal during the strong pass — medium, python

- Steps: itsdangerous, `npx supercov mutants` in a terminal.
- Actual: dozens of interleaved pytest progress lines from the forks
  (`tests/test_itsdangerous/test_url_safe.py ....F..FFFF....  [  1%]`), ending
  in `no tests ran in 4.23s`, before the summary. On Ctrl-C, pytest's
  `KeyboardInterrupt` banner with `supercov_strong.py:803` is printed.
- Expected (`docs/cli.md`: "The command's own output goes to a log beside the
  pass's results; stdout is the answer alone"): a progress line, as Rust and
  Go show.

### F14. The strong pass leaves gigabytes in `.supercov/strong` — high, core/JS and rust

- Steps: any `supercov mutants`; list `.supercov/strong` afterwards.
- Actual: one directory per pass is kept after it finishes: nanoid
  (`node:test`) 1.2 GB a pass (`results.jsonl.jobs/<n>/evidence`, 24 MB a job),
  2.3 GB after the journey; rust-hex 1.1 GB (a switched build). Every edit and
  rerun adds another.
- Expected: work directories removed when the pass ends, as `.supercov/work`
  is for runs.

### F15. Editing one file reruns every mutant — medium, core

- Steps: after `supercov mutants`, append a comment to one source file, run
  `supercov mutants` again.
- Actual: a new measured run and every mutant again (itsdangerous 521, pastel
  1,678, Go 189, Rust 96). For luxon that would be hours.
- Expected: verdicts of unchanged files carried over, or at least a word that
  everything will run again and how long that took last time.

### F16. Source files are not found at the project root or under a `test` directory — high, core and ruby (pre-existing)

- content-type 1.0.5 (`index.js` at the root, tests in `test/`):
  "No application source files were discovered"; it works with
  `SUPERCOV_SOURCE_ROOTS=index.js`. Many small npm packages have this layout.
- rack-test 2.1.0: only `lib/rack/test.rb` is measured ("across 1 source
  file(s)"); `lib/rack/test/cookie_jar.rb`, `methods.rb`, `utils.rb` and the
  rest are left out, so they have no mutants. A directory named `test` under
  `lib/` is taken for tests. Same in 4.28.0.

### F17. Maven: the instrumented copy fails the project's Checkstyle, and the message blames the test framework — medium, jvm

- Steps: java-diff-utils 4.12, `npx supercov -- mvn -B test` (the project's
  `mvn install` passes its Checkstyle).
- Actual: `You have 343 Checkstyle violations`, then "the test run wrote no
  coverage evidence (the command exited 1). Supercov attributes through each
  framework's own lifecycle, so the suite has to run on the JUnit Platform or
  TestNG." The suite is on the JUnit Platform; the build failed on the copy's
  instrumented sources. With `-Dcheckstyle.skip` it works.
- Expected: linters see the original sources (`SUPERCOV_SOURCE_TOOLS` exists
  for this), or a message naming the build failure.

### F18. Maven strong pass: slow, many timeouts, an estimate that grows — medium, jvm

- java-diff-utils (130 tests in 38 s): 315 of 809 mutants in 3 min 15 s, 26
  timeouts by then, and the estimate grew from "about 1m00s left" to
  "about 5m06s left". Not compared with PIT here; the timeouts are worth a look.

### F19. Vitest: "Unhandled Rejection: Channel closed" and the pass starting twice — medium, vitest

- Steps: destr 2.0.5, `npx supercov mutants`.
- Actual: in the strong pass, `⎯⎯⎯⎯ Unhandled Rejection ⎯⎯⎯⎯ Error: Channel
  closed ... ERR_IPC_CHANNEL_CLOSED` (tinypool), and the "instrumenting
  isolated workspace / strong pass: switches in 1 of 2 source file(s) /
  running ..." lines appear twice. The verdicts were complete (141), but the
  error looks like a failure.

### F20. Supercov's own hint for a missing build leads to a failing command — low, core/JS

- Fuse.js 7.0.0: Supercov suggests `supercov -- sh -c "npm run build && npx
  vitest run"`. The copy has no `dist/`, the project's build begins with
  `rm -r dist`, and the command fails at once; Supercov then prints only its
  timings, without the "exited 1 and recorded no test" explanation it gave the
  first time. The workspace line also prints the command unquoted
  (`running in isolated workspace: sh -c npm run build && npx vitest run`).

### F21. `--diff` treats an untracked install directory as uncommitted changes — medium, core

- pastel and rack-test with `bundle config set path vendor/bundle` (common in
  CI): `--diff HEAD~1` says "to the working tree: the run was measured with
  uncommitted changes", because of the untracked `vendor/`. Untracked files
  are added "whole" in that mode, so installed gems could count as the change.
  An `npm install` that rewrites `package-lock.json` does the same.

### F22. Strong verdicts move with a comment edit on a suite with random order — low, ruby

- pastel: 83.6 % before and 83.4 % after appending a comment to
  `lib/pastel.rb`; its RSpec runs in random order and has order-dependent
  failures. Worth a note that random order makes verdicts vary, or a fixed
  seed in the pass.

### F23. `runs latest report --format html` writes 1.2 MB of HTML to the terminal — low, core

- `npx supercov runs latest report --format html` without `--output` prints
  the document to standard output. `supercov report` writes a file and says
  where. Either write a file or refuse without `--output`.

### F24. A score over a few decided mutants reads as a real score — low, core

- rust-hex measured by 4.28.0: "Weak score 46.7 %" from 15 of 104 mutants,
  89 `unknown`; Ruby's weak reports are similar (pastel 1,697 of 1,784
  unknown). The summary does not say how many mutants the score is over.

### F25. `mutation score -` when nothing got a verdict — low, core

- aedile (F3): `Strong mutation for run_…: mutation score -`. Elsewhere the
  CLI writes `—` for no value; better still, say why (all 28 are errors).

### F26. Docs and help text name only JavaScript and Python — medium, docs (help fixed here)

- `supercov --help` said "Test the tests with mutants (JavaScript, TypeScript
  and Python)"; `supercov mutants --help` "lists the mutants of every
  JavaScript, TypeScript and Python source"; `runs <id> mutants --help`
  "StrykerJS (JavaScript, TypeScript) or mutmut (Python)". Fixed on this branch
  (see below).
- Still in `docs/cli.md` (docs owner): the resource table ("each JavaScript
  mutant StrykerJS would make and each Python mutant mutmut would make"), the
  Mutants section ("StrykerJS 10.1.1 (JavaScript, TypeScript) or mutmut 3.8.1
  (Python)"), and "Mutants of other languages are listed `error` until their
  language has a driver", which every language now has.
- `docs/supported-suites.md`, Ruby: "`supercov mutants test`", a command that
  does not exist (`supercov mutants` or `supercov runs latest mutants test`).
- `docs/mutation.md` (on `release/release-docs`): lists Kotest (F3), says the
  markdown shows survivors (F10), and says a runner with no driver is refused
  (F1). The rest of what it promises held: `supercov mutants` measures with the
  probes and reruns nothing on an unchanged checkout; the Mutants card, the
  Files tab's Mutants column; old runs list their mutants.

### F27. `runs <id> mutants test --help` listed `-- <command>` after the accepted-mutants paragraph — low, shared help (fixed here)

## Fixed on this branch

Shared help text that no workstream owns, with assertions in
`crates/supercov-cli/tests/cli.rs`
(`mutants_test_names_each_mistake_in_its_options_before_it_reads_a_run`):

- `supercov --help`: "Test the tests with mutants (every language Supercov
  measures)".
- `supercov mutants --help` and `supercov runs <id> mutants --help` name each
  language's tool (StrykerJS, mutmut, cargo-mutants, Gremlins, mutant, PIT);
  `--mutator` no longer says "StrykerJS mutator".
- `supercov runs <id> mutants test --help` lists `-- <command>` with the other
  options.

The test and `help_and_bundled_guides_need_no_project` pass on Linux CI.
`mutants_test_narrows_shares_and_gates_what_it_asks_about` and
`mutants_options_and_stryker_reports_that_cannot_be_read_are_refused` need the
repository's `node_modules` (`npm ci`) and were not run there; their fault
pins were renumbered for the merged sources by a line mapping, not by a run.

## Not covered

- The Playwright strong pass (blocked by F6) and Vitest Browser Mode.
- Kotlin under Maven, Java under Gradle, unittest, test-unit, Cucumber,
  cargo-nextest, wasm.
- A complete strong pass on luxon, uuid 9 and java-diff-utils (each over the
  ten-minute job limit), so their verdicts were not compared with any tool.
- Windows and macOS.
