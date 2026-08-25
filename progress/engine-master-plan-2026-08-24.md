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

## Checkpoint — 2026-08-25 Phase 3 Rust JS instrumenter complete

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
- The live Babel/oxc differential gate covers 240 exact decision/point/branch/
  limitation manifests, 33 hand-authored behavior/effect/vector/hit cases,
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
  The harness now obtains results through a dedicated machine channel and
  rejects incomplete, crashed, or console-contaminated result streams. Its
  default, CI, conformance, and release invocations all name Rust explicitly;
  TypeScript can be selected only as a diagnostic reference.
- The release transform gate now measures 500 distinct files inside the Rust
  engine rather than timing the legacy Babel transformer over one concatenated
  source. Current measurements are 25.60 ms median and 30.12 ms p95, with a
  2.56 s linear 50,000-file projection. The temporary Node/JSON migration
  boundary measures 54.96 ms median and is intentionally removed in Phase 4.
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
  The current 186-test TypeScript-reference run
  `2026-08-25T00-29-05-152Z` and Rust run
  `2026-08-25T00-30-26-768Z` both pass. They have identical 1,427 decisions,
  1,953 conditions, 551 covered conditions, outcomes, and MC/DC. The only
  diff is 19 line/branch observations in `engineInstrumenter`,
  `engineProcess`, and `engineEvidence`, which deliberately execute different
  outer-engine implementations. Rust completed the tests in 14.7 s versus
  54.8 s for the recursively instrumented TypeScript reference.
- Assertion phases without measured application evidence are now reported as
  an ambiguity warning, not falsely asserted to be transport loss. Static
  contract/data tests can legitimately have no application probe. Corrupt
  evidence and explicit transport failures remain errors. Unscoped health and
  readiness requests also form an attribution boundary instead of inheriting
  the launching test, while nested callbacks with explicit request scope
  retain that scope. Timestamp-only correlation remains diagnostic and cannot
  upgrade action/assertion confidence.
- The release contract audit also corrected a stale fixture golden that counted
  an exported function declaration as both an executable statement and a
  function entry. The frozen denominator has three real statements plus one
  function obligation; an explicit model regression prevents double-counting.
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
- Phase 3 is promoted. The independent syntax matrix is green for 43 cases on
  Node 22, Node 24, Chromium, Firefox, and WebKit. Essential SEO Rust dogfood
  run `2026-08-25T00-25-52-427Z` passed all 30 offline tests; a preceding VM
  snapshot tar race failed before test execution and the retry isolated that
  failure to Supermachine's bake path. The six production fixture shapes,
  retries, crashes, async context, concurrency, multi-worker attribution,
  complete Test262 corpus, MC/DC oracle, self-dogfood, and transform budgets
  are green. Every observed TypeScript/Rust deviation is either normalized
  execution identity/timing, proven same-engine workload nondeterminism, or
  the expected execution of different outer-engine implementation files.
  `instrument_candidate` now declares the complete
  `complete-js-instrumenter-v1` contract with no port-progress limitations.
  Rust selection remains private because the product engine shell is not yet
  complete. Phase 4 already owns frozen probe/agent-JSON contract slices and
  evidence packing/strict reading; discovery, workspace, supervision,
  analysis, solving, indexing, querying, and lifecycle are the next Rust ports.
- Phase 4 has begun with the first language-neutral analyzer slice. Rust now
  owns deterministic masking-MC/DC witness search using dense candidate
  bitsets, plus exact line/statement/function/branch/condition summary
  arithmetic. Manifest condition counts are explicit, so unexecuted decisions
  cannot disappear from the denominator. The independent Clang fixture and
  250 generated models require exact witness order and summary parity with the
  regression reference. This core accepts no JavaScript-specific types and is
  the shared verdict engine for every future language frontend.
- Phase 4 now also owns language-neutral evidence normalization and complete
  report reconstruction. Rust merges runtime, browser, scoped server and
  background evidence; de-duplicates vectors and hits; resolves retries and
  terminal outcomes; derives all/passed/failed views; preserves per-test and
  per-phase attribution; computes explicit action/assertion confidence; and
  aggregates lines, test files, runner/kind views and transport diagnostics.
  One hundred generated evidence models require exact serialized report
  parity, while real immutable Playwright, `node:test`, esbuild, webpack and
  SWC archives require exact report, attribution, outcome, filter and transport
  parity when read directly by the strict Rust archive reader. Independent
  Rust regressions prove that an unexecuted manifest decision cannot disappear
  from the denominator, timestamp overlap cannot claim causal confidence,
  expected failures cannot become verified coverage, malformed JSONL remains
  visible without discarding valid records, and cross-run evidence is rejected.
  The differential also found and removed an old TypeScript-only serialization
  leak where an internal `Set` appeared as `explicitPhases: {}` on line
  results despite not existing in the frozen public schema.
- Phase 4 spike S2 is closed by
  `progress/query-index-adr-2026-08-25.md`. On the pinned 100,000-line corpus,
  gzipped JSON misses the complete CLI gate at 16.473 ms p95 before process
  startup; authenticated+validated rkyv measures 5.231 ms, authenticated+
  verified FlatBuffers 9.306 ms, and the selected fixed-layout columnar index
  with SHA-256 header/page validation 0.129 ms. The fixed format is also stable
  across Rust/compiler layouts and 34% smaller than rkyv. Its section layout,
  checked arithmetic, corruption corpus, immutable publication and rebuild-on-
  version-mismatch rules are now Phase 4 implementation requirements.
- The production Rust crate now implements the selected index container rather
  than retaining the benchmark as an aspiration: a 4 KiB authenticated v1
  header binds evidence hash/length, archive schema, analysis identity and
  producer ABI; a checked section directory uses fixed little-endian widths;
  and every read authenticates each touched 64 KiB page before returning bytes.
  Publication uses unique temporary files, `fsync`, and atomic rename; readers
  reject symlinks, stale identities, malformed record shapes, overflow, bounds,
  overlapping regions and corrupt pages. Tests prove old mappings survive an
  atomic replacement and invalid inputs leave no published artifact. The typed
  coverage sections and agent query operators are the next layer; the shipped
  TypeScript gzipped-JSON cache remains in place until that layer reaches the
  same black-box query gates.
- The language-neutral exact smallest-test-set solver has moved into Rust.
  It preserves MC/DC witness-pair choices rather than flattening them into
  ordinary set cover, expands file-scoped setup evidence only with selected
  tests, recomputes every structural metric for each final subset, rejects
  background/unattributed evidence, proves unreachable targets explicitly,
  and enforces the frozen search-state budget. One hundred twenty generated
  mixed line/statement/function/branch/MC/DC models require exact selected and
  expanded identities, summaries and explored-state counts against the
  regression engine; independent Rust tests cover redundant vectors,
  unattributed evidence and combinatorial-budget failure. The remaining query
  work is the typed mmap column layer and the complete summary/files/file/
  decision/covers/test/diff agent surface.
- The typed mmap layer now stores interned UTF-8 strings, complete structural
  summaries and per-file gap arithmetic for all, terminal-passed and failed
  views as fixed little-endian records—not JSON report blobs. Readers validate
  record widths, reserved fields, references, UTF-8, view cardinality,
  decision/count ordering, limitation masks and recomputed gap scores. Five
  real immutable fixture archives require exact round trips for every summary
  and file gap. More importantly, the first production query operators now
  read only these columns: `coverage files` and `coverage gaps` produce
  byte-identical frozen agent-JSON envelopes across Playwright, `node:test`,
  esbuild, webpack and SWC with varied outcome filters, metrics, pagination
  offsets and limits. The same fixed records now materialize every observed
  test-kind, runner, and valid kind+runner projection. Each projection
  recomputes masking-MC/DC witnesses from its own selected vectors and records
  whether a missing obligation is covered by other tests or uncovered
  everywhere. Byte-identical live queries prove those provenance filters on
  the same five archives. A typed decision-gap section now also powers the
  agent-oriented `coverage file --group decision` query, with exact
  per-projection witness recomputation, location/missing ordering, totals and
  pagination; its live JSON is byte-identical across that archive matrix.
  Typed provenance-dimension records now power `coverage kinds` and
  `coverage runners`, including each dimension's independently recomputed
  structural summary and stable pagination. Their byte-level gate uncovered
  and fixed a Rust/JavaScript representation difference: integer-valued
  percentages now serialize as JSON integers (`100`, not `100.0`) everywhere
  in Rust agent output, preserving the frozen JSON byte contract centrally.
  A typed projection section now powers the complete `coverage summary`
  envelope for every outcome/kind/runner selection. It stores independently
  recomputed structural coverage, measurement blockers, transport health,
  action/assertion attribution, empty-evidence diagnostics, confidence,
  test outcomes, gap counts and source-scope roots/counts. The full summary
  JSON—not selected fields—is byte-identical on all five real archives for
  both provenance-filtered and passed-only views. Strict relation-range
  validation caught a scope-root-count/summary-flag layout overlap during the
  differential; the fixed-width layout was corrected before publication.
  `coverage scope` now reads typed scope-entry records with strict file/status/
  reason/package-root validation, limitation counts and kind masks. It
  preserves ambiguous/included/excluded ordering, roots, aggregate counts,
  measurement status and pagination byte-for-byte on real scoped archives;
  malformed source-scope or limitation shapes fail index construction rather
  than silently disappearing from the completeness model.
  The first normalized attribution graph is now indexed as typed line,
  confidence, test, phase and obligation-anchor records. `coverage covers`
  resolves exact per-line test provenance and causal action/assertion phases,
  paginates tests and phases independently, and reports decision/branch/point
  anchors without falsely calling a non-line location uncovered. Point-anchor
  test relations are retained so kind/runner filters recompute coverage rather
  than reusing an invalid aggregate. The complete agent envelope is byte-
  identical to the shipped CLI across immutable Playwright, `node:test`,
  esbuild, webpack and SWC archives, including filtered provenance and
  anchor-only locations. Independent index tests cover line confidence, test
  provenance, decision and point relations without relying on a serialized
  report blob.
  The reverse attribution edge, `coverage test`, now also runs exclusively on
  normalized typed columns. Per-test retries and terminal attempts, source
  lines, hit IDs, decision-vector groups with ternary short-circuit values,
  point/branch metadata, complete decision metadata and phase evidence counts
  are independently validated fixed records. Unique-test detail and ambiguous
  selector listings preserve the frozen per-category pagination contract and
  produce byte-identical agent JSON on the same five real archive families.
  This establishes bidirectional line/test attribution without coupling the
  query engine to JavaScript report objects and leaves the same evidence graph
  reusable by future language frontends.
  `coverage decision` now completes the MC/DC detail edge over typed decision,
  observation and condition records. It preserves short-circuit ternary
  vectors, per-vector provenance/phases/confidence, assertion-linked
  conditions, exact witness vectors and both witness-test sets. Kind/runner
  filters discard unrelated observations and recompute first-pair masking
  witnesses in Rust; aggregate witnesses are never reused across provenance
  subsets. Both exact-ID detail and ambiguous file:line selection, including
  independently paginated conditions/observations/tests, are byte-identical
  on all five real archive families. Index round trips independently require
  the reconstructed decision graph to equal the analyzer graph.
  The ungrouped `coverage file` surface is now Rust/index-native as well. It
  composes typed lines, point/function metadata, branch alternatives,
  independently filtered decisions, per-file tests and complete measurement-
  limitation records into the frozen obligation inventory. Other-test
  provenance is recomputed from exact test relations; MC/DC vector text and
  witness-based cross-kind coverage are derived from the normalized graph.
  Byte parity covers all five archive families, provenance filters, pagination
  and the `all`/lines/branches/MC/DC/functions metric selections. Independent
  records preserve limitation id/kind/location/source/reason and point source/
  test relations, including files that sit outside the measured denominator.
  The exact Rust smallest-test-set solver now also owns the complete
  `coverage minimize` agent surface. Outcome/provenance selection constrains
  candidates without shrinking the obligation denominator; the response
  preserves selected and setup-expanded IDs, exact recomputed summaries,
  explored-state accounting, candidate counts, per-test provenance and stable
  pagination. Its JSON is byte-identical on all five real archive families in
  addition to the 120 generated mixed-obligation solver models. Integer-valued
  targets use the shared JavaScript-number serializer, preventing `50.0` from
  silently breaking the frozen byte contract.
  Rust now also owns `diff` over two authenticated typed indexes. Covered line,
  branch-alternative and MC/DC-condition identities are compared without
  report JSON; percentage deltas, gained/lost counts and independently paged
  labels preserve the frozen envelope. Real historical archive pairs require
  byte parity for both all-attempt and passed-only views. Branch records retain
  their parent decision identity, and output sorting deliberately implements
  JavaScript UTF-16 order so non-BMP paths cannot create cross-engine drift.
  The remaining run-list/integrity/lifecycle agent surfaces remain
  before this path can replace the shipped query engine.
- Phase 4 now owns strict persisted-run discovery and the disposable index
  lifecycle in Rust. `run.json` is deserialized into a closed schema; IDs,
  SHA-256 fingerprints, Git revisions, archive schema/format/name/count and
  compressed length are validated; and run-store, run-directory, metadata and
  evidence symlinks are refused. Inventory returns valid runs and separately
  sorted rejection diagnostics, so one corrupt historical entry can no longer
  disappear silently or hide healthy runs. Exact/prefix/latest selection and
  stale-reason ordering match the frozen contract. Query-index identities now
  bind the actual evidence hash and length plus a deterministic compile-time
  fingerprint of the Rust engine/contracts source and Cargo lock, rather than
  the temporary harness constants. A query opens an existing mmap index only
  after authenticating every page and validating every typed section; stale,
  corrupt, truncated or linked indexes are reconstructed from authoritative
  evidence and atomically replaced. Evidence is hashed again around analysis
  and publication so a mixed-generation index is never accepted. Real fixture
  tests prove first-build/reuse/corruption repair, and a symlink attack test
  proves replacement leaves the linked user file untouched. Run-list output,
  current-project fingerprint creation, pruning/retention and atomic run
  publication still have to move into Rust before lifecycle ownership is
  complete.
  The typed `runs` operator is now also implemented over that inventory. It
  paginates metadata without triggering analysis, reads all/passed/failed
  percentages only from an already authenticated binary index, treats a bad
  disposable index as "not indexed" without mutating it, and reports stored-
  versus-current stale reasons in frozen order. Its agent data and pagination
  use the shared bounded JSON contract. Production CLI routing remains gated
  on Rust current-project fingerprint discovery; until then the operator is a
  tested engine layer rather than a partially exposed command.
- Rust now owns the current JavaScript project/source inventory and run-
  integrity inputs. The source walker deterministically discovers conventional
  roots, workspace packages, manifest exports and JSONC `tsconfig` roots;
  excludes declarations/tests/fixtures/config/tool/generated trees; and turns
  every unclassified first-party file into a blocking source-scope limitation.
  It never follows links and rejects a linked explicit root. The Rust parser,
  not regular expressions, discovers project-owned Playwright fixtures,
  relative compiled-output imports and build-config environment comparisons.
  Build selection distinguishes direct Jest/Vitest/`node:test`, generic builds
  and Vite, including project-owned environment overrides. Eight source-scope
  and ten complete project shapes—synthetic plus Playwright, `node:test`,
  esbuild, webpack and SWC repositories—have exact differential parity;
  independent tests cover safer inline-comment JSONC and symlink boundaries.
  Run fingerprints are now language-neutral engine code: source, tests,
  workspace package manifests/locks, nested build/test configuration,
  frontend shims, Rust transformation identity, execution environment and Git
  state occupy separately domain-separated SHA-256 inputs. Changes in each
  domain have independent regressions, generated caches cannot inflate the
  test fingerprint, links cannot enter a fingerprint, and an execution-only
  change now marks a run stale instead of being missed by the historical
  comparator. The JavaScript frontend contributes only its runtime shim files
  and frozen frontend version; future languages use the same Rust integrity
  implementation. Production CLI routing and the versioned atomic cutover
  still need to wire these authoritative Rust fingerprints into publication.
- Persisted-run queries now traverse the real Rust lifecycle end to end rather
  than only the archive differential harness. One path discovers and validates
  a stored run, hashes its authoritative evidence, lazily creates or reuses the
  authenticated binary index, opens its typed mmap sections and executes the
  shared agent-query operators. Summary, scope, files/gaps, dimensions,
  decision grouping/detail, file detail, covers/test attribution,
  minimization and historical diff all produce byte-identical bounded JSON on
  the five real fixture stores. Tests copy stores into isolated temporary
  projects and deliberately omit every old JSON/binary cache, proving first-
  query reconstruction rather than accidentally accepting historical output.
  Query execution is now path-independent engine code; storage policy remains
  solely in `run_store`. Public CLI routing remains gated on dynamic waiver
  overlays and human-output parity. Waivers must not be baked into the
  disposable evidence index because `supercov.waivers.json` is mutable project
  policy rather than run evidence; the Rust query layer must evaluate and
  annotate them at read time.

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
