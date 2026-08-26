# Rust compiler backend spike — 2026-08-26

## Decision

Public `rust-source-v1` cannot be implemented honestly by extending the
concrete-source rewriter alone. Supercov will use an owned, rustc-commit- and
host-matched compiler companion selected automatically as Cargo's
`RUSTC_WRAPPER`. The main Supercov binary remains the sole language-neutral
orchestrator, archive/analyzer/query engine. The companion is a versioned Rust
frontend component, not an imported coverage engine.

The current `ra_ap_syntax` source transformer remains a private differential
reference while this backend is built. It is not the semantic authority for
expanded Rust and must be removed from the Rust product path when the compiler
backend passes the frozen model.

## Executable findings

`npm run test:rustc-backend-spike` builds the development-only companion
against the repository's pinned Rust 1.95.0 compiler libraries, then runs a
fixture containing authored code, `macro_rules!`, a procedural macro,
build-script-generated `include!` source, const evaluation, unit tests and a
doctest.

The spike proved:

1. An exact-version `rustc_driver` callback sees HIR/MIR bodies produced by
   declarative and procedural macros as well as ordinary source.
2. Declarative expansion retains both the macro-definition span and invocation
   callsite. The procedural macro in the fixture collapses generated tokens to
   its invocation span; generated-token identity therefore needs a stable
   expansion/token ordinal in addition to authored source ranges.
3. Build-script output included from `OUT_DIR` appears as a real compiler
   source file. The wrapper runs after the build script, so Supercov can hash,
   instrument and bind it to package/build-script provenance without touching
   the checkout.
4. Const functions and const items expose MIR, but `optimized_mir` is invalid
   for const items. Runtime and CTFE bodies require separate provider paths.
5. Overriding the local `optimized_mir` query changes emitted code. The spike
   swaps the `authored` boolean branch and the compiled test observes `2`
   instead of `1`. This establishes a viable owned runtime probe-insertion
   boundary after expansion and type/borrow analysis.
6. Cargo's ordinary `RUSTC_WRAPPER` does not receive the compiler invocation
   for rustdoc's extracted doctest crate. Doctests need a matched rustdoc/test-
   builder companion or an independently equivalent owned extraction path.

The companion executable is about 600 KiB on arm64 macOS and dynamically uses
the exact `librustc_driver` already shipped by the user's `rustc` component.
`rustc-dev`, `llvm-tools` and `rust-src` are required to *build and validate*
the companion, but must not be user-run dependencies. The dylib name and
private ABI are compiler-build-specific, so a companion may never be used with
an unverified rustc commit.

## Rejected architectures

- Stable source transformation alone: cannot see final proc-macro output,
  expanded control flow or compile-time execution.
- `rust-analyzer` as the injection backend: it provides valuable independent
  expansion/provenance checks and a stable-built proc-macro server, but its
  pseudo-files are not the crate that rustc ultimately compiles. Replacing
  invocations with rendered expansions can change hygiene and semantics.
- `-Zunpretty=expanded`: nightly-only, rendered rather than lossless, and not
  an insertion API.
- LLVM/rustc coverage import: remains a development oracle only and violates
  the owned-measurement invariant for user runs.
- Bundling a complete compiler: unnecessary if exact companions reuse the
  compiler driver already present in each supported toolchain.

## Backend shape

1. The main binary reads the exact rustc commit, host, target and Cargo launch
   graph and selects a bundled, signed companion. Absence or mismatch fails
   closed; Rust remains private until the supported matrix is shipped.
2. The companion derives the denominator from expanded compiler structures and
   emits frozen source/expansion/generated provenance. It inserts Supercov
   runtime probes into runtime MIR only after semantic analysis.
3. A separate CTFE provider path records compile-time execution. Merely adding
   runtime calls to const MIR is invalid. If a callback-compatible interpreter
   hook cannot be made exact, a versioned Supercov fork of the relevant rustc
   const-eval provider is required; const completeness remains a release
   blocker until that experiment succeeds.
4. A rustdoc companion observes/extracts and compiles doctests with exact
   documented-source mapping. Hidden lines and merged doctests remain explicit
   traced gates.
5. Generated files are captured in the isolated Cargo target after build
   scripts and before crate compilation. External symlinks and provenance
   ambiguity fail closed.
6. The companion emits only frontend obligations and observations through
   evidence v3. MC/DC solving, attribution merging, persistence and queries
   remain in the shared Rust engine.

## Next implementation gates

1. Replace the deliberate branch swap with a real side-effecting Supercov MIR
   probe call and prove values, panics, drops and output remain identical.
2. Freeze compiler-companion capability/handshake JSON keyed by rustc commit,
   host, target and frontend build ID.
3. Derive stable expansion identities and complete branch/condition mappings
   from expanded HIR plus MIR source info.
4. Spike CTFE path tracing and rustdoc interception; either unresolved item
   blocks public Rust support.
5. Add exact-version mismatch, missing-companion and custom-toolchain failure
   tests before connecting the companion to `npx supercov -- cargo test`.

## Primary references

- Rust's `rustc_private` documentation states that compiler crates such as
  `rustc_driver` are unstable and require matching development components.
- `rustc_driver::Callbacks` exposes post-expansion/post-analysis callbacks;
  `rustc_interface::Config::override_queries` provides the versioned query
  replacement boundary exercised here.
- The rustc MIR documentation identifies `SourceInfo` as compiler source
  provenance and separates runtime MIR from MIR used for CTFE.
- rust-analyzer's architecture documents its expansion pseudo-files,
  source-map model and out-of-process proc-macro server.
- rustdoc documents doctest preprocessing and its unstable extracted-doctest
  output/test-builder interfaces.
