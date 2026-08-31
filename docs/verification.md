# Verification

Coverage is useful only if instrumentation preserves program behavior and the
reported obligations match what actually executed. Supercov fails closed when
it cannot establish either condition.

## What release checks cover

Every release is checked for:

- identical return values, thrown errors, and side-effect order before and
  after instrumentation;
- short-circuiting, getters, proxies, optional calls, `this` binding, defaults,
  exceptions, loops, async functions, and generators;
- exact line, branch, decision-vector, and MC/DC results;
- JavaScript behavior across a pinned TC39 Test262 corpus;
- supported JavaScript, TypeScript, and Rust runner contracts;
- Chromium, Firefox, and WebKit browser execution;
- source isolation, interrupted-run recovery, and atomic publication; and
- package installation and execution from a clean project.

MC/DC cases are also compared with an independent LLVM implementation so a
self-consistent error in Supercov's own calculation does not pass unnoticed.

## What happens when code cannot be measured safely

Supercov does not force a transform through code that observes its own source
text or creates source dynamically without a stable denominator. It leaves the
affected behavior uninstrumented and records a completeness blocker with the
reason and location.

Similarly, evidence from an unsupported runner or hidden remote boundary is
reported as aggregate, unattributed, or missing. It is not assigned to a test
that may not have caused it.

Inspect these states with:

```sh
npx supercov runs latest
npx supercov runs latest scope
npx supercov runs latest gaps
```

## How to review a coverage change

For a test added by a coding agent, check three things:

1. The wrapped test command still passes.
2. `diff` shows the expected obligations gained and no unexplained loss.
3. The test contains meaningful assertions and does not weaken application
   behavior merely to improve a percentage.

```sh
npx supercov diff <baseline-run> <new-run>
npx supercov runs <new-run> test "new test name"
```

Store the compared run ids in the review or agent summary when the evidence
needs to be reproducible later.

## Integrity of stored runs

Completed runs are immutable and integrity-bound to their evidence,
fingerprints, and schema. A run whose bytes are missing, corrupt, stale, or
incompatible is surfaced as such instead of being opened as a plausible report.

See [Evidence and runs](/docs/evidence) for retention, comparison, and shard
merging.
