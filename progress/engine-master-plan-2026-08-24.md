# Supercov engine end-state — master plan (2026-08-24)

Decision: optimize for best possible UX and best possible performance, no
shortcuts. Rewrites are approved. This document fixes the target architecture,
the acceptance gates, and the order of work. It deliberately does not touch
code; a compatibility sweep is in flight and Tier 1 (trust) still lands first.

## Committed end-state decisions

1. **Rust core engine, single static binary.** CLI, project discovery,
   workspace isolation, instrumentation orchestration, evidence analysis,
   and query engine all compile into one 5–15 MB static binary per
   platform. The current TypeScript engine is a *regression reference* only
   while the port is incomplete—not the semantic authority. As soon as the
   complete
   Rust engine passes the frozen differential and conformance gates, the
   cutover removes the old TypeScript engine in the same consolidation phase.
   There is no permanent engine selector and no extra fallback release.
2. **oxc for JS parsing/codegen** in the Rust instrumenter (published
   benchmarks: ~40x Babel, ~4x SWC for parse→transform→codegen). This is a
   true port of the ~1,600-line instrumenter, not a parser swap — Babel and
   oxc ASTs differ.
3. **Collectors stay in the target language.** The JS runtime/adapters remain
   JS generated into the isolated workspace; the future Python collector is
   Python generated the same way. The binary question is only the engine.
   Per language the engine grows exactly two things — where probes are
   inserted, and how test/phase identity propagates to a probe. The evidence
   contract, analysis, MC/DC pair search and query surface are shared and are
   never rewritten per language; probe v2's ternary-vector/epoch model is language-neutral
   precisely to keep that true.
   The ownership rule is stricter than merely moving hot paths: **everything
   that can live in Rust does**. Target-language code is permitted only where
   it must execute inside a runtime, browser, compiler/plugin API, test runner,
   or assertion framework. Such shims may propagate context and append frozen
   evidence records; they may not implement manifests, coverage arithmetic,
   MC/DC solving, merging, persistence, querying, or policy. Ahead-of-run
   source transformation also belongs in Rust whenever a sound parser exists;
   runtime hooks remain thin loaders for dynamic/generated modules. This keeps
   one correctness implementation and one performance profile across every
   language rather than accumulating a Python product, an OCaml product, etc.
4. **No resident processes — ever.** (User decision 2026-08-24; supersedes
   the earlier `supercov serve` proposal.) Every invocation is fire-and-
   forget; "no resident service" stays a product guarantee. Query latency is
   solved at the root instead: (a) Rust engine cold start ≤10 ms; (b) the
   query index becomes a memory-mappable zero-copy binary format so opening
   it costs milliseconds at any repo size — persistent *data*, not a
   persistent *process*, with the same integrity checks as today; (c) engine
   layering so read-only queries never load instrumentation code (on the TS
   engine: dynamic imports so query commands skip Babel — cheap interim win).
   MCP, if ever shipped, is a thin optional wrapper spawned and owned by the
   agent harness over the same CLI semantics — never an engine assumption.
5. **Probe architecture v2** — the real performance ceiling is instrumented
   runtime overhead, which no engine rewrite touches. Architecture gate
   ≤1.10x; post-architecture optimization target ≤1.05x. The
   frozen design uses base-3 decision frames (`unreached/false/true`),
   file-local numeric point indices, dense vector epochs for ordinary
   decisions, and per-attempt/phase epoch short-circuiting so hot loops enter
   the collector only once per obligation. V8 builtin coverage remains a
   possible cheap line/function source where
   attribution semantics allow (serial runners only — precise V8 deltas are
   process-global and cannot attribute concurrently interleaved tests, so
   probes remain the attribution mechanism; this constraint is load-bearing).
6. **Distribution matrix, ruff/uv pattern.** One release pipeline publishing
   the same binary everywhere: GitHub Releases artifacts; npm with
   per-platform `optionalDependencies` (esbuild pattern); PyPI platform
   wheels via maturin `bindings = "bin"` (well under the 100 MB PyPI file
   limit once the engine is Rust); Homebrew; `curl | sh`; cargo-binstall.
   Wrappers are exec-only glue.
7. **Frozen contracts, written as specs.** Evidence archive schema, run-store
   layout, CLI surface + JSON envelopes, waivers file format, and process
   supervision. (The no-resident-process decision removes serve entirely.)
   Both engines must pass the same black-box contract tests. Independent
   language behavior, coverage-model specifications, and external oracles are
   authoritative; TypeScript/Rust differences are diagnostics to investigate,
   not an automatic requirement that Rust reproduce a TypeScript defect.
   These specs are the Rust implementation's requirements document.

## Why a full rewrite is safe *for this project specifically*

The project already owns a runtime-agnostic conformance net:
- Test262 semantic-equivalence corpus (65,051 baseline-passing scenarios at
  revision `3655e7464de3d52643ecddd4b5f9f4f3e7f62398`) —
  validates instrumented-output *behavior*, not implementation.
- Independent Clang/LLVM MC/DC oracle.
- Golden fixture repos across Playwright/Vitest/Jest/node:test/opaque runners.
- The self-dogfood loop plus `supercov diff` for exact regression evidence.

A differential harness runs both engines on the same inputs and requires
identical frozen obligations plus semantically identical reports where the
contract is unchanged. It is a neighborhood/regression detector, not an
oracle. Every intentional Rust correction requires an independent semantic or
coverage-model test that demonstrates why the difference is correct; the
frozen contract is versioned deliberately when the correction changes it.

## Acceptance gates (performance)

| Metric | Today | Gate |
| --- | --- | --- |
| 500-file transform (median) | ~1,008 ms (Babel) | ≤50 ms |
| 50k-file monorepo transform | ~100 s extrapolated | ≤5 s |
| CLI query total (start + index open) | ~100–300 ms | ≤15 ms (Rust + mmap index) |
| Instrumented runtime overhead | ~1.04–1.06x pinned realistic | ≤1.10x architecture; ≤1.05x optimization |
| Evidence analysis, 25 MB raw | ~2 s cold | ≤200 ms cold |
| Engine binary (compressed) | n/a (needs Node) | ≤15 MB/platform |
| Workspace prep, 500 files | ~78 ms (clonefile) | unchanged (already floor) |

Gates are measured by the existing benchmark suite extended per phase; a gate
miss blocks flipping any default.

## Phase order and gating

- **Phase 0 (in flight): Tier 1 trust work.** Compatibility sweep, per-test
  empty-evidence diagnostic, docs reconciliation. Nothing below starts until
  the sweep's fixes land — the rewrite must port *fixed* behavior.
- **Phase 1: contracts + harnesses.**
  (a) Author the five contract specs from current behavior.
  (b) Differential/conformance harness: golden corpus of
  (fixture → evidence archive → report JSON) with a byte/semantic comparison
  mode able to run two engine builds side by side.
  (c) TS-engine query latency trim: dynamic imports so read-only queries
  never load the instrumenter stack (no daemon; fire-and-forget preserved).
- **Phase 2: probe architecture v2 contract.** First prototyped on the TS
  engine so Rust does not port an obsolete transport, but validated against
  independent semantic and coverage-model tests rather than TS behavior. Gate:
  identical MC/DC verdicts across Test262 corpus + full fixture matrix,
  overhead ≤1.10x, self-dogfood diff shows no lost attribution. Reaching
  ≤1.05x is deliberately deferred until the architecture and Rust parity are
  established.
- **Phase 3: Rust instrumenter crate (oxc).** Exercised behind
  `SUPERCOV_ENGINE=rust` by development, differential and ecosystem CI while
  the shipped TypeScript engine remains the user path. This selector is a
  migration tool, not a product feature. Gate: Test262 corpus green,
  exact frozen manifests across the matrix, independently correct behavior,
  and the 500-file gate met. A TypeScript differential remains a diagnostic.
- **Phase 4: Rust engine shell.** CLI, discovery, workspace (clonefile/
  FICLONE parity), run lifecycle, analysis (bitset MC/DC pair search),
  and query engine. Gate: every differential deviation on the full sweep and
  self-dogfood matrix is either eliminated or justified by an independent
  conformance test and deliberate contract revision; query cold-start gate
  met. Then perform one atomic
  cutover: Rust becomes the sole engine; delete the TypeScript instrumenter,
  analyzer, report/query engine, orchestration implementation, migration flag,
  and Babel engine dependencies. Preserve frozen contracts, golden outputs,
  corpora and black-box tests—not a second executable engine.
- **Phase 5: distribution matrix + Python.** Release pipeline for all
  registries; then the Python collector (generated conftest/import-hook shim,
  pytest adapter) rides on the binary. PyPI wheels ship here.
- **Phase 6: every other language, at full quality.** Rust, C/C++, Go, then
  JVM/Ruby/PHP. Two tiers per language: **Tier A** adapts native coverage
  output (LLVM profdata, `go test -cover`), **Tier B** owns the
  instrumentation (our probe-v2 form with task-local epochs) to reach parity
  under in-process parallelism. Full per-test attribution and assertion
  linkage are achievable in compiled languages — an earlier note claiming
  otherwise described the cost-optimal path, not the ceiling. Tier A is not a
  stepping stone to discard: it is **Tier B's differential oracle** (Tier B's
  gate is "identical structural verdicts vs Tier A, strictly better
  attribution"), a permanent second evidence source for code we do not
  compile ourselves, and the measurement that decides whether Tier B is
  urgent for a given language at all. Gate per language: a
  semantic-equivalence corpus of its own, an explicitly declared attribution
  tier per runner, and enumerated limitations; a language whose corpus is not
  green is a language we do not claim to support. Full design, per-language
  matrix, attribution ladder, tier-ordering guardrails and spikes S8–S10:
  `progress/multi-language-architecture-2026-08-24.md`.

## Checkpoint — 2026-08-24 complete Rust JS instrumenter candidate

- Phase 0 findings, Phase 1's five frozen v1 contracts, black-box harness,
  probe-v2 contract, and Rust workspace are committed. Published v1
  manifests/evidence remain unchanged. Probe v2 uses exact base-3 vectors
  through 32 conditions and the exact v1 frame above that numeric cap.
- TypeScript remains a useful regression reference while the port is private,
  but is not authoritative. Language semantics, frozen obligations, Test262,
  the independent MC/DC oracle, and black-box contracts decide correctness.
  Its semantic/property corpus, frozen vectors, reset recovery,
  interleaved-attribution tests, and measured 1.04–1.06x realistic runtime
  overhead remain green.
- The oxc 0.133 Rust transformer now implements the complete frozen JavaScript
  denominator: statements, functions, control decisions, logical value
  selection, optional members/calls, logical assignments, parameter and
  destructuring defaults, try/catch, zero-versus-entered `for-in`/`for-of`,
  switch match/no-match, exact wide-decision fallback, and explicit dynamic
  code limitations. It also ports `with`, direct/dynamic evaluation,
  Function source reflection, unsafe parameter/class handling, framework
  request handlers, generic HTTP/WebSocket callbacks, full manifest
  generation, source maps, probe-v2 registration, and real runtime evidence
  calls.
- Classic scripts remain scripts and bind helpers through the injected global
  runtime; modules retain the virtual runtime import. Directive prologues,
  parenthesized assignment name inference, anonymous default names, optional
  call receiver references, comments (including Test262 YAML payloads), and
  source-map destinations have dedicated regression handling. The 64,171-
  comment Mozilla staging stress file transforms and runs in about 1.6s after
  eliminating quadratic comment editing and line/column lookup.
- The live Babel/oxc differential gate covers 237 exact decision/point/branch/
  limitation manifests, 32 hand-authored behavior/effect/vector/hit cases,
  and 160 deterministic generated programs. Rust and TypeScript also produce
  byte-identical archived manifests and exact summary/files/gaps JSON for a
  mixed Vitest + two-worker Playwright production run, including request,
  popup, user-context, service-worker, and WebSocket attribution.
- The complete pinned Test262 gate at revision `3655e746...` is green over
  41,593 selected files. Four disjoint shards observed 65,053 baseline-passing
  scenarios in total with zero Rust transform failures and zero semantic
  failures. (The monolithic run observed 65,051; baseline host support has a
  two-scenario scheduling variance, and every execution is compared only to
  the passing baseline in the same invocation.) A representative monolithic
  timing measured 598.54s baseline execution, 14.67s Rust transformation, and
  454.59s instrumented execution; this conformance workload shows no gross
  runtime regression, though it is not the realistic overhead benchmark.
- The private production selector batches an entire direct workspace or Vite
  inventory through one Rust child and includes the Rust binary fingerprint in
  run/build integrity. The Rust child is excluded from application child-
  process telemetry. `SUPERCOV_ENGINE=rust` remains the only activation path.
- The complete supported-fixture matrix now runs through that Rust selector,
  covering Vitest, Playwright, native `node:test`, the retained Jest
  compatibility fixture, CommonJS and ESM opaque launch interception,
  esbuild, webpack, SWC, Next.js, distributed merge, and the bounded agent
  query workflow. The Playwright surface is green with two workers in
  Chromium, Firefox, and WebKit, including request fixtures, user-created
  contexts, popup frames, service workers, and WebSockets.
- Exact-fingerprint build reuse is a first-class Rust gate. A reused bundle
  and its current preloader now share collector identity by build fingerprint
  rather than run ID; otherwise cached bundle probes silently become
  background evidence. esbuild, webpack, and SWC each prove fresh and reused
  runs retain four attributed tests and 100% passed-only MC/DC. Pull-request,
  weekly conformance, and release workflows run the Rust parity and browser
  gates; weekly/release Test262 shards invoke the release Rust binary.
- Engine parity is no longer an aggregate-score check. Six production shapes
  (mixed Playwright/Vitest, native `node:test`, esbuild, webpack, SWC, and
  Next.js) now require byte-identical manifests plus exact normalized raw test
  and server evidence, deterministic full-report semantics, outcomes,
  explicit action/assertion attribution, confidence, and representative agent
  query envelopes. Normalization is restricted to run IDs, clocks, temporary
  paths, process-derived worker/attempt identity, and timestamp-only phase
  correlation; a TypeScript-versus-TypeScript repeat proves the comparator is
  stable under those rules. Probe v2 also no longer archives registered but
  unobserved decisions as empty snapshots, matching the frozen v1 evidence
  contract rather than merely producing the same aggregate score.
- Supercov self-dogfood now compares large archives in memory-bounded child
  processes rather than retaining two expanded archives and reports at once.
  The current 180-test runs have identical obligations, outcomes, background
  evidence, and all evidence outside exactly four tests that intentionally
  execute the selected outer engine. Those tests execute `src/instrumenter.ts`
  under the shipped engine and `src/engineInstrumenter.ts` under Rust, so their
  different implementation-file coverage is required rather than waived.
- A watchdog regression exposed why implementation parity is insufficient:
  the old parent sent SIGUSR2 to every Node descendant after 60 seconds, which
  could terminate a healthy unpreloaded test child. Diagnostics are now
  signal-free. One atomically elected preloaded process reports active resource
  types on a timer, while the parent remains observational unless the user set
  an explicit command timeout.
- The first language-neutral Phase 4 ownership slice is now real Rust code:
  evidence archives are collected, framed, gzip-compressed, fsynced, and
  atomically published by Rust whenever the private Rust engine is selected.
  Its streaming reader is also implemented for the coming Rust analyzer. The
  contract tests reject unsafe/unsorted/duplicate paths, non-canonical headers,
  symlinks, missing manifests, truncation, concatenated gzip members, trailing
  data, and leftover temporary files; they prove deterministic gzip metadata,
  arbitrary binary payloads, and true Unicode code-point ordering. This audit
  found and corrected two historical JavaScript deviations—locale-dependent
  ordering and permissive archive reads—instead of preserving them as Rust
  behavior. The internal Rust child is explicitly excluded from application
  launch telemetry.
- The Playwright parity fixture now exercises a failed first attempt followed
  by a terminal pass, a skipped test, and an expected failure. The gate asserts
  the complete observed view reports `flaky`/`skipped`/`failed`, passed-only
  retains only retry 1 of the flaky test, and expected-failure coverage cannot
  become verified coverage. Rust fixture CI also executes the real SIGKILL
  transaction recovery and hung-process watchdog paths before the Firefox and
  WebKit reruns, so engine selection is covered under failure supervision as
  well as normal completion.
- Phase 3 is not promoted yet. Supercov dogfood and the six ordinary fixture
  shapes are now green under semantic comparison, including retries, crashes,
  async context, concurrency, and multiple workers. Remaining gates include
  the complete browser/Node syntax matrix and Essential SEO dogfood, followed
  by an audit that classifies every TypeScript/Rust deviation against the
  independent correctness hierarchy rather than forcing blind equivalence.
  `complete: false` is therefore still deliberate. Phase 4's engine shell now
  owns frozen probe/agent-JSON contract slices and evidence packing/strict
  reading. Discovery, workspace, supervision, analysis, solving, indexing,
  querying, and lifecycle are still owned by TypeScript and are the next port
  after Phase 3 closes.

## Non-goals and guardrails

- No accidental behavior change during ports; every port lands behind a flag
  with differential diagnostics and independent semantic gates. "Faster but
  unexplained" is a failure. A proven correction to historical JavaScript
  behavior is required to differ, with its own regression test and any needed
  versioned contract migration.
- Windows becomes a CI matrix member before any binary GA — no shipping
  binaries for platforms the suite has never run on.
- Contracts (schemas, CLI, envelopes, process supervision) change only by
  versioned, deliberate revision — never as a rewrite side effect.
- Passing parity authorizes deletion, not indefinite coexistence. A Rust
  implementation is not complete while equivalent production engine logic is
  still shipped in TypeScript. Only unavoidable Node/browser runtime and
  runner hooks survive the cutover.
- The agent-facing UX work (skill/playbook, post-run hints, grouped queries)
  continues on the TS engine throughout; users never wait on the rewrite.
