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
- define language/frontend/model identity in evidence v3, including strict
  dual-read behavior for existing JavaScript evidence v2;
- specify limitation severity and when `measurement: complete` is legal;
- freeze runner/test/outcome/assertion/action identity and archive/query JSON;
- add malformed, truncated, mixed-language and unknown-version rejection
  vectors;
- produce a requirement-to-test traceability table so every public claim maps
  to an executable gate.

Exit gate: no implementation-defined semantics remain in the public Rust
coverage model or evidence envelope, and all frozen contract fixtures pass
through the shared analyzer.

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
- design kill-resilient observation transport so SIGKILL cannot turn executed
  obligations into silent absence;
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
