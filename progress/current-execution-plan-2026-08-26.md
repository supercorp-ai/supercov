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
and the compiler rejects both full-ID and probe-prefix collisions. A later
audit removed the remaining fixture-name shortcut: every source-backed
function and statement point is now injected. rustc code mappings and exact
MIR-span fallback locate statement entry; sequential statements coalesced into
one optimized block become an authored-order probe chain. A focused one-sided
branch proves its untaken statement remains uncovered while the taken and
following statements are observed. Dummy-span compiler harness functions keep
an explicit source limitation rather than receiving invented identity. The
candidate still carries blocking denominator limitations. The authored match
   slices below have since narrowed that surface. The six stable assertion
   macros now have exact passed/failed and MC/DC observations, including
   collapsed proc-macro output and panic/evaluation-order goldens. Exact nested
   assertion phase contexts now cover argument evaluation and restore on normal
   and unwind exits. `rust-probe-transport-v2` now publishes authenticated
   child-to-parent assertion phase definitions. A transport-global invocation
   nonce distinguishes repeated executions of the same assertion site and is
   collision-safe across concurrent processes; malformed derivations, missing
   parents, cycles and cross-attempt chains fail closed. The real compiler
   corpus corrected one false premise before freezing: a valid phase definition
   may have no committed observation when evaluation panics or touches only
   uninstrumented data. The shared engine now projects those verified dynamic
   contexts into distinct evidence-v3 assertion phases with source, causal
   parent and committed passed/failed status; missing verdicts remain unknown
   rather than inferred. Compiler-supervisor integration of that projection,
   CTFE and doctest obligation/probe mappings, plus full package and compiler
   fingerprints, remain R1 work. No measurement-complete claim is possible
   yet.

The production engine now also owns strict ingestion of the compiler manifest
candidate. Unknown fields/schema/model, premature completeness, malformed or
noncanonical source identities, duplicate IDs or probe ordinals, unsupported
obligation kinds, dangling/cyclic match ownership and mismatched match-arm
ordinals fail before evidence can be analyzed. The companion must still be
moved out of the development spike and connected to this boundary. The real
clean-build companion manifest now passes this production parser on every
spike run. That integration caught and specified three shapes a synthetic unit
fixture had missed: `authored-expansion` provenance, distinct
`branch-alternative` IDs, and nested matches owned by a parent scrutinee without
an arm index.

The same real clean-build candidate is now normalized by the production Rust
engine into the shared language-neutral `CoverageManifest`. The conversion
requires an explicit source-key-to-byte-snapshot map, rejects missing sources,
out-of-range offsets and non-UTF-8 boundaries, and derives line, column and
source text without resolving compiler keys through filesystem guesses. It
also produces a collision-checked ordinal resolver. Match-arm selection is
expanded offline into the selected arm plus every evaluated sibling's
`not selected` alternative, preserving the compiler frontend's group semantics
without reconstructing interleaved event timing. The rustc spike exercises
this normalization against authored, expanded and build-generated sources on
every run.
The source namespace is now closed as well: only normalized project-relative
`source:` keys and `generated:package:<package>:<out-relative>` keys are
accepted. Absolute paths, backslashes, empty components and traversal are
fatal, and the source snapshot map must equal the denominator's complete key
set—missing and extra snapshots are both rejected.

Authenticated `rust-probe-transport-v2` records now pass through a production
Rust evidence projector. It validates every string probe, ordinal and decision
vector against the normalized denominator; expands compiler selection
semantics; assigns test and dynamic assertion phase IDs explicitly; and keeps
context-zero observations in a separate background snapshot so they cannot
silently become passed-test coverage. Dropped and incomplete records remain
visible transport health rather than being discarded. The real isolated
libtest/compiler probe run now traverses the production transport reader,
manifest normalizer and evidence-v3 projection and proves a passed assertion's
causal phase and exact test context.
The shared Rust engine now owns the frozen libtest-context derivation too. Its
domain-separated FNV-1a input, reserved-value remap and fatal collision policy
are part of `rust-source-v1`; supervisor preflight rejects duplicate names and
any 64-bit collision before a process is launched. The compiler corpus's exact
known context remains a cross-implementation golden. The existing exact
process-per-libtest supervisor now performs this preflight per artifact and
places the resulting context in the authenticated runtime environment before
launch, which gives child threads the attempt identity without relying on
thread timing or global mutable attribution.

Exact compiler selection is now executable outside the spike as well. At
build time the companion records the rustc commit, release, host and SHA-256
of the exact `librustc_driver` it linked against. At selection time the engine
independently resolves the requested rustc, hashes its driver, hashes the
candidate executable, runs the candidate's strict handshake under that
toolchain's dynamic-library environment, and accepts exactly one
commit/host/driver/build match. Missing, duplicate, malformed, self-hash-
mismatched and merely nearby companions fail closed. The current private
candidate deliberately advertises CTFE and rustdoc/doctest publication as
false, so the same selector rejects it when public capabilities are required.
This closes identity negotiation only; production Cargo orchestration and the
remaining capability gates still block cutover.

Production Cargo orchestration and the first complete runtime path are now
executable too. Cargo supplies its actual rustc path to Supercov through
`RUSTC_WORKSPACE_WRAPPER`; the engine selects and re-verifies the exact
commit/host/driver/binary-matched companion for every compiler invocation.
All generated state lives under the run's `.supercov/work` directory with a
fresh private Cargo target. The companion writes strict paired manifest and
source-snapshot sidecars from rustc's own `SourceMap`; the engine rejects
missing, extra, duplicate and identity-changing units, then merges repeated
workspace compilation units into one normalized denominator. A full fixture
build exposed and fixed a real generated-test-harness collision: structural
markers are now keyed by `LocalDefId`, not textual owner names such as
`main`.

The same path now enumerates real libtest artifacts and executes one process
per selected test candidate with an OS-random 128-bit token, one bounded mmap
transport and one preflighted deterministic context. Every observation is
validated against the frozen denominator and projected into evidence v3;
supported assertion macros retain exact nested phase causality, while context
zero becomes a separate background result and cannot upgrade the test.
Malformed, unauthenticated, unknown or capacity-dropped evidence fails closed.
Interrupted reservations remain explicit attempt health: they can represent a
caught panic that correctly produced no decision outcome, so they are not
silently relabeled as transport loss. Ignored tests may legitimately attach no
runtime because their bodies did not execute. The production-shaped fixture
passes through the shared frontend validator and analyzer with nonzero line
and branch coverage and zero dropped records.

This closes the initial Cargo -> companion -> libtest -> evidence-v3 ->
analyzer path, not R1/R2. The compiler run now also uses the production
isolated-workspace and atomic archive/store lifecycle, and its published run is
queryable through the normal CLI. That roundtrip forced query-index schema v2
to replace the JavaScript-only scope assumption with typed source-discovery
and compiler-owned language/model scopes. The internal run still needs exact
capture of Cargo/libtest filter and retry semantics, build-phase evidence, the
full lifecycle crash/ENOSPC/concurrency matrix, CTFE publication and doctest
execution. The private companion continues to advertise CTFE and doctest
capabilities as false, so public selection still fails by construction.

The first real-probe doctest attempt was deliberately rejected. It exposed and
fixed an unstable-feature capability leak by adding rustc's empty
`-Zallow-features=` restriction, but the instrumented run still regrouped one
standalone plus two merged doctests into one three-test runner and therefore
changed visible command output. Passing verdicts are insufficient: doctest
probe publication stays blocked until execution grouping and output are also
equivalent.

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
   arm while retaining and measuring both reachable siblings. The marker bridge
   now also freezes each synthetic group's HIR parent, nesting site and arm,
   solves one parent-consistent CFG assignment, and fails compilation when that
   assignment is absent or ambiguous. Proc-macro matches nested in an arm body,
   a scrutinee and a guard retain independent exact selections. Separate
   condition markers select only Boolean switches that change the guard's
   accepting/rejecting reachability, so nested control flow is excluded while
   the synthetic two-condition guard emits the exact three ternary vectors.

`let else` is now exact as a branch rather than being mislabeled as an MC/DC
decision. Expanded HIR freezes one `matched` and one `else` alternative at the
refutable pattern. For authored source, rustc's exact branch region binds those
alternatives to the optimized MIR targets before native coverage metadata is
removed. Collapsed proc-macro source has no such retained region, so the built-
MIR bridge marks the final real/imaginary pattern edge, requires both markers
to survive borrow checking exactly once, removes them, and installs the same
first-commit runtime frame. Simple and nested patterns, two sequential authored
statements, and two sequential synthetic statements sharing one collapsed
callsite all preserve baseline behavior and exact per-invocation counts.

The `?` operator now has an independently owned compiler-backed branch path as
well. Expanded HIR freezes `continued` and `early return` alternatives, while
built MIR identifies the actual `Try::branch` call and its typed
`ControlFlow::Continue`/`Break` switch. Private endpoint markers survive borrow
checking exactly once, are rebound after enclosing structural edits, and are
removed before the first-commit runtime frame is installed. The frame begins
only after operand evaluation and `Try::branch` return, so a panicking operand
records neither alternative and does not create a false incomplete frame.
Exact behavior/evidence goldens cover `Result`, `Option`, sequential operators,
nested `value??`, and sequential/nested proc-macro operators whose source spans
all collapse to one invocation.

The CTFE provider spike is now executable rather than hypothetical. The
companion overrides `mir_for_ctfe`, inserts execution markers in original
blocks, splits multi-successor edges for independently identifiable edge
markers, and observes only those markers through a private in-process rustc
interpreter subscriber. Both true and false const-fn paths were observed while
const values and complete stdout/stderr stayed byte-identical to the ordinary
build. The one-function/16-bit marker proof has now been generalized to every
local CTFE body: marker identity is a domain-separated hash of crate,
definition, block-or-edge kind and local ordinal; collisions are fatal and
events retain the exact definition. The companion now also inserts explicit
entry and return markers and records the observing compiler thread. The corpus
reconstructs a per-thread nested invocation stack, requires balanced frames
after a successful compilation, and proves that the two `const_decision`
evaluations are separate frames with opposite edge paths. This replaces the
unsound idea of assigning flat adjacent events to a definition; missing return
markers after a panic or compiler crash can instead remain explicitly
incomplete. Edge identity is carried by each event, so concurrent evaluation
does not require guessing from adjacent log records.
Rust still remains private. The proof now covers distinct direct-const,
static, const-fn, const-generic-fn, generic-associated-const, anonymous
array-length and inline-const owners. It does not yet cover every CTFE branch
kind, failure mode, target, `RUSTC_LOG` coexistence or acceptable performance.

The first mapped CTFE evidence slice is now carried through the real run
boundary. Compiler-finalized event and mapping sidecars are published with
same-filesystem staging/rename and directory sync; marker and hit ordinals are
lossless decimal strings rather than unsafe JSON numbers. Strict Rust ingestion
requires one map/event pair per compiler unit, exact crate/definition/kind/site
identity, balanced per-thread invocation frames, and resolution of every hit
ordinal to the frozen denominator. Function and statement hits are archived as
exact `rustc` setup-phase evidence and survive evidence-v3 analysis plus normal
run querying. CTFE decisions now carry explicit start, condition and finish
mappings. Strict nested reconstruction produces exact masking-MC/DC vectors
and commits the frozen true/false outcome branch alternative. Engine corruption
tests reject semantic markers with unrelated hits, a finish mapped to the wrong
alternative, a condition without a frame and an invocation that exits with an
open decision. The real compiler corpus proves independent `[false] -> false`
and `[true] -> true` const-fn evaluations with unchanged values and output.

That outcome relation is frozen directly into every compiler manifest decision
rather than inferred from overlapping spans. The same relation corrected the
ordinary runtime path: decision outcomes now use an atomic ordinal observation
before the MC/DC frame is committed, while loop-entry alternatives retain their
separate first-commit frame. A crash can therefore leave a truthful outcome hit
plus an explicitly incomplete vector, but never a complete vector whose
outcome branch is missing. The manifest limitation now says *complete* CTFE and
doctest mapping remain; the controlled CTFE slice is real, but the wider const
corpus is still a release blocker and this is not a completeness claim.

The owner matrix exposed a real compiler distinction and closed it without a
source heuristic. rustc retains native branch regions for const functions but
not for direct consts, statics, anonymous consts or associated consts. Those
owner kinds now receive typed Boolean condition markers in built MIR before
borrow checking; the exact-version companion requires each marker to survive
once, reconstructs it from the CTFE MIR, and removes it before evaluation.
Runtime const-function instrumentation remains on its independent native-region
path. Exact corpus vectors now cover direct true/false consts and statics, two
instantiations each of a const-generic function and generic associated const,
an anonymous array-length decision, two independent inline consts, all four
masked paths through `(first || second) && third`, and separate outer/inner
const decisions. The complete CTFE branch-kind and failure corpus remains open.

CTFE now also reconstructs exact `match` and `let else` alternatives through
the same compiler-owned selection bridge used at runtime. A selected match arm
derives every sibling's not-selected alternative offline, and both the matched
and fallback `let else` paths retain their frozen branch identities. Const
`while` decisions carry an explicit manifest relation to their loop-entry
branch rather than relying on overlapping source ranges. Each CTFE invocation
commits only its first loop condition outcome for zero-versus-entered coverage:
an entered loop's later terminating false condition remains an exact MC/DC
vector but cannot fabricate a zero-iteration hit. The real corpus proves zero,
disabled and two-iteration paths with unchanged behavior; strict ingestion
rejects wrong loop alternatives. At this checkpoint CTFE `?`, assertions,
promoted/const-trait surfaces and the complete failure/platform/performance
corpus remained open; the following compatibility slice resolves the first two.

The next CTFE compatibility slice resolves the first two items against the
actual Rust 1.95 language boundary rather than assuming their runtime forms
also exist during const evaluation. Stable Rust rejects `?` in `const fn`
because `Try`/`FromResidual` are not const traits, and rejects
`assert_eq!`/`assert_ne!` in `const fn` because `assert_failed` is non-const.
Checked-in compile-fail programs require the exact baseline error count, codes,
spans and text from the instrumented compiler and require that a rejected
compiler invocation publishes no partial CTFE sidecars. Boolean `assert!` and
`debug_assert!` are valid const surfaces: compound short-circuit and direct-
const corpus cases now publish their exact successful vectors with unchanged
values and output, while a failing `assert!` preserves rustc's E0080 diagnostic
and publishes nothing. The companion now enables rustc's branch metadata
through exact-version internal configuration rather than leaking
`RUSTC_BOOTSTRAP` or unstable flags into user code. CTFE observation is active
during compilation, but atomic sidecar publication is gated on successful
analysis. Promoted/const-trait surfaces, the remaining failure/resource/
concurrency matrix, `RUSTC_LOG` coexistence and performance remain open.

`RUSTC_LOG` coexistence is now closed at the exact-version companion boundary.
The companion reproduces rustc 1.95's logger configuration and formatting but
applies the user's filter only to the ordinary logging layer; an independent
target-and-level filter enables the private CTFE observer without printing
extra interpreter events or suppressing requested logs. A direct compile gate
runs stock rustc and the companion with the same JSON/output-target settings,
requires requested interpreter records from both, and simultaneously requires
nonempty Supercov CTFE sidecars. This gate also exposed and fixed acceptance of
rustc's `--crate-name=value` form in addition to `--crate-name value`.
Promoted/const-trait surfaces, compiler crash/ENOSPC/concurrency and performance
remain open.

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
the process-per-test reference. Architecture checkpoint: the compiler backend
now proves the process-per-test path on a test that spawns a child thread. The
thread's work inherits the supervisor-owned attempt context through the mmap
transport environment, while the parent assertion verdict retains its distinct
authenticated assertion phase; child work is not falsely upgraded to assertion
confidence. Use this sound process boundary for the first public integration.
Retain in-process concurrent libtest only as a later optimization after its
thread/async/subprocess carriers prove equivalent attribution.

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
