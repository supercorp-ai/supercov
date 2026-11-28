# Changelog

## Unreleased

**Added**

- Java and Kotlin mutation testing, fully supported: `npx supercov mutants -- mvn test` (or `./gradlew test`, or an Ant build) lists PIT 1.17.1's mutants of the Java and Kotlin the run measured, decides from the run those no test reaches or infects, and runs the rest inside the build's own test JVM, one test at a time, each mutant in freshly loaded classes, under JUnit 5, JUnit 4 (through Vintage and under Ant's `<junit>`), Kotest, Spock and TestNG, with Maven, Gradle and Ant. Code a test class runs for all its tests (`@BeforeAll`, a `@MethodSource`, a static initializer, `@BeforeClass`, a Kotest spec's body) is credited to each of its tests; each JVM that shares the pass works in a directory of its own; each test runs twice without a mutant before it is a candidate, and each kill is confirmed without the mutant. `supercov runs latest mutants --format pit` writes PIT's XML report, and `supercov runs latest mutants pit <mutations.xml>` joins a PIT report with the run's verdicts. Against PIT on eight public projects (`benchmarks/mutation-jvm/`, `.github/workflows/mutation-jvm.yml`), of 5,937 mutants both tools listed, 5,919 agree; each of the 17 soundness violations is PIT's environment (statics already set up, state an earlier test left, `inline` code, identity-hashed test names, a load-dependent timeout), each checked by applying the mutant and running the project's own suite. The table is in `docs/mutation.md`.

**Fixed**

- A Go strong pass after a Go upgrade no longer type-checks its mutants with a helper the old toolchain built. Supercov builds its type-check helper with the project's `go` and keeps it in `.supercov/cache`, under a name that only its source decided, so after an upgrade (or with another `GOOS`, `GOARCH` or `GOFLAGS`) a pass reused the old build, whose standard library reads the export data of the toolchain that built it, not the one that now writes it. The helper is now kept per toolchain: its version, target and flags, as `go env` says them.
- A strong pass whose `--budget` is spent now ends as a finished pass, in time, not as a crash. A Rust test that left a process behind holding its output open (a `wrangler dev` started in a session of its own, say) could keep the pass reading that output past its stop; a pass that did not end within 8 s of its budget was ended with exit 143 and every sharded CI job showed as failed, though its verdicts were saved. The pass now ends every process a test started, whatever group it moved to, reads a test's output only briefly past its end, and a pass ended at its budget exits `0` (`2` with `--fail-on` or `--critical`, whose gate it could not judge).
- A Rust mutant whose tests all pass without ever running its mutated code is no longer reported as surviving. A Cloudflare Worker that a `cargo test` suite starts with `wrangler dev` is built by wrangler from the sources, not from the pass's switched build, so every Worker mutant "survived" tests that could not fail, some after minutes of them. A switched build's test processes now say when they first evaluate their mutant; one that no process reached is `no-coverage` with the reason `not-reached-mutated`, which says why, and a merge keeps it. Running Worker mutants in their mutated form comes next.
- An AVA suite measured on a machine whose disk is busy with other writes no longer fails with "Timed out while running tests" and "Failed to exit when running …". Supercov turned on Node's compile cache in every AVA worker thread, and Node writes a thread's compiled code as the thread ends: a temporary file and a rename for each module it compiled. AVA stops a run whose workers send nothing for 10 s, and with other writes on the disk each rename took about 0.2 s, so the workers of a two-file suite spent most of a minute ending. On a Linux runner with that load, 20 of 24 measured runs failed where AVA alone passed every time. A worker thread now compiles as it would without Supercov. A process still keeps the cache for the processes after it.
- Jobs of a split strong pass now each run their own mutants, and `supercov merge` holds every verdict they reached between them. Each job dealt its share from its own measurement, by what each mutant's candidates cost and which mutants its plan decided; where a suite covers a little differently from run to run (randomized inputs, timing), the jobs dealt differently, so some mutants ran in two jobs and others in none, and the merged totals came out well under the jobs' totals added up (never a wrong verdict). A mutant's share now follows from its id alone, the same on every machine, decided mutants included.
- Shards run one after the other against the same run, on one machine, now decide every mutant between them. Each shard's share was dealt from the mutants still without a verdict, so `--shard 2/2` after `--shard 1/2` took half of what the first left and left the rest undecided (never a wrong verdict; a later pass decided them). A shard's share is now its share of every mutant of the run.
- A Maven or Gradle project that builds for Java 7 is measured. Supercov's runtime class compiles with the project's main sources, and it used two lambdas, so gson (`<release>7`) failed to compile in the copy ("lambda expressions are not supported in -source 7") and no run was published. The class now uses no syntax or library method newer than Java 7 (`Map.merge` included).

**Changed**

- A strong pass over Vitest Browser Mode tests runs its jobs in a resident Vitest, one per worker, instead of starting the test command, and with it Node, Vitest, Vite and Chromium, for every mutant and test file. Each job gets a new page in a new browser context, as a command's own browser gave it; a job that ran past its limit there runs again as a command before it is a `timeout`, and a project with a global setup, browser commands of its own or a Vite plugin that serves requests itself runs its jobs as commands, as before. On mini-throttle (77 mutants, 4 CPUs) the pass takes 70 s where it took 121 s, with every verdict the same.
- Rust doctests before edition 2024 run a file's worth at a time. Each ran in a rustdoc of its own, so the crate was read once per doctest (168 times for tokio-rs/bytes). The doctests of one file now run in one rustdoc, one after another, each still in a process of its own and named by the line libtest prints as it starts it; a file with one doctest, or a harness the user gives a thread count or uncaptured output, runs each alone as before.

## 4.29.19

**Fixed**

- `supercov runs latest mutants test` on a merged run now runs under its parts' test command. A merged run's own command is `supercov merge …`, so a Rust pass refused it ("does not expose a stable Cargo invocation") and a JavaScript pass ran `supercov merge` as its tests, every mutant an `error`. Parts that ran one command give it; Cargo parts that differ only in the test names after `--` give it without those names; parts of different commands are refused, naming them, with `-- <test command>` to give one.
- A test the measured run skipped (`#[ignore]`, `test.skip`) is no longer counted among those that did not pass. Named after `cargo test -- --exact`, an ignored test was recorded as `passed: false` and nothing else, and when the command failed for another reason the strong pass said it "did not pass" ("12 of its 1558 test(s) did not pass … cannot gate", 10 of them ignored). Its record now says `skipped`, and the pass counts only tests that failed.
- A strong job that ran past its limit beside others and runs again alone has a grace of its own, rather than the pass's, before it is a `timeout`.

**Changed**

- A server under measurement rescans Node's module cache only after a load that can add to it. Every built-in load (a Next server requires `node:path` on nearly every request) made it list and check every key of the cache again.
- `docs/mutation.md` says how a merged run's mutants take their parts' command and that a skipped test is recorded `skipped`, and that Rust has no equivalent-mutant detection yet (accept such mutants in `supercov.accepted.json`). The Rust benchmark's base64 and fastrand rows are those of 2026-11-27, joined as far as each job's time allowed (base64 305 of 619, fastrand 211; no soundness violation), and cargo-mutants runs under a 10 GiB address-space cap there.


## 4.29.18

**Fixed**

- A Rust strong pass interrupted by Ctrl-C no longer saves the mutants whose tests were running as `killed`: the signal ended each test process, and the pass read that as a failing test. Each test process now runs in its own process group; the pass alone hears the signal, ends the groups, judges nothing they said, and exits 130 at once; the next pass runs those mutants.
- `--budget` now ends a Rust strong pass in time, as a finished pass. The pass ignored the stop and ran until the command ended it 8 s past its budget with exit 143, leaving its test processes running. Once the budget is spent, it starts nothing more, ends what it runs, and leaves its undecided mutants for the next pass: on a fixture, 0.3 s past a 32 s budget, exit 0.
- A Rust strong pass no longer calls a mutant a `timeout` because the machine became busier after the baselines ran. A test past its limit now runs again alone, after a run of its test without the mutant, with a limit of 5× the slower of that run and its baseline, at least 20 s; only past that is the mutant a `timeout`.
- A Rust test's time limit with a mutant is now cargo-mutants': 5× its baseline, at least 20 s (it was 3× + 10 s). On fastrand 2.3.0 one mutant was a `timeout` at 14 s where cargo-mutants' test passed.
- A deleted arm that leaves a Rust `match` whose remaining arms all diverge is now built on its own, and `unviable` where the `match` no longer type-checks, as cargo-mutants reports it.
- A Rust test process that could not be started now makes its mutant an `error`, not `killed`.
- On Windows, a Rust doctest's kill and its file are now named as on every other platform (`src/lib.rs`), so the next commit's pass keeps or reruns its verdict as it should; and a compiler error in a standard-library macro (`/rustc/…`) is no longer read as a workspace file.
- A Rust run measured with the value probes no longer fails tests by evaluating literal arithmetic out of place: in `n < 1 << 83` (`n` a `u128`) the probe evaluated `1 << 83` as an `i32`, which overflows. On itoa 1.0.15 this made 33 mutants `no-coverage` that cargo-mutants catches. An operand of literals alone now has no probe.
- A Rust mutant in a `const fn` called at run time is no longer `no-coverage`. The instrumenter cannot probe a `const fn`'s body, and the mutant read as code no test ran (12 mutants in bitflags, base64 and regex-syntax that cargo-mutants catches). Such a mutant is now linked to nothing and run.
- A Rust mutant no test reaches, or none infects, that does not compile is now `unviable`, as cargo-mutants reports it, rather than `no-coverage` or `survived`. The strong pass switches those into its build only to check they compile, for packages whose tests it runs, and `supercov merge` keeps that verdict. A compiler error inside a macro's expansion is read at the macro's call site.
- A Rust operator mutant that makes its expression group otherwise now runs as cargo-mutants' textual mutant does: `a - b * c` with `*` made `+` is `(a - b) + c`, not `a - (b + c)`. Such a mutant is switched around the whole regrouped expression; on regex-syntax 0.8.5 one survived Supercov's pass and was caught by cargo-mutants'.
- A Rust function whose return type holds an `impl Trait` deeper inside (`Result<impl Iterator<Item = u8>, E>`) now has its body mutants built on their own, so they are no longer wrongly `unviable`.
- A deleted arm of a `match` in a Rust `const fn` is no longer `no-coverage`: its branch is in the run's record and never taken, as the rest of a `const fn`'s body is, and the mutant is now linked to nothing there either.
- A Playwright strong pass no longer calls a mutant a `timeout` or a kill by how busy the machine was. An attempt is stopped at its limit (1.5 × its test's baseline + 5 s). An assertion that started late on a loaded machine, and would fail by its own timeout (an `expect` given 5 s or 12 s), ran past that limit there and was stopped as a `timeout`; on an idle machine the same assertion failed inside the limit and killed the mutant. Now, where every call the test is waiting on started before the limit and ends by a timeout of its own before the test's own timeout, the attempt runs until those calls end, and the test's outcome decides. A call with no timeout of its own, a real hang, still ends at the limit. The mutation score was already unaffected, since a `timeout` counts as detected; the label is now the same on every machine. On a fixture whose route answers three seconds late under the mutant, each of the four mutants that take that route is killed by its assertion past the limit, where all four were `timeout`s.

**Changed**

- Rust mutation testing is no longer in preview: budget and signals stop it in time, partial progress, timeouts judged at the machine's speed, verdicts carried across commits, mutants that compile as cargo-mutants' do, and unviable mutants found wherever they are. Against cargo-mutants 27.1.0 on eleven public crates, of 3,844 mutants both tools decided, 3,821 agree and none is detected by one and let live by the other; the 23 others are lint caps and a `vec!` case fixed here. The benchmark is `benchmarks/mutation/rust/`; the table is in `docs/mutation.md`.
- A missing macro (`vec!` in a `no_std` crate) in a Rust mutant's own code makes it `unviable`: rustc gives that error no code, and the pass read it as the switch's.
- The Playwright collector registers a test's page once. The suite's `page` fixture hands the test a proxy of a page the collector had already registered when its context opened it, and registered by its proxy as well, every page and its context were known twice: each action and assertion told the page and the app's cookies its phase twice, each context took Supercov's runtime and the test's scripts twice (and failed to expose its binding the second time), and each page's evidence was read and written twice. On the generated large Next.js app (300 tests in sixteen parts), the Playwright worker made 30 of Supercov's calls per test instead of 60, and the measured run retired 29.7% more instructions than plain where it retired 32.1% more, and took 26.5% more CPU time where it took 28.3%. Every answer a run gives is the same, save one correction: the summary's count of browser events placed by their phase (`attribution.browserExplicit`) counted the events of a page's duplicate snapshot a second time, and now counts each once (4,708 → 3,648 on the deterministic browser bench).
- npm's own process (`npm run`, `npx`, `npm exec`) no longer writes the compiled code of the hundreds of modules npm loads to the run's compile cache. It runs none of the project's code, and the commands it starts keep the cache. On the generated large Next.js app, its three launchers used 24 ms of CPU per test over plain where they used 14 ms.
- Supercov's Docker relay waits for a connection instead of looking for one every 10 ms. Most runs never use it, and its hundred wake-ups a second were about 60 ms of Supercov's own CPU on a half-minute run (6% of it on a part of the generated app).
- A test's record takes each page's evidence as the text the page wrote, rather than parsing it and writing it again; `tests/runtime/snapshots.test.mjs` pins the record to its `JSON.stringify`.
- `supercov mutants` on a new commit now keeps Rust verdicts no change could move, as it does JavaScript's: a Rust test is bounded by what its own function can name, including trait implementations by their type's or trait's name, and gives the bound up where attributes, name-building macros or top-level changes make it unsafe. On a fixture carried across seven commits, every kept verdict was the one a pass from nothing reaches (`docs/design/rust-mutation.md`, "After an edit").
- A Rust strong pass records each candidate a mutant passes as it passes it, so a pass stopped by its budget or a signal leaves the next pass only the rest of that mutant's candidates (`N mutant(s) resume where earlier passes left them`), as Playwright's pass does.

## 4.29.17

**Fixed**

- A strong pass over node:test, Jest, Mocha, AVA, node-tap, bun:test or Rust compiled to wasm no longer calls a mutant a `timeout` because the machine became busier after its baseline ran. A job stopped at its limit with no test failed now always runs again alone, after its tests run alone without the mutant, and its limit there is 3× the slower of that run and its baseline, + 10 s; only a job past that is a `timeout`. Before, only a job that had run beside others ran again, still judged by a baseline measured on a quieter machine: on a loaded CI runner, a kill and a survivor of a pass over Rust compiled to wasm came back as timeouts. A true timeout now costs one more run of its tests without the mutant.
- A pass that its budget or a signal stops no longer saves a verdict judged from a Vitest copy or a job the stop cut short. Once the pass begins to stop (`stopped-at`, which the Playwright pass already heeded), the Vitest driver and the driver of jobs write no verdict, and the mutant is left for the next pass. On a macOS runner, the budget fell while a mutant's test waited, that copy's process ended before the pass's own, and the mutant, whose test waits five minutes, was saved as `killed`.
- A command told to stop (a strong pass's budget, a signal, `SUPERCOV_COMMAND_TIMEOUT_MS`) no longer leaves behind a process group it started apart from itself, such as Playwright's web server. Such a group is told to end with the command, and where the command ended at once and the group took its time over the signal (a measured server writes what it measured first), Supercov returned while the server still held its port, and the next pass on a loaded machine stopped at once: `http://127.0.0.1:…/health is already used`. Now Supercov waits for the groups it told to end, up to the same grace the command gets, and ends what is left of them before it returns.
- A Playwright strong pass no longer calls a mutant every candidate kills an `error` because a loaded machine left its tests unrun twice. With tests side by side, a worker with nothing it could take yet ran through a test's copies while another worker's attempt ran, and the round ended with mutants still waiting for that test; a `rest` round that then decided nothing made them `error`s ("its tests did not finish, in two rounds"). Now such a round's mutants run one test at a time first (`ran one test at a time: not every candidate test ran in two rounds of tests side by side`), and only what that round leaves is an `error`. A fixture with two tests side by side, each killing both of its mutants, called one an `error` in 1 of 50 runs on a loaded runner.
- `tests affected` no longer panics on a Rust or Ruby helper that only gained functions, which its caller took as a map that cannot answer, selecting every test. Telling an addition from a change was written for JavaScript and TypeScript, and Python, but was also asked of the other languages Supercov reads, and read them as JavaScript. Elsewhere an added function can change what code written before it does (a Rust `fn` shadows what a glob import brought in, a Ruby `def` redefines a method of a class opened elsewhere), so a Rust, Ruby, Go, Java or Kotlin file that gained functions is now a change, and reaches the tests that may load it.

## 4.29.16

**Changed**

- A server under measurement writes a test's first hits and decision vectors about three times faster, the same bytes in the same order. Each turn's lines go through the attempt's file kept open, where the file was opened and closed for every turn that recorded something (a Next.js page renders over hundreds of turns), and each line is built directly rather than through the objects that carried it there; the evidence root is read once per environment. On a benchmark of 60 requests each running 12,000 points, recording took 0.7 s of wall time and 0.9 s of CPU against 2.2 s and 2.5 s, and the files were equal byte for byte. A file removed while the server holds it is opened again; on Windows, where a held file cannot be removed, each turn appends as before.
- The Playwright collector reads a page's evidence as the columns it stores. It took the page's events as objects, wrote them compactly, read them back as objects and made columns of them again: 26 ms for a page of 30,000 events at the end of each test, now 7 ms, and the same JSON.
- What a measured browser suite's time goes on, and why some of it stays, is in `docs/performance.md` ("Where a browser suite's time goes"). `scripts/speed/answers.mjs` writes down every answer a run gives, so two runs can be compared line by line, and the `speed` label's benchmark now compares its binaries' answers as well as their times ("Prove a speed-up changes nothing"). On a Next.js app's Playwright suite of 170 tests, main's own code ran beside the new code in the same run and every record of all 169 attempt files (280,000 lines) and all 387 page snapshots came out equal; two fixtures' runs gave the same answers, every line, test, decision and evidence record, as main's.

- `supercov mutants` on a new commit runs only the mutants whose verdict could have changed since an earlier strong pass of the same command, dependencies and configuration in `.supercov/runs` (a restored baseline among them): nothing to set up. A verdict is now found by its mutant's code rather than its line, so a line added above a function keeps its mutants' verdicts (each verdict's anchor is saved with it, `anchors` in `strong-mutation.json`), and a change to another function of the same file no longer runs the file's mutants again. A kill is kept where the test that killed it is still a candidate and runs as it did; a survival where every candidate now ran with it then and runs as it did; a timeout runs again, its time limit being the new run's. The pass's summary names each run it kept verdicts from with its commit and what reaching them took (`26 verdict(s) kept from run r5 (commit 1a2b3c4), which took 25 test run(s) to reach`), says why the rest run again, and `runs <id> mutants` says where each kept verdict was reached (`carried from run r5 (commit 1a2b3c4)`; `--json`: `carried.runs`, `carried.testRuns`, `strong.carriedFrom`). On a fixture's eight commits touching code, tests, a helper and configuration, carrying forward ran 72 mutants where passes from nothing ran 201, every verdict the same; on two pairs of consecutive commits of unjs/ufo, it kept 684 and 560 verdicts and ran 226 and 353 mutants where passes from nothing ran 878 and 886, all 979 and 987 verdicts the same.

**Fixed**

- A verdict kept after an edit could be wrong where the edit changed a function the mutant's tests never ran without it. A mutant that changes a branch can send a test into code it never ran, so a test's record without the mutant does not bound what it runs with it: in the fixture, `refund(20)` never calls `audit`, a mutant of `refund`'s condition sends it there, and once `audit` changed two such survivors were killed, but 4.29.15 kept them. A test now runs as it did only where nothing changed that it could run with the mutant switched on: for a JavaScript or TypeScript test, nothing its imports load and its code can name (`strong_reach.rs`); for a Python or Ruby test, no code its imports load; for any other, no code but tests'. `docs/design/strong-mutation.md` ("After an edit") has the argument.
- A Playwright strong pass no longer calls a mutant `survived` without running it once. A survivor of the shared server that no attempt ran (its candidates' requests reached its code without carrying its mutant) is sent alone, and its passes on the shared server were still recorded as its progress; where the budget stopped its round alone, the next pass found every candidate passed and called it `survived` (`tests: 1, durationMs: 0`), on a large application's e2e suite a mutant another run killed when it ran. Such passes are no longer progress: a pass stopped before the mutant runs alone leaves it with no verdict and nothing passed, and the next pass runs it again, on the shared server and then alone. A mutant left unfinished that no attempt has run yet is treated alike, and its next round runs every candidate, not only those left, so that what goes alone is all of them. And a round alone that the budget stops is no longer read as one whose command failed with its mutant switched on, which made a `killed` of an error that came after the stop.
- A Playwright strong pass no longer stops an attempt as a stalled `timeout` while its page could still end the wait. The stall check took a page quiet for a second past the test's window as unable to end the wait before the limit, but a page that polls on a timer is quiet between polls: on a large application's suite, an attempt waiting on `Page.waitForResponse` was stopped at 12.4 s, past its 12.25 s window, where the page's next poll at 12.5 s would have answered it and the test passed before its 16.3 s limit (another run said `survived`). Each document now keeps what it has yet to do (an init script, before the page's own scripts: its timers, animation frames, idle callbacks and sockets), and a stall stands only where nothing is due before the limit in any frame, no request is unanswered, no WebSocket is open and no worker runs. Otherwise the attempt goes on, and runs to its limit if nothing ends it: such a timeout's verdict says `it stalled at 2.5 s, with 1 unanswered request(s) that could still end the wait, so it ran to its limit to confirm`, each attempt's line says so, and the summary counts them (`2 of them confirmed by running to the limit`; `timeLimit.confirmedAtLimit` with `--json`). On the strong-mode Playwright fixtures (99 mutants), two verdicts moved by this fix, both on a page that polls every 4 s: the mutant that leaves its first poll unanswered, and the one that removes the first poll, now `survived` (they were stalled `timeout`s, though the test passes before its limit). The stalls on pages with nothing left to do still end where they did: the 17 stalled timeouts of the fixtures that held 4.29.15's stall check save the same 68 s. Where a page keeps something going until the limit (a request its server never answers), the timeout now costs its whole limit again.

## 4.29.15

**Fixed**

- A Playwright strong pass no longer calls a server mutant `survived` when its code runs in a Next.js route's `after()` for a request that named no test, such as a webhook a mock payment provider sends while the test waits for its effect. Next.js loads a route's module while serving the first request to it, and the handler wrapped then took that request's attempt as its owner: every later request naming no test ran as that attempt's, in a main round with the mutant of the test's first run, none, so the mutants of the work survived though the test fails with each. And the route's web `Request` carried no lifetime, so its `after()` work stayed the request's. Now a handler wrapped while a request is served has no owner, and takes the lifetime of the request being served: the webhook is a request no test named, and the work after its answer is the server's own, run alone with the server started with the mutant switched on. On a large application's e2e suite, 82 survivors of code a webhook route ran in `after()` had one candidate, a test that waits for that work's effect, which kills the two of them applied by hand. A Next.js gate (`scripts/next-after-webhook.mjs`) holds it.
- A Playwright strong pass trusts a survivor of the shared server only where an attempt of it ran it. A server's switch notes when an attempt's own mutant runs; a mutant its candidates' servers ran when measured, which lived through every candidate and which no attempt ran, runs again alone with the server started with it switched on, and that verdict is its own (`ran alone, with the server started with it switched on: its candidates passed on the shared server, yet no attempt of theirs ran it there: ...`). The pass warns on its output when it sends survivors alone, and when alone kills them, naming them. A worker process the first request starts, which carries that request's attempt, was another way to miss: a fixture test holds it. `docs/design/strong-mutation.md` has the argument.

**Changed**

- A Playwright strong pass keeps a worker after a mutant's attempt fails its test or times out, instead of letting Playwright end it and start the next test in a new worker with a new browser. The failure is the mutant's, in the app it was switched on in; a mutant that ran in the worker's own process still gets a new worker, a control (no mutant) that fails still ends its worker, and every kill is still confirmed by its test's next control on the same server. On the fixtures, the main pass of one started 2 worker processes instead of 33 and the serial round 1 instead of 31; the three strong-mode Playwright fixtures ran in 271 s instead of 305 s, with the same verdict for each of their 69 mutants.
- A Playwright strong pass ends a mutant's attempt as a `timeout` once it has stalled, rather than at its time limit: past its window (the test's baseline × the timeout factor + 1 s, longer than any run of the test that passed), the newest call the test made on Playwright's objects has not returned, no call still running can end by its own timeout before the limit (a click with no timeout, an `expect` given 15 s), and its pages have sent and answered no request for a second. An `expect` that gives up before the limit still fails the test, a kill, and a page that keeps asking its server runs to the limit as before. Most of a timeout's limit was the 5 s constant: on 8 shards of a large application's e2e suite, timeouts took 8.3 s each against a 2.2 s baseline, 16% of the mutant attempts' time, while 5,831 attempts that passed took at most 0.5 s past their baseline; ending each at its window saves up to 48% of what timeouts cost, 8% of a round. On a fixture, a click on an element the mutant keeps from appearing ends after 2.5 s of its 6.5 s limit, and an `expect` given 60 s in a 3.4 s test after 6.2 s of 10.2 s; the survivor a timer in the page wakes, a kill by a 4 s `expect` that gives up after the window, and a polling page's timeout at its limit are as before. The window counts from the test's own first call, so a slow page fixture in a new worker on a loaded machine is not taken for a stall, and the watch ends with the test's body, so nothing its fixtures' teardown closes is either. The verdict says where it stopped, on what, and where in the test (`stopped at 2.6 s of its 6.5 s time limit, ... waiting on Locator.click (#save) at tests/save.spec.ts:12:20 ...`), a timeout at the limit says what it last waited on, each attempt's line says either, and the summary counts the two apart and says what ending them early saved (`32 timeout(s) ended early once they stalled, saving about 6m05s`; `timeLimit.stalled` and `timeLimit.stalledSavedMs` with `--json`).
- `supercov merge` reads, merges and keeps once the runs made from one run without measuring whose archives are alike byte for byte: the jobs of a continuation round, each made from the same baseline. Each of them carried the baseline's whole archive and its runs' records, and a merge read every copy whole side by side and kept each copy's records under `parts/`. Eight such jobs of a large e2e suite (232 MB each) merged in 100 s with a peak footprint of 24.7 GB, killed for its memory on a 32 GB machine whose disk is in memory, into a run of 1 GB that each job of the next round then carried. Read once, they merge in 12 s with 12.5 GB, into a run of 244 MB, with the same coverage and strong verdicts. The merged run lists the first among its runs and says which were its copies (`7 run(s) hold run r1's evidence byte for byte, made from one run without measuring (...)`); each copy's strong verdicts are kept as any run's.
- A strong pass proves JavaScript and TypeScript mutants equivalent by their compiled code, and runs mutants that compile alike once (Trivial Compiler Equivalence, Papadakis et al., ICSE 2015). Before it runs anything, it compiles each mutant's file with the mutant in place, and the file as measured, with one pinned optimiser (oxc's minifier 0.133.0, built in, every option that trades exactness for size off), and compares the outputs. A mutant whose file compiles to the original's code is `equivalent`: not run, not a survivor, out of the score, with the reason `compiles to the same code as the original` wherever it is listed, and folded into a section of its own in the markdown and HTML reports. Mutants of one file that compile to the same code as each other are one program: the first runs, and the others take its verdict (`same-code-as:<id>`, `tests: 0`). Nothing to set up: the pass says what it spared and the test time saved (`--json`: `sameCode`), and a file it cannot compile is named and its mutants run as before. Of the 33,141 mutants of nine open-source TypeScript code bases (five unjs libraries, hono, tRPC, and two Next.js applications), 20 compile to the original (0.06%; 0.7% of one Next.js application's: emptied `if` blocks whose body did nothing, a fall-through `case` label) and 2,112 to another mutant's (6.4%, mostly an `if`'s test made `false`, its block emptied and its one call removed); compiling all of them takes about 3 s. On the strong passes of the five unjs libraries and of hono's JSX renderer, 269 of 4,258 mutants (6.3%) were not run, and running each with a build without this change gave the verdict it was given: every equivalent one survived every test, and every duplicate had its twin's verdict. The semantically equivalent mutants a person triages (`.select('')` for `.select('*')`, optional chaining on a value never null) are not what a compiler can prove, and still run.

## 4.29.14

**Changed**

- `supercov mutants` measures nothing where a run of the checkout as it is already holds the tests its command runs: a merge of the suite measured in parts on several machines (a weekly baseline restored into `.supercov/runs`), or a run of the whole suite. The pass's run is made from that run with those tests' records alone, as measuring them would record, with or without `--shard`, `--file` or `--diff` (`not measuring: run r4 of this checkout holds the tests the command runs (18 test(s) in 6 of its 61 test file(s)) ...`). The command's tests are every test of a merge whose runs each measured the command, and otherwise those of the test files it names (`tests/e2e/billing.spec.ts`, `billing.spec.ts`); a command that names none is measured, as is any checkout the baseline is not of, such as a pull request's. Before, every pass's job measured its specs first, about 7 minutes of each job on a large application's e2e suite. On a fixture suite (three spec files, two parts merged), a pass made from the merged run gave the verdicts a pass that measured first gives, measuring nothing.
- A merged run answers alone. It keeps what each run it was merged from answers with (its metadata, its record of what each test ran, and the entries of its archive that say what its tests loaded) in its `parts/` directory, a merge of merges what theirs do, so `tests affected` of a merged run, and the check of it against the checkout, no longer need its runs in `.supercov/runs`: a merged baseline can be cached and restored by itself. Before, with its runs gone, it answered `merged from run_…, which is no longer stored`; a merge made before still does, and says to merge the runs again.
- `mutants --format markdown` is a pull request's comment a CI can post as it is: each mutant's row says how many of the run's tests ran its line; a change's comment names its lines of code no test ran, file by file; survivors that only change a log line are folded in a `<details>` of their own, and so are the mutants `survived so far`, counted apart in the verdicts' line. `mutants --json` gives each mutant's `lineTests`, and `diff.unrunLines`.
- A mutant `survived so far` is an outcome of its own in every report: `survived so far` in the markdown and HTML reports, `Pending` in StrykerJS's `mutation.json`, `STARTED` in PIT's `mutations.xml`. It was counted as an error.
- A mutant in the arguments of what makes a logger only changes a log line (`logOnly`), as one in a logging call's does: `createLogger(...)`, `getLogger(...)` (log4js, winston, bunyan) and `pino(...)`, called alone or as a method, and `.child(...)` of a logger (a receiver named like one, or a logger such a call made). Replacing a logger's context object (`createLogger({ domain: 'integrations' })` → `{}`, `logger.child({ request })` → `{}`) was listed among the survivors to fix.
- `tests affected` of a Playwright run no longer takes another runner's tests and helpers to be files every test may load. A changed file no test's import places affected every test of the run; on a large application's e2e suite, a pull request that changed an integration test or its helper selected every e2e test. Each Node process of a Playwright run now notes what it loaded and read, the test runner's and its workers' as much as the servers', and such a file affects the run's tests only where one of them loaded or read it, or where it is in the tree of a configuration of a runner the run used (`tests/e2e/` for `tests/e2e/playwright.config.ts`). A run recorded before takes it as before.
- `tests affected` reads a JavaScript or TypeScript source added since the run through the imports as they are now: it affects the tests whose own file, or a file they ran code in, imports it, directly or through other files (`src/new.ts added since the run (imported by src/app.ts, which this test ran code in)`). One nothing of theirs imports is said to be of unknown reach (`unknown, new file`) where before it was not mentioned and selected no test; `--json` lists each under `newFiles`.
- A Playwright strong pass's first tier ends at 32 candidates or once its candidates take 60 s of their tests' measured time (at least four candidates), whichever comes first; the time grows, as the count does, where the run's kills come later. In an area of a large e2e suite whose tests take 6 to 14 s each, two rounds left 852 mutants resuming with about 6 candidates passed each, none at a 32-candidate tier: three or four more rounds just to label them. Of 3,972 kills of that suite, 95% came within 61 s of a mutant's candidates in tier order; a tier of 32 candidates or 60 s holds 93.4% of them where 32 candidates alone hold 93.7%, and costs a mutant 34 s of its tests on average where 32 candidates cost 40 s. Where a test takes 10 s, the tier is 6 candidates. `survived-so-far` means the mutant passed its first tier (32 candidates or 60 s), and the pass's summary says so; the run marks each such mutant (`lived` in `strong-mutation.json`), as a tier ended by its time has fewer than 32 candidates.

**Fixed**

- A strong pass's job (Jest, node:test, Mocha, AVA, node-tap, bun:test, Vitest Browser Mode, and Rust compiled to wasm under them) that runs past its time limit while other jobs ran beside it runs again alone before its mutant is a `timeout`. The limit comes from the file's baseline, which ran alone, and a job beside others may have spent its time waiting on them: where each job builds Rust compiled to wasm, every job waits on Cargo's lock on the build directory in turn, and on a loaded machine a pass of 7 mutants made a survivor and a kill `timeout`s. A job past its limit alone, or one whose mutant passed its hit limit, is a `timeout` as before; the pass's summary counts the jobs run again (`alone`).
- A command told to stop (a budget spent, Ctrl-C, `SUPERCOV_COMMAND_TIMEOUT_MS`) no longer leaves behind a process group one of its processes started after the signal. A Playwright runner that outlives the signal (a listener of its own takes it) started its web server all the same, in a group of its own, and the stop that ended the runner left the server on its port: the next pass said `http://127.0.0.1:…/health is already used`. While the command ends, its process tree is looked over again every 100 ms, and each new group is told as the command was and ended with it.
- Runs of one suite measured in parts on several machines merge again where each machine's harness writes its own settings files, which the suite's servers read: `tests/stack/.env.local` written by the measured command into the copy it runs in, with each machine's ports and keys. Such a file was recorded as an input of the run, so the parts read as measured on different files, and `merge` refused them, `--allow-changed` or not (`tests/stack/.env.local changed, and 1 of its tests reached it through a server that served them`). The refusal is as old as the record of what servers read (4.12.0), not new in 4.29.13: 4.29.12 refuses the same parts the same way. A file a server read that the command itself created or changed during the run, and that git does not track, is now left out of the run's record, and the run says so (`1 file(s) the servers read were written by the command and are not tracked, so they are not inputs of the run`). A tracked file the command rewrites, such as a mock's data it saves, is still an input. Parts recorded before must be measured again to merge.

## 4.29.13

**Changed**

- A Playwright strong pass's first tier takes first the tests that killed most of the run's other mutants of the mutant's file: a page component's or a shared service's tier no longer fills with the cheap tests that only render or pass through it. On a dashboard page's component of a large application's e2e suite (572 mutants, about 187 candidates each), a first round left 251 `survived so far`, most of them killed by the page's own spec files past the first 32 candidates. From the suite's kills, with those the first tiers made as the run's history, 58% of the mutants a first tier left to its rest have their killer in the next 32 candidates, against none before; over every kill, a tier of 32 holds 98.8% against 93.4%, and for mutants with 100 or more candidates 89.5% against 56.0%. The run's kills come from its earlier passes and the merged rounds before it, so a run's first round is as before. Measured on three page components of the same suite (3,458 mutants, 61 spec files), a second round decided 408 more mutants after the first's 806, mostly kills (416 to 738), and left 22 `survived so far`.

**Fixed**

- `supercov mutants --shard` continues a run whose command failed only because tests failed, as it continues one whose command passed: it saves into a run made from it and measures nothing. A failed test is no candidate in any run, so the verdicts are the passing tests' alone. Before, one timing-sensitive test failing while a large e2e area was measured (1 of 1,456 Playwright tests, unrelated to the mutated files) made each of the 8 shard jobs that restored the run measure the whole area again, about 8 minutes each, and decide nothing. The shard names what it leaves out (`continuing from run r4, whose command failed: 1 test failed while measuring and is not a candidate: checkout > pays`; `measurement.failedTests` with `--json`). A run whose command failed otherwise -- it ended without an exit status, recorded no test, or exited non-zero with no test failed -- is still not continued; where no run can be, the shard measures and says what kept the newest one out, naming its failed tests. A run whose command passed is still taken first. `docs/design/strong-mutation.md` has the argument.
- A mutant is `survived-so-far` only once it has passed a first tier's worth of candidates (32). A pass its budget stopped partway through a tier keeps the few candidates each open mutant passed, and those mutants were listed as `survived-so-far` too: on a settings page's component of a large e2e suite, every one of them had passed 1 to 4 of its 135 candidates, and the page's own specs, never run with them, killed 176 of the 317. They are open, as a mutant a budget left is, and the next pass resumes them.

## 4.29.12

**Changed**

- `tests affected` reads what a Rust test loads: the files and directories it names by path, in its own function (or its entry of a macro that declares a test per entry) or anywhere in its file outside another test, the module files of an integration test, and what a script it names imports and names in turn (`tests/fixtures/{name}` names `tests/fixtures`). A change to a fixture or a script affects the tests that name it; before, any file outside the measured source affected every Rust test.
- A file measured Rust source embeds with `include_str!`, `include_bytes!` or `include!` is part of the declaration that embeds it: a template or a script compiled into the binary affects the tests that ran that code, not every test.
- In a Rust run, a change to a project file that is neither Rust source nor a manifest -- a fixture, a script, documentation -- no longer reads as a configuration change that affects every test: it is a helper, judged test by test as above. Only Cargo's own configuration (`.cargo/config.toml`, `rust-toolchain.toml`, `.config/nextest.toml`) still affects every test.
- A change confined to a Rust file's test code (its `#[cfg(test)]` modules and `#[test]` functions) affects the tests in that code alone, not every test that ran the file's other code.
- `tests affected --names` prints a Rust test as its path in its crate (`tests::it_works`), which `cargo test -- --exact` and nextest's `test(=...)` match, instead of the record's `file::name`.

**Added**

- A Playwright strong pass runs a mutant with many candidate tests against its likeliest 32 first (its first tier: candidates whose own evidence ran its code first, then the cheapest to a kill), every such mutant before any runs the rest. One the tier lets live is `survived so far`: kept as the candidates it passed, never a verdict, not in the mutation score, listed by `mutants --status survived` as such (and alone by `--status survived-so-far`); the pass runs the rest where it has the time, the next pass where it does not. The tier grows where the run's own kills come later. On a large application's e2e suite, 93.5% of 3,448 kills came from a mutant's first 32 candidates, and a first tier costs 513 hours of test time where deciding every survivor costs 7,253: code every test runs (an auth middleware's mutants have about 1,170 candidates each: 39 minutes of test time to survive, 7 seconds for a first tier) gets its likely kills and its `survived so far` for a fourteenth of the time. A mutant a server must start with runs its tier in its round alone, and resumes from it in the same run. Measured on an area of the same suite whose code every test runs (sign-in, onboarding and promotions: 792 mutants, up to about 1,200 candidates each, which no pass could afford before), one round of 8 jobs of 16 CPUs with about 406 s each decided 760 of the 792: 128 killed, 200 timeouts, 176 survived, 173 with no coverage, and 83 `survived so far`, with 32 left. `docs/design/strong-mutation.md` has the measurements.
- `tests affected` names changed code no probe of the run measured -- another platform's, a feature the build left off, a `const fn` -- under `unmeasured` (`--json`) and in the text: no test's record can say what it reaches.

**Fixed**

- A Playwright strong pass no longer calls a server mutant `survived` when its code runs only in work a request leaves running after its answer: a cron route that answers at once and leaves its pipeline running (a promise no one awaits, `after()`, a timer), or a poller the server's first request started, in which later requests' batches are polled and read. Such work kept the context of the request that started it, so it ran that attempt's mutant (in a pass, usually none) whichever test waited for it, and the measured run credited it to that one test. A request's work now ends once its response has finished and that event's listeners have run: what it left running is recorded as the server's own work, every test is a candidate, and the mutant runs alone with the server started with it (`ran alone, with the server started with it switched on: its code ran in server work no request was waiting for`). What a request does before it answers is unchanged. A fixture test holds both kinds of work. `docs/design/strong-mutation.md` has the argument.
- A strong pass with `--budget` is ended at most 45 s past its budget, whatever holds it up, by a deadline of its own: the stop at the budget was made by the progress loop alone, and on a large e2e suite's area every job of a sharded pass ran 130 s past its budget and was killed by the job's time limit with nothing saved. The pass now also says when its budget is spent (`[supercov] budget spent, 452 s into the pass: stopping it; what it decided is saved into the run`), and a process ended past the deadline says so. The verdicts were saved into the run as they came.

## 4.29.11

**Changed**

- A Playwright strong pass on a large e2e suite decides more mutants in each round. On one area of a large application's suite (2,707 mutants, a pass split across 8 jobs of 16 CPUs with about 463 s each), the first two rounds after measuring decided 850 and then 1,138 mutants, where 4.29.10 decided 613 and 956: 39% more after the first round and 19% more after the second. The first three changes below make that difference; the fourth, measured on its own, shortens each job's start.
- A mutant whose code a server ran for requests that named no test, sent while several tests ran (a webhook a mock server sends), has for candidates the tests running when those requests ran it, not every test of the run. Only requests the run marks as sent while a test of their server ran count. A test that was not running then neither sent the request nor saw its answer, and its own evidence never ran the code. Where any of that work was a server's start or a timer, kept no time, came while no test of its server ran, or also ran outside the servers, every test stays a candidate. On a large application's e2e suite, a billing area's 515 mutants that a payment provider's webhooks reached went from all 450 candidate tests each to 90 at the median: the area's candidate runs from 264,936 to 79,891, and what its survivors cost from 142.5 to 45.8 hours of test time. Most of them run alone with the server started with them, against every candidate. `docs/design/strong-mutation.md` has the argument.
- A Playwright strong pass runs a test without a mutant only after a failure. A test's first run in each invocation used to have none (its baseline on that invocation's server); it now takes a mutant, and an attempt that passes needs no run without it. A kill is confirmed by the control after it, as before. An attempt that failed before any control of its test passed on its server (a cold server, whose first request to a page may fail where the next does not) decides nothing: the control runs, and the test runs the mutant again, warm. On a large application's e2e suite, the runs without a mutant were 11% and 31% of a job's slot time in a billing area's third and fourth rounds; they now go to mutants. A cold failure's line says it runs again (`fails (2.1s, before any control here: runs again after one)`). A test's first run still goes without a mutant where the mutant it would take has not lived through any candidate yet, in this pass or an earlier one: such a mutant fails its first candidate about half the time, and on the same suite every cold failure failed again, a timeout costing its limit twice.
- A Playwright spec file with a serial describe that a pass runs in every slot of a harness (since 4.29.8) takes, in a slot the harness did not give it to, only mutants no copy has taken yet in the round, each at most once, while that slot's own files have such mutants waiting. A worker runs its files in order, so every slot started on the same alphabetically first serial files: on a large application's e2e suite, a billing area's first round spent 87 to 94% of each slot's attempts there, on survivors-to-be, and its own files' likely kills waited. The slot the harness gave the file to takes every mutant of it, and once no slot's own files have fresh mutants, every slot takes all of them again.
- A strong pass's `next build` no longer checks types. The check emits nothing the server runs and says nothing of a mutant: Next's type-check worker reads the sources as their authors wrote them, the checkout the measured run already built and checked. On a large application's e2e suite it was 12 s of a 30 s build, paid before any test of each job of a pass split across jobs could start. A measured run still checks types. The strong command's log says Next's `Skipping validation of types`.

## 4.29.10

**Changed**

- A strong pass's survivors that only change a log line are counted on their own: a mutant whose mutated span lies wholly inside the arguments of a call to `console.*`, or to a method named `info`, `warn`, `error`, `debug`, `trace` or `log` on any receiver. They are still `survived` and still count in the score; the pass summary says `survived 185 (109 only change a log line)` and lists the other survivors first, `mutants --status survived` marks each (`(only changes a log line)`, `logOnly` in JSON), and its `Strong` line counts them. On a large application's e2e areas they were 109 of 169 survivors in one and about 10 of 41 in another; a pass of this release over another such area marked 75 of its 166 survivors (45%). A run listed before this release has none marked until its mutants are listed again.

**Fixed**

- A Playwright strong pass no longer calls a server mutant `survived` when the only request that reaches it names no test and was sent while tests ran: `curl` or another client a test starts outside Node, which carries no attempt's scope, so the mutant switched on per request never ran for it. The run now keeps whether a test was running when such a request came (one of the tests that server served, or one no server did; a server a harness starts for a later slot is checked for readiness while other servers' tests run, and that is not such a request), and the mutant runs alone with the server started with it (`ran alone, with the server started with it switched on: its code ran for a request that named no test`). Requests a test sends from Playwright (`request.newContext()`, `page.request`, `route.fetch` in a `page.route` handler) or from Node (`fetch`, `http`) carry the scope, and a fixture test now holds each of them.
- A Playwright strong pass no longer calls `survived` an arrow mutated whole (`() => undefined`) that a server module defines at its top level (`export const GET = async () => ...`, a handler in an object literal). Its switch is evaluated once, as the server loads the module, so no request's mutant reached it; such a mutant now runs alone with the server started with it (`it is a server module's top-level code`). With these two fixes, a pass over a large application's e2e area turned 3 of the 169 survivors 4.29.9 left there into kills, all of them module-level object literals in route modules.
- Recording a release for the compatibility corpus right after it is published no longer fails when npm serves the release before its native packages: npx installed it without one, and every command said `the optional native package @supercov/cli-linux-x64-gnu@<version> is missing`. `scripts/compat-record.sh` now asks npx again, with fresh metadata and without the install it kept, until `supercov --version` runs, for at most about five minutes.

## 4.29.9

**Changed**

- A mutant in a module-level object or array literal -- a translation table, a list of options, a navigation menu -- has for candidates the tests that read its value, not every test that loaded its module. A run measured with the mutants' probes reads each such literal through a proxy that records which test's work read which of its keys; a value read while a module loaded, or one made by a call, keeps every test as before. On a browser suite against one server, where loading a module was every test of the run, a survivor no longer runs them all: on a large application's e2e suite of 1,703 tests, 1,767 module-level mutants went from 2,444,383 candidates to 199,766, a translation table's 628 mutants from 1,605 candidates each to at most 32 (594 of them read by no test), and a list of languages' 317 from 1,605 to at most 65. The plan and the pass's summary say how many mutants were narrowed and by how much (`narrowed      7 module-level mutant(s) to the tests that read their value: 6 candidate(s) where loading their module gave 49`), and a mutant no test read is `survived` without running (`no-test-read-it`). `docs/design/strong-mutation.md` has the argument.

- `supercov merge` keeps once what the runs it merges recorded byte for byte alike. Each run's copy of each test (its records in order, with the entry each is in) and each entry's records of no test are hashed, and a copy an earlier run in the merge already holds is left out; one that differs in anything is kept beside the others. Shards of a continuation round, made from one measured run, merge to the size of that run: 351 MB where eight such runs made 2.8 GB. Shards that each measured the suite differ in their timestamps and keep every copy, as before.

**Fixed**

- Planning a strong pass no longer costs each record's size for each mutant. A test that ran a method whose export only the run as a whole ran used to copy the record's observations, every hit and decision, to add one statement; it now counts the statement as hit. A record's server work is read once, not once for each mutant; a decision's vectors are added through a set; and records and mutants are planned on every core. Planning one shard of a large application's e2e suite (973 records, 65,092 mutants) took 2.1 s where it took 86 s, its plan byte for byte the same, and the whole suite's plan (1,703 tests) 1 m 46 s where it took over 8 minutes.
- `supercov merge` of eight such shard runs, which plans the merged run to keep their strong verdicts, ran for more than 12 minutes without a word, and a CI job's limit ended it before the next round had a merged run to start from. It now takes about a minute, with the same verdicts: it lets go of the runs' evidence once the merged run holds it, and reads which tests each run measured on every core.

## 4.29.8

**Changed**

- A strong pass keeps partial progress. A mutant a pass left open (its budget spent, a job's time up) keeps the candidate tests that already passed with it, saved in the run's `strong-mutation.json` beside the verdicts, and the next pass, or the next round's jobs, runs it only against the rest; a mutant every candidate passed survived. A kill still needs one failing candidate and a survivor every candidate passing. The kept candidates merge across a sharded pass's runs (`supercov merge` says how many), carry over to later runs as verdicts do, and a candidate an edit could reach runs again. A pass says what it resumes: `37 mutant(s) resume where earlier passes left them, with 4 of 15 candidates left (the median): 412 candidate run(s) they passed are not run again`, and a budget's summary line says what the mutants it left keep. The Playwright driver records it.
- A Playwright strong pass spreads spec files with a serial describe over every invocation of `playwright test` whose command names spec files. A harness that shares tests out to slots (each with its own server and database) keeps such a file whole, in one slot, which then ran all its mutants while the other slots ended early; now each slot takes its own mutants of the file from the pass's shared queue. The harness and the command are unchanged: Supercov's preload adds the files to each invocation's file filters before Playwright reads them, and leaves `--list` as it is. On a large application's e2e area, whose tail one such file held in one slot of each job, the 294 mutants partial progress had left took two rounds of 8 jobs where they had gained about 50 a round: 2,156 of 2,160 decided, 32 of them errors a later round runs again.
- The progress lines name a file with a serial describe that only one invocation of several runs (one the pass could not spread), where it holds the round back: how many mutants wait for it and the time its invocation has left against the others'.

**Fixed**

- A Playwright pass its budget stopped could record false kills. The stop takes the app's servers down under the tests still running, and a test that then failed (its page's script never served) stood as a kill, since no control came after it; an attempt Playwright reported as interrupted counted as failing too. Now the pass notes when it began to stop, and no attempt that ended after that counts (as none after an error outside any test does); an interrupted attempt settles nothing, and the mutant runs that candidate again in the next pass, resuming from the candidates it passed. A kill that ended before the stop still stands.
- A command its budget, a timeout or a signal ended no longer leaves behind a process that left its process group: Playwright starts its web server detached and stops it only as it ends itself, so a stopped pass left the server running, and the next pass failed at once (`... is already used`), its mutants `error`s. Such groups are now told to end with the command, and killed with it after the grace period.

## 4.29.7

**Changed**

- A shard of a continuation round on an unchanged checkout no longer measures the suite again. `supercov mutants --shard k/n` makes the job's run from the newest restored run whose command passed, under an id of its own, and says so (`not measuring: the checkout is unchanged since run r4, so this shard saves its verdicts into run r9, made from it`). On a large application's e2e suite this was about 100 s of each 10-minute job.
- `--budget <seconds>` is the whole command's: measuring and planning spend it too, so a job ends when its budget says. Where measuring took half the budget or more, the summary says so (`measuring took 310 s of the 450 s budget, which leaves the pass 140 s`).
- The summary says how many earlier verdicts were not kept, and why (`6 earlier verdict(s) not kept, so run again: 4 their file changed, 2 a test that could kill them could reach a change`), and whether the runs they were kept from were measured on the checkout as it is (`1317 verdict(s) kept from runs r8, r7: the checkout is unchanged since them`). The JSON answer has them as `carried.notKept` and `carried.unchanged`, and `measurement` says whether the run was measured (`seconds`) or made from another (`derivedFrom`).

**Fixed**

- An earlier run measured on the same files, byte for byte, keeps every verdict a pass reached, whatever the two measurements recorded. On 4.29.6 a test that failed in one job's measurement at random dropped that run's verdicts for every mutant whose candidates shared a test file with it, and the next round ran them again.
- After an edit, a test that failed in the earlier run counts as reached only where the change reaches code that run covered, instead of always.
- `tests affected --json` says what the change reached of code the run covered (`beyondTheRun`).

## 4.29.6

**Changed**

- A Playwright strong pass tries each mutant's candidates cheapest to a kill first (a test's measured time over how likely it is to kill, a test that infected the mutant taken as four times likelier than one that only reached it), so slow tests go last, and decides mutants one after another. A file's copies run in sweeps, every test's first copy, cheapest test first, then every test's second; a copy takes the waiting mutant with the fewest candidates before its test still to try, then one its test infected, then the one closest to a verdict; and a copy whose every waiting mutant still has a cheaper candidate to try waits a sweep, while the test's copies left cover what it must run. A mutant a fast test kills no longer waits for, or holds, a slow test until its time limit, and a slow test runs the mutants its cheaper candidates let live. A survivor still runs every candidate. On a large application's e2e suite, the 572 mutants its billing area had left after four passes (8 jobs, about 6 minutes of a pass each), a pass decided 147 where 4.29.5 decided 57 from the same state: one file of slow tests that poll until a purchase is active held every mutant there, and its first test ran each of them before the next test began.
- A test's run without a mutant (its control) is the invocation's, as its server is: only the test that killed runs one before its next mutant, not every test of the file in the new worker Playwright starts after a failure.
- The progress lines name the costliest tests left, how many mutants wait for each, its time a run and its limit, and the time left is the round's work left (each mutant's candidates still to try, at each test's baseline and each invocation's own pace): `about 12m40s left in this round`.

**Fixed**

- A strong pass split across jobs can go on in a second round on the same commit. `supercov mutants --shard k/n -- <command>` now measures a run of its own even when the checkout is unchanged since the runs a job restored, keeps their verdicts, and runs its share of the rest; the last job's `supercov merge` takes the round's runs, which replace the restored ones. Before, every job of a second round saved its share into a copy of the latest restored run: on a large application's e2e suite eight jobs wrote eight copies of one run id, the merge refused them, and the 1,265 decided mutants they held could only be added up by hand. `docs/mutation.md`'s sharded CI example shows a continuation round.
- `supercov merge` says how many strong verdicts came from each run and how many the merged run's plan decides (`1265 strong verdict(s) kept from runs r8, r7: 1103 from r8, 150 from r7, 12 the merged run's plan decides`). A verdict two runs hold alike counts for the first run named.
- A run named twice in `supercov merge` is refused with why (copies of one run share its id, and the store holds one of them) and what to do, instead of `Each merged run must be unique` alone.
- A Playwright mutant with a candidate in a project others start from (a login `setup`, a `teardown`), which runs without a mutant, waited for it as unfinished, pass after pass. It is now an `error` saying so when no other test kills it, as a mutant with candidates under another runner is.

## 4.29.5

**Changed**

- A Playwright test run with a mutant now stops at StrykerJS's time limit, its baseline × 1.5 + 5 s, and its mutant is a `timeout` (detected). The baseline is the longest of the test's measured run and its runs without a mutant in the pass. The limit is Playwright's test timeout for the attempt, so a click or an `expect` waiting for an element the mutant broke ends there instead of at its own 30 or 60 s; a `test.setTimeout()` or `test.slow()` in the test may shorten it but not lengthen it. On a large application's e2e suite, a broken page's kill used to wait out the test's timeouts: the 90th percentile kill took 60.7 s, and now 7.1 s. In the same budget its billing area's pass decided 787 mutants where it decided 611, and found 645 where it found 481. A timeout is confirmed as a kill is, and is no longer run a second time, one test at a time, with twice the time.
- `--timeout-factor <x>` on `mutants test` and `mutants`, or `SUPERCOV_STRONG_TIMEOUT_FACTOR`, sets the factor (default 1.5).
- The summary says how many timeouts ran past their time limit and what the limit is; the progress lines show each running mutant attempt's limit and say when it is near it; a timeout's reason gives its limit and baseline.

## 4.29.4

**Changed**

- A Playwright strong pass runs the whole plan in one invocation of the command, without restarting a worker or a browser for each mutant. Each test a mutant needs is declared as many times as it may run (its copies, in describes of their own), so one worker runs them one after another with the browser it launched, and each copy takes the next mutant waiting for that test from a queue the pass's processes share. A mutant goes from candidate to candidate as copies take it and stops at its first kill; the main pass no longer ends before the survivors' other candidates run in a further invocation. On a large application's e2e suite a mutant took about 50 s, and an area of 1,688 mutants decided none in a CI job's ten minutes.
- A kill is confirmed by a run of the same test with no mutant on the same server (its control) that passes; a failed control puts the kill in doubt, and it runs again one test at a time. Kills in different invocations of a harness, each with a server of its own, are no longer confirmed again as though they had shared one.
- Verdicts are written as they are reached, so the progress line counts them, and a pass stopped by its budget keeps them.
- The progress line says what the pass does: the round, how many invocations, baselines, mutant attempts and controls, the tests running and for how long, and when nothing has ended for two minutes, what it waits on. Before Playwright starts it shows the command's last line, and each invocation prints a line for each mutant attempt, so a harness that watches its processes sees them work. The estimate of the time left is paced from the first mutant attempt, not the command's start. The command's process tree is listed after ten minutes, not every minute.
- Mutants a server must start with run several at once, one per invocation of a harness that starts several, each invocation's server started with its own; one its server did not run runs alone as before.
- A `describe.serial` runs in order in one worker but goes on past a kill, and a project others depend on (a login setup) runs as it is in every invocation.

**Fixed**

- A strong pass stopped by its budget while a later round (a confirmation, a timeout's retry) had not yet run its tests no longer makes that round's mutants `error`s; they are left for the next pass.

## 4.29.3

**Fixed**

- A Playwright strong pass over a large plan ran out of memory before its first test. Playwright makes every repetition of every test before it runs one, and the pass repeated each test as often as its busiest file had mutants: on a large application's e2e suite, 77 tests and 15,603 mutants to run, a Node process died out of a 4 GB heap and every mutant was an `error`. An invocation of Playwright now holds at most about 20,000 test repetitions, and the main pass runs the rest of its mutants in further invocations. Each test worker reads only the rows of the repetition it runs, not the plan. On a plan of 15,603 mutants, `playwright test --list` used 200 MB where it used 1.16 GB, and the whole pass runs with a 256 MB heap.
- `playwright test --list` in a strong pass, as a harness runs it to share the tests out, lists each test once and reads nothing of the pass. It used to list every repetition of every test and clear the pass's files.
- A harness that starts several Playwright processes at once in a strong pass, each with a share of the tests, no longer stalls. Each process cleared the pass's files as it started, so the processes started before it waited out every baseline, and a pass of 1,688 mutants decided 25 in 8 jobs. They now share the pass's files, and each mutant's verdict is decided once from whichever processes ran its tests, after the command has ended. A pass stopped by its budget keeps the verdicts of what it finished.
- A mutant whose candidate test's run without a mutant was stopped before it ended, rather than failed, is no longer an `error` saying its candidate tests fail without any mutant. It is left for the next pass.
- When a Node process of a strong pass runs out of heap, the pass names it, by its command line and pid, with its heap size, and says where the plan was too large for it. Each mutant left without a verdict says the same in its reason.

## 4.29.2

**Added**

- `mutants test --budget <seconds>` (and `supercov mutants --budget`): once the time is spent the strong pass stops, saves its verdicts and ends as a finished pass, saying how many mutants it left without a verdict for the next pass. A sharded CI job needs no `timeout` around it.

**Changed**

- A strong pass runs the cheapest mutants first, by the recorded times of the tests each runs against, so a pass cut short by a time limit has decided as many mutants as it could. On a large application's integration suite each 450 s shard had decided only 29 to 40 of its 2,460 mutants, its time spent on a few that ran hundreds of tests each.
- `--shard k/n` splits the mutants into shares of about the same cost rather than the same count, so every job gets its part of the costly ones. Every job computes the same split from its own run.
- The strong pass's "about ... left" is the plan's estimated cost of the mutants left at the pace so far, and no longer swings as a few costly mutants end.

**Fixed**

- A Vitest mutant that made a hook fail, such as a `beforeAll` that seeds a database through a schema module the mutant broke, was an `error` ("its copy of ... ran none of the tests it was to run"), though Vitest's own run fails. It is now `killed`, the hook's error its reason.
- A Vitest test file that did not load or set up before its mutant ran, such as a setup file whose database did not answer, killed the mutant ("the test file did not finish"). Its copy now runs again, and where it fails again the mutant is an `error` that names the file's own error. A copy that ran none of its tests says why where it can.
- Sent SIGINT or SIGTERM, a strong pass stops every process it started and ends within seconds, its verdicts saved: one still running 8 s after the signal is ended. Passes stopped by `timeout --signal=INT` were still running 25 s later and were killed. A signal that comes while the pass prepares its copy, or that the parent process left ignored, now stops it too.

## 4.29.1

**Changed**

- `supercov merge` keeps the strong mutation verdicts of the runs it merges, so a strong pass split across machines with `--shard` adds up to one run with every verdict, and `merge` says how many it kept and from which runs. A kill in any run is a kill; a survival stands only where the run that let the mutant survive measured every test of the merged suite that could kill it, and is otherwise left for a pass over the merged run. A run whose verdicts are of another mutant set is named and lends none.
- After an edit, `supercov mutants` keeps verdicts from every earlier run of the same command that holds one, not only the latest: each mutant's comes from the newest run that can lend it, and the summary names each run (`193 verdict(s) kept from runs r2, r1`). A run whose record of what its tests ran is gone no longer stops older runs from lending theirs. In `--json`, `carried.from` is a list of run ids.
- [Mutation testing](docs/mutation.md) shows a strong pass split across CI jobs, merged, and cached for the next commit.

**Fixed**

- A process Playwright starts as a `webServer` from its config's folder, such as a mock server in `tests/e2e` that imports `@playwright/test`, failed to start under Supercov ("ENOENT ... tests/e2e/.supercov/node_modules/playwright.mjs"), and the run recorded no test. Playwright is now found from the project's root, whatever folder the process starts in.

## 4.29.0

**Added**

- Mutation testing. `supercov mutants -- <test command>` measures the suite, lists the mutants StrykerJS 10.1.1 would make of every JavaScript and TypeScript source the run measured, and runs each against the tests that reach it, under Vitest, Vitest Browser Mode, Playwright, Jest, `node:test`, Mocha, AVA, node-tap and `bun:test`. It saves `killed`, `survived`, `timeout`, `no-coverage` and `error` into the run, with the mutation score and the survivors. Vitest runs many mutants at once in one resident Vitest; Playwright carries each test's mutant with its requests, so one server serves several; Jest and Mocha run in resident runners. See [Mutation testing](docs/mutation.md).
- In preview, the same for Python (mutmut 3.8.1's mutants, under pytest and `python -m unittest`), Rust (cargo-mutants 27.1.0's, under `cargo test` with its doctests and cargo-nextest, and Rust built for wasm in Node, under WASI, in Chrome, and when JavaScript tests load it in Node or in Playwright's pages), Go (Gremlins 0.6.0's, every mutator, under `go test`) and Ruby (mutant 0.17.1's light operators, under RSpec, Minitest, test-unit and Cucumber). A Rust or Go mutant that does not compile is `unviable`, counted apart and left out of the score. On Windows the Python and Ruby strong pass does not run yet (its drivers fork); those mutants are counted not run.
- The pass runs no mutant no test reached (`no-coverage`) or infected (`survived`), and each other mutant only against the tests whose own evidence reached or infected it, the likeliest killer first, stopping at the first failure. A mutant under a runner with no strong driver is an `error`, counted per runner, never a survivor. Each verdict is saved as it is made, so a pass that is interrupted or killed resumes where it stopped; Ctrl-C stops every process the pass started, and a pass removes its copies and builds when it ends. After an edit, `supercov mutants` keeps an earlier verdict for each mutant whose file is as it was and whose candidate tests nothing changed could reach, as `tests affected` reads a change. A large Vitest pass runs in bounded runs of each test file's slots and up to 2,000 more, so Vitest's process does not run out of memory reading them (all of hono's mutants at once did). The pass refuses a run that recorded no test, and exits `2` after its answer when the measured command failed, since a failing test kills nothing. Every verdict was compared with the reference tool's on public projects, and every disagreement applied by hand and the suite run as written: StrykerJS's full kill matrix and `disableBail` runs on 20 JavaScript projects across every runner, mutmut on 4 Python projects, cargo-mutants on 3 crates, Gremlins on 5 Go modules and mutant on 5 Ruby projects. No mutant the reference tool kills is a survivor in Supercov's verdicts, other than the subset, flaky and time-dependent kills [Mutation testing](docs/mutation.md) lists with each tool's numbers, machines and times. End to end on 16 CPUs, hono took 177.1 s against StrykerJS's 258.5 s, and tRPC, in 8 jobs, 2,018.1 s against 2,782.2 s.
- The weak report: `supercov runs <id> mutants` reads, from the run alone, which mutants no test reached, which a test reached but never gave another value (`not-infected`, a certain survivor, with the input that is missing), and which are equivalent; after a pass, each mutant's verdict beside it, and `--status survived` lists the survivors. A plain run places no probe and lists no mutant; the first query lists a run's mutants from its sources, runs from every earlier release included. `SUPERCOV_MUTANTS=1` (which `supercov mutants` sets) records the values that decide more of them.
- The pull request gate: `--diff <base>` keeps the mutants on lines the change touched; `--format markdown` writes the mutants left alive, the survivors once a pass ran, as a table for a pull request's comment, from `runs <id> mutants` and from `supercov mutants` (with `--output <file>`); `--fail-on` fails the command on weak statuses (`runs <id> mutants`) or verdicts (`mutants test`); `--critical <glob>` requires every mutant in those files killed or accepted; `supercov.accepted.json` lists mutants no test can tell apart, with a reason.
- Mutation testing in both HTML reports, `supercov report` and `runs <id> report --format html`: a Mutation testing card with the weak and the mutation score, every verdict counted with why a mutant has none, each language, and the mutants left alive by file; a Mutants column with each file's score; each line's mutants under it in the source. `--diff <base>` keeps a change's mutants.
- `mutants --format stryker` writes StrykerJS's `mutation.json` (the mutation testing report schema), which mutation-testing-elements and the Stryker Dashboard read, and `mutants --format pit` PIT's `mutations.xml`, which PIT's report aggregator and Sonar read. `--stryker` joins StrykerJS's reports and mutant's session results by mutant.
- `supercov docs mutation`, and a `/supercov:mutants` command in the Claude Code plugin and Gemini CLI extension. The agent skill runs mutation testing after writing tests, and with `--diff` on a branch.
- A backward-compatibility suite: runs that every minor release from 1.0 to 4.28 recorded of a `node:test` and a pytest fixture, each read by every build with the numbers its release gave, every query and report format, shards merging, `diff` across releases and `merge` refused across them with its reason.

**Changed**

- A change to the shape of a file a run stores fails the build until `tests/compat/store-shapes.txt` changes with it, and says what the change owes: a version bump with a refusal or a conversion, or reading the old shape as well.
- Each release records its own runs into the compatibility corpus, from its checkout before it is published and from npm after.
- A large run publishes sooner: its views, index and files are worked out side by side (#231), and a passed view that differs from the whole view only by skipped tests that recorded nothing has the whole view's index records copied rather than built again. On Excalidraw's unit run, publication took 5.2-5.5 s instead of 7.0-7.6 s (GitHub Actions 4-CPU runners, three rounds, 2026-11-13).

**Fixed**

- Runs recorded before 4.24.0 no longer opened: every query stopped at "unknown field `meta`", because 4.24.0 renamed a field of the evidence without a new schema version. Evidence is read in either shape.
- Python runs recorded before 4.14.0 kept what ran while no test was running under the kind `unit`, so `minimize --kind unit` refused. It is read under its own kind, whatever release stored it.
- A Mocha suite run with `--check-leaks` failed on Supercov's own globals. They are no longer among the global object's keys.
- A second run of a suite in the same checkout restored the instrumented modules Next.js bundles from the frontend cache without the browser runtime they import, and every test importing one failed with "Cannot find module .../browser-runtime.mjs" (107 of a Next app's unit test files). The runtime is now cached with them.
- Since 4.25.0 a test's Jest docblock was no longer its first comment, so `@jest-environment` was ignored and the test ran in the project's default environment; and a `'use strict'` directive became a plain string, so the file ran sloppy. Both stay where the file had them ([#235](https://github.com/supercorp-ai/supercov/issues/235)).
- Rust compiled to wasm and loaded by JavaScript tests, in a project with JavaScript of its own, published no Rust coverage: the crate's records were read after the JavaScript run had archived and removed them.
- A Rust file holding `const _` ended the run instead of being instrumented.
- A package that keeps its code in a root `index.js`, or names its entry without `./` (`"main": "index.js"`, a bare `bin` path), had no source found: "No application source files were discovered" (content-type). That file is a source now.
- Jest 27 and 28 ran on Jest's defaults: the project's configuration was not read (`readInitialOptions is not a function`), so a suite whose configuration names its tests found none. It is read from Jest 27 on.
- A Playwright config that names no `testDir` found "No tests" under Supercov, which ran it from its own config's directory. Its tests are found beside it.
- An untracked install directory (`vendor/bundle`, a virtual environment, `node_modules`) or a lockfile an install rewrote made a run read as measured with uncommitted changes, so `--diff` compared the working tree and could count installed files as the change. A run with only those is measured on its commit.
- Supercov's hint for a suite that needs a build its command does not run suggested `sh -c "<build> && <command>"`, which fails where the build starts by removing its output folder, absent in the copy. The hint makes the folders first and quotes the command.
- `runs <id> report --format html` asked for in a terminal printed the page into it. It is written to `.supercov/reports/coverage-report.html`, replaced each time, and its path said.
- Go tests of one name in two packages (`TestLRU` in a module's root and in a subpackage) shared one record's evidence, so the root package's code read as run by no test.
- A Ruby suite that loads test-unit before Supercov's runtime (`ruby -rhelper`, as Rake's test task does) ran its tests after the runtime had closed its evidence, and a child started with `--disable-gems` failed to find `RbConfig`.

**Contributors**

- [@npapagna](https://github.com/npapagna)

## 4.28.0

**Added**

- `runs report --format` writes five more formats other tools read: `istanbul` (Istanbul's `coverage-final.json`), `istanbul-summary` (its `coverage-summary.json`), `clover`, `jacoco` and `sonar` (SonarQube's generic coverage XML). A tool that reads Istanbul's file, such as fallow's CRAP gate, now reads a Supercov run of any language it measures, Bun's tests included ([#226](https://github.com/supercorp-ai/supercov/issues/226)). Each was checked against its readers: `istanbul-lib-coverage` and `istanbul-reports`, fallow, SonarQube, Qlty and JaCoCo's DTD all read Supercov's own totals.
- Istanbul's file places each statement, function and branch by the source the run recorded, counted in each language's own columns. A file changed since the run is refused, naming the line, rather than placed wrong. Counts are `1` or `0`: Supercov records whether code ran, not how often. JaCoCo has no bytecode here, so a measured line counts as one instruction.

**Fixed**

- Cobertura's export left out branches on a line Supercov does not measure on its own, such as a later line of a statement written across several, or a default in a signature that spans lines, so its branch totals fell short of Supercov's. They are stated on the line where their statement starts, as in the new line formats.
- A Rust test compiled to wasm and run in Chrome (`wasm-pack test --headless --chrome`) could fail with `webdriver POST /session timed out after 60.0s` when Chrome's driver stopped answering. A driver that gives no session within 25 seconds is now started again on a new port and asked once more, within the runner's 60.
- The ports Supercov gives the servers it starts, Chrome's driver and a Worker's inspector, could be taken by another process before the server listened on them, and `wrangler dev` failed with `Address already in use`. They are drawn from below the range the system hands out by itself and checked free before use, and the proxy in front of Chrome's driver can no longer share a port with it.
- A Rust closure written in a `static`, a `const` or a `const fn`, such as the one `LazyLock::new(|| ..)` takes, runs when it is called, but was declined as compile-time code and left unmeasured. It is measured now, with what it holds; only the code the compiler evaluates stays declined.
- `check` said a Rust file holding a const context or a macro such as `json!` was outside the source roots and not counted, and the line view showed every line of such a file as `PARTIAL`. Those files are counted: the note now names only files discovery left out, and a site's limitation stays on its own line.

**Contributors**

- [@Fidasek009](https://github.com/Fidasek009)

## 4.27.0

**Changed**

- Measured runs of JavaScript suites are faster. On a Vite canvas app's Vitest suite under jsdom, a measured run takes 1.28× the plain run, against 1.49× with 4.26.0. On a Next.js app, its Vitest suite takes 1.10× the plain run, against 1.23×, and its Playwright suite 1.13×, against 1.15×. Measured in GitHub Actions on 4-CPU runners, three rounds each.
- Every Node process of a measured run uses Node's compile cache (Node 22.1 or later, outside Windows), kept with the run's evidence, unless the command sets `NODE_COMPILE_CACHE` itself. Vitest workers also share the compiled code of instrumented modules, so each module is compiled once per run rather than once per test file.
- A probe outside a loop is a call that checks for itself; probes in loops keep the inline check. Instrumented code is smaller, which browsers and bundlers parse faster. Modules name their helpers without a per-file suffix, and decisions use shorter temporaries.
- A Playwright context that outlives its test, such as a worker-scoped one, now holds one copy of Supercov's page script. Each test added another copy and every page ran all of them.
- The work after the tests is faster: records are parsed once, on every core, probe places are resolved in parallel, and Supercov allocates with mimalloc outside Windows.

**Fixed**

- A test that spied on `Map.prototype.set`, `Set.prototype.add` or another built-in collection method counted Supercov's own calls when a probe first ran, and could fail only under Supercov. Probes now record through the methods as they were when Supercov loaded, and add to arrays by index.

## 4.26.0

**Changed**

- Measured runs of large JavaScript apps are faster and ship less. On a Vite canvas app's Vitest suite under jsdom, a measured run takes 1.49× the plain run, against 1.74× with 4.25.0, and `merge` of its runs takes 4.7 s instead of 12.4 s. Its instrumented production bundle is 8.4 MB, against 9.7 MB. Measured in GitHub Actions on 4-CPU runners, three rounds each.
- An instrumented module carries its probe table's key instead of every probe's id, and a probe that always runs right after another is recorded with it rather than separately. The report reads both back, so coverage, per-test hits and phases are unchanged.
- Instrumented code no longer reads the `Uint32Array` global on each decision it records; under Vitest's jsdom environment every such read was a getter call.
- When this checkout holds build output (`dist/`, `.next/` and the like) that the copy the command runs in starts without, and the command does not run the project's `build` script, Supercov says so before the suite runs, not only after a failed run.
- `merge` leaving runs out because `the dependencies have changed` says why that happens across CI jobs: every job must install the same lockfile. The merge docs show how for a repository without a committed lockfile.

**Fixed**

- `supercov -- bun test` failed with `Unknown command: bun` when Bun ran the script (`[run] bun = true` in `bunfig.toml`, `bun --bun run`, `bun run supercov`): Bun drops a leading `--` before Supercov sees it. Yarn 1's `yarn supercov -- ...` and PowerShell's way into a package manager's `.ps1` shim drop one too, and Yarn 1 also took `--exact-attribution` for its own, so the run went ahead without it. A first word that is no Supercov command but a program on `PATH`, or a path to one, is now measured as the test command, and Yarn 1's words are restored from what Yarn recorded ([#209](https://github.com/supercorp-ai/supercov/issues/209)).
- In a suite that starts several Playwright processes side by side over one run, such as a sealed suite whose containers each run `playwright test file:line`, one test of a file could now and then fail with `No tests found`. Playwright removes every cached file of a source before it writes one, including the entry another process had just written and was about to read the source map of. That test was then placed by its compiled lines, and its `file:line` filter missed it. Entries in the run's cache are no longer removed.
- The gem's `supercov`, started with no arguments from a directory whose path holds a space or a parenthesis, failed: its wrapper handed the path to a shell.
- A Playwright test that closed a browser context sent the next phase to it anyway; the call failed and its cookies were tried again host by host. Closed contexts and pages are skipped.

**Contributors**

- [@Fidasek009](https://github.com/Fidasek009)

## 4.25.0

**Changed**

- Measured runs of Next.js apps with Playwright and Vitest are faster. On a Next.js app's Vitest suite and Playwright flow, a measured run takes 1.04× the plain run, against 1.15× with 4.24.8. On a larger Next.js 16 app on Turbopack (1,100 Vitest tests and 170 Playwright tests on one worker), a measured run takes 1.15× the plain run; 4.24.8 could not build it (see Fixed). Measured in GitHub Actions on 4-CPU runners, three rounds each, every run building the app from nothing.
- Supercov's own calls into Playwright no longer appear among a test's steps in its report.
- An assertion that is over within its call, such as `expect(value).toBe(1)` on a value the test already holds (outside `expect.poll`, `.resolves` and `.rejects`), no longer becomes the page's current phase: what the page does after it stays with the action before it. An assertion whose arguments or matcher reach the page still gives the page its phase first.
- On Node 22.15 and 23.5 or later, Supercov's module hooks run in the importing thread rather than a thread of their own, and on Node 22.14 or later its `--enable-source-maps` maps the project's files only, unless the command asks for source maps itself.

**Fixed**

- A Next.js app whose `instrumentation.ts` imports Node-only modules inside `if (process.env.NEXT_RUNTIME === "nodejs")` failed its measured Turbopack build with those modules in the Edge bundle. That comparison is now left for Next to decide.
- `import test from "@playwright/test"` failed under Supercov with `(0 , _test.default) is not a function`.
- Tailwind CSS v4 found no sources in Supercov's copy, because its automatic source detection reads `.gitignore` files above the project and `.supercov` holds one; the measured build lacked every utility class only the app's own components used.
- A CommonJS spec and an ES module spec run by one Playwright worker failed with `Playwright detected inconsistent test.use() options`.

## 4.24.8

**Fixed**

- A Playwright test file could fail at `does not provide an export named …`, and then the run reported `No tests found`, when it imported an ES module that another Playwright process was still writing into the shared compile cache. 4.24.3 wrote that cache whole only from the process's main thread; Playwright 1.59 compiles ES modules in its loader thread, which now writes whole too. Most often hit by the first tests of a Docker relay run, whose containers share one cold cache.

## 4.24.7

**Fixed**

- Vitest or Mocha started by Bun from its entry point (`bun node_modules/vitest/vitest.mjs run`) passed and recorded no test: Bun loads no Node preload, so the runner was never set up, and all its coverage was counted as background while the run reported a complete measurement. The Bun adapter now sets these runners up as Node's preload does, so their tests are attributed under Bun as under Node ([#205](https://github.com/supercorp-ai/supercov/issues/205)).

**Contributors**

- [@artile](https://github.com/artile)

## 4.24.6

**Fixed**

- A Bun test could be recorded with no source file: Bun reported Supercov's wrapper around `test` as an `unknown` frame in front of the test's own, whether the test was registered directly or by `call`, `apply` or `bind`. Tests with the same title in different files then shared one identity, a file filter selected none, and `merge` collapsed them. Each keeps its file now ([#200](https://github.com/supercorp-ai/supercov/issues/200)).
- A Vitest run of a package whose Vitest and Vite are installed only in that package, not at the repository root, failed loading Supercov's generated config because it looked for Vite from the copy's `.supercov`. It looks from the runner and the project's own config first, through Vitest's own dependencies under pnpm's strict layout, and an error Vite itself raises while loading is no longer hidden by trying another copy ([#199](https://github.com/supercorp-ai/supercov/issues/199)).

**Contributors**

- [@artile](https://github.com/artile)

## 4.24.5

**Fixed**

- A Bun suite whose `bunfig.toml` preload imports `bun:test` recorded no tests: Bun loads configured preloads before `BUN_OPTIONS`, so the preload took Bun's own test API before Supercov's adapter could wrap it, and all coverage was counted as background. The adapter now goes first in the copy's preload list, the project's own preloads following in their order; the project's `bunfig.toml` is not changed ([#197](https://github.com/supercorp-ai/supercov/issues/197)).
- After a JavaScript run of a package inside a monorepo, links to the rest of the repository stayed in the package's `.supercov`, so a plain test run in the package (`npx vitest run`) also collected the other packages' tests through them. They are removed when the run ends, and the next run makes them again.

**Contributors**

- [@artile](https://github.com/artile)

## 4.24.4

**Fixed**

- `merge` refused a unit run and an end-to-end run of the same commit for different denominators when a file had a type-only import (`import { type T }`): whether that import counted as a line depended on the test command, so `node --import tsx` dropped it and Vitest kept it. The file's nearest tsconfig alone decides now (`verbatimModuleSyntax`, `preserveValueImports`, `importsNotUsedAsValues`, `emitDecoratorMetadata`), as tsc, esbuild, swc, Babel and ts-node do, so every command counts it alike.
- A Playwright suite with many Vitest files beside it could have Vitest taken for its test fixture module, and every Playwright test then crashed with `Cannot redefine property: Symbol($$jest-matchers-object)`. Another runner's own API (`vitest`, `@jest/globals`, `bun:test`, `node:test`, `mocha` and the like) is never taken for the fixture.
- A run whose test harness started the build from its own code (`execSync('npm run build')`) and whose build failed was told to add `npm run build` to the command. The run now names the build it saw start and its exit code, and gives no such hint.

## 4.24.3

**Added**

- A process the run measured that a crash signal ended -- SIGSEGV, SIGBUS, SIGABRT and the like -- is now named when the run ends, with the signal and how long it ran. Such a process says nothing itself: a backend that crashed right after it printed its URL read as one that never became ready, and a Playwright worker as one that "exited unexpectedly". Exits by SIGTERM or SIGKILL, which teardowns send, are not named.

**Fixed**

- A container that ran to its end with exit code 0 -- a short install step started with `docker run` or `docker compose run` from the same image as the servers -- is no longer named as one that sent no coverage back.
- Playwright 1.59 could crash while loading its config under coverage (`Cannot add property metadata, object is not extensible`): it read a compiled file from its transform cache while another process was still writing it. Files go into that cache whole now.
- On Docker Desktop a container created right after the run started could see the previous run's copy of the project, empty or with old files, so `docker exec` found no Playwright. Such a container now waits until the new copy shows, at most 1.5 s after the run starts; nothing waits elsewhere.

## 4.24.2

**Fixed**

- Tests run as root in a container -- a sealed worker running Playwright by `docker exec` -- wrote their evidence readable by root alone, so the run, as the checkout's user, stopped at each file with "Permission denied (os error 13)" and recorded no run. Evidence a container writes is now readable by every user, and when the run ends, whatever a container left that another user owns in the copy and the run's directory -- Playwright's `test-results`, `.last-run.json` -- is opened to the run's user, so it is read and removed.

## 4.24.1

**Fixed**

- A container that mounts the project read-only (`.:/workspace:ro`) failed at its first recorded line with EROFS in 4.24.0: the run's evidence paths were moved under `/workspace/.supercov`, inside the read-only mount. Supercov's own directory is now mounted writable where the container sees it, and the rest of the mount stays read-only.
- A container that ran with the run's settings and sent no coverage back -- a server that stopped at its first probe -- is now named when the run ends, whatever the tests did. Before, the warning came only when no test reported either. Tests started with `docker exec` that report nothing are named on their own.
- A command that builds under another name (`npm run build:ci`, `turbo run build`) is no longer told to add `npm run build` when it fails, and a run that recorded no test says so instead of saying that none failed.

## 4.24.0

**Added**

- Playwright tests that run in a container are measured, started with `docker exec` or `docker compose exec` in a container the project is mounted into (`.:/workspace`). The command is given Supercov's settings, with each path moved to where the container sees the project, and each test is credited with what it ran. A container the project is mounted into is measured whatever its image, and the project's `node_modules` is mounted into it read-only. See [supported suites](https://github.com/supercorp-ai/supercov/blob/v4.24.0/docs/supported-suites.md#a-server-in-a-container).
- When containers were given the run's settings and no test or coverage came back from them, the run says so, rather than reading as complete with nothing in it.

**Fixed**

- A browser chunk of an instrumented module held the text of every condition in the file, including a route's server-only `loader` and `action` (`process.env.QSTASH_RETRIES`), so server source reached the browser and a check for secrets in the client bundle failed. A module now carries only each decision's id and number of conditions.
- `supercov merge` with no run named took the newest run of each command, so the runs several machines made of one command, copied into one `.supercov/runs`, merged as one. Each is now merged; a command measured again in the same project still replaces its earlier run.
- When tests could not load build output the copy lacks, a command that already builds was told to add a build. It is now told its build did not make the files.

## 4.23.0

**Added**

- Apache Ant builds, Ivy-resolved ones included, are measured. Supercov reads `build.xml` and what it imports, includes through entities or calls through `<ant>` and `<subant>`, and finds the tests from each `<batchtest>` and the product code from what each `<javac>` compiles. Tests run by `<junit>` are attributed one by one through a result formatter Supercov adds next to the project's own, and tests run by `<junitlauncher>` and `<testng>` through the JUnit Platform and TestNG listeners. A test that a suite runs again is named within that suite, and a test Ant stops at its timeout is recorded once, as not passing. A build whose harness sits outside the project is laid out in the copy as far as the build reaches. See [supported suites](https://github.com/supercorp-ai/supercov/blob/v4.23.0/docs/supported-suites.md).
- An Ant build in which Supercov finds no `<junit>`, `<junitlauncher>` or `<testng>` task is told so, with what such a task needs, instead of being given JUnit Platform advice.

**Fixed**

- A Java condition that assigns a variable, such as `while (!stopped && (n = in.read(buf)) >= 0)`, failed to compile once instrumented, because javac no longer saw `n` as assigned. It compiles now.
- An `else if` that narrows a type with an `instanceof` pattern and has no `else` failed to compile once instrumented.
- Supercov's Java runtime carried a dangling doc comment, which JDK 23 and later warn about, so a project compiled with `-Werror` failed to build.

## 4.22.0

**Added**

- Native `bun:test` coverage attribution for individual test bodies, including async and concurrent tests, parameterized rows, and done callbacks. Lifecycle hooks are recorded separately as setup. Existing Bun options and configured preloads are preserved. Callback outcomes and native Bun verdict limitations are described in [supported suites](https://github.com/supercorp-ai/supercov/blob/v4.22.0/docs/supported-suites.md#bun).

**Fixed**

- Instrumented application modules initialize the runtime before their first probe under `bun test`, including projects with browser configs, nested frontends, aliases and preloads. TypeScript browser sources also receive the runtime fallback without requiring a Dockerfile. Native `bun test` no longer selects an unrelated browser runner from an unused package script ([#176](https://github.com/supercorp-ai/supercov/issues/176)).
- Coverage without test attribution no longer implies that a process ran in a container or VM. Interrupted Bun callbacks retain their named coverage and assertion-phase metadata with an unknown outcome ([#177](https://github.com/supercorp-ai/supercov/issues/177)).
- Concurrent Docker container creation no longer interleaves diagnostic journal records and loses warnings, including the warning about an image built from an earlier source copy.

**Contributors**

- [@artile](https://github.com/artile)

## 4.21.0

**Added**

- A run whose tests failed loading files the project has and its isolated copy does not, such as `dist/` an earlier build made, names those files and says why: the copy leaves out build output, and nothing built it there. The failures read as broken tests before, with `build=0ms` in the timings the only sign.

**Changed**

- A Rust `for` loop over a non-empty array literal, a constant of the file that is one, a range between integer literals or constants of the file that are one, a `split`, `iter::once`, or a `chain` with one of these on either side (`for x in [a, b]`, `for k in KINDS`, `for i in 0..8`, `for copy in 1..=MOST`, `for part in text.split(',')`, `for phase in steps.iter().chain(once(&last))`, through `iter`, `enumerate`, `map` and the like) counts only its entry, not a zero-iterations outcome it can never take, as a JavaScript loop over an array literal already does.
- A Rust run of other tests than the run before it (`cargo test -- other_test`) reuses that run's instrumented build; anything after the command's `--` used to make Supercov instrument and build the project again.

**Fixed**

- A browser suite with no JavaScript source whose tests drove servers in two languages, such as Puma beside a Python server, published a run per server that named neither of the others: `runs <id>` said nothing of the run beside it. Each names the other now, as a browser run and its servers' runs already did.
- `supercov -- cargo test` ran the tests without a target runner Cargo is configured with when Supercov could not read the runner, such as one that leads to the crate itself, and they passed where plain `cargo test` fails. The run is refused, naming the configuration, as `cargo nextest run` already was.
- A Rust run ignored `SUPERCOV_COMMAND_TIMEOUT_MS`: a hung test kept it running for good, and a value of it or of `SUPERCOV_DIAGNOSTIC_INTERVAL_MS` that is no number of milliseconds was not refused. It now refuses such a value before starting anything and keeps the deadline as other languages' runs do: past it, the tests still running are stopped, no other is started, what ran is published (nothing, where no test reported), and the run exits `124`.
- An assertion question about the last value of a Kotlin `if` or `when` branch that ends a lambda whose value is used (`val x = run { if (c) { log(); 4 } else 5 }`) asked what happens if the value is skipped, a change that would not compile; it asks what happens if the branch yields the default value (null, 0 or false) instead, as for any other branch value. A lambda whose value nobody takes (`run { .. }` on its own) is asked as before.
- A Cargo runner, compiler or wrapper configured by a relative path out of the workspace (`runner = "../tools/run"`) was refused on Linux and macOS and accepted on Windows. It is run from where the path leads on every platform, as Cargo runs it; a path that leads back into the workspace is read from the copy, and one that is the workspace itself is still refused.
- `runs <run> assertions` panicked when the run's saved assessment was valid JSON but no object, as a file edited by hand or replaced can be; such a file is read as no assessment, as one that is not JSON already was.
- Supercov panicked when its output stopped being read part way, as `supercov -- npm test 2>&1 | head` or `supercov runs latest gaps | head` stops reading it; it goes on without printing what is no longer read.
- A JavaScript or TypeScript file Supercov could not parse was reported one column to the right of the error.
- `security` read a line question the provider refused as finding nothing on those lines; their lines are counted as not checked one by one, as lines whose question did not fit already were.
- A `--json` page cut to fit one reply could still be refused as too large: the note saying where the rest starts was added after the page was fitted. The page is fitted with its note.
- A container time from the Docker daemon with anything but digits after its seconds made the Docker relay panic while reading it; it is read as no time.
- A test command that could not start says whether there is no such program or it may not be run, before the system's own words, and exits `127` or `126` by that on every platform rather than by the message's wording; a step of Supercov's own that the system refused no longer reads as a file that may not be run.
- A Kotlin file with a class body on one line (`class C { val x = 1 }`), two members on one line, or a line starting with an identifier that begins with a keyword (`set_value = ...`) was read as not parsing and left unmeasured; Supercov now builds the Kotlin grammar itself with upstream's unreleased fixes for these. A file ending inside an annotation could stop the run for good; it now ends.
- Kotlin files using context parameters, `when` guards, multi-dollar strings, `suspend { }`, `fun interface`, `actual constructor`, annotated accessors, and two dozen other constructs were read as not parsing and left unmeasured. Supercov now keeps its own copy of the grammar and checks it against every Kotlin file of six real projects (Exposed, kotlinx.coroutines, ktor, moshi, Now in Android, OkHttp): 165 of their 5,429 files were left unmeasured, now 3, two of them templates that are not Kotlin.
- `assertions assess` and its `--dry-run` estimate took minutes before printing anything on a run whose tests share most of what they run, as a merged or end-to-end run's do: each request and each cache key worked out its test's code and its files' functions again. They are worked out once, and the requests are the same: 400 tests sharing 6,000 statements went from 166 s to 4 s on one core.
- A measured `wrangler dev` whose wrapped configuration could not be written left the files it had written beside the configuration; they are removed, and the configuration is used as it is.
- `runs <run> assertions assess --changed` said nothing of requests Jev did not answer: with every request failing it printed the request count and exited `0`, leaving `tests affected` nothing to go on. It now ends with why when none was answered, and says how many failed, their statements not asked, when some were.
- A Rust expression statement behind a `#[cfg]` the target leaves out (`#[cfg(windows)] continue;` on Linux) was counted as a line no test runs; it is left out, as a `let` or an item behind one already was.
- `merge` left out a run whose test files had changed and whose record of its files could not be read without saying so; it names the run and why, as for every other run it leaves out.
- `SUPERCOV_SOURCE_ROOTS` naming several roots that do not exist said "a, b does not exist"; it says "do not exist".
- `SUPERCOV_SOURCE_ROOTS` naming a directory outside the project was refused as "not a regular file or directory"; it is refused as outside the project.
- `merge` refused a run that holds no record of its files, where the latest run holds one and nothing else differs, with an empty list ("differing domains: "); it names the record ("differing domains: recorded files").
- `import` said a file of the project that is not UTF-8 text was "no such file in the project"; it says the file is not a text file.
- A browser suite whose `SUPERCOV_SOURCE_ROOTS` named only its JavaScript was refused when the project also held Go, Java or Kotlin source outside those roots (`SUPERCOV_SOURCE_ROOTS=src left no Go source to measure`). Such a server is left unmeasured, as a Python or Ruby one already was.
- Supercov panicked as it started when its own path was not UTF-8 text, as a directory name on Linux can be, and when an argument was not. It starts, and refuses such an argument by name; a `cargo nextest run` that has to name Supercov as its runner says the path is not UTF-8.

## 4.20.3

**Fixed**

- `runs <run> files` and `gaps` listed a file the run left out, such as a Worker no test ran or a module no build compiled, as covered; they say it is not measured, and why.
- `scope` and the run's summary called a run complete whose Worker no test ran; that Worker leaves the run's measurement incomplete, as `runs check` already judged it.

## 4.20.2

**Changed**

- A Rust Worker no test of a run started, or whose tests loaded no build of its measured source, is not measured, instead of counting every line as one the tests missed: its files leave the run's totals, the file view gives the reason at the top, the run names them after the tests' output, and `runs check` refuses to judge a floor over such a run.
- A build made before the run is named in that reason: Supercov builds nothing itself, so a Worker the tests load from an earlier `build/` is built in the measured command or the tests.

## 4.20.1

**Changed**

- A Rust Worker that `cargo test` starts is measured whatever configuration the test gives `wrangler dev`, wherever it is and whatever its `main` imports: a test kit that writes its own configuration per test, with a module of its own that loads the built Worker, is measured as the crate's own configuration is. Supercov hands `wrangler dev` a copy of the configuration beside it, removed when wrangler exits, and leaves every configuration as it was written.
- `SUPERCOV_RUN_ID` is set in every process a measured Rust, Go or Java/Kotlin command starts, as it already was for JavaScript, Python and Ruby, and is documented as the way a test knows it is measured.

**Fixed**

- A test that copied a Worker crate's `wrangler.toml` as a template for its own got a `main` naming Supercov's entry, which wrangler could not find beside the copy; the crate's configuration is no longer rewritten.
- Where tests started `wrangler dev` and no measured build of a Worker crate ran in it, its files name each `wrangler dev -c <path>` the tests started and why one was not measured, instead of saying no test started a Worker.
- wrangler's `.wrangler/` state in the copy was synced back into the project; it stays in the copy.
- `cargo test -- --nocapture` under Supercov showed nothing a test printed; each test runs without capture and its output is shown as it is written.

## 4.20.0

**Added**

- Cloudflare Workers in Rust go further: code in a Durable Object is measured and credited to the test whose requests reached it, and every Worker of one `wrangler dev -c a/wrangler.toml -c b/wrangler.toml` session is measured, read through workerd's own inspector. A Worker crate is measured as worker-build compiles it (wasm, a release unless `--dev`, with its features), counts whatever `cargo test` compiles, and runs that started different Workers merge.
- JavaScript and TypeScript code under no source root that first-party source imports, such as a `shared/` folder beside `src/`, counts as source with no `SUPERCOV_SOURCE_ROOTS` needed.
- Kotlin `for` loops and Go loops that name only a condition record their branch and MC/DC outcomes.

**Changed**

- A bug in Supercov's JavaScript or Rust instrumenter on one file no longer fails the run: that file runs as written, and the run says it was not measured and why.
- Queries no longer re-hash evidence that has not changed since it was verified (0.07 s instead of 0.30 s on 150 MB), and reading a run's record of its source no longer loads the whole archive.
- A command ended by a signal exits 128 plus the signal in every language, one that cannot start exits `127` or `126`, and one that fails before any test ran keeps its own status.
- `minimize` without `--target` keeps everything the tests together cover.

**Removed**

- The old Rust frontend's scope fields (`unit`, `measurementComplete`), leftovers of the assertion-map confidence, Python evidence transport version 1, and cache locations only old releases wrote.

**Fixed**

- Ruby files loaded after Bootsnap 1.26.1's setup ran without Supercov's probes, so a Rails app counted no branches; Supercov's loader stays in front. Code called inside a `Ractor`, and a local first assigned with `||=`, no longer fail under measurement.
- Java and Kotlin conditions that open with a comment, `instanceof` pattern conditions, TestNG tests beside the JUnit Platform, and Gradle builds whose root holds only `settings.gradle` are measured and compile.
- `make test` and other wrappers around Go, Gradle or Maven no longer receive flags the wrapper refuses.
- Rust: a Cargo configuration's `build.rustflags` apply again when `RUSTFLAGS` is unset, `cargo +<toolchain> test` builds doctests with that toolchain, a runner named with a backslash is found on `PATH`, an attributed boolean `let` is instrumented, and a workspace's `exclude` and one-line `members` are read correctly.
- `tests affected` follows package.json subpath imports (`#lib/x`), imports of a package's own directory and aliases from an extended tsconfig, and a runner configuration added since the run reaches only that runner's tests.
- `security` reads files near the request size limit in windows, names the paths and callers it did not ask about, and no longer reports paths through default imports or aliases that do not reach the function; `quality` and `security` refuse a `--run` that names no run before asking anything.
- Damaged indexes, evidence archives, region files and browser driver replies claiming enormous sizes are refused instead of aborting, allocating or running for a long time.
- Projects holding a symbolic link loop can be measured.
- Many query answers are clearer: pages past the end say so, counts of one are singular, stale warnings name only the files read, `diff`, `kinds`, `runners` and the summary honour `--kind` and `--runner`, unknown words and flags are refused by name, usage mistakes under `--json` answer in JSON, and `file` and `line` give the scope's reason for a file the run left out.

## 4.19.0

**Added**

- `wasm-pack test` is measured: `--node`, and `--headless --chrome`, where Supercov reads each test's page through the driver before the runner closes it. Each wasm test is credited its own lines and decisions. Tests run in Firefox, Safari or Edge are run and reported as not measured.
- `cargo nextest run` measures a Rust Worker its tests start with `wrangler dev`, as `cargo test` does.

**Changed**

- A Rust Worker crate no test ran says why in `gaps` and the file view: no test started it with `wrangler dev`, or a test started it with `wrangler dev --remote`, where it runs on Cloudflare's edge and cannot be read.

**Fixed**

- Files a project's `.gitignore` ignores, such as `.zig-cache/` build output, no longer make Rust runs stale.
- A container a test created through Supercov's Docker relay could lose its note (for example that its image was built from an earlier copy) when the client finished quickly.

## 4.18.0

**Added**

- Rust Workers are measured. A Playwright suite that starts one with `wrangler dev` credits each test the Worker work done while it ran. A `cargo test` test that starts `wrangler dev --local` itself is credited exactly what its Worker ran, and the Worker crate counts though `cargo test` never compiles it.
- Rust compiled to wasm is measured: `cargo test --target wasm32-unknown-unknown` under `wasm-bindgen-test-runner`, `--target wasm32-wasip1` under `wasmtime` or `wasmer`, and wasm-bindgen crates that node:test, Vitest, Jest or a Playwright page load, each test credited its own lines and decisions.
- Containers started by any Docker client are measured, not only Compose's: `docker run`, Testcontainers (Ryuk included) and Docker SDKs go through a relay Supercov puts on `DOCKER_HOST`.
- `supercov import <tracefile> --test <name>` adds line coverage recorded where Supercov cannot run (an LCOV file) to a run, as a new run. It is credited by line only, to a test of its own or to the background. An LLVM profile is exported as LCOV with LLVM's tools first.

**Changed**

- Rust code a `#[cfg]` leaves out of every build (a feature that is off, `cfg(loom)`, a build script's `--cfg`) is outside the denominator instead of counted uncovered. Rust probes record into one mapped region per process, and covered counts are unchanged.
- Assessment answers survive dependency and configuration changes. A changed lockfile, `package.json` or test configuration asked the whole suite again; now only questions whose text changed are asked. Answers saved by earlier releases are asked once more.
- A multi-location `line` query answers every location it can and lists the rest under "Not answered" instead of failing.
- `line`, `file`, `decision` and `source` warn only about files they read that changed since the run, and name them. A page cut to fit one reply says where the rest start. `patch` on a failed suite still shows the change's coverage.

**Fixed**

- A JavaScript program that hands itself over with `process.execve`, or a Ruby one with `exec`, lost everything it had run; it is kept now.
- `cargo test` failed in a workspace with a `cdylib`-only or binary-only crate.
- Supercov returned the instrumented Worker that `wrangler dev` built in its copy to the project's `build/`, where a build that embeds it would ship it. It stays in the copy.
- wrangler's `.wrangler/` state and LLVM profiles (`default.profraw`) in the project no longer make Rust runs stale.
- The file view of a file the run never measured said "every line is covered and measured"; it now says nothing was measured.
- A filtered `assertions` reading counted statements none of the picked tests was asked about as not asserted, and showed 0% for test files whose tests assert. They are now counted apart.
- The judge misread Jest and Vitest null checks: `toBeNull()` fails on `undefined`, so a `return null` changed to `undefined` is caught.

## 4.17.2

**Fixed**

- Supercov could change what a Python program computed. A value made of three or more `or`/`and` operands (`path = given or os.environ.get("X") or shutil.which("x")`) came back as `True` instead of the operand, in every measured file after the first one with decisions, so code under test took other paths and tests failed that pass without Supercov.
- A Jest project whose `jest-config` package sits only under `jest-cli` or `@jest/core` (npm's layout beside `next/jest`) ran under Jest's defaults: its configuration was not read, so tests it leaves out (`testPathIgnorePatterns`) ran too. It is now read through the Jest that runs.
- A Jest test that opens with a docblock (`/** @jest-environment jsdom */`) ran in Jest's default environment ("window is not defined"): Supercov's loader went in front of the comment. It now goes after it, on the same line.
- A measured run left generated scripts in `.supercov/workspaces/`, so a project whose tests lint `.` (`eslint .`) failed after it. The copy now keeps only its caches.
- `runs clean` during a run that had just started said "could not acquire the Supercov project lock"; it now says a run is active.
- `--limit 0` lists every entry in `runs <run>` queries (`file`, `gaps`, `files`, `line`, `test` and the rest), as it already did for `assertions`, `quality` and `security`.

## 4.17.1

**Fixed**

- Assessment asked only one test about a statement at a module's top level (`const limit = 10;`, `const router = express.Router()`): it runs once, when the first test loads the module, and was recorded for that test alone, often one an auth check turned away, so it read "1 of 1 asked, not asserted". Every test that runs the module's functions now counts as running it too, after the test that loaded it. JavaScript, TypeScript and Python.

## 4.17.0

**Added**

- `assertions` and `assertions assess` take `--kind`, `--runner`, `--tag`, `--title` and `--test-file`, as every query does. `assess --tag @e2e` asks only those tests about the statements they ran (`--dry-run` estimates just that) and saves the result apart from the run's whole assessment; `assertions --tag @e2e` reads what those tests assert.
- `tests affected` takes the same filters: from a run that merged two runners' suites, `tests affected --names --runner playwright` lists only Playwright's affected tests, for its `--grep`.

**Fixed**

- A Next.js project whose `next.config` sets `turbopack.root` to its own directory could not run Turbopack under Supercov (`Next.js package not found`, or a `node_modules` link "out of the filesystem root"): in the copy that root excluded the dependencies it links to. The root is now widened to hold them.
- A node:test suite that mocks modules (`--experimental-test-module-mocks`) and maps import paths with a preload that patches Node's resolver (`--import tsx` 4.20, tsconfig `@/` paths) failed under Supercov with `Cannot find module '@/...'`: Supercov loaded node:test before the command's own preload, and Node's mock loader kept the resolver it found then. The adapter now loads when a test first asks for node:test.
- `assertions assess` refused a checkout that lacked any file the run had read, such as a git-ignored `.env` only CI generates. It now checks only the files its questions quote, each assessed statement's source and each test's own file, and still names any of those that changed.

## 4.16.0

**Fixed**

- Django projects are recognised: `supercov -- python manage.py test` measures Django's runner like unittest, each test credited with its own coverage, in projects that declare themselves only by `manage.py` and `requirements.txt`.
- A Rust test run through a package script keeps its arguments: `supercov -- npm test -- adds` with `"test": "cargo test"` ran every test. The command is also split the way `sh` splits it, keeping empty arguments and backslashes in double quotes and joining line continuations.
- Assessment shows each test its own code when tests in one file share a name: Python classes (`TestCart::test_total`, `TestOrder::test_total`), Minitest classes, JUnit `@Nested` classes, Rust inline modules and JavaScript `describe` groups.
- A Rails-style Ruby test whose first word has four letters (`test "adds numbers"`) is found for assessment; it was read as a numbered minitest/spec example.
- A Next.js route handler at the root of `app/` (`app/route.ts`) ties what it runs to the request that ran it, as deeper handlers do.
- A Rust condition in an array length runs at compile time and is declared unmeasured, like a `const fn` body, instead of counting as a gap no test could close.
- Rust assessment no longer asks about changes that cannot compile or change nothing: a raw pointer held in a local becomes `std::ptr::null()`, a closure with a return type keeps its braces, `0.0` becomes `1.0`, `None` becomes `Some(Default::default())`, and a value spelled with its path counts as the same value.
- A Python test deep in its file is sent with `...` only where lines are left out, not between adjacent decorators.
- A Java project with two build files is told once, not twice, that its build stopped failing on warnings.

## 4.15.0

**Changed**

- `--tag`, `--title` and `--test-file` leave tests out with a leading `!`: `--tag '!api' --tag '!cron'` reads the tests with neither tag, the browser tests of a suite whose others are tagged, from the one run.
- Upgrading Supercov re-indexes a stored run on its first query only when the analysis that writes its index changed, not on every release: a release that changes only instrumentation, runners or queries reads a 1,100-test run in 3.6 s, not 23 s.
- Re-indexing a run builds its three views (all, passed, failed attempts) one at a time and parses each test's record as the evidence is read, so the decompressed evidence and the three views are never held at once: on a run of about 3,000 tests the first query peaks at 6.5 GB instead of 8.1 GB, in the same 12 s, with the same index.

**Fixed**

- `gaps --format jsonl` opened the run and checked the checkout again for every file: 129 s for 17,454 rows of a 1,100-test run. It reads the run once, at 5 s, with the same rows.
- `line` with many locations in `--json` answered as many as fit one reply and listed the rest in `remaining` only, so a pager following `nextOffset` stopped after the first reply, and a location given twice was answered twice. The reply now pages as every other query does (`pagination.nextOffset`, `--offset`), each location once.
- A server whose `emit` something else wrapped before Supercov loaded (a preload that put its own `emit` on `http.Server.prototype`) hid its requests from Supercov: what ran on a request's own events, a body read on `end` or a page rendered as the response went out, was credited to no test. Supercov now also reads each request from `diagnostics_channel`, which nothing hides.
- A Playwright test read Supercov's own cookies: `context.cookies()` listed the cookies that carry the test's scope, so an assertion on cookies failed only under measurement, and `storageState({ path })` wrote them into the suite's saved login state. Both now give the suite's cookies alone.
- The run a missing MC/DC condition `needs` could be one its decision cannot produce, such as `a` true and the decision false for `a || b`. It is now worked out from the decision's own `&&`, `||` and `!`: one run, or the pair when no test evaluated the condition, that shows the condition deciding by the rule Supercov counts by.

## 4.14.0

**Changed**

- A run reads back for some of its tests: `--tag`, `--title` and `--test-file` narrow every `runs <id>` query and `gaps --format jsonl` to the tests they pick, from the run already measured, with no second run. The first query with a filter works it out from the run's evidence and keeps it beside the run (about half a minute on a 1,100-test run); the test run itself does no more than before.
- Supercov records each test's tags: Playwright tags and `@word`s in JavaScript test titles, pytest markers, RSpec tags and `type`, Cucumber tags, JUnit `@Tag`s and TestNG groups. A tag naming a level (`@unit`, `@component`, `@integration`, `@e2e`) sets the test's kind, ahead of its path. `runs <id> test` lists a test's runner, kind and tags.
- `runs report`, `runs check`, `runs patch` and `supercov report` take `--kind`, `--runner`, `--tag`, `--title` and `--test-file`, and read only those tests: an `lcov` file of the E2E tests alone, a floor for them, or whether they reach a change.
- An RSpec example or Cucumber scenario is titled by its description, so `--title` finds it, though its identity stays the `spec/cart_spec.rb[1:2]` position.
- `runs <id> line` takes several locations in one call (`line a.ts:10 a.ts:20`), or a file of them with `--from-file`, and reads the run once for all of them: on a 1,100-test run twenty lines cost about what one does. Each line lists its state, its gaps and its first three covering tests; asked alone, a line lists them all. Its `--json` reply is `coverage.lines`, and locations that do not fit one reply are listed in `remaining`.
- An MC/DC gap names the value each missing condition never took: `` `sum < 50` was never true `` in `file`, `line` and `decision`, and `missingOutcomes` (`["true"]`) in their `--json`. `file --group decision` lists the missing conditions under each decision, with the run that would show them deciding.

- A query of a stale run names the files that changed since it, tests and source, those added included, beside "test files changed".
- A page that stops short gives the command that shows every entry at once (`all 61 at once: ... --limit 61`), and `--help` says what `--limit` does.

**Fixed**
- A header a Playwright test set with `context.setExtraHTTPHeaders` (an `X-Forwarded-For`, say) did not reach the app under measurement: Supercov set the context's headers back to those it was created with at every step. A test's own headers are now left alone.
- What Python and Ruby ran while loading, before any test, was counted in the `unit` row of `kinds` and `--kind unit`. It is now in no kind's row, as it is for JavaScript, so `--kind unit` and a tag picking the same tests agree.
- Re-indexing a run, after an upgrade or for a new filter, waited seconds to free the analysis it had written: 30 to 45 s became 18 to 25 s on a 1,100-test run.

## 4.13.0

**Changed**

- `&&` and `||` used as a value are MC/DC decisions in every language. In Java, Kotlin, Go and Rust the whole expression is the decision (`ok = a && b`); in JavaScript, TypeScript, Python and Ruby the compound left operand is (`(a && b) || fallback`, `a || b || "default"`), and so is the operand of `!`. A Kotlin operand that smart-casts the rest keeps branch outcomes; a Go module older than 1.18 declares a limitation. MC/DC totals rise where such expressions exist.
- An MC/DC gap says what run would close it: `line` and `file` JSON gain `neededVector` and `needs` (`TF -> F`, "`a` true, `b` false, decision false"), or say no test evaluated the condition.
- Go `range`, Java for-each and Kotlin `for (x in xs)` loops record whether they ran zero times or at least once, as other languages' loops do.

**Fixed**

- A test helper written as a one-line arrow that returns a check (`const loginError = (page) => expect(page).toHaveURL(url)`) returned nothing once measured in TypeScript, so `await loginError(page)` went on before the check finished: the check then ran against the next page, or a failure went unreported. The arrow now returns the check.
- A failed JavaScript test was blamed on missing build output whenever the project had a build script. The run now names the failed tests and points at the build only when no reported test failed.
- `runs <id> line` and `file` decoded the whole run to answer for one file: 3 to 5 s per query on a 1,100-test run, 13 s cold. They now read one file, 0.7 s on the same run, same answers.
- A `--json` page larger than the 64 KB reply limit was refused, so a pager could stop after its first page. It now returns the entries that fit, with `nextOffset` after them.
- Builds that failed only under measurement: Go conditions on a named boolean type (`type Flag bool`); a Go `//go:build ignore` file of another package beside the real one; Kotlin conditions smart-cast by `isNullOrEmpty()`/`isNullOrBlank()`; Rust files where `vec![x; n]` starts a longer expression; React Router/Remix server entries with `export default handleRequest;`.
- `cargo test -q` on an edition 2024 crate with doctests failed ("merged doctest child 0 ran for a case the harness did not report"); quiet output now matches Cargo's.
- A pytest `xfail` that passed read as failed; it is a pass, as pytest counts it (a strict one still fails).
- A Python `for` whose `else` returns never recorded zero iterations; a Ruby ternary on a literal made its file unparseable to the probes.
- `supercov docs reports` was missing from every installed copy.
- On Linux, "command still running" listed each thread of a process as a process of its own and named Node `MainThread`.
- `runs <id> decision file:line:column` ignored the column it lists.
- `merge` dropped runs it could no longer fingerprint without a word, said "another build of Supercov" for runs of two languages, and now counts files added since a run.
- The whole-suite hint told `npm t` and `npm run -s test` to run `npm test`.
- Assessment asked whether a no-op change (`Vec::new()` to an empty value, `return nil`, `x = null`, `count := 0`) was caught, so those statements always read as not asserted.
- A method read as text through a property (`box.method.toString()`) was instrumented, changing its text.

## 4.12.2

**Fixed**

- `supercov runs <run-id> gaps --format jsonl` refused every run too large for one JSON reply, the runs it exists for, with the reply's 64 KB message. It now writes every obligation however many there are.
- A test command that ran `cargo build` in the copy (a test that builds the CLI it drives) had that build synced back over the project's `target/`, so the project's next build linked crates with Supercov's probes in them. What Cargo builds in the copy now stays there.
- A function whose source the code reads (`f.toString()`, `String(f)`, a template) returned the text Supercov printed instead of what was written: `function f() { return 1; }` read back as `function f() {\n\treturn 1;\n}`. Such a function now keeps its text exactly, comments and spacing included.
- The GitHub Action removes the file it reads the run's id from. A self-hosted runner, whose temporary directory is not emptied between jobs, kept one for every step.

## 4.12.1

**Fixed**

- Playwright tests that share one page (a serial describe that opens it in `beforeAll`, a worker-scoped page) were each credited with everything the tests before them ran in that page, and with the `beforeAll`'s own work. Each test now gets only what ran during it; what ran before it counts in the run as no test's.
- An iframe the page removed (a client-side navigation away from the page that held it, an embed closed) took everything it ran with it: a test that clicked in it was credited with none of it. Its evidence now reaches the test as it goes.

## 4.12.0

**Added**

- A change to a mock server affects only the tests it served. Each Node process of a Playwright run notes the project files it loads and reads and the tests it serves, so `tests affected` names those tests, and `merge` takes the suite's run with only them measured again, leaving their old records out. A mock change meant measuring the whole suite again; one to a mock's data file went unseen.
- `supercov runs <run-id> gaps --format jsonl` writes every uncovered obligation, one JSON object per line, with its file, line and source: every MC/DC condition of a run in one pass, without paging.
- `supercov diff <older> <newer> --gained` lists only what the newer run covers that the older did not: a test measured alone, against the suite's run, shows what it newly covered.
- The CLI guide shows how to merge runs measured on two machines: each measures part of the suite, and one job merges their runs.

## 4.11.1

**Fixed**

- A Playwright test could not answer a native dialog (`window.prompt`, `confirm`, `alert`) it waited for: `dialog.type()` returned a promise instead of a string, and `accept()` or `dismiss()` never finished, so the page could not be closed. A dialog is now Playwright's own object, as without Supercov.

## 4.11.0

**Added**

- Browser tests of a server `docker compose` builds measure it: Node, Python, Ruby, Go, Java and Kotlin. The image is built from Supercov's copy, and Supercov's `docker` gives each service built from it the run's settings and the project's `.supercov`, mounted at its path, so each test is credited its own work as for a server started directly. Those services are built first, so an image left from an earlier run is never used, and the containers are stopped, not removed, when the command ends. A Python or Ruby image that moves the code (`COPY backend/ /srv/`) is matched to the copy by its files' paths.
- A project whose Playwright is installed beside its config alone (`e2e/node_modules`) can be measured from its root, where its server is built. Every worker failed ("Cannot find package '@playwright/test'").

**Fixed**

- TypeScript in a project that builds images stopped at its first probe wherever Supercov's runtime was not installed ("reading 'mcdcBegin'"): a TypeScript server built in a container, or pages an image's build prerenders. Since 4.0 its files carried no copy of the runtime, as JavaScript files do; in such projects they carry one again.

## 4.10.1

**Added**

- Browser tests of a Java or Kotlin server built by the test command (`./gradlew bootRun`, `mvn spring-boot:run`, `javac`) measure it as they do a Go server: what it ran is swept every 5 ms and credited, by time, to the browser test running then, if only one was.
- Python and Ruby tests are given a kind by where they live, as JavaScript tests are: a Rails app's `test/system` and `spec/features` tests are end-to-end, `spec/requests` and `tests/integration` integration. Every one was a unit test.
- A Go, Java or Kotlin server's run says how many browser tests had its work only while another test ran too, as under parallel workers, and so were credited none of it, and that `--workers=1` credits each its own.

**Fixed**

- Browser tests of a large instrumented frontend ran twice as long as without Supercov (76 tests of stemdeck: 2.4 min against 1.2; now 1.8). Each page's evidence was read back at the end of every test as tens of thousands of separate values, half a second a page; it is now read as one piece of text, 13 times faster. Each directly served file also carries a third less of Supercov's runtime, which a browser fetches once per file at every test's fresh context, and a page stores its evidence only when it is leaving.
- A click whose handler sent the page to another origin (`window.location.href` to a sign-in or masquerade URL) recorded nothing the handler ran, though the test passed only because it ran: a page's evidence survived a navigation only through the origin's localStorage. A leaving page now hands its evidence to the collector.
- A Ruby run with a thread of no test's own beside the tests (Action Cable beside Capybara's Puma in Campfire's system tests) could not be published ("references undeclared context"): two threads could each open an evidence file, and a new test phase was readable before it was declared. A test phase that ended while that thread entered a loop could also raise in the test's thread, or crash Ruby 4.0.
- A Rails app measured no branches or conditions: Bootsnap, which `config/boot.rb` loads, runs each file from its compiled cache, past the loader that adds Supercov's probes, so only lines were counted (0.08% of a Rails server's branches against 1.9%). Measured Ruby now runs with Bootsnap's compile cache off; its load-path cache stays.
- A Python project's virtual environment, linked into the run's copy, was synced by `uv run` there: its editable install was pointed at the copy, and a `hatch-vcs` version file regenerated and came back as a change, so the server's run read as stale at once. The copy now runs with `UV_NO_SYNC=1`.
- A Rails app using Sprockets served no assets under Supercov: its `app/assets/config/manifest.js` had code put above its `//= link` directives, which Sprockets reads only from the top. Directives stay first.
- A package whose `tsconfig.json` includes a sibling (`../packages/ui/src`) was refused ("source root is not a regular file or directory"). Such a root is outside the project and is left out.
- A package inside a larger repository ran in a copy of that package alone, so anything its suite reached as `../` was missing: a Playwright `webServer` running `go run ../cmd/testserver`, a tsconfig extending `../../tsconfig.base.json`. The copy now sits at the package's path in the repository, with the repository's other entries linked beside it from the checkout. Only the package is copied and measured.

## 4.10.0

**Changed**

- Published to PyPI, RubyGems and crates.io only. npm reported one of its native packages published and kept it staged instead ([npm/cli#9889](https://github.com/npm/cli/issues/9889)), a version that can never be published, so 4.10.1 is this release on every channel.

## 4.9.0

**Added**

- Browser tests of a Python server measure the server. When a Playwright suite starts a Python program -- through `webServer`, a fixture or a test -- `npx supercov -- npx playwright test` measures it, and each request runs as the test that sent it: the standard library's HTTP server, Flask, Django and FastAPI, under their own servers, gunicorn or uvicorn, streamed responses included. A request that names no test, such as a webhook, is credited by time as it is for a Node server. A project with no JavaScript source was refused ("No application source files were discovered"); one with a Node server too gets the server's Python run beside its browser run, and `merge` merges each.
- Browser tests of a Ruby server measure it the same way: a Rack app -- Rails, Sinatra or any other -- under Puma, WEBrick or another server, each request run as the test that sent it, a webhook credited by time, and the server's Ruby run published as the run or beside the browser run.
- Browser tests of a Go server started with `go run` measure it: the copy's Go is rewritten with probes, and what the server ran is swept every 5 ms and credited, by time, to the browser test running then, if only one was.
- A server a Python or Ruby suite runs in its own process -- a pytest-django `live_server`, Django's `LiveServerTestCase`, Capybara's Puma under a Rails system test -- credits each request to the test that process was running, by time. Its requests counted as no test's, or all as the first test's that started the server's thread.
- A browser run's summary names the runs of the servers its tests drove, and each server's run names the browser run (`companions` in JSON). The pull-request comment Action reports each server's run in a section of its own, and lists a changed file as not run at all only when no run measured it.
- `merge` reads what Ruby tests require (`require_relative`, a literal `require` found in the project, `autoload`, and what `.rspec` requires), so a changed Ruby helper leaves out only the runs whose tests load it. Every Ruby helper counted as loaded. A glob of support files or a change to `$LOAD_PATH` still counts every one.
- A Python helper that only gained functions leaves every run in, as a JavaScript one does: a plain `def` that nothing written before names, with any import only it uses. A decorated function counts as a change, since a decorator runs when the module loads.

**Fixed**

- A virtual environment inside the project was copied into the run's copy without its interpreter, whose link points outside the project, so a server started as `.venv/bin/python` did not start. The copy uses the environment itself.
- A Python or Ruby process started from a measured Node process reported "SUPERCOV_CONTEXT was not a valid Supercov identity" as a limitation. It reads the Node process's test now.

## 4.8.1

**Added**

- A request that names no test is credited by time with parallel workers too, where each worker's tests have an app server of their own: only the tests a server served count for it. It was credited only when one test was running anywhere.

**Changed**

- `tests without-evidence` says of a test whose file imports no measured source that it does (`importsMeasured` in JSON): its assertions can only have checked test code, mocks or fixtures. The summary's warning names that cause first.

**Fixed**

- A runner's configuration named in the command (`playwright test --config e2e/playwright.config.ts`, `vitest -c ...`, `jest --config ...`) was ignored when the project had one at its root, so the run loaded the root configuration and ran other tests than the command did.

## 4.8.0

**Added**

- A request to your app server that names no test, such as a webhook from a mock Stripe server Supercov does not measure, is credited to the test that was running when the server handled it. It counted as no test's, and the tests whose only coverage it was were flagged as recording none. A test is credited only when it was the only one running then, and the run's `Attribution` line and the test's own page say it was credited by time.

**Changed**

- `merge` names a run recorded before 4.7.0 as recorded by a Supercov older than 4.7.0, on the day it ran, instead of "an earlier Supercov". Runs record their release from 4.7.0 on.
- `supercov --help` lists the settings a measured command reads from its environment (`SUPERCOV_SOURCE_ROOTS`, `SUPERCOV_TEST_KIND`, `SUPERCOV_RUN_ID_FILE`, `SUPERCOV_SOURCE_TOOLS`, `SUPERCOV_KEEP_WORKSPACE`). They were described only in the CLI guide.

**Fixed**

- A Node server handler that reads the request body (`req.on('end', ...)`) ran as no test's: Node emits a body's events outside the handler's context. The request and its response now carry the test's scope into them.

## 4.7.0

**Added**

- `SUPERCOV_RUN_ID_FILE` names a file the recorded run's id is written to, for a script that queries the run it just made. `latest` is another run when two commands record runs side by side. The GitHub Action uses it.
- `tests affected` answers for a merged run from the runs it was merged from. It refused, naming them.
- `merge` and `tests affected` read the imports of Python tests, with pytest's `conftest.py` files and `pytest_plugins`: a changed helper counts only for the runs and tests whose files import it.

**Fixed**

- `merge` refused Python, Ruby, Go, Rust and JVM suites run by different commands ("differing domains: integrity fingerprint"), and a merged run of those languages read as out of date. A run's fingerprint there holds its command; a merge is now checked with the command of the run it was merged into.
- A change to source left out of what is measured, such as a simulator outside `SUPERCOV_SOURCE_ROOTS`, made every run out of date for `merge`. It is now judged as a helper is.
- A page measured under Playwright could not fetch from another origin that allows only the headers it uses: Supercov added its coverage headers to every request, so the browser asked that origin first and was refused. Requests to other origins are now left as the page made them. The app hears which test a request belongs to from cookies on its hosts (the base URL's, `localhost` and `127.0.0.1`), which reach its frames on another of those hosts too, and from the headers on the page's requests to its own origin.
- `merge` of a run recorded by another Supercov release answered only "differing domains: instrumenter, …". It now says the run was recorded by an earlier release, which instruments differently, and to measure it again. Runs record the release that made them (`recordedBy`).

## 4.6.0

**Added**

- `merge` adds a run of new tests to the runs before it when the tests brought helpers or runner settings with them. It judges each file that differs by what the earlier run's tests did with it: a helper their files do not import, or one that only gained functions, leaves the run in; so does another runner's configuration, and a Playwright change to timeouts, retries, workers, reporters or which tests to run. A run left out is named with the files and functions that changed. The merged run lists what it crossed (`merged across:`).
- `merge --allow-changed` accepts the changes Supercov counts, when you know they alter nothing the earlier tests ran. Product code, dependencies and a run's own test files are never accepted.

**Fixed**

- `tests affected` reported no test affected when a helper the tests call changed, since helpers sit outside what is measured. A test is now affected by a helper its file imports, and every test by one Supercov cannot place.
- A Playwright configuration change made every unit test affected and every Vitest run out of date for `merge`, and a Vitest one did the same to Playwright runs. A runner's settings now count only for runs that used it.
- `merge` with no run named left out a run whose source had changed without saying so. It now names it and the changed files.

## 4.5.0

**Added**

- A GitHub Action: `uses: supercorp-ai/supercov@v4` with `command: npm test` runs the tests and comments on the pull request which changed lines no test ran, with the run's line, branch and MC/DC coverage. One comment kept up to date, the lines marked in the diff, optional floors, the same report in the job summary. Every supported language; no account, token or server.
- Runs of one project can be started at the same moment. A second `supercov --` was refused with `already active`, because every run rebuilt the project's one copy. Each run takes a copy of its own (`workspace`, then `workspace-2`), held by a lock of the operating system. A run alone has the first copy, as before; a copy past the first is removed a day after its last run. `runs clean` is refused, with status 2, while a run is active.
- `supercov merge` with no run named merges the runs recorded on the source as it is now: the newest of each command, leaving out merges. A script no longer has to find what each run was called.
- `merge` adds a run of a new test alone to the runs before it. It refused them (`differing domains: tests`) when only a test file had been added. A run is left out, and named, when a file of its own tests or a file tests load has changed since.
- The LCOV export has LCOV 2's `MCDC`, `MCF` and `MCH` records. Read back by lcov 2.6, its condition total is the run's MC/DC.

**Changed**

- `supercov report` of a large run takes seconds: 10 min 25 s for a run of 2,000 tests over 300 files, now 19 s, and the same report byte for byte.
- A JSON reply too large for one answer says how many of the entries asked for fit, and to follow `pagination.nextOffset`.
- Suites started from one command, each with its own `SUPERCOV_TEST_KIND`, are one run with every kind. This worked and is now documented.

**Fixed**

- A test command reads the keyboard on a terminal (macOS and Linux). A debugger at a breakpoint, a watch mode or `npx` asking to install never got what was typed: the command's first read of the terminal stopped it. Ctrl-C then reaches the command and still ends the run as interrupted; Ctrl-Z stops both.
- What Go, Maven and Gradle print names the project on a terminal too (macOS and Linux). A Go compile error read `../../../../price.go:4:28`, and Maven and Gradle named a report in Supercov's copy.
- `runs patch` in a project kept below the top of its repository counts a new file not yet added to git, and `--annotate github` names each file from the top, where a workflow looks for it.
- `return undefined as T` is no longer read as a statement no test asserts.

## 4.4.0

**Added**

- `supercov diff` says which files a difference is in, the one that lost most first. In `--json`, `files` and `byFile`.
- `assertions --weak` lists the statements asserted on an answer under 75%, the least certain first.
- A statement no asked test catches names the test files none of whose tests were asked.

**Changed**

- Go coverage no longer counts files the toolchain ignores (a name beginning with `_` or `.`) or the programs under a top-level `examples/`. chi reads 86.8% of lines where it read 77.5%.
- Python and Ruby coverage no longer counts top-level `docs/`, `examples/`, benchmark directories and `fuzz/`. h11 reads 98.2% of lines where it read 68.8%. A run made before this reads as stale; name a directory in `SUPERCOV_SOURCE_ROOTS` and it is source.
- `cargo test` prints what it prints without Supercov: every test, the failures together and one result for each binary and each package's doctests, on standard output, in colour on a terminal. Supercov still runs every binary, as `--no-fail-fast` does.
- `assertions assess` sends about half as much for JavaScript, TypeScript and Python: a request shows the function each asked statement is in, and no file the test imports. On eight projects it was 28 to 81% of the tokens and as accurate. The first assessment after upgrading asks everything once.
- No lint is reported on what Supercov builds for Rust, so `cargo nextest run` prints no warnings about probes. This costs one rebuild after upgrading.

**Fixed**

- `cargo nextest run` works with any nextest from 0.9.138. Only 0.9.138 and 0.9.140 were accepted, so the current release was refused before a test ran.
- `cargo test -- --ignored` runs on a package that has doctests.
- A Go failure names your file and your line, and a compile error its column: `./crypto.go:4:15` where it read `../../../../crypto.go:4:38`.
- A Rust panic and a doctest's name carry the line you wrote: `src/lib.rs:8:9` read `9:84`.
- A failure reads as it does without Supercov under Mocha, ts-node, `node:test`, AVA, tap 16 and 21, `unittest`, Minitest, RSpec and test-unit: your own lines, no frame of Supercov's and no path into its copy.
- test-unit runs a test the way it does without Supercov. Supercov's hook made every test take the path test-unit keeps for old scripts.
- What a Go, Maven, Gradle or Cargo command writes is in your project when it ends: `cover.out`, `target/surefire-reports`, `build/test-results`, a snapshot a test rewrote.
- What Maven, Gradle and Go print names your project and not Supercov's copy, when the output is not a terminal.
- A report a command writes names your project: under Vitest `coverage-final.json`, `clover.xml` and a Cobertura report's `<source>` named Supercov's copy.
- `runs latest scope` lists a Go, Rust, Java or Kotlin run. It answered that the run held no inventory.
- A Playwright failure names the line you wrote when the test file has not changed for three days. macOS had removed the source maps Playwright keeps in the temporary directory, and they are now kept beside the copy.
- `assertions --limit 0` lists every row, as it does for `quality` and `security`.

## 4.3.0

**Fixed**

- `security --run` no longer says a run's tests catch nothing when nobody asked them. Beside a run with no assertion assessment it printed "102 are executed by tests that are not judged to catch any line of them", and under almost every flagged file "no test is judged to catch a change to any line of it"; beside the same suite assessed the number was 5. A run that was not assessed now says so and names the command that assesses it.
- `quality` and `security` read every JavaScript and TypeScript file a coverage run measures. They kept path rules of their own, which called the App Router handler `app/api/wordpress/test/route.ts` a test, left `components/` and `proxy.ts` of a Next.js application unclassified, and second-guessed a root named in `SUPERCOV_SOURCE_ROOTS` by its name. Coverage's decision is now taken for the files it measures; generated and vendored files stay out.
- The most `assertions assess --dry-run` says an assessment can send is the most it can send. It counted one request for each test, where a test whose statements other tests also run is asked again each round: a suite of end-to-end tests was told "at most 404 requests" and sent 741. The figure is now the pass itself with every answer a no.

**Added**

- A merged run can be assessed. `assertions assess` refused one for holding "multiple source manifests", which were the same manifest once per run merged, so a project whose suites run as separate commands had an asserted share for each suite and none overall. Answers saved when the parts were assessed are reused. `tests affected` on a merged run names the runs to ask instead.
- `quality --dry-run` and `security --dry-run` print how many requests there are and what they cost, on stderr beside the requests themselves, and both commands end by saying what the assessment was charged.

## 4.2.1

**Fixed**

- `supercov diff` lists what was lost. Its text printed the count and then gains only: "lost: 2 lines, 12 branches" stood over 73 items that were all gains, and a comparison with nothing but losses had an empty body. Losses come first, marked `-`, then gains, marked `+`. `--json` always had both.
- The next-page command `diff` prints keeps `--kind` and `--runner`.
- Under Playwright with Chromium, what a page runs while it loads is counted for the action that loaded it. The page was never told its phase before it ran, so those hits were placed by their time, and one stamped with the millisecond `page.goto` resolved went to the `expect` that followed. In a four-test suite some lines did in 8 of 12 runs, and now and then all of a test's.
- A hit with no phase of its own, stamped with the millisecond one phase ended and the next began, is the ending phase's. Other browsers still place a page's first hits by time.
- Nothing of Supercov's is still being deleted when `supercov` exits. The copy a run replaces goes to `.supercov/.trash`, and it was unlinked only after the command had ended, by a process Supercov waited a tenth of a second for. A command started right after that walks the project could meet a directory on its way out: AVA, which looks for test files in every directory, failed on `EINVAL: invalid argument, scandir '.supercov/.trash/…'`. The trash is emptied while the tests run, and Supercov waits for what is left before it exits.

## 4.2.0

**Fixed**

- A Jest failure reads as it does without Supercov also when another tool runs Jest in its own process (`require("jest").run()`, as react-scripts and Vue's CLI do). With a single test file, or `--runInBand`, the code under a test file's frame was still the line Supercov had wrapped, and an assertion's stack showed Supercov's own frames.
- Next.js no longer warns about the copy's lockfile however it is started. 4.1.0 took the warning out only in a process Next's own command had started, and only on Next 16; it is gone from a server of your own and from Next 15.4 and 15.5 too.

**Added**

- `supercov diff` says when both runs measured the same source. What differs is then not a change in the code: two end-to-end runs of one commit read "lost: 11 lines, 31 branches" with nothing to mark it. In `--json`, `inputs.sourceChanged` and `inputs.testsChanged`.

## 4.1.1

**Fixed**

- A Jest failure reads as it does without Supercov, on Jest 29 and 30 with Babel, ts-jest and @swc/jest. An application frame printed as `../../../../src/crypto.js:10:11`, the code under a test file's frame was the line Supercov had wrapped, and a wrapped `expect` had a second frame.
- Under ts-jest a frame names the line you wrote. ts-jest compiles the text it is given and reads no source map in it, so a frame read `src/crypto.ts:51:10` for line 10 and no code was printed under it.
- Jest's lcov report names a file as your project does: `SF:src/crypto.js`, where it read `SF:../../../../src/crypto.js`.
- A project of a Vitest workspace prints failures like the root project does. 4.1.0 fixed the root project only.

**Changed**

- Under Vitest, `coverage-final.json` has named a file by its path in Supercov's copy since 4.1.0, where it named the project's. The lcov report reads `SF:src/index.js` since then, where it read `SF:../../../../src/index.js`.

## 4.1.0

**Fixed**

- A Next.js application with a `middleware.ts`, or a route with `runtime = 'edge'`, runs again. Since 4.0.0 every request to one with a middleware failed on `Cannot read properties of undefined (reading 'mcdcBegin')`, and `next build` failed collecting page data for an edge route: Next runs that code in a context of its own, where nothing had installed Supercov's runtime.
- Code that runs in Next's edge runtime is measured, and what a test's request runs there is attributed to that test. It read 0% before 4.0.0.
- A Vitest failure reads as it does without Supercov. Application frames printed as `../../../../lib/crypto.ts:3:11`, a test file's frames sat in column 1 or at the statement's start (`:12:7` for `:12:35`), and the code under the failing line was missing for a failed `expect` and for any application frame. On Vitest 4 a test file's positions were also mapped twice, to `:12:0`.
- A failed assertion has one stack frame where you wrote one line. A wrapped `expect` added a second frame on the same line.
- The `no test` row is what no kind's row counts. A line a test file's own setup ran was in both: a Vitest run read `unit 9.30%` and `no test 2.33%` under a total of 9.30%.
- Next.js no longer warns that its workspace root "may not be correct" because of the copy's lockfile. A lockfile your project really has twice still warns.

**Added**

- `supercov runs <run> tests without-evidence` lists the tests that made assertions and recorded no coverage. The summary's warning named one of them and now names this query.
- `files --group dir` has a `no test` column wherever code ran while no test was running, so a directory's `All` no longer stands above every kind unexplained: `.` read `e2e 18.18%`, the other kinds `0.00%`, `All 100.00%`. In `--json` each directory has `coveredByTests`.

## 4.0.1

**Fixed**

- A file a formatter or a fixer rewrites during the command is still measured. With `prettier --write src && node --test`, Prettier wrote the source it had read over the instrumented copy: the tests passed and the file read 0% covered. `eslint --fix` did the same, and the next step failed on code that was already fixed. The change is kept beside the copy now, and the tests run the copy.
- A fix a tool makes during the command reaches your project. `prettier --write`, `eslint --fix`, `biome check --write` and `dprint fmt` change your files when the command ends, as they do without Supercov, and a tool later in the same command reads the changed text. The run measured each file as it was before, says which files changed, and reads as stale; the next run measures them as they are. A file you edited while the command ran keeps your edit.

## 4.0.0

**Breaking**

- Supercov runs the command you give it, and no build of its own. It used to run a JavaScript project's `build` script first whenever the tests might need its output: twenty seconds before a suite that builds the application itself, and a cancelled run when that build failed. If your tests read build output, put the build in the command, as in `supercov -- sh -c "npm run build && npm test"`, or in a `pretest` script; a failed run in a project whose command does not reach its `build` script says so. Coverage is the same: measured on esbuild, webpack, SWC, tsc, Vite with Playwright, and Next.js.
- `SUPERCOV_BUILD_COMMAND` is gone, with the build cache for JavaScript projects and the settings Supercov inferred for a build from `process.env.X === "…"` comparisons in a bundler's config. A JavaScript project is instrumented one way now, whatever builds it.

**Fixed**

- Type checks that pass on your project pass on the instrumented copy. In a project with one function that returns what an `if` narrowed, `tsc -b`, TypeScript 7, `vue-tsc -b`, svelte-check, the Angular compiler, ts-jest, ts-node, ts-loader, fork-ts-checker-webpack-plugin, `@rollup/plugin-typescript` and tsd all failed under Supercov; all pass. Across nine TypeScript projects, 29 of 4,344 declarations had another type in the copy, and none do now.
- A value that passes through a probe keeps its type: `strict = false` read `strict?: any`, and a Next.js route handler read `const GET: any`.
- A test still narrows what it tests: `if (client) return client` returned `Client | null` in the copy.
- An operand of `&&`, `||` or `??` is still narrowed by the one before it, `const ok = a && b` still narrows through `ok`, and `user?.name` still narrows `user`. Not yet: `this.pet` by `this.pet?.name`, because a member may be a getter and is read once.
- Biome, oxlint, dprint, cspell and knip judge your source as you wrote it, as ESLint and Prettier already did. A test script that ran one of them first failed: they read the instrumented copy, and oxlint and dprint found no file at all. This holds for a tool in a workspace package's own `node_modules/.bin`, for one installed for the whole machine, and on Windows. `SUPERCOV_SOURCE_TOOLS=typos,stylelint` runs other tools the same way. A change such a tool makes to a source file (`--write`, `--fix`) is not applied during a measured run, and the run says so.
- Node's own coverage (`node --test --experimental-test-coverage`) reports a top-level function where it is, whatever the length of the file. The same file padded by 0 to 15 lines matched the report without Supercov at 3 of the 16 lengths; it does at all 16.
- c8 no longer counts an uncovered branch on line 1 of a file that is run directly.
- A minified build of the instrumented copy, such as Vite's, stays out of your project's `dist/`.
- A wrapped default compiles under `isolatedDeclarations`; h3 failed with `TS9011`.
- `(buffer[0] as string) += child` is printed as an assignment that parses; two of hono's files could not be read.
- A file with a `// @__NO_SIDE_EFFECTS__` annotation before a documentation comment is instrumented; zod could not be measured.
- `node -r ts-node/register --test` works in a package without `"type"`.

## 3.0.5

**Added**

- `runs <run> files --group dir` shows coverage by directory, split by the kinds of test that ran; `--depth` keeps more levels and `--metric` picks another metric. In `--json`, every file of `files` and `gaps` carries `totals` and `covered` for each metric; with only what was missing listed, no percentage could be worked out for a file or a directory.
- A run's summary ends `By test kind` with a `no test` row when code ran only while no test was running (a server starting, modules loading). One kind of test at 35.83% under a total of 41.58% read as two answers; the row is the difference, and `--json` has it as `coverageByTests`.

**Changed**

- A Next.js application is measured where Next.js keeps its code. Root `components/` and `pages/` are source in any project, and `proxy`, `middleware` and `instrumentation` files at the root of one that depends on `next`. Files the App Router serves (`route.ts`, `page.tsx` and the rest) are source whatever their folders are called: five `app/api/<service>/test/route.ts` endpoints were left out as tests.
- A source root you name is source whatever it is called. `SUPERCOV_SOURCE_ROOTS=src,scripts` measures `scripts/`, and a root that is a file is always measured; only what lies below a named root is still recognised as a test or a tool script by its path.
- `runs <run> scope` prints the scope by directory and reason, the ambiguous places first, with the `SUPERCOV_SOURCE_ROOTS` value that would settle them; `--files` lists every file as before, and `--json` adds `groups`. The summary names the same places when instrumentation is incomplete.
- The lines about synced, withheld and deleted files name their top-level directories with counts (`.next/ 2547, test-results/ 6`), and deletions are reported only for files the project still has.
- The build Supercov runs before the tests is announced with its reason. When it fails, the run says it built the instrumented copy, names `SUPERCOV_BUILD_COMMAND`, and keeps that copy until the next run; `SUPERCOV_KEEP_WORKSPACE=1` keeps it after any run.
- `runs` marks a run in which no test ran (`NO TESTS RAN`).

**Fixed**

- `supercov -- <command>` no longer fails a `next build` on a type error in code nobody wrote. A probe in a condition stops TypeScript narrowing through it, so an inferred return type widened and a caller outside the source roots failed the build's type check (`'supabase' is possibly 'null'`). That check now reads each file as you wrote it, as `tsc --noEmit` already did.
- A Playwright config written in TypeScript loads in a package without `"type"`. The generated wrapper imported it, which Playwright 1.60 on Node 24 ran as CommonJS with its `import` statements in place; the wrapper now requires a CommonJS config and imports a module, as Playwright does.
- `SUPERCOV_SOURCE_ROOTS` takes effect on a project that has already run. The first run's workspace was reused, so the run kept reporting automatic scope until `.supercov/workspaces` was deleted.
- A build the command makes from the instrumented copy stays out of the project as a whole. Only files that imported Supercov's runtime were kept back, so a Playwright run left a `.next/` of 2,547 files in the checkout, 432 of them with probes.
- A failed `node:assert` call under Vitest no longer prints Supercov's own stack frames between the test's lines.
- The warning about tests that made assertions without source-coverage evidence lists code outside the measured source among its causes.

## 3.0.4

**Changed**

- `assertions assess` asks up to three more tests before calling a statement not asserted: of the tests not yet asked, those whose name shares words with the statement's line (`corsOrigin: a false value` for `return corsOrigin(origin)`), then those that ran the least other code. Checked by making each change and running the tests: on click, 154 of the 382 statements called not asserted were caught by a test, and the extra tests find 46 of them while calling 8 asserted that no test catches; on supergateway they find 22 of 54 and add none. A statement that was asserted stays asserted. Listings say up to 18 tests are asked, and an assessment cost 8% (marshmallow) and 18% (click) more.
- The question Jev is asked about a deleted statement says what the deletion does. A statement of several lines (a loop, a `try`, a call written over several lines) is named whole; shown by its first line, only that line was read as gone. Of 100 such verdicts checked on six projects by making the change and running the tests, 22 were wrong before and 9 after. In Python, a deleted `return` or `raise` says that execution carries on with what follows: of the skipped returns and raises of five projects, 61 of 371 verdicts were wrong before and 44 after. These statements are asked once more, since the question changed.

## 3.0.3

**Changed**

- `quality diff` shows each check that fired in either snapshot with its value before and after (`long_method 0.93 → 0.41, no longer present`), and lists the files a change added, with what fired on them, and removed. It printed only which checks appeared, so a refactor that removed a smell, lowered one or extracted a helper into a new file showed no effect. When the two snapshots assessed different files, it also gives health over the files both assessed: a one-file snapshot against a whole repository read as 3.7 to 5.3 with that file unchanged.

**Fixed**

- `quality diff` counts a file whose health did not move but lost a check as improved, not declined, and tells an edit from the model's own variation by the file's hash rather than its size.
- `runs <run> assertions` prints how many tests are asked about a statement (15) in its heading; 3.0.2 printed `{MORE_TESTS}`.
- A metric with nothing in it, such as branches in code without any, reads `nothing to measure (0/0)` in a run's summary and `—` in the kinds, runners and per-file tables, instead of 100%. `gaps` and a file's gap list say there are none instead of printing an empty table.
- A run's summary no longer ends with a "Runtime action phases" block. It counted lines inside phases that no current feature records, so it read 0 on every run.
- A statement that sets `false` before a comment mentioning `true` (`let ready = false // true once open`) is flipped in the code, not in the comment, so Jev is asked about the change that matters.
- Counts read "1 file", "1 test" and "1 covering test" instead of "1 files", "1 tests" and "1 covering tests"; a file's assertion percentage prints as a number; the security report no longer prints double blank lines.

## 3.0.2

**Added**

- `assertions assess --refresh` asks every question again instead of reusing saved answers, and keeps the new ones. A reused answer is the one Jev gave before, and there was no way to replace one shown to be wrong.

**Changed**

- `assertions assess` asks first the tests whose file quotes text the statement writes. It went round-robin over test files in alphabetical order, so of 179 tests running a log line the 15 asked missed the one that compares the whole log. With the change rules below, a reviewed supergateway run went from 154 statements not asserted to 88 (93.4% to 96.2%) at the same cost, and all seven the reviewer had shown were caught by an existing test came out asserted; a later run went from 94.8% to 97.2%.
- Changes that could never be caught are no longer asked. `return undefined` (or `None`) is skipped instead of "returning undefined", and a returned or declared `true`/`false` is flipped instead of becoming undefined. A TypeScript `throw` throws a bare `new Error()` instead of being deleted, since deleting a guard's throw does not type-check, and a constructor's `super(...)` is not assessed.
- Not-asserted listings say how many of the tests that run each statement were asked; the statement view says the tests not asked were not judged.
- `supercov report` includes the five newest runs and writes the report whatever its size; `--runs N` includes N. It included ten and refused a report over 24 MB after preparing them all, three minutes on a large project.
- A run keeps what `report` and `assertions assess` read from its evidence. On a supergateway run, a one-run report takes 1.1 s instead of 7.2 s, and planning an assessment 6 s instead of 16 s.

**Fixed**

- Commands Supercov prints (next pages, `assertions assess`, a run's follow-up commands) are written the way you started it: `supercov …` from the PyPI, Ruby and Rust packages and the Go command from Go. They always said `npx supercov`.
- node:test todo tests are counted as node:test counts them, with their own `Todo` line in the summary. A todo test without a body was missing, a todo whose body failed counted as a failure, and the tests of a `describe.todo` counted as passes.
- `security file <path>` shows what the summary flagged. A check the line pass confirmed read `no` with the file question's value, judged against the quality cutoff instead of the check's own; it now lists the confirmed lines.
- `supercov quality` counts each file as good, fair or weak by the score it prints, as the HTML report does: a file shown as `good (8.0/10)` was counted fair for its 7.98.
- `assertions assess --dry-run` says the most the pass can cost beside its first round. The stated "1.5 to 3 times the first round" was 4 times on a 1,076-test suite.
- A failing Playwright test's code frame shows your test's line, with the caret on the matcher, instead of Supercov's instrumented copy of it.
- One answer or request is counted in the singular ("1 answer reused").

## 3.0.1

**Fixed**

- A TypeScript test run through ts-node, or compiled with `tsc`, no longer fails to typecheck when an assertion awaits one of its operands (`assert.ok(await ready())`): the rewritten call lost the assertion's `asserts` signature (TS2776), and the file failed before any of its tests ran.

## 3.0.0

**Breaking**

- The agent-written assertion map is removed: `assertions.json`, `runs <run> assertion <id>`, `assertions report`, `validate`, `check` and `supercov assertions schema`. Jev assesses assertion coverage instead. Runs recorded before 3.0.0 need rerunning for `tests affected`, `runs source` and `assertions assess`.
- Branch coverage counts each decision outcome once in every language. Python, Ruby and Rust counted each `if` twice, so their branch percentages change: a Python project that read 14/18 reads 8/10, with the same outcomes missing.

**Added**

- Assertion coverage by [Jev](https://typesafe.ai): `runs <run> assertions assess` asks, test by test, whether changing each executed statement would make the test fail, and saves the answers. `runs <run> assertions` reads them offline a page at a time, down to one file, one statement (`<file>:<line>`) or one test (`--test <name>`), and the file and line views show the verdicts. Answers are cached by exact question, so after a commit only what changed is asked. About 0.007 cents a statement; 85-96% of verdicts matched running each change. JavaScript, TypeScript, Python, Ruby, Go, Rust, Java and Kotlin. See [assertion coverage](https://supercov.com/docs/assertions).
- `tests affected` lists first the tests judged to catch the change and the tests named like the changed files; after `assertions assess --changed`, `--ran-changed` and `--asserting` narrow the list.
- Per-test attribution for Mocha 5 to 12 (`--parallel` included), AVA (concurrent tests included) and node-tap 15 and newer, with nothing added to the command. debug-js/debug, sindresorhus/p-map and npm/ini, measured in aggregate before, are exact.
- node:test and Jest tests are named by their `describe` blocks, and `tests affected --names` prints names the way each runner's filter matches them.
- Sandbox and container SDKs that take an environment as `env`, `envs`, a plain map or a builder (Testcontainers) get Supercov's settings, so guest coverage is credited to the test that started it. A guest started without them still runs and is counted.

**Changed**

- Ruby 3.3 and newer credit every test that runs a line, not only the first; dry-inflector on Ruby 3.3 takes 2.0 s instead of 46 s.
- JavaScript probes cost almost nothing on repeats: a loop over `??`, `||` and optional calls takes 0.14 s instead of 0.56 s, and lru-cache's million iterations 0.3 s instead of 177 s. Long files instrument in linear time, and a 63,740-test Vitest suite records in under two minutes.
- Rust test code, and items the target never compiles, are not measured.
- JVM projects that take JUnit from a parent pom discover their tests, and Gradle JUnit 4 suites are attributed.
- `supercov security` reads only the code under review; held-out RealVuln F1 rises from 0.42 to 0.44.

**Fixed**

- Coverage no longer credits outcomes a test never reached: an optional-chain link the chain was cut short before (`o?.inner?.value`, `f?.()?.b`), or a `switch` case reached by falling through (JavaScript, Go). Nested JavaScript switches are measured.
- A function shipped as text by name (`"(" + worker + ")"`) runs where it is sent instead of throwing.
- `runs check`, `patch` and `report` count code run outside a test, as the summary does.
- Monorepo packages imported by name, TypeScript compiled into another directory, linked source directories, sockets and pipes are measured.
- On Windows, a run no longer takes over another live run's lock, a TypeScript project with a tsconfig runs, and an environment copied from `process.env` reaches a sandbox guest.
- A Ruby or Python command with no test framework, or Ruby older than 3.3, is no longer reported as a Supercov bug.

## 2.0.1

**Added**

- A Supercov plugin for coding agents. The agent uses Supercov on its own when asked for tests, coverage, refactoring or a security scan, and runs the scan when `TYPESAFE_API_KEY` is set instead of asking again. It installs in Claude Code (`/plugin marketplace add supercorp-ai/supercov`), Codex (`codex plugin marketplace add supercorp-ai/supercov`) and Gemini CLI (`gemini extensions install https://github.com/supercorp-ai/supercov`). Cursor and GitHub Copilot read the same plugin, and `npx skills add supercorp-ai/supercov` adds the skills to other agents.
- Four commands in Claude Code and Gemini CLI: `/supercov:coverage` writes one test for untested code and reports coverage before and after, `/supercov:security` and `/supercov:quality` check the repository or a change, and `/supercov:review` checks a branch for what it introduced.
- The skills point agents at `supercov docs`, so they follow the installed version, and a check fails the build when a command they or the guides name stops existing.

**Fixed**

- `supercov security` without `TYPESAFE_API_KEY` says security needs the key, not quality.

## 2.0.0

**Breaking**

- `supercov clean` is removed. Use `supercov runs clean` for runs and `supercov quality clean` for assessments, with the same `--keep` and `--dry-run`. Cleaning runs never removes an assessment, which costs money and cannot be reproduced from the repository.
- Evidence written by 2.0.0 cannot be read by 1.x. 2.0.0 reads 1.x evidence.

**Added**

- `supercov security`: twelve security checks asked of every file with [Jev](https://typesafe.ai), from secrets and injection to mass assignment and weakened configuration, each mapped to its CWE classes. A file is clean or names what fired, with the line and code where the model confirms one. On fifteen held-out repositories of the RealVuln corpus it scores F1 0.47 (precision 54%, recall 41%) for about two cents a repository, where Semgrep scores 0.14; authorisation stays file-level. See [Security surface](docs/security.md).
- `security patch` reports what a change introduced, and `quality patch` asks the security checks too. `--run latest` shows which flagged files no test executes.
- `supercov report` writes one self-contained interactive HTML file from stored runs: whether a run passed and is still current, what changed since the run before, and which line, branch or MC/DC condition to test next. It opens without a server, network or Supercov, so it can be attached to a pull request. It carries assertion coverage and saved quality and security assessments on the same timeline. See [Portable HTML reports](docs/reports.md).
- `supercov quality` and `supercov security` can use Jev through another provider, such as OpenRouter, with the variables TypeSafe's SDKs read: `TYPESAFE_BASE_URL` and `TYPESAFE_DEFAULT_MODEL`. See [Use Jev through another provider](docs/quality.md#use-jev-through-another-provider). Contributed by [@untitaker](https://github.com/untitaker). (#47)
- A quality snapshot records which source it read: the run's source digest and a SHA-256 per file.

**Changed**

- Subprocess-heavy Python suites run several times faster. A child interpreter took about 930 ms to start under 1.2.0 and now takes about 17.6 ms, against 13.4 ms without Supercov: 7,107 tests starting 16,787 interpreters run in 309 s against 226 s. (#40)
- Python runs finish sooner: h11's 3,900 tests take 4.2 s against 2.3 s without Supercov, and its archive shrinks from 70 MB to 32 MB. Every measured obligation, vector and test is the same as before.
- Publication analyses a run once and writes the query index, assertion map and summary from that analysis, so the first query no longer analyses it again, in about a third of the memory.

**Fixed**

- A measured child process no longer changes what its test sees: descriptors, stderr, output, exit status and isolated, positional or bytes environments are what they are without Supercov. Reported by [@maik-intellicoach](https://github.com/maik-intellicoach). (#40)
- `runs latest files` and `runs latest file <path>` agree on a Python script that runs only as a child process. (#40)
- Instrumented bytecode no longer reaches ordinary Python caches. Discard caches 1.2.0 wrote; see [troubleshooting](docs/troubleshooting.md). (#40)
- `.supercov/` ignores itself in git. (#40)
- A suite mixing pytest and unittest publishes.
- A `match` statement without a catch-all case is measured instead of failing to compile.
- A process that outlives the test command, such as multiprocessing's resource tracker, no longer fails the run with `could not read Python evidence`.
- The timings line reports publication as its own phase, and its total is the command's wall time.

## 1.2.0

**Changed**

- Python suites run nearly four times faster under Supercov. On h11's 3,900 tests, pytest's own session takes 3.4 to 3.9 seconds where 1.1.1 took 12.8 to 14.1, against 2.0 to 2.3 without Supercov, on CPython 3.13 and 3.14 alike; the whole command takes 18 to 22 seconds rather than 32 to 36.
- Python is measured by probes compiled into each measured module as it is imported, replacing the `sys.monitoring` observer, which is removed rather than kept behind a switch. Nothing on disk changes, and tracebacks keep their line numbers.
- CPython 3.9, 3.10 and 3.11 are measured. 3.12 was the floor.
- Decisions the observer left out of the denominator without saying so are measured: on h11, 704 branches and 172 conditions rather than 692 and 160. A Python project's branch and condition totals can rise for that reason alone.
- A process that ends mid-test -- `os._exit`, SIGTERM, SIGKILL, a killed xdist worker -- keeps what it executed.
- Running under `-X no_debug_ranges` no longer removes lines from the measurement.

**Fixed**

- A Python suite using `pytest-subtests` published no result. Each subtest is reported under its test's `call` phase, so one phase arrived several times and the run was refused. A phase reported more than once is now one phase, and a failing subtest still fails its test. Thanks to [@maik-intellicoach](https://github.com/maik-intellicoach), whose 6,872-test run lost 2 hours 43 minutes to it. (#40)
- When Supercov itself fails a run, what the run measured is kept in `.supercov/failed-evidence/<run id>` and the failure names that path, instead of being deleted with the work directory. Reported by [@maik-intellicoach](https://github.com/maik-intellicoach). (#40)
- A thread started by a measured test is freed when it ends, rather than when the cycle collector next runs.
- A `match` case with an or-pattern whose first alternative fails is no longer also reported as not selected.
- Opening a run's index verifies its pages about five times faster on Arm macOS and Linux, where the CPU's SHA-256 instructions were going unused, and an index stores each list of tests once however many lines name it: a real 206 MB index came out a quarter smaller.

## 1.1.1

**Fixed**

- `SUPERCOV_SOURCE_ROOTS` narrows what is measured in every language. It was read on the JavaScript path alone while the reference described it without one, so a Python project that set it measured everything anyway: a vendored tree beside a flat layout came out at 660 files instead of about fifteen. If you already set it on a Python, Ruby, Go, Rust or JVM project, this release measures the roots you named and nothing else, and your numbers will change accordingly. A root is a directory or a single file, since a flat layout keeps its own modules at the root, and `supercov runs latest scope` lists what was kept and what was left out. (#35)
- Roots that name nothing to measure are refused, with the name quoted, rather than published as a run over zero files. (#35)
- A Python suite whose threads outlive its tests no longer ends with a `FileExistsError` from the collector. (#35)
- `supercov runs latest` costs what it shows rather than what it could show. A summary of a 300-file project took 21 seconds and 4 GB of memory, and 81 seconds where files are as long as generated bindings; it now takes 8 and 9 seconds. What it prints is unchanged. (#35)
- `go run github.com/supercorp-ai/supercov/cmd/supercov@latest` asks GitHub again when it answers with a gateway timeout, rate-limits the request or drops the connection, instead of ending the install outright. It says so while it waits.

## 1.1.0

**Added**

- `--exact-attribution` credits every test with what it reached, by running the suite in order: `-parallel=1` in Go, and JUnit's parallel execution turned off in the workspace copy. Without it a suite that runs its tests at once is measured as its author runs it, and what overlapped is recorded against the run rather than against a test.
- A measurement says how its tests were attributed, so coverage that belongs to no single test is visible rather than implied.

**Fixed**

- A Go package whose tests all call `t.Parallel()` published no run at all and exited 1. Probes are a store into one shared array, so what a test reaches while others run beside it cannot be credited to it — but the coverage is real and the package was fully measured. The run is published, and each parallel test is named, carries its real outcome, and is credited with nothing. (#30)
- Go 1.26's `new(value)` failed to parse, and a single unparseable file silently zeroed the whole measurement: 0 files measured, exit 0. The value form is read, and a file Supercov cannot parse is reported rather than taking the run down with it. (#31)
- A Ruby test's coverage is read as the lower bound it is. `Coverage.start(oneshot_lines: true)` reports a line once per process, so a line some earlier test reached first is missing from this one's record. It is marked partial rather than presented as the whole of what the test ran.
- Cleaning old runs kept the largest run IDs rather than the newest runs.

**Changed**

- `supercov` with no arguments lists the supported languages, each with its own install and start command.
- A source-discovery scope is reported in the language that was measured.

## 1.0.1

**Changed**

- `supercov` with no arguments names the product and leads with quality. Quality, coverage and assertions each get a section in the same shape, in that order, replacing the two paragraphs about passing a test command and the lone pointer to the assertion map schema.
- Getting started opens on a score, with a prompt to paste rather than a shell export, and the starter prompt everywhere is now `Measure code quality with npx supercov and show me the weakest files.`

## 1.0.0

**Added**

- `supercov quality` scores your code with [Jev](https://typesafe.ai). Twelve named properties are asked of each file as yes/no questions and Supercov does the arithmetic, so every part of a score is a claim you can check against the file. With no argument it assesses the repository you are in.
- `supercov quality patch` reports what a change introduced: the same properties asked differentially, plus six risk checks for a credential in source, untrusted input in a query, a change to who may do what, a test that now checks less, a schema migration, and debugging left behind. With no range it reviews uncommitted work, or your whole branch when the tree is clean. `--annotate github` prints workflow annotations and `--run <id>` marks findings in code no test exercises.
- `quality gaps`, `file`, `scope`, `snapshots`, `show` and `diff` read a saved assessment with no key and no network. `diff` reports what declined between two.
- Source files are found the way coverage finds them, across every supported language. `SUPERCOV_SOURCE_ROOTS` declares them explicitly.

## 0.0.55

**Fixed**

- Mixed Vitest Node and Browser Mode projects now get the correct runtime and browser evidence commands for each resolved project, including inherited configurations, project globs and project selection. Browser module-initialization coverage is preserved separately from per-test evidence.
- React Native and Expo JavaScript tests work with Babel-transformed runtime imports and Jest's jsdom setup. Expo's forwarding launcher preserves the original Jest configuration instead of recursively loading the generated configuration.
- Manifest-declared Jest setup files are excluded from application instrumentation, so injected bindings do not break hoisted mock factories. Duplicate test names remain distinct from retries, including parameterized cases and delayed retries.
- Jest captures per-test coverage after teardown, preserving React effect cleanup executed by Testing Library.

**Added**

- A runnable React example shows how displayed values, accessible names, disabled states and error messages can remain unchecked at 100% line coverage, and how targeted assertions catch four concrete regressions.
- Regression checks cover mixed Vitest projects, React hydration in jsdom and Chromium, and React Native/Expo component tests. Nightly checks run pinned Bulletproof React and Bluesky JavaScript suites with documented configuration and exclusions.

## 0.0.54

**Added**

- Vitest Browser Mode is measured per test, like any other Vitest run. A browser-mode project collected no tests at all: Supercov injected its node setup, which reaches `node:fs`, and Vite externalises that for the client, so the suite failed before a single test was collected. Browser mode now gets a setup that can run in a browser, and its evidence travels over Vitest's own browser command channel -- awaited as part of the test, with the test file taken from Vitest rather than from the browser realm, and working for every provider.
- An expression rendered inside JSX is measured on its own. A JSX tree is a single statement, so `aria-label={label(state)}` and a child `{formatted(value)}` were counted as covered the moment the component rendered once, even when the expression never evaluated. It is also the seam a UI assertion attaches to: with one statement for the whole tree, `toHaveAccessibleName` naming the attribute and `toHaveTextContent` naming the child could not be told apart, and neither could be credited. Only expressions that can independently fail to evaluate become obligations -- `{value}` is reached exactly when the tree is, and `onClick={() => save()}` is measured where the handler is called rather than where it is created.
- `expect.element(...)`, `expect.soft(...)` and `expect.poll(...)` are recognised as assertions. The chain root is a member call rather than a bare `expect`, which left every Vitest Browser Mode UI assertion without a passing occurrence and so unable to earn credit.

**Upgrading**

- A project with JSX gains obligations for the expressions rendered inside it, so its line and statement percentages move. Measured on three React codebases, the obligation count grew 2.3%, 5.5% and 9.5%; roughly four in five containers earn nothing. A project sitting at 100% may drop: the expressions it gains were being reported as covered without evidence that they ever evaluated.

## 0.0.53

**Fixed**

- An assertion inside a validator callback records its own passing occurrence. `assert.throws(fn, validator)` is the standard way to assert *what* an error says rather than only that one was thrown, so the inner assertions are the interesting ones -- and every nested assertion phase was skipped, leaving them with no passing occurrence and so unable to earn credit however carefully they were mapped. The same call seen twice, once through the module proxy, is still recorded once.
- A branch outcome is recorded when it happens rather than when control leaves the construct, so a `catch` or loop body that calls `process.exit()` no longer reports its outcome as never taken. The commit sat in the generated `finally`, which `process.exit()` skips; statement probes fire in place and survived, so the report contradicted itself -- crediting every line inside a catch while reporting the catch as unentered. On a project holding 100% the only way to make it agree was to delete a correct error path.

## 0.0.52

**Added**

- The map report says when a `watch` entry changes what a flow rests on. Naming a file that already holds the flow's nodes widens it to the whole file, so a neighbouring function's body is a review again -- sometimes what the author means, never something that should happen unsaid. Naming a file it already depends on whole, such as its own test, is redundant and reported.

**Changed**

- A change assessment's `affectedFlows` is the author's judgement -- the flows this change invalidates -- and no longer restates every known dependent. Only the flows it names lose their acknowledgement, so one explanation can answer for a change that touches no claim.

**Fixed**

- A `for...of` over an array that cannot be empty no longer carries a zero-iteration obligation no test could ever reach. It applies to a non-empty array literal, or a `const` bound to one that nothing else in the file can touch; a spread element does not count, since it may contribute nothing. Decided from syntax and symbol resolution alone -- a type can be asserted or optimistic, and an obligation dropped on a wrong answer is a branch that runs and is never reported again.
- A JavaScript or TypeScript comment containing `</script` no longer makes its file impossible to instrument. Code generators rewrite it inside comments so output cannot close an HTML `<script>` element early; the restore read the two spellings as different comments and, matching in source order, one it could not recognise consumed the rest. A comment that genuinely cannot be restored is now named, rather than reported as counts that are usually equal.
- A word in a test filename no longer overrides a runner that can only drive the whole system. A Playwright spec named `checkout-integration.spec.ts` was reported as an integration test, hiding browser evidence from E2E coverage. A directory still outranks the runner; among path tokens the most specific wins, so a spec under `tests/e2e/` is no longer read as integration.
- A change in the assertions view no longer grows with the project, so none can outgrow the JSON page cap and become unfetchable. `knownFlows` and `exposed.tests` are now a sample with an exact count (`exposed.testCount` is new); one oversized item made the pagination loop skip a change silently.
- Recording a change assessment no longer de-acknowledges every flow it names. The exhaustive list was mandatory and naming a flow cost its credit, so one no-op manifest edit could take a whole map to zero asserted statements.
- A stale flow names what moved. A map edit said `claim or inputs changed; needs rechecking` whatever the author had done, and now names the part -- the explanation, the watch list, the nodes; the basis is one hash, so the parts are recorded separately. A change in a whole-file dependency no longer blames the declaration that changed for holding a node it does not hold.
- Removing a `watch` entry that Supercov reports as redundant no longer restates the claim. Such an entry never became one of the flow's dependencies, but was still part of what the acknowledgement rested on, so deleting it cost a full re-acknowledgement.

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
