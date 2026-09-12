# Supergateway: fresh capture restores 84/100 agreement

## Outcome

The fresh full-suite capture restores **84/100 agreement** against the same
saved Stryker outcomes, without removing the exact successful assertion-witness
requirement, restoring helper-name assumptions, or changing any production
analyzer rule. The target of at least 85/100 is **not yet met**. The calibration
command's new `--check` mode reports that failure explicitly.

This is mutation-outcome agreement, not “84% of the whole library is asserted.”
The saved 100-mutant benchmark covers five `src/lib` files. The entire existing
suite was captured, but there is no full-repository mutation oracle here.

## Same application and tests

Both native and instrumented runs used the existing prototype worktree:

`/Users/domas/Developer/supercorp/supergateway/.claude/worktrees/asserted-coverage-prototype`

It is at `7d838fa`. It contains nine test-file additions/changes relative to the
main Supergateway checkout, including the library unit tests used for the
original comparison. Running the main checkout instead would silently change
the suite. Nothing in either application checkout was edited. The original
converted coverage and inventory were left intact.

New work remains in the main Supercov checkout on
`codex/asserted-js-integration`. Only a calibration script and reports were
added; no product code, runtime probe, sample test or configuration changed.
No mutation campaign ran. No commit or push was made.

The capture metadata says `git.dirty: true`; that is preserved in the results,
not replaced by a claim that the entire enclosing repository was clean. An
independent hash of every `src/` and `tests/` file matches the preceding frozen
replay before and after calibration, and the worktree's local `git status`
remained empty.

## Captured evidence

After `npm run build`, both commands passed with Node 24.18.0:

```sh
TS_NODE_TRANSPILE_ONLY=1 npm test
TS_NODE_TRANSPILE_ONLY=1 /Users/domas/Developer/supercorp/supercov/target/debug/supercov -- npm test
```

- Both runs: **84 passed, 0 failed, 25 pre-existing TODOs** (109 registered).
- Archive: `run_2e5d74892cf7c201`.
- 84 captured passed test records; **504 passed assertion phases**.
- 273 instrumented assertion call sites; call sites and dynamic phases are
  different counts because some sites execute repeatedly.
- 82 records link to modeled static tests. Two tests using unsupported callback
  shapes remain unlinked and are named in diagnostics.
- 101 retained source observations, every one checked against an exact matching
  source position, assertion method and passed phase in its owning test.
- The legacy 661-site inventory is unchanged. Its 364 contractual sites now
  have 78 `evident`, eight `partial`, zero `presence`, 278 `unresolved` candidates.
  These remain unverified candidates, not semantic proofs.

The analysis uses the existing converter and a temporary output directory. Its
`coverageRunner: "vitest"` option disables the legacy V8/ts-node line remapping:
Supercov already records original-source positions, including for Node tests.
It does not mean these tests were run in Vitest.

## Same fixed 100-mutant denominator

| Result | Original prototype, old evidence | Current analyzer, old evidence | Current analyzer, fresh evidence |
| --- | ---: | ---: | ---: |
| Correct under original binary scoring | 94 | 10, from forcing all limits into survival | **84** |
| True kills | 88 | 0 | 78 |
| False kills | 4 | 0 | 4 |
| Missed kills under binary scoring | 2 | 90 | 12 |
| True survivals under binary scoring | 6 | 10 | 6 |
| Unknowns in the additional availability view | 1 | 100 | **2** |

Fresh positive-prediction precision is **95.1%** (78/82); kill recall including
unknowns is 86.7% (78/90). In the availability view, two actual kills are unknown
and ten are predicted survivors. Correct/fixed-cohort remains **84/100**.
Reporting 84/98 = 85.7% instead would hide unresolved work and would not meet
the user's requested target. The denominator is never reduced to cross 85%.

The five stored timeouts retain the original “killed” convention. Excluding
them gives 80/95. The four false kill predictions are the same historical
survivors, not new successes disguised by reclassification.

| File | Correct / mutants | Missed kills | False kills |
| --- | ---: | ---: | ---: |
| `corsOrigin.ts` | 13/16 | 0 | 3 |
| `getLogger.ts` | **10/21** | **11** | 0 |
| `getVersion.ts` | 2/2 | 0 | 0 |
| `headers.ts` | **21/21** | 0 | 0 |
| `sessionAccessCounter.ts` | 38/40 | 1 | 1 |

## The next useful capability is localized

The missing logger predictions are mutants 41, 42, 43, 44, 46, 56, 57, 60, 61,
62 and 63, around formatter output and logger selection at lines 44–45, 73
and 77. The remaining session-counter miss is mutant 145 at line 48.

The logger tests already assert useful behavior, but the analyzer recognizes
only the direct console mock call-count assertions. It cannot trace arguments
through expressions such as:

```ts
const infoCalls =
  infoStream === 'log' ? log.mock.calls : error.mock.calls.slice(0, 1)
assert.equal(infoCalls[0].arguments[1], 'hello')
```

The returned logger also comes from several prebuilt objects. Observing that
some calls happened does not automatically establish which object's behavior
was checked. Recovering these cases needs source-/instance-aware selection and
flow, not credit for both conditional arms or every earlier executed line.
No unvalidated rule was added just to cross the threshold.

## Regression target and reproduction

New script: `scripts/asserted-supergateway-recapture.mjs`. It fixes the source,
test, inventory, fixture and mutant identities to the previous replay, verifies
all positive assertion witnesses, and exposes the unchanged prediction policy.
It writes detailed results before returning a failing check, so failures remain
inspectable.

The target is at least **85 correct out of 100 including unknowns**, no more
than four false kills, and no loss below the current 78 true kills. These are
sample regression thresholds, not proof of correctness or global accuracy.

From the Supercov checkout, using a previously unused output path:

```sh
node scripts/asserted-supergateway-recapture.mjs \
  /Users/domas/Developer/supercorp/supergateway/.claude/worktrees/asserted-coverage-prototype \
  run_2e5d74892cf7c201 \
  /absolute/path/to/new-result.json --check
```

This currently exits **1 because the 85/100 capability gate fails at 84/100**.
Omit `--check` for measurement-only use. It does not rerun the suite or mutants.
It refuses to overwrite previous results.

[Validated results, coverage summary, exact misses and gate](asserted-supergateway-recapture-gate-2026-09-09.json).
[Initial fresh result, preserved unchanged](asserted-supergateway-recapture-2026-09-09.json).

## Public query and execution coverage

The ordinary-archive text query works with freshness checks, including:

```sh
/Users/domas/Developer/supercorp/supercov/target/debug/supercov \
  runs run_2e5d74892cf7c201 asserted --file src/lib/headers.ts --limit 3
```

However, JSON querying with `--limit 1` fails with `RESPONSE_TOO_LARGE`:
129,943 bytes against the 65,536-byte cap. Shared test evidence/diagnostics are
not paginated. A smaller site page cannot remove that shared overhead. This is
a reproduced known integration limitation, separately logged below, not a
reason to rerun or discard the archive.

The public archive adapter has a different, incomplete assertion-site inventory
from the frozen prototype, so its candidate counts are not substituted into
the 661-site benchmark. The legacy converter also does not join separately
archived server streams, whereas the public adapter does. Thus the frozen
benchmark is not advertised as public end-to-end score parity.

Separate execution coverage: **97.31% lines**, 97.22% statements, 95.45%
functions, and **80.62% MC/DC** (104/129 conditions). There are eight source-scope
measurement limitations; this is not a complete measurement of all code.
Six tests have assertion phases but zero attributed coverage, which may be
valid for uninstrumented/static observations; the run reports that warning.

## Speed and checks

The test runner reported **37.493 s native → 43.814 s instrumented**, about
**+16.9%**. This is one sequential pair, not a controlled overhead benchmark,
but it does not establish the <10% target. Supercov's complete invocation took
49.435 s, including workspace preparation, instrumented build and publication.
Those costs must not be confused with incremental assertion-marker overhead.
No probes were added or removed in this task.

Calibration's current-analyzer-plus-Rust-join pass is query-time work; the first
fresh pass took about 1.64 s. Existing query overhead remains a separate budget.

Passed: both application runs, all witness/source/cohort checks, 14 analyzer
tests (three opt-in integration/external cases skipped), and three reporting
tests, all 14 runtime tests, and the exact-copy check for all 53 packaged runtime
assets. The intentionally unmet capability gate is not presented as a passing
test. No product analyzer rule changed, so the prior adversarial semantics were
not weakened.

## Next

Implement one narrowly scoped logger dependency rule with both useful positive
cases and conditional/unrelated-mock negative controls. Reuse this archive and
the frozen oracle while iterating; cross 85/100 without raising false kills or
dropping unknowns. In parallel workstreams, repair shared-evidence JSON
pagination and measure/optimize test-time overhead. Those are distinct remaining
problems, not solved by the recovered agreement.
