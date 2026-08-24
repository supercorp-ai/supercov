# Supercov engine end-state — master plan (2026-08-24)

Decision: optimize for best possible UX and best possible performance, no
shortcuts. Rewrites are approved. This document fixes the target architecture,
the acceptance gates, and the order of work. It deliberately does not touch
code; a compatibility sweep is in flight and Tier 1 (trust) still lands first.

## Committed end-state decisions

1. **Rust core engine, single static binary.** CLI, project discovery,
   workspace isolation, instrumentation orchestration, evidence analysis,
   and query engine all compile into one 5–15 MB static binary per
   platform. The current TypeScript engine becomes the *reference
   implementation* and is retired only after sustained differential parity.
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
   never rewritten per language; probe v2's bitmap model is language-neutral
   precisely to keep that true.
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
   runtime overhead (currently 1.14x synthetic), which no engine rewrite
   touches. Target ≤1.05x. Candidate designs, to be settled by measurement:
   counter-array probes (`c[i]++` istanbul-style) instead of string-keyed
   calls; per-attempt epoch short-circuiting so hot loops hit each probe once
   per test; V8 builtin coverage as a cheap line/function source where
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
   Both engines must pass the same black-box contract tests. These specs are
   the Rust port's requirements document.

## Why a full rewrite is safe *for this project specifically*

The project already owns a runtime-agnostic conformance net:
- Test262 semantic-equivalence corpus (65,053 baseline-passing scenarios) —
  validates instrumented-output *behavior*, not implementation.
- Independent Clang/LLVM MC/DC oracle.
- Golden fixture repos across Playwright/Vitest/Jest/node:test/opaque runners.
- The self-dogfood loop plus `supercov diff` for exact regression evidence.

A differential harness (run both engines on the same inputs, require
byte-identical manifests and semantically identical reports) turns the
rewrite from "risky big bang" into "make the diff zero, then flip."

## Acceptance gates (performance)

| Metric | Today | Gate |
| --- | --- | --- |
| 500-file transform (median) | ~1,008 ms (Babel) | ≤50 ms |
| 50k-file monorepo transform | ~100 s extrapolated | ≤5 s |
| CLI query total (start + index open) | ~100–300 ms | ≤15 ms (Rust + mmap index) |
| Instrumented runtime overhead | 1.14x synthetic | ≤1.05x |
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
- **Phase 2: probe architecture v2 on the TS engine.** Done before the port
  so Rust targets final evidence semantics instead of porting twice. Gate:
  identical MC/DC verdicts across Test262 corpus + full fixture matrix,
  overhead ≤1.05x, self-dogfood diff shows no lost attribution.
- **Phase 3: Rust instrumenter crate (oxc).** Shipped inside the npm package
  as an optional napi addon behind `SUPERCOV_ENGINE=rust`; TS instrumenter
  remains default. Gate: Test262 corpus green, byte-identical manifests vs TS
  instrumenter across the matrix, 500-file gate met. Flip default after one
  release of zero differential findings.
- **Phase 4: Rust engine shell.** CLI, discovery, workspace (clonefile/
  FICLONE parity), run lifecycle, analysis (bitset MC/DC pair search),
  and query engine. Gate: differential harness zero-diff on the full sweep
  matrix and self-dogfood; query cold-start gate met. Flip default, keep TS
  engine one full release as fallback, then delete.
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

## Non-goals and guardrails

- Zero behavior change during ports; every port lands behind a flag with a
  differential gate. "Faster but slightly different" is a failure.
- Windows becomes a CI matrix member before any binary GA — no shipping
  binaries for platforms the suite has never run on.
- Contracts (schemas, CLI, envelopes, process supervision) change only by
  versioned, deliberate revision — never as a rewrite side effect.
- The agent-facing UX work (skill/playbook, post-run hints, grouped queries)
  continues on the TS engine throughout; users never wait on the rewrite.
