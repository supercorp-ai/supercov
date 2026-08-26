# Rust coverage v1 requirement traceability

This table distinguishes a frozen semantic requirement from implementation
evidence. A frozen row is not complete until every listed gate is green. The
private frontend must fail closed for every open row.

| ID | Requirement | Current executable evidence | Promotion gate |
|---|---|---|---|
| RCV-IDENTITY-1 | Stable authored-source identity and generic aggregation | `rust_instrumenter::discovers_rust_obligations_with_exact_ranges_and_stable_ids` | Cross-crate generic, trait and repeated-expansion corpus |
| RCV-POINT-1 | Statements and function entries | Rust instrumenter/runtime unit tests | Independent rustc behavior differential corpus |
| RCV-MCDC-1 | Source-ordered ternary masking MC/DC | probe-v2 vectors and Rust runtime short-circuit test | Full decision-kind golden corpus plus LLVM cross-check |
| RCV-SEMANTICS-1 | Preserve values, moves, borrows, drops, panic and ordering | Compiler companion injects the production-shape mmap runtime plus real MIR calls; baseline/instrumented values, errors, caught panic, drops and output match while all four ordinals arrive | Property/differential corpus across supported toolchains |
| RCV-BRANCH-1 | Every frozen branch alternative | Initial `if`, loop and match discovery tests | Exhaustive branch-kind contract vectors |
| RCV-EXPANSION-1 | Declarative macro expansions | Compiler-backend spike sees expanded HIR/MIR and macro-definition/callsite spans | Stable expansion identity, real probes and provenance corpus |
| RCV-EXPANSION-2 | Proc/derive expansions | Compiler-backend spike sees proc-macro-generated HIR/MIR and invocation provenance | Stable generated-token identity, real probes and derive corpus |
| RCV-GENERATED-1 | Build-script and included generated source | Compiler-backend spike sees `OUT_DIR` source and mutates emitted MIR | Generated-source identity, real probes and crash corpus |
| RCV-CONST-1 | Const/static/const-fn execution | Exact-version companion inserts in-memory CTFE block and split-edge markers; both const-fn paths are observed with identical values/output | Full const/static/inline-const/const-generic manifest, semantics, concurrency, crash and performance corpus |
| RCV-DOCTEST-1 | Doctest extraction and attribution | Scoped exact-rustdoc launcher observes standalone/merged sources; hidden lines and merged `__doctest_N` path/line identities are mapped without enabling unstable user code | Runtime probes, exact per-doctest attempt transport, wrapper composition and full doctest corpus |
| RCV-TRANSPORT-1 | Bounded, authenticated, crash-safe observation publication | Frozen `rust-probe-transport-v1`; thread/process concurrency, descriptor/payload exhaustion, malformed token/context/header/descriptor/checksum, symlink, incomplete record and killed-writer tests | Dynamic exact-context propagation, Linux target matrix and Windows implementation before those targets are claimed |
| RCV-ATTRIBUTION-1 | Exact run/worker/test/retry/phase identity | Transport records a context ID per observation; rustc marker-derived entry/normal-exit/unwind instrumentation separates five concurrent ordinary, attribute-macro and expected-panic libtests; child-thread work stays context-zero; process-per-libtest reference remains exact | Owned child/async propagation or automatic exact fallback, retry, subprocess, crash and late-work corpus |
| RCV-ARCHIVE-1 | Strict evidence v3 publication and query | Evidence/archive/run-store tests | Full CLI and lifecycle crash matrix |
| RCV-ORACLE-1 | No external product measurement | Contract assertion | rustc/LLVM oracle-only CI with product-dependency audit |
| RCV-PERF-1 | Warm and cold runtime at most 1.10x | Benchmark harness exists | Stable median gate on representative Rust corpus |

The open gates are intentional release blockers, not deferred semantics.
