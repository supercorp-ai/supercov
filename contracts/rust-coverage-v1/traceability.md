# Rust coverage v1 requirement traceability

This table distinguishes a frozen semantic requirement from implementation
evidence. A frozen row is not complete until every listed gate is green. The
private frontend must fail closed for every open row.

| ID | Requirement | Current executable evidence | Promotion gate |
|---|---|---|---|
| RCV-IDENTITY-1 | Stable authored-source identity and generic aggregation | Frozen source-identity-v1 tuple plus compiler-manifest candidate; exact manifests survive two clean target directories, repeated declarative expansions aggregate, distinct proc-macro invocations remain distinct and generated IDs contain no ephemeral path | Cross-crate generic/trait/derive corpus and collision/fingerprint matrix |
| RCV-POINT-1 | Statements and function entries | Compiler candidate emits stable function/statement points across authored, repeated declarative, proc-macro and generated source; selected function probes carry their manifest-derived ordinal | Remaining executable-point semantics, complete probes and independent rustc differential corpus |
| RCV-MCDC-1 | Source-ordered ternary masking MC/DC | Compiler candidate flattens authored `&&` conditions and identifies `if`, `if let` and let-chain atomic conditions; probe-v2 vectors remain language-neutral | Real compiler decision probes, full decision-kind goldens and LLVM cross-check |
| RCV-SEMANTICS-1 | Preserve values, moves, borrows, drops, panic and ordering | Compiler companion injects the production-shape mmap runtime plus real MIR calls; baseline/instrumented values, errors, caught panic, drops and output match while all four ordinals arrive | Property/differential corpus across supported toolchains |
| RCV-BRANCH-1 | Every frozen branch alternative | Compiler candidate emits stable true/false alternatives for expanded `if`, `if let` and let-chain decisions | Remaining branch kinds, real alternative probes and exhaustive contract vectors |
| RCV-EXPANSION-1 | Declarative macro expansions | Compiler candidate aggregates function, statement, decision and branch identities for two expansions of the same authored macro token ranges | Real decision/branch probes and nested/external macro corpus |
| RCV-EXPANSION-2 | Proc/derive expansions | Compiler candidate binds collapsed proc function/statement/decision/branch output to stable callsite + complete expansion chain + owner-local ordinal and separates repeated invocations | Real generated probes and attribute/derive/nested proc corpus |
| RCV-GENERATED-1 | Build-script and included generated source | Compiler candidate keys owned `OUT_DIR` function/statement/decision/branch source by project-relative package root + out-relative path and is byte-identical across target directories | Package/build-script fingerprints, real generated probes and crash/symlink corpus |
| RCV-CONST-1 | Const/static/const-fn execution | Exact-version companion inserts in-memory CTFE block and split-edge markers; both const-fn paths are observed with identical values/output | Full const/static/inline-const/const-generic manifest, semantics, concurrency, crash and performance corpus |
| RCV-DOCTEST-1 | Doctest extraction and attribution | Scoped exact-rustdoc launcher observes standalone/merged sources; hidden lines and merged `__doctest_N` path/line identities are mapped without enabling unstable user code | Runtime probes, exact per-doctest attempt transport, wrapper composition and full doctest corpus |
| RCV-TRANSPORT-1 | Bounded, authenticated, crash-safe observation publication | Frozen `rust-probe-transport-v1`; thread/process concurrency, descriptor/payload exhaustion, malformed token/context/header/descriptor/checksum, symlink, incomplete record and killed-writer tests | Dynamic exact-context propagation, Linux target matrix and Windows implementation before those targets are claimed |
| RCV-ATTRIBUTION-1 | Exact run/worker/test/retry/phase identity | Transport records a context ID per observation; rustc marker-derived entry/normal-exit/unwind instrumentation separates five concurrent ordinary, attribute-macro and expected-panic libtests; child-thread work stays context-zero; process-per-libtest reference remains exact | Owned child/async propagation or automatic exact fallback, retry, subprocess, crash and late-work corpus |
| RCV-ARCHIVE-1 | Strict evidence v3 publication and query | Evidence/archive/run-store tests | Full CLI and lifecycle crash matrix |
| RCV-ORACLE-1 | No external product measurement | Contract assertion | rustc/LLVM oracle-only CI with product-dependency audit |
| RCV-PERF-1 | Warm and cold runtime at most 1.10x | Benchmark harness exists | Stable median gate on representative Rust corpus |

The open gates are intentional release blockers, not deferred semantics.
