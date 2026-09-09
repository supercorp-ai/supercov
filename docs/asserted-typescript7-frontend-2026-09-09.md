# Native TypeScript 7 frontend and Windows follow-up

Work is isolated on `codex/asserted-js-release-review`. No sample application
code, sample tests or test-time runtime probes were changed. Nothing is tagged
or published. The earlier API investigation remains a historical record.

## What changed

- Windows compiler lookup now passes a file URL to `createRequire`. The native
  Node probe reproduced EISDIR / lstat `C:` for a namespace-prefixed path on
  both architectures, while the file-URL form selected the correct compiler.
  Source/test identity paths also consistently use forward slashes. Rust's
  canonical containment and source-freshness checks remain unchanged.
- A compiler frontend boundary separates parsing, program creation, resolution,
  symbols and lifecycle from the existing assertion rules. Version 7.0.2 uses
  its actual native ASTs, enum values and checker. Native declaration handles
  are resolved into AST nodes; symbols retain identity through a per-session
  cache. No TypeScript 5 predicate inspects native nodes.
- Config overlays exist only in memory. Original configs are extended so paths,
  explicit ambient types and compiler defaults remain the project's. Analysis
  selects the same source/test roots as the old frontend and disables emit.
  Parsing rejects source mismatches; invalid native config fails closed.
- Queries hash the native JS client package, platform compiler, executable and
  standard libraries before/after analysis. Sessions close on success and errors.
  The existing analyzer build identity now includes all new frontend inputs.
- Both compilers are pinned in development, under different package names. The
  build invokes TypeScript 5's executable explicitly because both packages
  advertise `tsc`. No dependency is substituted in the analyzed application.

## Calibration performed so far

The native and legacy backends were run against the **same unchanged** Supergateway
source, archive and fixed historical Stryker cohort. All 100 per-mutant predictions
match, including unresolved cases. Facts match exactly across **661 sites and 82
linked tests**; there are 101 accepted observations and 504 passed assertion phases.

Both retain 84/100 agreement: 78 true kills, four false kills, six true survivals,
ten false survivals and two unresolved killed cases. Unknowns remain in the
denominator. This is a backend regression calibration, not a new mutation campaign,
not TypeScript 7 recompilation of the sample, and not global assertion accuracy.

Native ordinary-archive integration separately runs real Node and Vitest tests,
checks source/witness linkage and all 14 existing counterexamples, and exercises
pragmas, pagination and freshness. The installed npm consumer succeeds with both
5.8.3 and 7.0.2 locally. Compiler identity tests mutate isolated package copies,
never the installed development compiler.

One diagnostic comparison measured about 1.26 s for the legacy analysis/join and
2.95 s for native analysis/join. This is not a benchmark or test-time overhead:
all new native work happens during queries. No new runtime overhead number is
claimed; the existing runtime and statement/assertion probes are untouched.

## Explicit limits

- Only native **7.0.2** is enabled. It needs Node 22.12+ and its platform package.
- Legacy ts-node/V8 generated-line remapping is not supported by the native
  frontend. Use original-source evidence from normal Supercov captures.
- Native module resolution currently follows actual import/export references.
  Helper-only or ambiguous specifiers are reported, not guessed. Built-in Node
  modules having no filesystem resolution are not new frontend limitations.
- Compiler defaults really differ between versions; matching this cohort is not
  evidence that every TypeScript 7 source/config is modeled identically.
- All assertion candidates are still unverified; `assertionScore` remains null.

## Verification and next gate

- Final [legacy calibration](asserted-legacy-frontend-calibration-final-2026-09-09.json)
  and [native calibration](asserted-typescript7-calibration-final-2026-09-09.json)
  record implementation identities. Their facts SHA-256 is identical:
  `6e57807a5fe2f3c99fea1ee909c591c583b6ea7f9bace3dc943e909bd232cb89`.
  Deep equality of the full facts and all 100 predictions was checked separately.
- Local `npm run check` passed: Rust, runtime, analyzer, ordinary archive and
  installed package checks. All 53 runtime assets remain exact. Explicitly
  enabled external source parity passed for both sample codebases. Separate
  native archive integration passed all 10 checks, including 14 counterexamples.
- The Windows fix at `408a3f2` passed [all eight native targets and the complete
  release-set verifier](https://github.com/supercorp-ai/supercov/actions/runs/34377618812).
  This does not by itself verify subsequent TypeScript 7 changes.
- The updated CI gate can reuse those exact native binaries only when the run is
  successful, repository/version/platform/digests match, and the source diff
  contains only explicitly allowed post-run analyzer, check or documentation
  changes. It exercises the current installed npm package with both compilers.
  Changes outside that allowlist require rebuilding the native matrix.

Next: finish Node 22/24 compiler and installed-package gates on Linux/macOS/Windows
for the new frontend commit, then review the experimental release. No tag,
version bump, publication or merge is authorized by the build-only review request.
