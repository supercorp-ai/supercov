# Rust frontend insertion and attribution ADR — 2026-08-26

Status: accepted for private implementation. Rust is not yet a claimed product
frontend.

## Product boundary

Rust user runs are measured only by Supercov-owned probes. `rustc -C
instrument-coverage`, LLVM profiles and `llvm-cov` are development oracles;
they are not invoked by the product, required on the user's machine, or used
as a degraded fallback. The existing Cargo test command remains the only user
configuration.

## Decision

The first owned Rust frontend has four layers:

1. Cargo metadata identifies workspace packages, targets, crate roots and
   source ownership in the isolated Supercov workspace.
2. A pinned, lossless rust-analyzer concrete-syntax frontend discovers source
   obligations and applies byte-range edits ahead of compilation. It never
   rewrites the user's real checkout.
3. A generated, std-only Rust probe runtime is compiled with the instrumented
   crates. Product execution does not depend on an external coverage crate.
4. Cargo builds tests once with `--no-run --message-format=json`. Supercov
   enumerates the resulting libtest binaries and runs each selected test in a
   separate supervised process. The process environment carries exact run,
   worker, test, retry and phase identity, so observations from every
   instrumented workspace crate share one exact test identity without relying
   on unstable libtest callbacks or thread-local test discovery.

Process-per-test is the initial correctness architecture, not an optimisation.
It preserves exact attribution across ordinary threads, async executors and
workspace-crate boundaries. Batched in-process execution may be added later
only with an equally exact runner/context carrier.

## Why not the other insertion points

- A custom `rustc_driver`/MIR pass uses permanently unstable compiler APIs,
  requires the exact compiler internals plus `rustc-dev` and LLVM libraries,
  and must be built for each rustc revision. It is not a universal stable-Cargo
  bootstrap.
- An out-of-tree LLVM pass is tied to rustc's exact LLVM ABI and sees lowered
  control flow after important Rust source structure and macro provenance have
  been transformed. It is a poor authority for the source denominator and
  per-condition MC/DC identities.
- Stable `-C instrument-coverage` is intentionally retained as an independent
  structural oracle. Its profile format may require LLVM tools matching the
  compiler, and consuming it in user runs would violate Supercov's ownership
  rule.

Cargo's documented `RUSTC_WORKSPACE_WRAPPER` remains useful for observing the
actual compiler launch graph and preventing uninstrumented workspace members
from silently entering a run. It does not itself provide instrumentation.

## Frozen initial Rust coverage model

The private frontend will emit evidence v3 with a Rust-specific coverage model
and explicit obligations for:

- executable statements and function/closure entries;
- `if`, `while`, match guards and assertion conditions;
- source-ordered atomic conditions in `&&`/`||` control decisions, preserving
  probe-v2's unreached/false/true ternary vectors;
- true/false outcomes, logical short-circuit/right-evaluated outcomes;
- zero/entered outcomes for `for` and `while` loops;
- selected/not-selected match arms and `?` continue/early-return outcomes.

The model is source-oriented. Generic monomorphisations share their source
obligation, matching the user-visible question "is this source behavior
covered?"; an expansion/instantiation query can be added without changing the
base denominator.

## Private-stage limitations and release blockers

The first source frontend must declare, never hide, any unmeasured surface:

- declarative-macro bodies and proc-macro/derive-generated executable code;
- build-script/generated Rust outside the frozen source graph;
- `const` evaluation and `const fn` contexts where runtime calls are illegal;
- `no_std` and unsupported targets until an allocation-free compatible runtime
  exists;
- doctests until rustdoc launch and source mapping are owned;
- inline assembly, FFI implementation bodies and dynamically loaded code;
- tests that cannot be reproduced as an exact libtest process.

These limitations make early Supercov dogfood diagnostic, not GA. Complete
Rust support requires either closing them in the lossless frontend or adding a
versioned compiler-expansion backend. If a compiler backend is required,
Supercov will ship automatically selected packages keyed by rustc commit and
platform; it will not ask users to install or configure third-party coverage.

## Correctness gates

Before public Rust support:

1. original and instrumented programs must have identical values, panics,
   stdout/stderr, drops, side-effect ordering, borrow behavior and test results;
2. checked-in statement/branch facts must agree with the rustc/LLVM development
   oracle wherever the models overlap;
3. independent MC/DC goldens must cover short-circuiting, masking, overloaded
   calls around conditions, panics, `?`, matches and loops;
4. macros, generics, async, threads, subprocesses, retries, ignored tests,
   fixtures, workspaces and crash recovery need black-box corpora;
5. evidence v3, attribution, query, minimization, integrity and cleanup use the
   existing shared Rust engine without a Rust-only analyzer;
6. Supercov must dogfood its own full Rust workspace with source unchanged and
   every limitation visible in the query model.

## Decision evidence

This decision was checked against rustc 1.95.0 (LLVM 22.1.2). The official
rustc documentation describes stable `-C instrument-coverage` as LLVM
intrinsic/profile instrumentation and warns that compatible LLVM tools may
need to match the compiler. The rustc development guide states that external
compiler-driver APIs are inherently unstable and require compiler/LLVM
components. Cargo documents `RUSTC_WORKSPACE_WRAPPER` as a stable wrapper for
workspace-member rustc invocations.

- https://doc.rust-lang.org/rustc/instrument-coverage.html
- https://rustc-dev-guide.rust-lang.org/rustc-driver/external-rustc-drivers.html
- https://rustc-dev-guide.rust-lang.org/rustc-driver/intro.html
- https://doc.rust-lang.org/cargo/reference/environment-variables.html

