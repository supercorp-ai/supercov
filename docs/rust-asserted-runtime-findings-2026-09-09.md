# Rust assertion runtime: first slice and findings

Worktree: `/Users/domas/Developer/supercorp/supercov-asserted-typescript-analyzer`.
Branch: `codex/asserted-typescript-analyzer`, based on `ccfd015`, following the
uncommitted TypeScript extraction. No sample application changes, commits,
pushes, publishing, or public Rust backend promotion.

## Status: Step 5 is partially implemented, not finished

The handover's next step was Rust statement attribution, followed by runtime
assertion analysis and calibration against `cargo-mutants`. This slice adds:

- Post-run attribution to **an assertion invocation that is itself an exact
  compiler-mapped statement**. It requires matching file, line, column and
  source text, and a measured statement point. It does not guess an enclosing
  statement or an earlier producer statement.
- That location on compiler-projected evidence-v3 events. Attribution requires
  the original process and assertion context: inherited native threads and
  subprocesses retain their existing weaker phase links, not the caller's
  statement location.
- A Rust runtime analysis module, joined through the existing facts engine.
  Runtime links stay separate from value observations. Covered but untraced
  values remain analysis limits, not claims that the suite lacks assertions.
- A real Cargo fixture, a source-hashed 40-mutant oracle, an internal archive
  reader, and a repeatable calibration script with an explicit known-defect
  TODO regression.

No test-runtime probe, transport record, compiler injection, or deduplication
rule was added or changed. The new work is post-run. This is **not** full test
statement attribution: precomputed operands still require more evidence or
source analysis. Function-entry points are a deliberately bounded inventory,
not the planned Rust effect/decision-site inventory. All are classified
`review`; no global assertion percentage is emitted. Unmeasured points remain
listed separately and must not be interpreted as test gaps.

The implemented analyzer makes **zero mutant-kill predictions**. Its unknowns
must not be advertised as perfect accuracy. It is plumbing and a falsification
exercise, not a validated scoring system.

## Finding 1: the runtime-only promotion rule is unsound

The product plan says a production root function entered during an assertion
operand returned the value compared by the matcher. That implication fails
without any complicated async or external-library behavior:

```rust
assert_eq!({ discarded(21); 42 }, 42);
assert_eq!(masked(21) % 2, 0);
assert_ne!(weak(21), -999);
```

Each phase enters just one production function; filtering out nested callees
does not solve these examples. A function can have its result discarded,
coarsened by an operator, or compared with a predicate accepting many values.
Conversely, a result computed before the phase or on a joined thread can be
checked exactly without the producer ever entering that assertion phase.

All eight fixture functions compute `value * 2`; only how their results are
tested differs. No application bug was introduced or fixed.

| Function | Test observation | Passing assertion execution link | Caught / survived |
| --- | --- | --- | --- |
| `direct` | Exact equality in operand | Yes | 5 / 0 |
| `precomputed` | Local value, later exact equality | No | 5 / 0 |
| `smoke` | Call only | No | 0 / 5 |
| `discarded` | Return discarded inside operand block | Yes | 0 / 5 |
| `masked` | Only result modulo 2 compared | Yes | 3 / 2 |
| `weak` | Result must differ from -999 | Yes | 0 / 5 |
| `threaded` | Joined thread result compared exactly | No | 5 / 0 |
| `never_called` | Not executed | No | 0 / 5 |

Oracle: `cargo-mutants 27.1.0`, 40 mutants, 18 caught, 22 survived,
zero unmapped, timeouts, or unviable mutants. The rule "phase-linked function
means its mutations are caught" gives TP 8, FP 12, FN 10, TN 10:
**45% agreement, 40% predicted-kill precision on this adversarial fixture**.
This is a counterexample to that rule, **not an estimate of accuracy on typical
projects**, and not a new accuracy measurement of the TypeScript analyzer.
Nor does a surviving mutant automatically mean a test is wrong: the parity
test intentionally checks a weaker property.

The safe implementation keeps these links outside the join's `Observation`
objects. Even emitting `Presence` there would currently assign
`gap:value-not-asserted`, which is not justified by a temporal link alone.
The join returns seven covered-function limits and one not-reached function
in this fixture. The missing Rust source recognizer is explicit.

## Finding 2 — RUST-ASSERT-001: second assertion loses its phase

Status: **reproduced, not fixed**. This is a compiler/runtime-context issue
outside the post-run attribution changes. Root cause is not yet established.

Reproducer in `tests/fixtures/rust-asserted-runtime/tests/contract.rs`:

```rust
#[test]
fn exact_result_in_operand() {
    assert_eq!(direct(21), 42);
    assert_eq!(direct(2), 4);
}
```

Observed through the private compiler Cargo runner:

1. The test passes, and both decision vectors have outcome `true`.
2. Two distinct assertion phases exist, at source lines 5 and 6.
3. Line 5's decision and `direct` events belong to its passing assertion phase.
4. Line 6's decision and `direct` events belong to the **base test phase**;
   its created assertion phase has no outcome/status.
5. Transport health reports zero incomplete and zero dropped descriptors.

Therefore this is not simply first-hit deduplication: the second function hit
and decision exist, but their context is wrong. Normal MC/DC vectors survive;
per-assertion linkage underclaims. A healthy transport does not certify correct
semantic binding of compiler contexts. The new report explicitly lists
`unsettledAssertions` and never manufactures a pass from the surrounding test.

The calibration script runs the desired two-links assertion as a TODO only
when this exact symptom is present. Set `SUPERCOV_ASSERTED_STRICT_PHASES=1`
to make the known regression a failing test. Other failures still fail the
calibration. This is not a fully green end-to-end gate while the TODO remains.

## Finding 3: the documented public Rust path was overstated

At `ccfd015`, ordinary `supercov -- cargo test` enters `rust_run::run_direct_rust`,
the legacy source rewriter. It does not publish the compiler transport-health
entry or the compiler's per-invocation assertion evidence. The compiler route
is the private `__run-rust-compiler` command and requires a matching rustc and
libtest companion. The existing promotion assessment already documents this;
the assertion handover did not preserve the distinction.

Calibration deliberately uses that private route with
`requirePublicCapabilities: false`, just like the existing compiler spike
gates. It still executes the real stock-libtest Cargo suite; it does **not**
establish that this feature is available through the public command. No public
capability gate was relaxed or bypassed for shipping.

## Reproduce

From this worktree, using the installed Rust 1.95.0 toolchain, including its
compiler development libraries and `library/test` sources:

```sh
cargo build --offline -p supercov
cargo build --offline -p supercov-engine --example rust_asserted_runtime
RUSTC_BOOTSTRAP=1 RUSTUP_TOOLCHAIN=1.95.0 \
  cargo build --offline --manifest-path spikes/rustc-backend/Cargo.toml

# Real instrumented Cargo run, then compare with the saved source-hashed oracle.
node scripts/rust-asserted-runtime-calibration.mjs

# Additionally execute the 40 mutants again, not just replay their labels.
SUPERCOV_CARGO_MUTANTS=/absolute/path/to/cargo-mutants \
  node scripts/rust-asserted-runtime-calibration.mjs

# Explicit red gate for RUST-ASSERT-001 (also executes the normal fixture first).
SUPERCOV_ASSERTED_STRICT_PHASES=1 \
  node scripts/rust-asserted-runtime-calibration.mjs
```

The script requires `cargo-mutants 27.1.0` for a fresh oracle. It never installs
it implicitly. The one used here was installed into a task-owned temporary
directory with `cargo install cargo-mutants --version 27.1.0 --locked --root …`.
The script checks hashes of all three fixture files before reusing labels,
checks every fresh mutant name and outcome against the saved fixture, and
refuses to drop unmapped, timed-out or unviable cases from the comparison.
This calibration harness currently targets the local Unix development layout;
it is not a cross-platform release gate.
The archive reader refuses an empty or partly unmeasured function inventory
instead of silently shrinking its calibration denominator.

Each invocation makes a fresh temporary sample project; its path is printed.
The original fixture is not mutated. Kept artifacts include source, native
and instrumented stdout/stderr, the frozen archive, `analysis.json`,
`comparison.json`, `timings.json`, and fresh mutation outcomes when requested.
Temporary outputs are deliberately retained for inspection.

The final full rerun's artifacts are at
`/var/folders/4q/cdhj37516zzbc7z9ckrst0x40000gn/T/supercov-rust-asserted-adkIMD`.
Its fresh 40 outcomes matched the saved oracle exactly. The fixture and oracle
live under `crates/supercov-engine/tests/fixtures/`; source-format changes also
change those hashes and require explicit oracle regeneration.

## Verification completed

- `cargo test --offline --workspace`: 452 engine unit tests, 20 CLI tests,
  19 contract tests, and seven new Rust runtime-analysis integration tests
  passed. The workspace's existing opt-in parity tests do not by themselves
  exercise external datasets.
- `cargo clippy --offline --workspace --all-targets -- -D warnings` passed.
- `cargo fmt --all -- --check` and `git diff --check` passed.
- JavaScript runtime tests: 14 passed. Packaged runtime assets: 53 exact.
- TypeScript extraction tests with both external sample paths set: nine
  passed, including fresh facts and Rust join parity over all 2,474 sites.
- Real private-compiler Cargo run plus fresh mutation oracle: completed;
  RUST-ASSERT-001 remains one **known failing TODO**, not a passing regression.
- Strict-phase mode independently reproduced the intended failure (exit 1,
  one failing regression, no TODO). Its artifacts are in the sibling temporary
  directory `supercov-rust-asserted-dQk0nb`.

The earlier TypeScript extraction and both sample repositories are unchanged
by this Rust slice. The compiler companion and runtime template were built,
not edited. No public Rust route was changed.

## Performance interpretation and next gate

The first mutation calibration took 33.375 seconds, including its own baseline,
builds and test runs. Earlier successful analysis measured about 2.52 ms for
archive loading and 0.13 ms for analysis plus join. Those are single tiny-sample
observations, not a benchmark or evidence of an equally accurate replacement.
Executable startup, instrumented build/run, companion setup and discovery all
cost extra; the script logs them separately and measures the native test binary
as well as warm Cargo invocation. Tool installation/build time and engineering
discovery are not hidden in an analysis-only speedup claim.

Final full rerun, rounded single-run measurements on this machine:

| Work | Time |
| --- | ---: |
| Warm native Cargo suite | median 25.3 ms |
| Already-built native test executable | median 2.14 ms |
| Exact libtest companion preparation/check | 2.47 s |
| Private compiler instrumented build, suite and publication | 6.40 s |
| Analyzer executable, including startup and archive read | 330 ms |
| Archive decoding inside that process | 2.52 ms |
| Runtime analysis plus join inside that process | 0.135 ms |
| Fresh 40-mutant campaign, baseline included | 31.99 s |

The analyzer's sub-millisecond core produces **unknown value coverage**, not
the same answers as mutation testing. Dividing mutation time by that number
would be an invalid accuracy-equivalent speedup. Even the complete pipeline's
timings here are not a comparison against an optimized mutation system, and
the native executable is much cheaper than spawning Cargo per experiment.

**No accuracy-equivalent speedup, 10,000× advantage, or general 10% runtime budget
has been established.** No new runtime probes were added, but that fact does not
prove the existing compiler backend's overhead meets the target.

Next: one bounded source-to-assertion recognizer for **primitive exact equality
of a direct call or a simply bound local**, checked against these saved mutants
and additional held-out counterexamples. It must reject discarded/masked
operands, custom equality, mutable aliases and unmodelled thread transfer.
Do not upgrade phase-only links to value evidence. Keep RUST-ASSERT-001 as a
separate pending fix unless fixing it is explicitly requested; it does not
prevent this bounded source-analysis work. Full statement markers, Rust effect
sites, public integration and large-crate calibration remain later steps.
