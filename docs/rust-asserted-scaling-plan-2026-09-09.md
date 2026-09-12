# Frozen scope: identity scaling experiment

Written before executing the new workload. No recognizer changes or namespace
expansion belong to this step. Keep the existing mutation/coverage oracles intact.

Use a new deterministic, synthetic dependency-free Cargo workload, held out from
the earlier recognizer implementation. It is not an independently maintained real
crate or an accuracy benchmark. Increasing replicated families measures scaling,
not the diversity of supported Rust semantics.

Sizes: 8, 32 and 128 families. Each family has four production functions
(producer, executed decoy, masked producer and uncalled function) and three tests
(renamed direct equality, immutable value copies and masked equality). Production
arithmetic and comparison expectations are fixed before running the analyzer.

Freeze generated source hashes and expected structural counts before capture:
4N functions, 3N tests, N uncalled functions; with identities enabled, 2N value
witnesses on N return boundaries, 3N unresolved function boundaries. In disabled
mode the file's renamed imports remain unsupported. Do not use mutation-kill
labels for unresolved cases. Require identical complete coverage inventories,
runtime analyses and source hashes for each off/on pair. A mismatch stops the
experiment; never discard inconvenient sites or events.

Run three repetitions at each size, alternating off/on order across repetitions.
Each capture starts in a new project/target directory; measure warm native Cargo
and already-built native test execution too. Benchmark captures run sequentially,
not alongside builds/tests we launch. Repeat full queries against each archive,
checking semantic equality each time; do not skip integrity verification to make
the query faster. Use optimized Supercov/compiler/analyzer binaries, record their
hashes, and retain the same ordinary Cargo test profile for the fixture.

Separate archive read/decode, runtime analysis, integrity verification, source
read, source recognition and join in the internal reader. Retain existing run
timings, but do not sum instrumentedBuildMs and testCommandMs: the former contains
the latter in this checkout. Their difference is the pre-execution orchestration/
build interval, not pure rustc time. Test-command time includes Cargo and runner
bookkeeping, not only application execution. Document this timer issue, no fix.

Decision: do not expand supported syntax unless this experiment gives a credible
account of scaling and the additional identity work. A small noisy median below
10% does not prove the user's <=10% budget. If timings are materially noisy or
source recognition grows superlinearly, report the unresolved performance gate
and identify the next measured bottleneck; do not tune the recognizer during the
benchmark or claim a mutation-testing speedup from a cheaper, different result.

## Amendment after the first pair, before further scaling

The first 8-family pair stopped: exact cross-run runtime-analysis equality failed
solely on opaque phase ID strings. Inspection showed every other field, link and
link multiplicity identical. `rust_phase_projection::phase_id` hashes the dynamic
context and invocation nonce; identical source executions need not have identical
phase IDs across fresh runs. Same-archive repeated-query equality is still exact.

The benchmark now verifies a bijection between opaque phase IDs and the tuple
(test attempt, assertion location/operation, exact assertion source). This is
valid for this fixture's one invocation per test/assertion; merged or split
phases reject the comparison. All other fields and every link remain compared,
including point, statement, order and multiplicity. Unit tests deliberately
change/drop/duplicate links and merge/split phases. No recognizer or fixture
source change; original source hashes remain fixed.

Stopped run retained at
`/var/folders/4q/cdhj37516zzbc7z9ckrst0x40000gn/T/supercov-asserted-scaling-7zAPkJ`.
It is exploratory evidence and will not be silently pooled into the three full
repetitions. Restart all planned pairs after this comparator correction.
