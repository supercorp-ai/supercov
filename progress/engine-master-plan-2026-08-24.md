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
  solely in `run_store`. Reviewed MC/DC waivers are likewise Rust-owned and
  remain a dynamic overlay: strict v1 parsing, ECMAScript-whitespace source
  matching, ID/line/positional selection, first-waiver ownership, applied,
  contradicted and unmatched classifications all run at read time, never enter
  evidence or disposable-index identity, and never mutate raw totals. An
  independently missing real-fixture condition proves byte-identical waiver
  summary, file inventory, grouped-decision sorting/counts, file obligations
  and condition detail. Public CLI routing now remains gated on current-
  project integrity wiring, structured public argument errors and human-output
  parity—not missing coverage semantics.
- Phase 4 lifecycle ownership has moved into Rust as an isolated engine layer.
  Run state is a closed typed schema; project locks use exclusive creation,
  live-owner detection, incomplete-write grace and owner-checked release;
  recovery derives every cleanup target from root plus validated run ID rather
  than trusting persisted paths. A killed pre-publication run is marked
  abandoned, while a fully renamed run is treated as the durable terminal
  record and only transactional leftovers are removed. Publication hashes the
  immutable evidence before and after copying, verifies the staged copy and
  compressed length, fsyncs both files/directories and exposes the run with one
  final directory rename. Retention is deterministic, dry-run safe, project-
  locked, preserves live work, and distinguishes `prune` (history/transients,
  keep shared cache) from `clean` (also current and legacy owned caches).
  Recursive deletion remains off the foreground path: owned trees are renamed
  into durable trash and one PID-locked child sweeps them. Every existing
  ancestor is checked for links before create/rename/delete; a regression with
  a linked `.supercov/evidence` proves external user data is untouched. Rust
  and TypeScript have exact differential results for prune, clean, dry-run,
  active-run preservation, cache policy and dead-run recovery. The Windows
  process-ownership implementation now exists and cross-compiles with warnings
  denied; actual Windows runtime and NTFS crash behavior remain explicitly
  gated on the configured compatibility-matrix run rather than being inferred
  from a cross-target build.
- Phase 4 now also owns isolated workspace and stable build-cache publication.
  The Rust layer performs deterministic tree copies with clonefile/FICLONE
  fallback through `reflink-copy`, excludes only the frozen generated-output
  roots and marker-owned Supercov stores, relocates internal links, rejects
  links escaping the canonical project root, and exposes a real
  `node_modules` mount point containing per-entry links. Every preparation,
  refresh, recovery and source-prune operation requires the live project lock.
  Stable cache refresh retains only explicitly fingerprint-selected artifacts,
  keeps the previous complete generation through the publication boundary,
  restores it on failure, recovers the newest complete generation after a
  killed process, and defers obsolete trees to the lifecycle sweeper. Rename
  boundaries fsync both source and destination parents when necessary. A
  black-box TypeScript/Rust differential compares complete file contents,
  modes, exclusions and link targets for isolated copies, cached refresh,
  artifact reuse, pruning and interrupted publication; the independent Rust
  tests additionally make the project lock and unchanged-source guarantees
  explicit. The public Rust engine now has a real SIGKILL-equivalent crash test:
  it is killed while cache staging and the live lock are both present, the last
  complete generation must survive, the next Rust invocation must recover and
  publish, and the source-project hash inventory must remain unchanged. That
  test passes on APFS and is part of the three-OS Rust platform gate. Windows
  dependency-directory mounts and relocated internal directory links now use
  NTFS junctions rather than privileged symbolic links; ordinary top-level
  dependency metadata files are isolated copies. Windows-only Rust tests verify
  both behaviors without Developer Mode. The Rust workspace now also has a
  private operation boundary used to force the ordinary-copy backend, ENOSPC
  on an in-progress staging copy, and failure of the second publication rename.
  All three tests prove source immutability, prior-generation survival,
  transaction cleanup, and (for rename failure) restoration through the third
  rename; they run on every platform rather than existing only in the
  TypeScript reference suite. Actual NTFS execution remains open until the
  matrix runs and is not inferred from Unix parity. Public execution is now
  available only through the explicit migration selector; it cannot become the
  default until the remaining platform gates close.
- The POSIX process-supervision contract is now implemented in Rust behind a
  private black-box surface. Commands start in a dedicated process group with
  inherited stdio and no runner-specific argument changes; positive-integer
  diagnostic/timeout configuration is validated before spawn; no timeout is
  invented by default. Sanitized descendant snapshots contain only PID, PPID,
  executable basename, state and accumulated CPU time and are collected
  through native process APIs rather than a possibly hanging `ps` subprocess.
  SIGHUP, SIGINT and SIGTERM handlers are installed with a serialized
  `sigaction` guard, forwarded to the entire group, escalated after the frozen
  grace period and restored byte-for-byte after each command. Explicit timeout
  returns 124; signal interruption returns 129/130/143; spawn and wait failures
  cannot become successful tests. Diagnostic-output failure is observational
  and cannot alter or orphan the command. A black-box Node parent/grandchild
  regression proves full-tree cooperative signal delivery, while another
  proves invalid configuration cannot spawn and diagnostics never expose a
  private argument. Windows now closes the pre-assignment escape race by
  creating every command suspended, assigning it to a private Job Object with
  `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`, and only then resuming its primary
  thread. Console control events provide cooperative interruption while
  `TerminateJobObject` is the grace-period authority; dropping the supervisor
  is the crash-safe full-tree boundary. Windows-only tests cover ordinary exit
  propagation and a parent/descendant timeout escape attempt, and the existing
  filesystem matrix now builds the Rust binary and runs the Rust platform gate
  on Windows, macOS, and Linux. The Windows code and tests cross-compile under
  MSVC with clippy warnings denied; the first real Windows Actions result is
  still a release gate, not silently treated as green locally. The unavoidable
  JavaScript capability/remote launch interception remains a runtime shim;
  provider-neutral local process ownership no longer needs to remain
  JavaScript after build orchestration is wired.
- Language-neutral external-phase orchestration is now Rust-owned as well. A
  frontend supplies explicit preparation/build commands and exactly one
  terminal test command; Rust validates that plan before spawning, invokes a
  lifecycle observer before each phase, records each phase's typed result and
  monotonic duration, and stops before the test on any failed preparation or
  build. One `ProcessSupervisor` and one saved/restored signal guard span the
  complete plan, closing the otherwise dangerous gap where a SIGTERM could
  arrive after the build exited but before the test spawned. This deliberately
  does not move JavaScript config/runtime generation into a generic process
  abstraction: the frontend shim still contains unavoidable Playwright,
  Vitest, node:test and remote-capability hooks, while Rust owns when and how
  every resulting command runs.
- The first private Rust-owned execution is now complete for a direct
  `node:test` project. Rust discovers the project, fingerprints source/tests/
  dependencies/configuration/frontend artifacts, prepares the isolated stable
  workspace, instruments application source with a direct runtime ABI,
  instruments native ESM `node:assert` argument evaluation for exact assertion
  phases, emits the complete sorted manifest, supervises the test command,
  packs the frozen evidence archive, atomically publishes immutable run
  metadata, prunes copied source and serves the persisted run through the Rust
  mmap query index. The black-box fixture proves five concurrent test scopes,
  100% line/branch/MC/DC, all three executable lines and both MC/DC conditions
  assertion-linked, no fallback attribution, a valid/complete passed view,
  unchanged project source and no retained raw-evidence directory in the
  shared workspace. This integration exposed and fixed two real boundaries:
  generated modules/scripts now use one explicit direct-runtime global instead
  of virtual/legacy bindings, and persisted-run queries derive validity from
  the test exit code rather than defaulting it to false. This remains private:
  A second black-box case now proves the same complete structural result for
  CommonJS plus top-level and nested `require` assertion bindings. Assertion
  discovery is resolved by lexical symbol identity, so shadowed imports,
  shadowed `require`, and unrelated assert-shaped APIs cannot be falsely
  claimed. Native `node:test` suites using imported `expect` matchers are
  attributed as exact assertion phases as well. The CommonJS module-export
  assignment is correctly retained as background/setup execution: the all-
  evidence view is structurally complete while the passed-test-only view does
  not falsely attribute module initialization to a test assertion.
- The second private Rust-owned execution frontend now runs zero-configuration
  Vitest projects, including commands hidden behind an npm script. Rust writes
  an isolated merged Vitest configuration, injects the unavoidable local
  setup/reporter shims, transforms lexical Vitest `expect` matchers, and keeps
  the exact run/worker/test/retry/attempt scope active for each serial worker
  attempt. The black-box fixture proves four passing tests, 100% line/branch/
  MC/DC, three assertion-linked executable lines, two assertion-linked MC/DC
  conditions, exact per-test phase operations, unchanged source, and a valid
  structurally complete persisted query. Snapshot evidence now retains
  de-duplicated explicit phase events instead of discarding the only causal
  link between an assertion and its obligations. Empty request contexts still
  suppress inherited process carriers, so unscoped health/background work is
  not accidentally promoted into a test. Both direct Node and Vitest fixtures
  pass the complete Rust differential suite.
- The third private Rust-owned execution frontend now runs Playwright through
  the same isolated lifecycle, including commands hidden behind npm scripts.
  Rust writes a path-confined merging config, relocates the original config,
  installs the coverage reporter, and configures the existing unavoidable
  page/request/browser collector shim. Project discovery—not a hardcoded
  package list—supplies custom fixture modules, test exports, and assertion
  imports. The black-box fixture uses a project-owned
  `@acme/browser-fixtures` package and proves two parallel worker processes,
  four unique attempt identities, four passing tests, full line/branch/MC/DC,
  exact assertion-linked obligations, unchanged source, and valid persisted
  Rust queries. Native assertion phase IDs have their own namespace so they
  cannot collide with Playwright action/assertion phases. This closes the
  direct Playwright runner boundary.
- Rust now owns the first complete build-backed browser execution as well.
  Vite project source remains byte-for-byte untouched in the isolated copy.
  Rust freezes an immutable per-file transform map keyed by original-source
  SHA-256, and a minimal generated Vite plugin returns only the matching Rust
  code/source map while resolving the isolated application-runtime ABI. A hash
  mismatch is a hard error rather than silently instrumenting changed source.
  Cache/Rollup/build outputs remain confined to the isolated workspace, and
  build then test run under one Rust process supervisor. The browser black-box fixture builds a real page, starts the
  project's unchanged Vite preview command through Playwright `webServer`,
  runs four cases in two workers, collects non-empty frame snapshots with
  explicit action phases, and reconstructs 100% line/branch/MC/DC plus exact
  per-test vectors from the persisted archive. Build and test timings are
  recorded separately and source remains unchanged.
- The initial generic compiler/build frontend is Rust-owned too. Rust places
  one isolated runtime plus declarations inside the narrowest discovered
  source root, rewrites its own virtual ABI import to a source-relative
  physical module, and leaves the generic build command otherwise unchanged.
  A strict TypeScript `rootDir` fixture proves compilation, emitted-module
  loader relocation, exact assertion attribution, and full persisted coverage;
  an independent esbuild fixture proves the same contract when the runtime is
  bundled into output. Neither fixture changes source or normal build output,
  and both record build/test durations separately. The same unmodified generic
  path now runs real webpack and SWC fixtures: both preserve source, publish a
  valid measurement-complete run, retain four passed `node:test` attempts and
  reconstruct 100% line/branch/MC/DC with assertion-linked obligations. This
  exposed a source-scope defect rather than a compiler special case: a root
  `build.*`, `gulpfile.*`, or `gruntfile.*` is build/tool configuration, not an
  ambiguous product file. Rust owns that correction and an independent test;
  the temporary TypeScript reference was aligned only so the live
  differential continues to diagnose unexplained changes.
- The authoritative conformance and browser gates were rerun after the pure
  Rust shell landed. Node 22, Node 24, Chromium, Firefox, and WebKit each pass
  all 43 syntax/runtime cases. The monolithic pinned Test262 invocation at
  revision `3655e746...` selected 41,593 files and observed 65,051
  baseline-passing scenarios, with zero Rust transform failures and zero
  semantic-equivalence failures. Its measured durations were 595.04 s for the
  original baseline, 17.05 s for Rust transformation, and 453.79 s for the
  instrumented corpus. This compares observable program behavior directly;
  it does not treat the TypeScript engine as the semantic oracle.
- Pure Rust-shell self-dogfood now runs the repository's native `node:test`
  command without the product TypeScript CLI. Run
  `rust-self-dogfood-2026-08-25T` passed all 188 tests, recorded 788 assertion
  calls, and published 394 evidence entries (40.7 MB raw, 2.53 MB compressed).
  It exposed a correctness bug that fixture-only probe observations could
  expand and overwrite the supposedly frozen manifest. The analyzer now
  treats the manifest as the sole denominator: out-of-scope synthetic
  observations are ignored, while an in-scope unknown decision or metadata
  mismatch is a hard error. Independent regressions cover all three cases.
  The repaired archived run is valid and queryable at 52.98% lines, 40.77%
  branches, and 28.30% MC/DC; structural completeness is false only because
  the current tests do not execute every declared obligation.
- Essential SEO also runs end to end through the private Rust shell with no
  Supermachine/provider special case. Run
  `rust-essential-seo-dogfood-2026-08-25c` built the isolated Remix/Vite app,
  launched the existing opaque offline command, passed all 30 Playwright E2E
  tests, packed 90 evidence entries, and served the stored run through the
  Rust mmap query path. Its report is valid and measurement-complete across
  154 included source files with zero ambiguous files, limitations, or corrupt
  records: 49.20% lines, 32.79% branches, and 12.88% MC/DC, including 1,130
  assertion-linked lines. A fully cold Supermachine layer rebuild took 317.5 s;
  the ready-pool suite took 38.1 s. Rust initialization, workspace preparation,
  adapter setup, instrumented build, and evidence publication measured 0.16 s,
  0.14 s, 5.00 s, 3.05 s, and 0.55 s respectively. The run also found and
  fixed an assertion-transform semantic hazard: a matcher such as
  `expect(await value()).toBe(...)` can never move its receiver `await` into a
  synchronous attribution callback. Such sites remain honestly
  execution-covered instead of receiving false assertion attribution.
- The current full checkpoint is green: 103 Rust engine tests, 189
  TypeScript/reference tests, type checking, clippy with warnings denied, all
  generated/real differential models, and direct Node, Vitest, Playwright,
  Vite, esbuild, tsc, webpack, and SWC integrations. Remaining atomic-cutover
  blockers are now narrower: execute and pass the newly wired Windows Job-
  object plus APFS/NTFS crash/filesystem matrices; require a sustained zero-
  unexplained-diff release window; then
  delete the TypeScript instrumenter, analyzer, discovery, orchestration,
  persistence/query implementations and Babel engine dependencies in the same
  consolidation. Only unavoidable Node/browser/test-runner collectors and
  runtime shims survive.
- Public cutover wiring has started with the lifecycle commands. The Rust
  binary now identifies itself honestly as a private differential candidate,
  parses `prune`/`clean` plus `--keep`/`--dry-run` directly, invokes the
  Rust-owned locked lifecycle, and reproduces the frozen human summaries and
  exit-2 argument failures. Live dry runs against the real Supercov store were
  byte-identical to the temporary TypeScript CLI for both commands. This does
  not flip the npm wrapper or authorize engine deletion; public run/query,
  structured-error, native packaging, and cross-platform gates remain.
- The complete public agent-JSON query grammar now enters Rust directly in the
  candidate binary. Rust parses the frozen instance-first `runs <id> coverage`
  hierarchy, every resource selector/filter/metric/group/sort/pagination option,
  minimization and `diff`; opens or rebuilds authenticated immutable indexes;
  computes current-project staleness with the same Rust frontend inputs used by
  execution; overlays reviewed waivers; and emits the bounded v1 success/error
  envelope. Query failures retain typed identities through the operator layer,
  so source/test/decision misses, ambiguous selectors, empty provenance views,
  unavailable scope, unreachable targets and solver limits are never recovered
  by parsing diagnostic strings. A public black-box differential covers every
  resource plus malformed hierarchy, numeric pagination, filters, selection,
  minimization, diff and exact structured failures. All outputs are byte
  identical to the temporary reference except deliberately excluded engine-
  identity staleness fields: the Rust instrumenter/configuration fingerprint is
  independently more complete and must differ from a run produced by the old
  engine. The differential also found a real reference bug in
  `file:line:column` parsing; Rust had the documented behavior, so the temporary
  TypeScript implementation was corrected and independently regressed instead
  of teaching Rust the bug. Integral timing metadata now uses JavaScript-number
  serialization, closing the last otherwise-spurious JSON byte difference.
  Human rendering now uses the same typed `IndexedQueryOutput` enum as JSON—no
  serialization round-trip and no second query implementation. Every summary,
  inventory, gap, dimension, scope, grouped/file detail, decision, cover,
  test, minimization and diff variant reproduces the frozen text, pagination
  command and warning/error behavior. Black-box tests cover both the complete
  webpack run and the mixed-outcome Playwright run with blocking source-scope
  limitations; a live Essential SEO comparison additionally exercised large
  uncovered-obligation pages, decision sorting, exact test detail and subset
  minimization without a difference. The only normalized text remains the
  intentional old-engine fingerprint reason. Both human and JSON query slices
  are active in the private Rust candidate; the npm wrapper remains on the
  shipped engine until wrapped execution and platform gates close.
- Public wrapped execution now enters Rust through
  `SUPERCOV_ENGINE=rust npx supercov -- <command>`; the default npm path remains
  TypeScript and there is still no permanent product selector. Rust generates
  exact UTC run identities, discovers/fingerprints the current project,
  prepares and instruments the isolated workspace, supervises build plus test,
  packs and atomically publishes evidence, renders progress/timings, and
  returns the original child exit. Failed suites publish their useful evidence
  with a failed terminal lifecycle state. SIGHUP/SIGINT/SIGTERM are forwarded
  to the complete process group, return 129/130/143, publish no misleading run,
  release the project lock, and leave only the small recoverable terminal
  state. RAII cleanup removes transient evidence and copied source on every
  return path; abandoned runs are recovered before new work starts. A public
  black-box test covers empty command, success, failure, source isolation,
  durable evidence, diagnostics, lock cleanup, and SIGINT.
- Runner identity and assertion attribution now survive the public build-tool
  path rather than merely the private direct fixtures. Assertion-only Rust
  transforms emit inline source maps, Node runs with source-map support, and
  node:test registration columns are canonicalized to the source line because
  Babel, esbuild, SWC and TypeScript disagree on mapped call-expression
  columns. Vitest lexical `expect` calls remain ahead-of-run transformed;
  Playwright's project-discovered assertion module uses a static fallback plus
  an assertion-phase bridge. The bridge creates one Playwright phase before
  assertion arguments execute and reuses that same ID for matcher/browser
  work, preserving both synchronous coverage causality and action linkage
  without duplicate phases. The temporary TypeScript reference was aligned to
  this independently tested contract so this accuracy improvement is not hidden
  by parity normalization.
- Whole-engine public parity is green across the mixed Playwright/Vitest,
  native node:test, esbuild, webpack, SWC and Next.js fixtures: exact frozen
  manifest, evidence semantics, report, per-test attribution, outcomes,
  confidence, and human/JSON queries. Probe v2 deliberately removes three
  duplicate server records in the mixed fixture while retaining the identical
  unique hit/vector set; the comparator permits only this strict multiset
  reduction and still rejects any missing unique observation. Nested npm/pnpm/
  yarn scripts receive both generated runner configs because the top-level
  command cannot soundly predict a later runner process. The complete
  `npm run test:rust` and `npm run test:rust-engine` gates pass after these
  changes. Windows Job Object ownership is now implemented without a child-
  escape window and the three-OS compatibility workflow runs the Rust platform
  tests; the MSVC target cross-build and clippy gate pass locally. Rust-engine
  mid-publication kill/recovery is green on APFS, and NTFS directory mounts no
  longer depend on privileged symlink creation. Remaining cutover blockers are
  a real green Windows/macOS/Linux matrix (including NTFS crash, junction, copy-
  fallback, injected rename and ENOSPC cases), native platform packaging,
  sustained zero-unexplained-
  diff releases, and then the one atomic deletion of the TypeScript engine and
  Babel engine dependencies. Language/runtime collectors remain; duplicate
  engine implementations do not.
- Native npm distribution now has a frozen target registry and a generated-
  package contract for macOS arm64/x64, Linux arm64/x64 glibc and musl, and
  Windows arm64/x64. The exec-only npm loader detects Linux libc, resolves the
  exact platform package, rejects name/version mismatches and malformed or
  missing binaries, and permits `target/debug` only inside a source checkout.
  A clean packed-install integration disables lifecycle scripts, installs the
  primary plus platform tarball, completes a real Rust coverage run, and then
  proves wrong-version, missing-binary, missing-optional-package and unsupported-
  target errors. The Apple arm64 release binary is 4.1 MiB uncompressed and
  1.9 MiB under deterministic gzip, comfortably below the 15 MiB gate. A
  manual-only eight-target native artifact workflow now builds on native
  macOS/Linux/Windows arm64/x64 hosts, adds glibc/musl Linux variants, performs
  packed installs on matching hosts, validates direct musl execution, enforces
  the compressed-size gate, records binary/tarball SHA-256 digests and uploads
  the generated artifacts. Every npm tarball also receives GitHub artifact-
  attestation build provenance from the workflow's ephemeral OIDC identity.
  A final aggregate job accepts only a complete eight-package, single-version
  release set whose tarball sizes and SHA-256 digests match each native job;
  it independently checks every packed npm manifest and embedded binary.
  It is deliberately not triggered by ordinary pushes
  so this groundwork consumes no Actions minutes until explicitly requested.
  The
  distribution ADR explicitly rejects WASI as an unsound fallback for a CLI
  that owns processes/signals/filesystem transactions. Platform packages are
  generated from release binaries and are not committed or published by this
  checkpoint. The native matrix is defined but has not yet produced a real
  hosted-run green result; the attestation step is wired but therefore has not
  yet produced signed provenance. Initial npm package claims,
  coordinated publication, GitHub artifacts, PyPI/Homebrew/cargo-binstall/opam
  and C-compatible wrappers remain Phase 5 gates.
- The shared producer boundary was first frozen independently as
  `contracts/frontend-v1`. The first real Python spike exposed that v1 had no
  honest phase kind for ordinary test-body execution: mapping pytest's `call`
  phase to `assertion` would overclaim causality, while `background` would lose
  exact test attribution. V1 remains immutable and checked in; the current
  `contracts/frontend-v2` adds only the `test` transition and records the
  reason for the version change. A language frontend contributes a complete
  obligation manifest, normalized observations, run/worker/test/retry/phase
  identity and action/assertion transitions only to its declared precision,
  plus explicit limitations. Rust retains manifest merging, validation,
  attribution, analysis, persistence and every query. Attribution is declared
  per actual runner and execution model rather than optimistically per
  language. Strict Rust types reject unknown fields, invalid identities,
  duplicate or inconsistent limitations, unexplained precision downgrades,
  impossible exact causal linkage and exact test causality from parallel-
  unattributed execution. Python and LLVM adapters must pass this contract
  unchanged. A Rust analyzer-entry validator
  now additionally binds a declaration to the manifest's exact limitation-ID
  set and observed runner set, verifies exact test/scope/retry identities, and
  rejects illegal, duplicate, unresolved or cyclic phase transitions before
  coverage analysis. The declaration is not added to frozen v2 archives yet;
  that requires an explicit archive-schema migration before an adapter can be
  publicly enabled.
- The private Python Tier-A adapter has started. A checked-in pytest fixture
  runs under coverage.py 7.15.4, exports through documented `Coverage` and
  `CoverageData` APIs, and differentially reconstructs 10/12 executable lines
  and 6/8 branch arcs in the shared Rust analyzer. The importer rejects unknown
  fields, malformed line partitions, source/path drift, run/context/outcome
  inconsistencies, unattributed executed facts and non-branch measurement. It
  preserves import-time module execution as background rather than dropping
  it. A Python 3.14 run also proved that coverage.py's `sys.monitoring` core
  warns dynamic contexts can be incomplete; exact attribution is therefore
  accepted only from explicitly selected `ctrace` or `pytrace` collection.
  Pytest setup/call/teardown phases are exact, but action and individual
  assertion linkage are explicitly unavailable. MC/DC vectors and exact
  columns are blocking structural limitations, never zero-sized success. The
  producer contract, fixture, golden export, architectural decision and public
  API sources are recorded in `contracts/python-coverage-v1` and
  `progress/python-tier-a-spike-2026-08-25.md`. Public CLI execution, xdist and
  subprocess matrices, archive v3/model migration, owned Python MC/DC probes,
  packaging and broad dogfood remain unfinished gates. The next private step
  has proven a real two-worker pytest-xdist run: the generated plugin starts a
  separate coverage.py collector in each worker, uses a run-unique suffixed
  data file plus static worker context, leaves the controller uninstrumented,
  and records outcomes only in the worker. The public API combines those files
  without deleting them; the golden import preserves both worker identities,
  both background import contexts and exact per-test arcs. Rust now requires
  the real supervised test exit code rather than manufacturing success and
  preserves pytest expected-failure semantics. Broader xdist scheduling,
  worker crash, retry and subprocess cases are still open.
- Shared analysis no longer hardcodes the JavaScript coverage-model label for
  every language. `CoverageReportRequest` now carries an optional strict
  `CoverageModelDeclaration`; absent declarations retain the byte-compatible
  JavaScript masking-short-circuit model, while the private Python importer
  selects `python-native-branch` and names only coverage.py executable
  statements, branch arcs and pytest identities as measured. Atomic-condition
  outcomes, MC/DC, exact columns and action/assertion causality are listed as
  unmeasured and remain blocking manifest limitations. This is currently an
  in-memory analyzer contract only. It is intentionally not smuggled into
  frozen evidence v2; archive v3 must make the frontend declaration and model
  mandatory and retain a dual reader for historical JavaScript runs.
- The private evidence-v3 candidate now does that migration explicitly rather
  than modifying the frozen public writer. V3 has its own magic, retains v2's
  canonical framing, and requires strict `frontend.json`,
  `coverage-model.json` and `manifest.json` entries. The versioned reader
  recognizes v2 and v3; the legacy v2-only API deliberately rejects v3 so no
  old caller can misclassify it. The current public writer still emits v2.
  Python's typed importer can produce deterministic v3 entries, and a complete
  write/read/analyze round trip revalidates frontend identities and limitations
  before reproducing the native model and oracle totals. Unknown coverage-model
  fields are fatal. V3 still needs corruption/fuzz coverage, run-store/query
  integration, agent-contract versioning for exposing the model, and a staged
  public migration before its status can leave `private-candidate`.

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
