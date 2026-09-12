# JS/TS assertion release review — 2026-09-09

## Scope and isolation

The shared checkout remains on `codex/asserted-js-integration`. With user approval,
the JS/TS changes were copied to a separate review snapshot based on `ccfd015`.
Only the JS/TS `ServerRecord.statement_id` definition and propagation were taken
from the mixed `coverage_report.rs` diff. No new Rust assertion modules, compiler
identity experiments, Rust fixtures, spikes, `go.mod` or `m_test.go` were included.

The three existing commits after main are the statement instrumentation, generic
assertion join and JavaScript site inventory this feature depends on. Existing
statement markers remain in the baseline; this review adds no test-time probes.

## Checks and findings

- Full `npm run check` passed in the shared checkout, including workspace format,
  Clippy, Rust tests, runtime/assets, analyzer tests, ordinary archive integration,
  packed consumer and native-install metadata checks.
- The isolated snapshot compiled with only its selected JS/TS changes. It uses
  its own dependency install and Cargo target directory; no shared binaries were
  overwritten to perform this check.
- Full `npm run check` also passed in that isolated snapshot. Replaying the fixed
  cohort from its own binary and analyzer preserved 84/100 agreement (78 true
  kills, four false kills, ten false-survival predictions and two unresolved
  killed cases); the ordinary public query succeeded.
- **Parity reporting correction:** the earlier packaging record's Rust parity
  invocation had not supplied external fixture paths, so that test returned
  without comparing data. It should have been listed as an opt-in check, not a
  completed frozen comparison. Enabled explicitly during this review, it now
  passes against all **661 Supergateway sites** and **1,813 Essential SEO sites**.
  This checks the join on frozen prototype facts. It does not validate all source
  inferences or change the separate historical 84/100 mutation agreement.
- Both optional external TypeScript source-parity checks were enabled in the
  isolated snapshot. Essential SEO passed immediately. Supergateway exposed a
  stale suppression counter (155 versus 153) after the two pragmas moved into
  a separate hint list. The test now verifies that exact migration and both
  samples pass. No model credit, site denominator or production rule changed.
  See [the review bug log](supercov-bugs-release-review-2026-09-09.md).
- Fresh dependency installation reported 13 existing development dependency
  advisories. `npm audit --omit=dev --json` reported **zero** production advisories.
  No dependency upgrade or audit fix was applied as part of this feature.
- Docker is installed but its daemon is unavailable locally. Linux and Windows
  execution therefore requires the authorized GitHub Actions branch run, not a
  claim that local macOS tests proved those platforms.
- TypeScript 7.0.2's new native API works on the small fixture, but it is not
  compatible with the old API expected by this analyzer. See
  [the source/API investigation](asserted-typescript7-investigation-2026-09-09.md).

## Release preparation

An experimental feature note was added under `CHANGELOG.md`'s existing Unreleased
section. It preserves the TypeScript limit, candidate-only meaning and current
instrumentation scope. The release version remains **0.0.42**; no tag, registry
publication or GitHub release was requested or performed.

The review branch and build-only workflows may be pushed/dispatched. The publish
workflow must not be invoked. Platform results must be tied to the exact commit
tested, with failing and unexecuted gates reported explicitly.

## Remote verification and release decision

The authorized review branch is `codex/asserted-js-release-review`. Product code
was pushed as `1668e3ae7d4ff971efb1420dea54adea775e3684`; follow-up `1cf6b31`
contains only external-test corrections and research documentation/fixtures.
The shipped files, Rust sources, packaging scripts and workflow inputs are
unchanged between those commits. Both external source-parity tests were rerun
successfully at the follow-up, with neither skipped.

- [Full Linux CI](https://github.com/supercorp-ai/supercov/actions/runs/34375398993)
  passed at `1668e3a`.
- [Native artifact matrix](https://github.com/supercorp-ai/supercov/actions/runs/34375395031)
  at the same commit has passed all four Linux targets and macOS arm64.
- Both Windows targets failed the installed JS/TS assertion query despite
  successfully installing the supported compiler. See REVIEW-004 in the
  [bug log](supercov-bugs-release-review-2026-09-09.md). This is separate from
  TypeScript 7's intentionally unsupported API.
- Intel macOS was still building at 16:27 UTC. Its result is not claimed here.
  The complete release set cannot be certified with the Windows failures.

No publish workflow, tag, version bump, GitHub release or PR was created.
The branch is available for review, **not ready for an all-platform release**.
First resolve and rerun the Windows gate. Then choose whether to release the
explicitly limited experimental analyzer or separately implement/calibrate the
TypeScript 7 backend. Compiler compatibility work belongs in post-run analysis;
it does not call for additional test-time probes.
