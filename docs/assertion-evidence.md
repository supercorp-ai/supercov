# Assertion evidence (experimental JS/TS)

Supercov can analyze which source behaviors existing assertions appear to check,
using an ordinary run archive, its matching source, and the existing statement
and assertion-phase evidence. This work happens after tests. It adds no new
test-time probes, does not run mutants, and does not rewrite your tests.

This is candidate evidence, not a proof that arbitrary changes are safe.
`assertionScore` remains null. Execution-only links, test gaps and analysis limits
are different things; do not interpret a passing assertion nearby as protection.

## Requirements

Use the **npm Supercov launcher**, which supplies the installed analyzer assets.
The current JS/TS adapter is exercised with Node's test runner and Vitest.
Browser, background, merged, retried and ambiguously attributed records have
explicit limitations; other language assertion analyzers are not enabled here.

The analyzed project must provide a TypeScript compiler API, **even for a
JavaScript project**. For example, add TypeScript as a development dependency
before recording the run. The analyzer uses that project's compiler, not a
silently substituted global or bundled version. Installing it afterward changes
the dependency fingerprint, so rerun the suite. Missing/incompatible compiler APIs
produce an error. **TypeScript 5.8.3 and native 7.0.2 are tested.** Version 7.0.2
uses its own native parser/checker, not a fallback to TypeScript 5. Install its
platform-specific optional dependency too; the report hashes both the JS client
and the native compiler package, including its standard libraries. This backend
requires Node 22.12 or newer (Node 24 tested), and uses original-source evidence
from ordinary Supercov archives, not legacy ts-node/V8 generated-line coverage.
Native module resolution currently follows actual import/export references;
unresolved helper-only specifiers remain visible as compiler limitations.
Other native compiler versions are not enabled until separately calibrated.
Do not downgrade an application's compiler just to improve an assertion report.

Compiler compatibility and regression checks do not prove general correctness.
All candidates remain unverified and `assertionScore` remains null. Query-side
native compiler work adds no test-time instrumentation.

Standalone native/Python/Ruby distributions do not currently bundle this JS/TS
analyzer. Use the npm launcher, or explicitly set `SUPERCOV_PACKAGE_ROOT` to an
installed npm package directory. Normal coverage commands are unaffected.

## Run, inspect, follow evidence

```sh
npx supercov -- npm test
npx supercov runs latest asserted --limit 5
npx supercov runs latest asserted --file src/core.ts --json
npx supercov runs latest asserted --site '<site-id>' --json
```

The summary reports candidate counts and the site denominator, without claiming
a verified assertion percentage. Each ordinary site row contains its source,
candidate classification, available facts, and an `evidence.pointer` for full
details. Global tests, attempts, execution links, diagnostics and source-scope
limits are referenced under `evidence`, not repeated inside every site page.

```sh
npx supercov runs latest asserted --evidence /tests --limit 5 --json
npx supercov runs latest asserted --evidence /diagnostics --json
npx supercov runs latest asserted --evidence /sites/0/facts --json
```

Pointers use JSON Pointer syntax: escape `/` in a property name as `~1` and `~`
as `~0`. Use an empty pointer (`--evidence ''`) for the document root. Follow
returned pointers rather than constructing indices from filtered page offsets:
site pointers index the complete, stable inventory.

## Pagination and oversized records

All JSON responses stay within the normal response budget. Pages may contain
fewer than `--limit` results: **follow `pagination.nextOffset`**, not offset plus
the requested limit. `hasMore: false` marks the end.

An oversized site/hint is returned as `detailOnly: true` with an
`evidence.pointer`. Nothing has been discarded. An evidence page returns immediate
object members or array entries as `items`. Small entries contain `value`; large
ones contain `detailOnly: true` and another `pointer` to inspect. String leaves
return `text` chunks, with offsets/counts measured in Unicode scalar values,
not bytes. This allows reading one large observation or diagnostic completely.

```sh
npx supercov runs '<run-id>' asserted --evidence '<returned-pointer>' \
  --offset 0 --limit 4000 --analysis '<analysisId>' --json
```

Every page carries an `analysisId` hashing the complete derived document and its
provenance. Pass the first page's id as `--analysis` on follow-ups to reject mixed
analyses if the compiler, analyzer, or derived results change. Use a fixed run id
instead of `latest` while paging. Source/run freshness checks still run on every
query. Derived results are not cached or written into the archive.

The experimental report shape is `reportSchema: 2`; global evidence arrays from
the earlier development view have moved to the evidence pointers above.

## Optional assertion hints

```ts
// observes: src/core.ts#compute return value
assert.equal(compute(), 4);
```

The comment must precede a statement containing exactly one recognized assertion.
The project-relative file, exact function/owner name, and optional literal source
substring must select one inventory site. Optional `via ...` is explanation only.

```sh
npx supercov runs latest asserted --pragmas --json
```

Origin is `user-suggested`; validation is separately `analyzer-supported`,
`unresolved`, or `invalid`. Support requires the named assertion's exact passing
witness and a connection supported by the ordinary effect rules. Strength remains
presence/value/total; presence does not mean value protection. Unsupported
decision, absence and internal-state paths remain unresolved. A missing inventory
match may reflect unsupported source rather than a bad declaration.

Hints cannot inject observations, borrow another assertion's evidence, inflate
coverage or remove sites from the denominator. Editing comments changes test
source and requires a new run. Supported hints retain the analyzer's limitations;
they are not formal proofs.

## Agent workflow

Read coverage first, then inspect assertion candidates and their evidence before
writing another test. An execution gap may need a reachable scenario; an assertion
gap may need a stronger check. An analysis limit needs investigation—not a test
written just to satisfy the analyzer. Keep the original denominator and unresolved
work visible. Never automatically treat an `evident` candidate as permission to
change application behavior.
