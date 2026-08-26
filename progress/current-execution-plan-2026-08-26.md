# Supercov current execution plan — 2026-08-26

This is the current sequenced execution plan. The architectural end state,
invariants and no-shortcuts policy remain defined by
`progress/engine-master-plan-2026-08-24.md`. This document replaces that
plan's now-obsolete migration ordering with the repository's actual state and
one falsifiable critical path.

## Corrected baseline

As of npm `supercov@0.0.16` and commit `911bbee`:

- the production engine, CLI, instrumenter, analyzer, storage, lifecycle and
  query implementation are Rust;
- the TypeScript/Babel engine, selector and fallback have been removed;
- the remaining JavaScript is unavoidable generated runtime/test-runner glue
  executed inside Node, browsers, Playwright and Vitest;
- JavaScript/TypeScript is the only public coverage frontend;
- owned Rust coverage automatically detects `cargo test` and runs end to end,
  but is private and explicitly measurement-incomplete;
- owned Python has a private denominator/evidence candidate but no owned probe
  injection and is not on the current critical path;
- npm native distribution is live for six macOS/Linux targets; Windows and
  other registries remain gated future work.

The old objective's JavaScript rewrite and atomic-cutover clauses are complete.
They remain regression invariants, not future tasks.

## Current objective

Make Supercov's owned Rust-language coverage frontend independently correct,
zero-configuration, exactly attributable, crash-safe and public-ready on the
single Rust engine, while preserving the verified JavaScript/TypeScript
frontend. Prove the Rust frontend with versioned contracts, independent
rustc/LLVM development oracles, semantic differential/property corpora and
Supercov-on-Supercov dogfood. Do not claim Rust support while any denominator,
semantic, attribution, lifecycle, platform or performance release gate is
unproven; fail closed and report limitations instead of publishing partial
confidence.

Python and later languages resume only after this milestone proves that the
shared frontend protocol works for a second, compiled language without a
language-specific analyzer.

## Governing invariants

1. Every user run is measured by Supercov-owned instrumentation. rustc/LLVM
   coverage is a development oracle only and is never a product dependency or
   fallback.
2. The user's checkout and ordinary build artifacts are never modified.
   Generated state stays transactionally under `.supercov` and is recoverable
   after interruption, ENOSPC and process death.
3. The shared evidence, analysis, MC/DC, attribution, storage, query,
   minimization and lifecycle implementation remains language-neutral Rust.
4. Unsupported or unmeasured behavior is explicit and blocks a
   measurement-complete claim. No denominator may silently omit code.
5. Original and instrumented programs must preserve values, errors/panics,
   stdout/stderr, drop order, side effects, scheduling-relevant behavior,
   borrowing and compilation results.
6. Exact test/worker/retry/phase identity is required. Evidence from failed,
   flaky, skipped, background and terminally passing attempts must remain
   distinguishable.
7. Existing commands remain the only user configuration:
   `npx supercov -- <working test command>` must detect and instrument the
   selected language automatically.
8. Correctness and architecture precede micro-optimization, but public Rust
   support still requires a fair warm/warm and cold/cold overhead of at most
   1.10x.
9. Full hosted matrices and corpus sweeps are manual release gates. Ordinary
   work is validated locally to conserve GitHub Actions minutes.

## Critical path

### R0 — Freeze the second-language contracts

Turn the private evidence-v3 and Rust coverage-model candidates into reviewed,
versioned specifications before expanding implementation.

Work:

- define exact Rust statement, function, branch and masking-MC/DC obligations,
  source locations, stable identities and reachability semantics;
- define language/frontend/model identity in evidence v3 and make it the sole
  archive format; this pre-1.0 repository has no legacy v2 reader or writer;
- specify limitation severity and when `measurement: complete` is legal;
- freeze runner/test/outcome/assertion/action identity and archive/query JSON;
- add malformed, truncated, mixed-language and unknown-version rejection
  vectors;
- produce a requirement-to-test traceability table so every public claim maps
  to an executable gate.

Exit gate: no implementation-defined semantics remain in the public Rust
coverage model or evidence envelope, and all frozen contract fixtures pass
through the shared analyzer.

Checkpoint (2026-08-26): the evidence-v3 envelope, coverage-model-v1
declaration and target Rust source model are frozen. V3 is the sole archive;
frontend/model language mismatch, unknown declaration fields, malformed or
partial recognized JSONL, missing identities and incompatible merged models
fail closed. JavaScript/TypeScript now publishes v3 and passed the complete
node:test, Vitest, Playwright, build-adapter, merge, agent-query, watchdog and
isolation matrices. The Playwright migration exposed and fixed a real scoped-
record merge bug: server evidence now attaches only to the exact full
execution scope rather than the first equal test/retry status record. Timeout
runs publish a command-level setup outcome without inventing test coverage.
`contracts/rust-coverage-v1/traceability.md` records every still-open Rust
implementation/promotion gate; those open rows keep Rust private.

R0 exit gate: **green locally on 2026-08-26**. `npm run check`, the full
fixture/browser/build/merge/isolation/watchdog matrix, the frozen engine
contract and the bounded agent-query evaluation all passed. Hosted workflows
remain manual-only and were intentionally not invoked.

### R1 — Complete the Rust denominator and semantics-preserving probes

The current concrete-syntax frontend covers an initial subset and explicitly
reports macro and const limitations. Complete every normal Rust source surface
before optimizing it.

Work:

- statements; functions, methods, closures and async bodies;
- `if`/`if let`, `while`/`while let`, `for`, `loop`, `match` arms and guards;
- `&&`/`||` atomic conditions and masking MC/DC;
- `?`, `let else`, destructuring/refutable patterns and early exits;
- assertions and panic paths without changing evaluation or formatting;
- generics/monomorphizations, traits/default methods and nested modules;
- generated/build-script sources and include/module ownership;
- macro expansion, proc-macro/derive output, const/const-fn evaluation,
  doctest source mapping and no_std/target constraints.

The source-transform backend cannot honestly cover every expansion surface by
itself. Run a time-boxed compiler-expansion spike first. Choose and document a
maintainable owned backend—potentially a stable source path plus automatically
selected rustc-versioned compiler components—rather than hiding expansion
gaps. If exact support is not yet possible, Rust remains private.

Checkpoint (2026-08-26): the compiler-backend spike selected an owned,
rustc-commit- and host-matched compiler companion as the public architecture.
An exact Rust 1.95.0 `rustc_driver` wrapper observed authored, declarative-
macro, procedural-macro and build-script-generated HIR/MIR, and an
`optimized_mir` provider replacement changed the emitted fixture behavior.
The same experiment proved that const/CTFE bodies need a separate provider
path and that Cargo's ordinary `RUSTC_WRAPPER` does not observe rustdoc's
extracted doctest crate. Those are release blockers, not exclusions. The
frozen `rust-compiler-companion-v1` envelope now requires exact compiler-driver
identity, rejects unknown or nearby companions, and cannot claim public
readiness until expanded provenance, runtime probes, generated sources, CTFE,
doctests and exact test attribution are all present. The concrete-source Rust
transformer remains only a private differential reference and will not be the
public injection authority.

The next spike increment removed the fixture-defined runtime: the companion
now appends a synthetic private runtime module to the in-memory crate AST and
inserts real side-effecting calls into optimized MIR. The temporary bitmask has
also been removed. The companion injects the same std-only mmap runtime used
by the engine, and both a normal binary and an actual test process publish all
four expected MIR ordinals through an authenticated supervisor-created file.
The checkout hash stayed exact, and ordinary-versus-instrumented behavior
preserved values, `Result` errors, caught panic status, drop ordering, stdout
and stderr.

`rust-probe-transport-v1` now freezes the 128-byte header, 40-byte descriptors,
record kinds, per-record process/context identity, bounded payload, 128-bit
task binding, release/acquire publication, 64-bit metadata/payload checksum and
fail-closed health rules. Executable tests cover eight concurrent threads,
eight concurrent processes, descriptor and payload exhaustion, wrong token,
malformed context, corrupt/truncated headers and descriptors, symlink refusal,
an uncommitted reservation and process kill after a committed observation.
This closes the wire/crash slice, not R1/R2: exact dynamic context propagation,
the complete Rust denominator, CTFE/doctest publication, no_std and the target
matrix remain release blockers.

The next attribution slice is also executable in the companion spike. The
target runtime now supports nesting-safe thread-local context entry/restoration.
The companion derives test identities from rustc's post-expansion
`rustc_test_marker` records, so it covers ordinary tests and tests generated by
procedural attribute macros without repository or framework names. It injects
entry plus normal/unwind restoration in optimized MIR. Five concurrent tests,
including an expected panic, retain distinct deterministic context IDs; helper
scopes restore both after return and propagated panic. A child thread produces
context zero rather than inheriting by timing or global state. Public R2 still
requires owned child/async/subprocess propagation or an automatic switch to the
existing exact process-per-test path whenever that explicit gap appears.

The compiler now also emits the first strict manifest candidate under the
frozen Rust source-identity-v1 rules. Function-entry obligations use
project-relative original byte ranges for authored, included and declarative-
macro tokens; two expansions of the same declarative macro body aggregate to
one obligation. Proc-macro output that collapses to its invocation is instead
bound to the stable callsite, complete expansion chain and textual compiler
owner path, so repeated invocations remain distinct. Owned `OUT_DIR` source is
keyed by project-relative package root and out-relative path. Two entirely
clean Cargo target directories produced byte-identical candidates, with no
target hash or absolute scratch path. IDs are SHA-256-derived and any in-run
digest collision is fatal.

The next structural increment now walks expanded HIR rather than concrete
source. It adds executable statement points; `if`, `if let` and let-chain
decisions; source-ordered `&&`/`||` atomic conditions; and true/false branch
alternatives. Those identities aggregate repeated declarative expansion
tokens, distinguish repeated proc-macro invocations with an owner-local
ordinal, and stay byte-identical across clean targets. Synthetic condition
display comes from rustc's expanded HIR instead of falsely printing the macro
invocation as the condition. The exploratory 0/1/2/3 function ordinals are
gone: runtime MIR hits now carry the u64 prefix of the exact manifest point ID,
and the compiler rejects both full-ID and probe-prefix collisions. The
candidate still carries blocking denominator limitations. The authored match
slice below has since narrowed that surface, but nested synthetic match arms,
synthetic match-guard decisions, let-else, `?`, assertion, CTFE and doctest obligation/probe mappings,
plus full package and compiler fingerprints, remain R1 work. No measurement-
complete claim is possible yet.

The first dynamic decision slice is now executable as well. The companion
uses rustc's exact source-to-optimized-MIR branch regions to locate each
authored condition edge, translates those edges into Supercov-owned
token-bearing frames, and emits frozen string decision IDs plus ternary values
through `rust-probe-transport-v1`. Exact goldens cover `&&`, `||`, mixed
`(a || b) && c`, nested `if` bodies, an outer `&&` whose second condition is a
value-producing inner `if`, `if let` and a three-atom let chain, including every
exercised short-circuit vector and parallel libtest attribution. The outer
frame remains open while the inner decision completes without the vectors
merging. Baseline and instrumented behavior remain exact.
The work also removed hidden control flow from external macro implementations
such as `assert!` and `println!` from the caller's authored denominator.
Generated owners use a fail-closed expanded-span fallback that accepts only
one compiler-typed boolean MIR branch for the exact condition. Declarative
macro expansions sharing an authored identity, distinct proc-macro
invocations and build-script generated source now all emit gated exact
decision vectors.

Decision frames reserve their bounded mmap descriptor at evaluation start and
commit only at the final outcome. A compiler-level condition panic and a
killed process now leave explicit incomplete health without publishing a false
complete vector.

The native-profile blocker is now closed rather than hidden. The exact-version
companion adds rustc's internal `no-profiler-runtime` switch, removes the native
MIR coverage statements and maps before codegen, and gates both the absence of
`.profraw` output and LLVM profile/coverage symbols in the linked executable.
Rustc's branch correspondence is used only during compilation; every published
observation remains Supercov-owned. A broader nested/derive/external expansion
corpus and the rest of the R1 denominator are still required. Rust remains
private and measurement-incomplete.

Compound `while` and nested-pattern `while let` conditions now use the same
exact ternary decision model. Pattern conditions are not assumed to be one MIR
switch: the companion finds the condition subgraph's common dominator and
instruments every dominated edge into its terminal true/false blocks. For the
separate zero-iterations/entered obligation, it climbs to the actual natural
loop header, starts one first-commit frame only on the external entry, and
redirects backedges past that start. The executable fixture proves two zero
and one entered compound-`while` invocation, three zero and one entered
`while let` invocation, exact short-circuit vectors, multiple iterations and
no per-iteration relabeling or duplicate loop observation.

Authored `for` loops now use the frozen `loop-entry` branch kind without
inventing a Boolean decision. The companion overrides rustc's documented
post-borrow-check/pre-optimization MIR boundary, binds the exact
`Iterator::next` `Option::None`/`Some` switch while it is still structural, and
inserts the same crash-visible first-commit frame before ordinary optimization.
This survives optimizer-specific iterator lowering without importing native
coverage. The corpus proves empty and multi-iteration loops, two loops in one
function, nested loops, a body with no backedge, and a panic from `next()`.
Nested switches bind to the smallest enclosing authored loop. The panic leaves
an incomplete frame and no false alternative. Compiler-generated for/while
scaffolding is no longer counted as authored statement coverage, and candidate
branch/decision kinds are gated against the frozen contract enums.

Reachable authored `match` arms and match guards now have a first exact
compiler-backed slice. The candidate manifest persists one stable selection
group relating every frozen `match-arm` branch and both of its alternatives.
At runtime, one crash-visible frame begins after the scrutinee has evaluated
and commits only when an arm is actually selected; the language-neutral
frontend can derive that arm's selected alternative and every sibling's
not-selected alternative from the single raw ordinal. Exact goldens cover a
two-condition guard and all of its ternary vectors, guard rejection, nested
matches, identical and empty bodies, a local declarative-macro match and an
irrefutable one-arm match that correctly creates no branch. A panicking guard
leaves an incomplete selection frame and no fabricated alternative, while
baseline/instrumented values and output remain identical. Synthetic
proc-macro match tokens exposed a real boundary: their arm spans collapse to
one invocation location. The companion now inserts semantics-neutral private
arm markers in built MIR, maps them through rustc's real/imaginary match edges,
requires each marker to survive borrow checking exactly once, removes them,
and only then installs runtime hits. Unguarded and compound-guard proc-macro
matches retain exact arm identities without pre-analysis runtime calls.
Separately, built-MIR reachability excludes a statically unreachable authored
arm while retaining and measuring both reachable siblings. Nested synthetic
matches and synthetic match-guard MC/DC still require equivalent semantic
markers; they remain explicit blockers and publish no fabricated decision
vectors.

The CTFE provider spike is now executable rather than hypothetical. The
companion overrides `mir_for_ctfe`, inserts execution markers in original
blocks, splits multi-successor edges for independently identifiable edge
markers, and observes only those markers through a private in-process rustc
interpreter subscriber. Both true and false const-fn paths were observed while
const values and complete stdout/stderr stayed byte-identical to the ordinary
build. Edge identity is carried by each event, so concurrent evaluation does
not require guessing from adjacent log records. Rust still remains private:
the proof covers one controlled const function and does not yet supply the
complete const/static/inline-const/const-generic manifest, crash-safe event
publication, `RUSTC_LOG` coexistence or acceptable performance corpus.

The rustdoc launch boundary is now proven as well. Cargo's ordinary
`RUSTC_WRAPPER` still does not see synthesized doctest crates, so Supercov uses
a scoped launcher for the exact ordinary rustdoc and adds its compiler
companion as rustdoc's test-builder wrapper. The controlled fixture observes
standalone source, merged bundle source and merged runner source; maps hidden
and visible standalone lines through rustdoc's path/offset metadata; and joins
merged `__doctest_N` owners to the runner's source path, line and test name.
The launcher removes its private unstable-option bootstrap before user code is
compiled, and a stable `compile_fail` feature-gate case proves there is no
capability leak. Baseline/intercepted output (excluding elapsed-time values)
and checkout hashes match. Public tracing still requires real probes, exact
per-doctest attempt transport, custom rustdoc/wrapper composition and the full
doctest semantics/crash corpus.

Correctness corpus:

- original-versus-instrumented differential programs checking values, panics,
  output, drops, side effects and ordering;
- property/fuzz generation over nested control flow, patterns, ownership,
  async and error propagation;
- compile-pass and compile-fail cases across supported editions/MSRVs;
- an independent LLVM/rustc structural oracle where models overlap;
- independent MC/DC goldens rather than self-expected vectors;
- real ecosystem crates selected for macros, async, workspaces, generated code
  and unusual build graphs.

Exit gate: every in-scope source construct has a proven obligation/probe model;
every remaining exclusion is an explicit public blocker; zero unexplained
semantic differences.

### R2 — Complete automatic Cargo/runner attribution

Replace the current narrow `cargo test`/one-process-per-test prototype with a
runner architecture that is both exact and compatible with existing commands.

Work:

- preserve Cargo package/target/feature/profile/test filters and arguments;
- support unit tests, integration tests, doctests, examples and benches where
  they act as tests;
- support standard libtest, nextest and custom harnesses through discovered
  launch behavior rather than repository-specific names;
- carry exact identity across threads, async executors, subprocesses,
  workspaces, retries and crashes;
- attribute assertion/action phases for Rust assertions and common test
  frameworks without hardcoding application code;
- persist background and late evidence without assigning it to the wrong test;
- integrate the frozen kill-resilient transport with dynamic context carriers
  so committed observations survive termination and any uncommitted/lost work
  becomes explicit health rather than silent absence;
- fail closed for genuinely ambiguous mixed-language or unsupported runners.

Prefer an owned in-process context carrier once it proves the same isolation as
the process-per-test reference. Retain process-per-test as a correctness oracle
or explicit fallback only if its semantics and UX are acceptable.

Exit gate: the concurrency/crash/retry matrix produces exact, deterministic
per-test evidence with no contamination, loss or repository-specific setup.

### R3 — Make lifecycle and performance release-grade

Optimize the proven architecture, not a partial one.

Work:

- authenticate and incrementally reuse the isolated instrumented workspace and
  Cargo artifacts without stale evidence;
- reduce transformation/setup, cache authentication, runner process and
  evidence-publication costs;
- use compact buffered/mmap-safe observation records and avoid repeated
  identity encoding/compression;
- verify concurrent-run locking, atomic publication, deterministic cleanup,
  retention and recovery from signals, rename failure and ENOSPC;
- benchmark representative small, workspace, macro-heavy and async projects;
- test APFS, common Linux filesystems and NTFS/Windows before claiming those
  native targets.

Exit gates:

- fair cold/cold and warm/warm total runtime at most 1.10x the identical
  uninstrumented command;
- no source/build modification and no terminal work debris after success,
  failure or recoverable interruption;
- evidence/query results remain identical before and after cache reuse;
- query latency and binary-size gates from the master plan remain green.

### R4 — Dogfood to a public Rust release

Use Supercov itself as the primary agent workflow, then confirm generality on
unrelated repositories.

Work:

- run the entire Supercov Rust workspace through the public zero-config path;
- use bounded queries to select gaps, write tests, rerun and verify exact diffs
  and attribution, recording every agent UX defect separately;
- require measurement completeness before interpreting the percentage;
- compare against independent oracle facts and ensure the real checkout is
  byte-identical after every run;
- repeat on unrelated pinned Rust repositories across the supported runner and
  build matrix;
- run the full local suite, corpus, crash and performance gates;
- use one consolidated hosted release matrix and one release only after all
  required native artifacts are available.

Exit gate: Rust support can be enabled without a flag, configuration,
third-party coverage dependency or hidden limitation, and an installed npm
package reproduces the verified local behavior.

## Work after the Rust milestone

Only after R0–R4 pass:

1. finish the owned Python transformer/runtime/pytest frontend using the same
   frozen v3 protocol and development-only coverage.py oracle;
2. add C/C++, then Go and OCaml under the same denominator, semantics,
   attribution, lifecycle, performance and zero-configuration gates;
3. add later JVM, Ruby, PHP and other frontends only when their strongest
   independent oracle and runner identity model are defined;
4. distribute the same Rust binary through PyPI, crates.io/cargo-binstall,
   opam, GitHub Releases, Homebrew and useful C-compatible packaging—wrappers
   may not duplicate analysis or coverage semantics;
5. return to the deferred human query presentation in
   `progress/cli-query-ux-feedback-2026-08-26.md` without changing the stable
   agent JSON contract accidentally.

## Definition of current-goal completion

The current objective is complete only when R0–R4 are all green and evidenced
in this repository. A private prototype, aggregate percentage match, successful
happy-path dogfood run, explicit limitation, or performance promise does not
satisfy a release gate. Any discovered semantic or attribution uncertainty
keeps Rust private and becomes a traced requirement with a reproducing test.
