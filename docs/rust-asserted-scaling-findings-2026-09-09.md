# Assertion identity scaling: query work is the bottleneck

Worktree: `/Users/domas/Developer/supercorp/supercov-asserted-typescript-analyzer`,
branch `codex/asserted-typescript-analyzer`. This is a Git worktree of
`supercorp/supercov`, not a separate project. It shares repository history while
isolating working files from the other agents' checkout. Nothing was pushed or
committed; no sample applications, recognizer rules or runtime probes changed.

## Decision

**Do not broaden syntax yet.** All nine completed off/on pairs retained the
expected evidence, but source analysis scales badly. At 512 functions and 384
tests, a full optimized query takes 3.62 seconds, of which 3.57 seconds is source
recognition. Parsing and repeated searches must be addressed before expansion.

This does not prove the <=10% runtime overhead target. The actual test-command
interval is noisy, including a +24.4% median difference at the smallest size.
Nor does this establish a mutation-testing speedup or global assertion score.

## Frozen workload and gates

The plan was written before measurement in
`rust-asserted-scaling-plan-2026-09-09.md`. A new synthetic Cargo workload was held
out from earlier recognizer development. It is not a maintained external crate
or evidence of general Rust accuracy. Each family has a conditional producer,
executed/discarded decoy, masked producer and uncalled function; three tests use
renamed direct equality, immutable copies and masked equality. There is exactly
one assertion invocation per test. The unused glob-import warning is retained;
sources were not tweaked after timing began.

| Functions | Tests | Identity records | Return witnesses | Witnessed boundaries | Unresolved boundaries | Uncalled functions |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 32 | 24 | 56 | 16 | 8 | 24 | 8 |
| 128 | 96 | 224 | 64 | 32 | 96 | 32 |
| 512 | 384 | 896 | 256 | 128 | 384 | 128 |

All these counts passed in every enabled capture. Disabled mode retains every
function but produces no return witnesses because renamed imports are outside
its supported namespace. Unresolved is not a prediction that mutations survive.
No new mutation campaign was run for this performance-only workload.

The complete coverage inventories match exactly in every pair. Runtime analyses
match under a checked bijection of run-local phase IDs; all other fields, links,
order and multiplicities remain compared. The initial exploratory pair stopped
because phase IDs contain invocation nonces and are not stable cross-run. That
failure and the comparator amendment are preserved in the plan. It was not a
product correctness failure or permission to drop phase membership.

Each of 18 captures has one first query and three warm queries: **54 warm replays**
match their own archive's full semantic report exactly, with only timing fields
removed. The cross-run phase mapping is not used for same-archive replay.

## Measurement

Rust 1.95.0, Node 24.18.0, ARM64 macOS. Optimized Supercov, compiler companion and
reader binaries; the generated application uses ordinary Cargo's test profile.
Three repetitions per size, alternating off/on order. Every capture uses a fresh
project and target directory. Benchmark captures run sequentially; builds,
regression tests and profiling we initiated were kept outside timed captures.
Other machine activity was not globally controlled. This is a small local sample,
not a confidence bound. Raw repetitions, binary/source hashes and artifact paths
are saved in `rust-asserted-scaling-results-2026-09-09.json`.

### Query costs: medians of nine warm replays per row

| Functions | Full query, off | Full query, on | Source analysis, off | Source analysis, on |
| ---: | ---: | ---: | ---: | ---: |
| 32 | 35.0 ms | 38.4 ms | 3.32 ms | 8.96 ms |
| 128 | 82.3 ms | 189.4 ms | 48.55 ms | 152.73 ms |
| 512 | 775.3 ms | 3,623.7 ms | 727.82 ms | 3,571.52 ms |

Each 4x size increase multiplies enabled source analysis by about 17x and 23x.
Disabled-mode analysis also grows roughly quadratically. The main cost is not
decompression: at the largest enabled size, archive read is 2.90 ms, decoding
4.15 ms, runtime analysis/join 1.09 ms, integrity verification 8.28 ms, source
discovery/read 18.57 ms and the final source join 0.36 ms. Phase medians need not
sum to the median process time, which also includes process startup/serialization.

The source-analysis interval includes the recognizer's own snapshot hash check,
baseline runtime analysis, parsing and matching. Source-read includes discovery
and Cargo/library-identity preparation, not only filesystem reads. Every replay
performs integrity verification; none uses stale cached facts to lower timings.

### Run costs: medians of three independent captures

| Functions | Pre-execution setup/build off / on | Test command off / on | Evidence publication off / on | Entire private run off / on |
| ---: | --- | --- | --- | --- |
| 32 | 4,610.5 / 4,710.0 ms | 692.9 / 861.8 ms | 12.3 / 12.0 ms | 5,860.9 / 6,071.6 ms |
| 128 | 4,632.5 / 4,681.3 ms | 736.6 / 698.9 ms | 22.4 / 23.8 ms | 6,001.0 / 5,982.2 ms |
| 512 | 5,035.1 / 4,997.7 ms | 739.8 / 789.7 ms | 68.4 / 74.2 ms | 6,557.5 / 6,501.5 ms |

The existing `instrumentedBuildMs` interval includes `testCommandMs`. The table
subtracts it exactly once and calls the result pre-execution setup/build, not
pure compiler time. `testCommandMs` includes Cargo and runner bookkeeping, not
just application test code. The overlap is documented, unfixed, as
`RUST-TIMING-001` in `supercov-bugs-scaling-step-2026-09-09.md`.

Whole-run medians change by +3.6%, -0.3% and -0.9%. Those small end-to-end changes
do not certify low test-execution overhead: test-command medians change by
+24.4%, -5.1% and +6.7%, and individual samples are noisy (small enabled samples:
674, 862 and 1,370 ms). No more runtime probes were added, but that alone is not
a measured <=10% bound on the complete feature.

Native baselines remain cheaper: warm Cargo is roughly 24–31 ms and the built
test executable roughly 2.3–6.9 ms. Native build-only medians are about 179–424 ms;
first execution of new binaries has additional startup cost. These private
instrumented runs are cold-build workflows, so comparing only their total with
warm native execution would mix different work. The existing private backend's
several-second setup/build cost is not evidence of a fast complete product path.

Compressed evidence grows from 20,961 to 22,787 bytes, 77,063 to 84,139 bytes,
and 298,388 to 332,106 bytes with identities enabled. Uncompressed metadata growth
is roughly 6.8–7.0%. Size medians retain the original archives; compression may
vary with run-local IDs.

## What to optimize next, without changing semantics

Source inspection identifies concrete repeated work:

1. `rust_asserted_source.rs:512` loops over results, then at line 527 reparses the
   same complete test file for each result, including namespace validation.
2. Inside that per-result loop, line 570 walks **every test function body**,
   rather than selecting the source bodies needed by that attempt's assertion
   phases. It performs work before discovering that an assertion is from another
   test and has no matching phase in this result.
3. `unique_identity` at line 346 scans every compiler record for each lookup.
   Function/point/owner searches are also linear in several places.

A separate post-benchmark `sample` capture is retained at
`/tmp/supercov-source-profile.YJiZLr/query.sample.txt`. Release configuration strips
symbols, so it did **not** identify Rust helper-level cost shares. Do not treat
its opaque addresses as a successful fine-grained profile. The measured phase
timers establish the source-analysis bottleneck; the code above identifies
candidate repeated work, not its individually measured contribution.

Next bounded engineering step: parse/validate each source file once per query,
index exact source locations and compiler identities, and avoid re-analyzing
unrelated test bodies. Keep duplicate/conflicting identities visible; preserve
full namespace checks and source fingerprints. Cache immutable source structure,
not dynamic pass/fail or cross-attempt value provenance. Require identical
witnesses, binding paths, limits, unresolved sites and inventories on all existing
fixtures, then rerun this unchanged scaling corpus. Do not loosen guards or add
Rust syntax support in the same change.

## Reproduction and verification

From this worktree, without custom RUSTFLAGS/CARGO_ENCODED_RUSTFLAGS:

```bash
cargo build --release --offline -p supercov -p supercov-engine \
  --bin supercov --example rust_asserted_runtime
RUSTC_BOOTSTRAP=1 RUSTUP_TOOLCHAIN=1.95.0 \
  cargo build --release --offline --manifest-path spikes/rustc-backend/Cargo.toml
node --test scripts/rust-asserted-scaling-calibration.test.mjs
node scripts/rust-asserted-scaling-calibration.mjs
```

`--plan-only` writes the deterministic workload/expected counts without running
it. Full runs preserve the plan, sources, raw logs, evidence, first/warm query
measurements and pair checks in fresh temporary directories. The completed run
root is `/var/folders/4q/cdhj37516zzbc7z9ckrst0x40000gn/T/supercov-asserted-scaling-lbe2UW`.
One-time optimized builds took about 2m26s for Supercov/reader and 15s for the
compiler companion; this cost is not included in the benchmark table.

Workspace regression tests pass (469 engine, 20 CLI, 19 contract, seven Rust
integration tests), as do four new benchmark/comparator unit tests and Clippy
with warnings denied. Existing alias and runtime calibrations retain their
source-hashed mutation oracles and witness counts. They did not run fresh mutants
for this step. `RUST-ASSERT-001` remains a failing desired-behavior TODO, not fixed
or replaced by guessed evidence. This is not an all-green end-to-end release gate.
