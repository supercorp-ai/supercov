# Rust coverage v1 requirement traceability

This table distinguishes a frozen semantic requirement from implementation
evidence. A frozen row is not complete until every listed gate is green. The
private frontend must fail closed for every open row.

| ID | Requirement | Current executable evidence | Promotion gate |
|---|---|---|---|
| RCV-IDENTITY-1 | Stable authored-source identity and generic aggregation | `rust_instrumenter::discovers_rust_obligations_with_exact_ranges_and_stable_ids` | Cross-crate generic, trait and repeated-expansion corpus |
| RCV-POINT-1 | Statements and function entries | Rust instrumenter/runtime unit tests | Independent rustc behavior differential corpus |
| RCV-MCDC-1 | Source-ordered ternary masking MC/DC | probe-v2 vectors and Rust runtime short-circuit test | Full decision-kind golden corpus plus LLVM cross-check |
| RCV-SEMANTICS-1 | Preserve values, moves, borrows, drops, panic and ordering | Initial transform behavior test | Property/differential corpus across supported toolchains |
| RCV-BRANCH-1 | Every frozen branch alternative | Initial `if`, loop and match discovery tests | Exhaustive branch-kind contract vectors |
| RCV-EXPANSION-1 | Declarative macro expansions | Blocking limitation test | Owned expansion instrumentation and provenance corpus |
| RCV-EXPANSION-2 | Proc/derive expansions | Blocking limitation test | Owned expansion instrumentation and provenance corpus |
| RCV-GENERATED-1 | Build-script and included generated source | Blocking limitation path | Generated-source identity and crash corpus |
| RCV-CONST-1 | Const/static/const-fn execution | Blocking limitation test | Compile-time probe path and semantics corpus |
| RCV-DOCTEST-1 | Doctest extraction and attribution | Open | rustdoc mapping and hidden-line golden corpus |
| RCV-ATTRIBUTION-1 | Exact run/worker/test/retry/phase identity | Process-per-libtest runner test | Concurrency, retry, crash and late-work corpus |
| RCV-ARCHIVE-1 | Strict evidence v3 publication and query | Evidence/archive/run-store tests | Full CLI and lifecycle crash matrix |
| RCV-ORACLE-1 | No external product measurement | Contract assertion | rustc/LLVM oracle-only CI with product-dependency audit |
| RCV-PERF-1 | Warm and cold runtime at most 1.10x | Benchmark harness exists | Stable median gate on representative Rust corpus |

The open gates are intentional release blockers, not deferred semantics.
