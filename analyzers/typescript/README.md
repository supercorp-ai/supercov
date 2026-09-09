# TypeScript assertion facts

This is the extracted TypeScript analyzer for Supercov's asserted-coverage
prototype. It reads source, a site inventory and per-test evidence, and emits
JSON facts for `supercov_engine::asserted_coverage::join`. It does not run tests,
mutate source, read a mutation report, or decide whether a site is asserted.

This package is an internal engineering milestone, not a published feature or
a proof of mutation equivalence. Rule revision `source-linked-v3/archive-3`
preserves typed rejected-witness provenance through the join. It retains the
three unsafe-inference removals introduced in v2. Other source-flow rules remain
unverified.

## Ordinary-archive query (experimental)

The public query accepts an ordinary run, through a built checkout or npm package:

```sh
supercov runs <run-id> asserted --json
supercov runs <run-id> asserted --file src/core.mjs --json
supercov runs <run-id> asserted --site <site-id> --json
supercov runs <run-id> asserted --pragmas --json
```

Run these in the project that produced the archive, with its TypeScript compiler
API installed. First build this analyzer (`npm ci --ignore-scripts && npm run build`)
and the CLI (`cargo build -p supercov` from the repository root). No converted
input files or prototype checkout are needed. The old API below remains for
parity work. The new adapter supplies original-source evidence in memory.

The npm artifact now includes this analyzer and its checked source/build identity.
It has not yet been released. Standalone native/Python/Ruby packages do not bundle
the JS/TS analyzer. Use the npm launcher or an explicit installed package root.
See [the public guide](../../docs/assertion-evidence.md) for report-schema-2
evidence pointers, adaptive pagination, oversized records, and `--analysis` pins.
The query rejects stale run fingerprints and mismatched analyzer protocols or
builds, and records analyzer/compiler hashes. It does not cache derived results.
Both Vitest and node:test now have measured end-to-end archive/query tests.
Node server journals preserve statement ids and join only under the exact full
attempt scope. Browser, background, ambiguous and retry/merged-run support remain
explicit limits. Existing runtime probes are unchanged.

Every `candidate` verdict is labeled `analysisCertainty: "unverified"`. Runtime
links mean execution only. `assertionScore` is null; prototype parity is not
semantic validation. Adversarial regressions now reject credit for discarded or
overwritten values, deceptive helper names, ignored callbacks, caught failures
and assertions that never executed. Observations require an exact successful
assertion call witness; execution alone never supplies a value observation.
Rejected observations remain separate as `tests[].witnessIssues`; affected
candidates expose their provenance under the same field. Missing or inconclusive
evidence can produce `limit:assertion-witness`, never value credit. A recorded
failure remains distinct from absent capture, and absence of a call record does
not prove non-execution. See [the public limitations and calibration notes](../../docs/assertion-evidence.md).

## Build and use

From this directory:

```sh
npm ci --ignore-scripts
npm run build
node bin/analyze.mjs --config analysis.json --output facts.json
```

`analysis.json` is an invocation configuration, not a new contract format in
the application being tested:

```json
{
  "projectRoot": "/path/to/project",
  "inputDirectory": "/path/to/converted-evidence",
  "sourceDir": "app",
  "testDir": "tests/unit",
  "tsconfig": "tsconfig.json",
  "coverageRunner": "vitest"
}
```

`projectRoot` and `inputDirectory` are resolved relative to this configuration
file. Source/test directories and the tsconfig path are relative to the project.
Omit `sourceDir` to use the project's tsconfig file list, as the reference
Supergateway invocation does. An explicit `sourceDir` selects the prototype's
source/test directory walker. `coverageRunner: "vitest"` means source-position
coverage; the default preserves the legacy node:test V8 line translation.

Input layout:

```text
inventory.json
cov/index.json
cov/<test-id>.lcov
cov/<test-id>.outcomes.json     optional
cov/<test-id>.setup.lcov        optional
cov/<test-id>.phases.json       optional
cov/<test-id>.statements.json   optional
```

These remain the legacy prototype/evidence-converter inputs. The new ordinary
archive query instead uses Rust's archive reader and effect inventory with an
in-memory adapter; it does not require this on-disk layout.

The CLI writes JSON facts to stdout when `--output` is omitted and diagnostics
to stderr. It refuses to overwrite an existing output file. It does not modify
inputs or create files in the analyzed repository.

Programmatic use after building:

```js
import { analyze } from "./dist/analyze.js";

const { facts, diagnostics } = analyze({
  projectRoot: "/path/to/project",
  inputDirectory: "/path/to/converted-evidence",
  sourceDir: "app",
  testDir: "tests/unit",
  coverageRunner: "vitest",
});
```

All analysis state belongs to this call. Prototype environment variables are
not consulted. The compiler API is loaded from the project being analyzed;
callers may explicitly supply `typescript` as a compiler-API object. Failure to
load a compiler is an error, never a silent substitution with another version.
The build compiler is explicitly pinned to TypeScript 5.8.3. Projects using
TypeScript 7.0.2 select a separately calibrated native frontend: native syntax
predicates and enum values, resolved symbol handles, an in-memory config overlay,
and a compiler process closed after analysis (including failure). The compiler
package, native binary and standard libraries participate in provenance checks.
No compiler is substituted. Native analysis needs Node 22.12+ and original-source
evidence; legacy generated-line remapping is rejected. Module specifiers without
a resolvable actual import/export reference remain explicit limitations.
Other native versions are not enabled until separately calibrated.

## Facts, not verdicts

The existing schema-1 vocabulary is retained. Two extraction changes remove
dependencies on previously computed verdicts:

- `decision.earlyExitDownstream` is emitted whenever the source shape and
  outcome evidence apply. The prototype omits it if a branch is already strongly
  observed. The engine already checks that condition before using a witness.
- A timer-cancellation derivation carries optional `requiresTotal: [siteId]`.
  It is a candidate, active only if one listed callback site has total evidence.
  The engine owns that test. Missing means an unconditional legacy dependency;
  an empty list means the condition cannot be satisfied.

The companion engine update is required for `requiresTotal`: older engine
versions ignore unknown JSON fields and would misinterpret these candidates as
unconditional. The experimental public query enforces the rule/capability
handshake, including `assertion-witness-issues-v1` for rejected-witness provenance;
packaging includes the checked analyzer and tests it from a clean npm install.

The recursive flow cache is deliberately populated in the same effect-first
order as the prototype. Changing that order changes bounded flow results;
redesigning the cache is not part of this extraction.

## Assertion hints (experimental)

A leading comment on one assertion's expression statement can suggest a link:

```ts
// observes: src/core.ts#compute return value
assert.equal(compute(), 4);
```

Syntax: `// observes: <project-relative-file>#<function> [source snippet] [via explanation]`.
Use forward slashes. The exact function/owner name and optional literal source
substring must identify **one inventory site**, not an entire function body.
Use the normal `asserted --file ... --json` view to inspect available sites.
Missing, ambiguous, outside-project, malformed and unattached targets receive
diagnostics. A statement with multiple recognized assertions is ambiguous.
`via` is retained as explanatory text; it is not executed or trusted as proof.

Inspect hints with `asserted --pragmas`, optionally `--json`, `--file`, `--site`,
`--offset`, and `--limit`. This separately paged view does not repeat global
execution evidence. Its summary covers all hints; pagination covers the filtered
selection. Site/file filters apply to suggested production targets; omit filters
to see malformed or missing-target hints.

Each result separates `origin: "user-suggested"` from `validation`:

- `analyzer-supported`: the **named assertion** has an exact passing witness
  and the ordinary effect rules support this link. `strength` remains presence,
  value, or total; supported presence is not value protection.
- `unresolved`: insufficient witness or connection evidence, including effects
  not reached in that test, targets absent from the inventory, and decision/internal-derivation
  paths this bounded validator does not handle. A missing inventory match can
  mean unsupported source, not a bad declaration. Missing support is not proof
  the claim is false.
- `invalid`: malformed/ambiguous target or attachment.

Pragmas are separate from observations: they cannot inject a boundary, borrow
another assertion's evidence, add assertion credit, or remove a site from the
denominator. Passing hints expose the selected assertion's observations. They
retain the normal analyzer's limitations, not a formal proof of arbitrary-change
safety. Source edits (including comments) invalidate the old run normally.
Everything runs after tests using existing evidence; no new probes or mutation
runs are introduced. The `assertion-hints-v1` capability gates the new transport.

## Verification

```sh
npm test
SUPERCOV_ASSERTED_PROTOTYPE=/path/to/asserted-coverage-prototype \
SUPERCOV_ASSERTED_SECOND_PROJECT=/path/to/essential-seo \
  npm run test:parity
```

Local tests exercise the public API and CLI using ordinary source/test fixtures.
Their evidence files are controlled parser fixtures, not measured application
coverage. External comparison regenerates the prototype in temporary directories
and requires unchanged site inventory, coverage and test identities. Reviewed v2
observation deltas are regression snapshots, not soundness oracles. Every retained
observation must have a matching successful phase. The strict old fact-comparison
helper remains separately tested, but current analyzer output is deliberately
NOT identical to the unsafe prototype.

Engine-port parity still feeds original prototype facts into the Rust join and
compares complete site sets, statuses, strengths, reason kinds, covering-test
counts, assertion-bearing test sets and decision flags. Current-analyzer candidate
summaries are reported separately. Neither comparison changes the sample source,
reruns mutation testing, or establishes semantic soundness. The opt-in ordinary
archive integration separately executes 14 concrete variants under native Node
and Vitest. JS-ASSERT-005's former TODO now passes; other model limitations remain.

See [the public guide](../../docs/assertion-evidence.md) for scope, calibration
and remaining limitations. Repository-only historical notes are not shipped.
