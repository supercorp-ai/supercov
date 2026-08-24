# Multi-language coverage architecture — Phase 6 (2026-08-24)

Companion to `engine-master-plan-2026-08-24.md` and
`engine-research-2026-08-24.md`. Target: eventually every language at the
same evidence quality Supercov gives JavaScript today — structural MC/DC,
per-test attribution, assertion linkage — not a degraded second tier.

## Correction to an earlier framing

An earlier session note said compiled languages should "adapt, never
reimplement": consume LLVM's `.profraw`, and report the resulting loss of
per-test attribution and assertion phases through the honest-degradation
tiers. That described the *cost-optimal* path and wrongly presented it as the
capability ceiling. It is not. Full parity for compiled languages is
achievable; it requires owning the instrumentation rather than only consuming
native coverage output. Both are on the roadmap, in that order, because
adapting is genuinely good for the common case and ships far sooner.

## What "full quality" decomposes into

The coverage-confidence boundary is unchanged and language-independent:

1. **Structural completeness** — every measured obligation was observed.
2. **Causal confidence** — which test observed it, whether execution was
   linked to a recognised passing assertion, and through which test kind.
3. **Semantic correctness** — whether the assertion checks the right
   behaviour. Out of reach in every language, forever, without an independent
   specification or fault injection.

Only two things actually vary per language:

- **where probes are inserted** (source transform, IR/MIR pass, or native
  coverage output), and
- **how the current test/phase identity propagates** to a probe.

Everything else — the evidence archive contract, the analysis engine, MC/DC
pair search, the query surface, waivers, the run store, `diff` — is shared
and is never rewritten per language. Probe v2's per-decision bitmap model is
deliberately language-neutral so that this stays true.

## Two tiers per language

**Tier A — adapt.** Consume the language's native coverage instrumentation
(LLVM profdata for C/C++/Rust, `go test -cover`, coverage.py). Structural
MC/DC is full quality here where the toolchain provides it. Attribution
quality depends entirely on the execution model (see the ladder below).
Assertion linkage comes from framework listener APIs where they exist.
Weeks of work per language.

**Tier B — own.** Our own instrumentation emitting probe-v2 form, with a
thread-local or task-local epoch selecting the bitmap arena, plus assertion
shims. Achieves parity in every execution model including in-process
parallelism. Months per language family.

Tier A is not a compromise on *data quality*; it is a compromise on *which
execution models we can attribute*. That distinction is what makes shipping
Tier A first honest rather than expedient.

## The attribution ladder

This is the load-bearing insight. Attribution quality is a property of the
execution model, not of the language:

| Rung | Execution model | Attribution | Cost |
| --- | --- | --- | --- |
| 1 | Process per test | **Exact** — no interleaving exists to disambiguate | Free (`LLVM_PROFILE_FILE=%p.profraw`) |
| 2 | Serial within one process | **Exact** — snapshot-diff counters at test boundaries | Small linked shim (`__llvm_profile_reset_counters` + buffer read) |
| 3 | Parallel within one process, context propagated | **Exact** — owned probes, task-local epoch selects arena | Tier B: our IR/MIR pass |
| 4 | Parallel, no context propagation | Aggregate only | Declared limitation, never faked |

Rung 1 is the *common case* for modern Rust: `cargo nextest`'s whole
architecture is process-per-test. It is not an edge case, and its attribution
is strictly better than our JS path because there is no interleaving at all.
Rung 3 is where Tier B earns its cost; thread-local access on x86-64/ARM64 is
a couple of instructions, comparable to LLVM's own counter increments.

## Assertion linkage mechanisms, in order of preference

1. **Official framework listener APIs** — best available, no source rewriting,
   more reliable than what we do in JS today. GoogleTest's
   `TestEventListener::OnTestPartResult` reports every assertion with its
   result; Catch2 has equivalents.
2. **Macro or header shims** — workspace-only, no user repo changes.
   `assert()` comes from `<assert.h>` (shadow via forced include);
   Rust `assert!`/`assert_eq!` and third-party assertion macros
   (`pretty_assertions`, `claim`) are macros, so they are syntactically
   identifiable and hygienic — *easier* to wrap than JS's dynamically
   imported `expect`.
3. **Workspace source transform** — the JS approach, for languages with
   neither of the above.

Go deserves a note: it has no macros, but `testing.T` is passed *explicitly*
to every test and `t.Error`/`t.Fatal` are ordinary methods on it. That value
identifies the test better than any ambient context, which sidesteps the
goroutine-local problem for attribution entirely.

## Per-language matrix

| Language | Tier A source | Attribution (Tier A) | Assertion linkage | Tier B insertion point |
| --- | --- | --- | --- | --- |
| JavaScript/TS | *is* the reference implementation | rung 3 (AsyncLocalStorage carrier) | source transform (native `assert`, imported `expect`) | done |
| Python | coverage.py tracer | rung 1/2 (pytest-xdist is process-per-worker) | pytest's own assertion rewriting hooks | AST rewrite via import hook (pytest's proven mechanism) |
| Rust | LLVM profdata via rustc coverage (MC/DC status: **spike S8**) | rung 1 with nextest, rung 2 with libtest shim | `assert!` macro shim; custom harness (`libtest-mimic`) | rustc MIR pass vs LLVM plugin — **ADR in S8** |
| C/C++ | `-fcoverage-mcdc` profdata | rung 1 with ctest-per-process or `--gtest_filter` sharding | GoogleTest/Catch2 listener API (free), `<assert.h>` shim | LLVM pass |
| Go | `go test -cover`, `GOCOVERDIR` | rung 1/2 (per-process binaries) | wrap `t.Error`/`t.Fatal` (explicit `T`) | source rewrite (Go's own coverage works this way) |
| JVM / Ruby / PHP | later; each has a mature bytecode or tracer hook | TBD | TBD | TBD |

## Ship gate: no language without its own oracle

**Principle: a language does not ship until it has (a) a semantic-equivalence
corpus proving our instrumentation preserves program behaviour, (b) an
explicitly declared attribution tier per supported runner, and (c) its
limitations enumerated in the measurement model.**

This is the real cost driver, and it is what makes "perfect accuracy" a
checkable claim rather than marketing. JavaScript's credibility rests on
65,053 baseline-passing Test262 scenarios plus the independent Clang/LLVM
MC/DC oracle. The equivalents:

- **Rust** — rustc's own test suite; MC/DC cross-checked against rustc's
  native coverage output on the same programs.
- **C/C++** — GCC/LLVM torture tests plus csmith-generated programs;
  MC/DC cross-checked against `llvm-cov` (already our oracle today).
- **Python** — CPython's test suite; cross-check against coverage.py for
  line/branch agreement.
- **Go** — Go's own test suite; cross-check against `go test -cover`.

A language whose corpus is not green is a language we do not claim support
for. No exceptions, including for demo purposes.

## Known limits — per-language and shared

1. **Semantic correctness** — unchanged, language-independent, permanent.
2. **Coverage build ≠ release build.** Instrumenting at low optimisation
   changes what a "decision" is once inlining and branch folding apply. Every
   C/C++ coverage tool carries this caveat; we document it rather than
   pretend otherwise.
3. **Monomorphisation and macro instantiation is a design decision, not a
   bug.** One source decision instantiates N times across Rust generics and
   C++ templates. Per-instantiation and per-source-location MC/DC are both
   defensible; LLVM merges by location. We must pick one, document it, and
   probably offer expansion as an option.
4. **Work-stealing async requires ecosystem cooperation.** Thread-local
   breaks across await points in tokio; correct attribution needs task-locals
   or `tracing`-style span propagation. This is the same dependency as our JS
   reliance on AsyncLocalStorage — solvable, not free.
5. **`unsafe`, inline asm, FFI boundaries, `dlopen`, JIT, `include!`** —
   uninstrumentable; declared through the limitation machinery we already
   have. The JIT/`dlopen` class is the direct analog of JS `eval`: no stable
   pre-run denominator exists.
6. **Cross-language runs already work.** The coverage carrier is HTTP headers
   and environment variables, which is language-neutral by construction — a
   Playwright test driving a Rust server needs no new transport, only a
   collector on the server side.

## New spikes

- **S8 (→ Phase 6, Rust): insertion-point ADR.** Establish rustc's MC/DC
  support status and stability; then decide Tier B insertion between a rustc
  MIR pass, an out-of-tree LLVM plugin, and a source transform. Must include
  the *ongoing maintenance cost* of tracking LLVM/rustc release cadence,
  which is the dominant long-term cost of this phase. Exit: written ADR with
  a maintenance-burden estimate per option.
- **S9 (→ Phase 6, C/C++): Tier A sufficiency.** Determine clang's current
  per-decision condition cap and whether it is configurable. Our JS
  implementation already observes decisions with 10 conditions; if Tier A
  cannot express those, Tier A must degrade explicitly rather than silently
  merge them. Exit: cap documented, degradation behaviour specified.
- **S10 (→ Phase 6): attribution-ladder validation.** End-to-end proof of
  rung 1 on a real repository: instrument a Rust crate, run under
  `cargo nextest`, and produce per-test evidence in our archive format with
  MC/DC verdicts matching `llvm-cov` on the same run. Exit: a fixture in the
  golden corpus, in the same shape as today's JS fixtures.

## Ordering

Python first (already Phase 5 — largest agent-coded ecosystem, one adapter
covers most of it, and the import-hook mechanism is de-risked by pytest
itself). Then Rust Tier A → Rust Tier B, since the Rust community's
process-per-test norm means Tier A already lands at rung 1. Then C/C++
Tier A (GoogleTest's listener API delivers assertion linkage for free) →
Tier B. Go after that. JVM/Ruby/PHP only once the shared core has absorbed
four languages without accumulating per-language special cases in the engine.
