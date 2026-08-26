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
5. Overriding the local `optimized_mir` query can insert a real call to a
   Supercov probe after expansion and type/borrow analysis. The companion
   injects the same std-only mmap runtime used by the engine into the in-memory
   crate AST, so the fixture does not define or install it and its source hash
   remains unchanged. Both a normal binary and an actual test process publish
   all four expected ordinals through the authenticated transport.
6. Cargo's ordinary `RUSTC_WRAPPER` does not receive the compiler invocation
   for rustdoc's extracted doctest crate. A scoped exact-rustdoc launcher can,
   however, install the same companion through rustdoc's test-builder-wrapper
   boundary without repository configuration.
7. The ordinary and instrumented behavior binaries have identical stdout,
   stderr, values, `Result` errors, caught panic status and drop ordering. The
   synthetic runtime is tagged by a compiler-only source name and can be
   excluded from the user denominator without a path heuristic.
8. A separate `mir_for_ctfe` override can insert semantics-neutral markers
   into in-memory const MIR. Original basic blocks get execution markers;
   multi-successor edges are split and get edge-specific markers, so coverage
   does not depend on reconstructing interleaved interpreter event order.
9. rustc's CTFE interpreter already emits an internal event for each executed
   MIR statement when an in-process subscriber enables its exact target. A
   private subscriber records only Supercov's marker constants and has no
   formatting/output layer. The fixture observed both true and false const-fn
   edges and every original block while const values and complete program
   stdout/stderr remained byte-identical to the baseline.
10. The scoped rustdoc launcher observes standalone synthesized stdin, merged
    bundle source and the merged runner source. Standalone path/line/offset
    metadata maps hidden and visible generated MIR back to authored doc lines;
    merged `__doctest_N` bundle owners join to the runner's exact source path,
    line and test name.
11. Enabling the unstable rustdoc wrapper option does not leak unstable Rust
    into user compilation. The companion removes only its injected response-
    file option and bootstrap before compiling user doctests, while preserving
    rustdoc's own merged runner bootstrap. A stable `compile_fail` feature
    gate, ordinary/intercepted output comparison and source hash guard all
    pass.
12. The frozen `rust-probe-transport-v1` is a fixed-layout, bounded mmap file
    created and authenticated by the supervisor. Release/acquire descriptor
    commits preserve every completed observation after process kill. Per-record
    process and context IDs prevent the wire format from relying on timing.
    Thread and process concurrency, descriptor and payload exhaustion, wrong
    token/context, corrupt/truncated records, symlinks and an uncommitted
    descriptor all fail closed or produce explicit loss health as specified.
13. Exact libtest identity is available after expansion without naming a test
    framework. The companion joins each function to rustc's generated
    `rustc_test_marker`, derives a deterministic collision-checked context ID,
    enters it at MIR function entry, and restores the previous nested context
    on normal returns, existing cleanup resumes, and direct unwind actions.
    Five concurrent ordinary, procedural-attribute-generated and expected-
    panic tests retain separate contexts. Work on an unpropagated child thread
    is retained as context zero rather than guessed; that explicit health is
    the trigger for an exact process-per-test rerun until child/async context
    propagation is independently proven.
14. Compiler source identity no longer depends on diagnostic strings, Cargo
    target hashes or process paths. A strict manifest candidate hashes frozen
    canonical tuples for function entries, rejects digest collisions, merges
    two declarative expansions of the same authored token range, separates two
    proc-macro invocations whose output spans collapse to their callsites, and
    maps `OUT_DIR` source through project-relative package and generated paths.
    Two clean builds with unrelated target directories emit byte-identical
    candidates. The candidate is explicitly measurement-incomplete until all
    remaining point, branch and decision obligations use the same identity.
15. The first expanded-HIR denominator slice now emits function and executable
    statement points, `if`/`if let`/let-chain decisions, source-ordered logical
    atomic conditions and true/false alternatives. Repeated declarative macro
    expansions aggregate all four shapes; repeated procedural invocations stay
    distinct through an owner-local ordinal; proc-generated condition display
    is reconstructed from HIR rather than mislabeled with its invocation text.
    Selected MIR function probes no longer use toy 0/1/2/3 ordinals: each emits
    the u64 prefix of its exact manifest ID, with a second collision gate for
    the shortened transport key.
16. Rustc's THIR-to-MIR branch regions preserve the exact optimized true/false
    blocks for authored boolean conditions, pattern matches and every let-chain
    atom. The companion translates those regions into Supercov MIR calls: a
    token-bearing per-evaluation frame records ternary source-order values and
    publishes the frozen string decision ID through `rust-probe-transport-v1`.
    Goldens observe every short-circuit shape for `&&`, `||`, mixed
    `(a || b) && c`, both `if let` results, and a three-atom let chain. Parallel
    libtests keep their exact contexts, while production runtime tests cover
    nested frames and a frame finished on another thread. Frames now reserve
    their bounded mmap descriptor at evaluation start and commit only at the
    final outcome; compiler-level condition panic and killed-process tests
    leave one explicit incomplete descriptor without committing a false
    vector. This also corrected a denominator error: private control flow
    expanded inside external `assert!`/`println!` implementations is not
    misattributed to the authored caller. Generated
    owners now bind only when one compiler-typed boolean MIR branch has the
    exact expanded condition span. Declarative expansions sharing one authored
    decision ID, two distinct proc-macro invocations and build-generated source
    all emit their exact gated vectors; nested, derive and external expansion
    shapes remain a promotion corpus gate.
17. The exact-version companion combines branch-region retention with rustc's
    internal `no-profiler-runtime` switch, then removes every native coverage
    statement and mapping before codegen. The spike sets an LLVM profile path
    as a tripwire and requires that no file be created; it also requires the
    linked executable to contain no `__llvm_profile` or `__llvm_cov` symbol.
    Rustc provides compile-time branch correspondence only. All runtime
    observations and product evidence remain Supercov-owned.

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
   runtime probes into runtime MIR only after semantic analysis. Source
   identity v1 is now executable for function and statement points plus the
   first `if` decision/branch shapes: authored and
   declarative-expansion ranges aggregate by stable source tuple, synthetic
   proc/derive output adds expansion-chain and owner identity, and generated
   output uses package/out-relative provenance. Unknown, aggregation-mismatched
   or colliding identity fails manifest publication. Runtime function hits use
   the manifest-derived probe ordinal rather than a parallel ID namespace. For
   the first authored decision slice, compiler branch regions provide only the
   source-to-optimized-MIR correspondence; the companion translates them into
   Supercov frames and removes native MIR counters before codegen. No native
   profile is imported.
3. A separate CTFE provider path records compile-time execution. Runtime calls
   in const MIR are invalid. The first exact-version experiment now injects
   block and split-edge markers into `mir_for_ctfe` and captures their
   interpreter events in-process without emitting compiler-log output. This
   avoids a copied CTFE machine or bundled compiler fork. It remains private
   until the full const/static/const-generic corpus, manifest mapping,
   crash-safe publication, `RUSTC_LOG` coexistence and performance gates pass.
4. Runtime probes publish into the frozen bounded transport. Each record binds
   the task token, process ID, and a supervisor-resolved 64-bit context ID; the
   shared engine alone maps that context to run/worker/test/retry/phase and
   converts valid observations into evidence v3. The injected runtime has a
   nesting-safe thread-local carrier, and the companion activates it from
   rustc's own generated test marker rather than test-name heuristics. Child
   threads and executor migrations do not inherit TLS; they stay context zero
   and require owned propagation or exact rerun fallback.
5. A scoped rustdoc launcher selects the exact ordinary rustdoc and injects
   the compiler companion as its test-builder wrapper. The first proof maps
   standalone hidden lines and joins merged bundle/runner identities without a
   second extraction pass. Runtime probes, exact test attempt context, custom
   wrapper composition and the full doctest corpus remain required.
6. Generated files are captured in the isolated Cargo target after build
   scripts and before crate compilation. External symlinks and provenance
   ambiguity fail closed.
7. The companion emits only frontend obligations and observations through
   evidence v3. MC/DC solving, attribution merging, persistence and queries
   remain in the shared Rust engine.

## Next implementation gates

1. Extend the proven expanded-HIR `if` slice to nested control decisions,
   loops, match, let-else, `?`, assertions and remaining executable statement
   semantics; bind their real MIR/CTFE probes to the same manifest IDs. Add
   derive, nested/external expansion, generic/trait, include/module and
   package-fingerprint corpora before treating the candidate as a complete
   manifest.
2. Extend the proven libtest entry/unwind carrier through child threads, async
   executors, subprocesses, retry, late work and phases, or activate the exact
   process-per-test fallback whenever context-zero work is observed. Prove
   no_std and supported-target behavior. Windows remains a separate explicit
   target gate.
3. Generalize the proven CTFE marker path across every frozen compile-time
   surface, map every marker into the frozen manifest, and make publication
   crash-safe. Extend the proven rustdoc interception/mapping path with runtime
   probes and exact per-doctest attribution. Either incomplete area blocks
   public Rust support.
4. Add exact-version mismatch, missing-companion and custom-toolchain failure
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
