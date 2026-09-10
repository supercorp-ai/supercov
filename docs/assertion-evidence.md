# Assertion evidence (JS/TS)

Supercov can analyze which source behaviors existing assertions appear to check,
using an ordinary run archive, its matching source, and the existing statement
and assertion-phase evidence. This work happens after tests. It adds no new
test-time probes, does not run mutants, and does not rewrite your tests.

This is candidate evidence, not a proof that arbitrary changes are safe.
`assertionScore` remains null. Execution-only links, test gaps and analysis limits
are different things; do not interpret a passing assertion nearby as protection.

An unsupported operand in a passing assertion can leave its relationship to
code covered by that test unknown. Such sites report an operand-shape limit,
not proof that an assertion is absent. This does not give those sites assertion
credit or hide known execution gaps.

Supported Node console-mock observations preserve the assertion's source projection:
a call count, an argument, the whole history, or a slice or map. Their `mock` details
retain the receiver and access path. Predicate strength applies to that projected
value: equality of a count is not equality of the logged arguments. These records
do not supply production-site credit or validate an observation hint until the
mock lifetime, selected calls and source-site dependence are established. This
remains separate from establishing that the count itself was checked.

For a bounded synchronous subset, `mock.countEvidence` now models the exact mock
creation, reset boundary, read location and participating calls. It distinguishes
the installed method from saved mock references, restores a previous instance,
and preserves copied counts and call-history snapshots across later calls or
resets. Instance locations are scoped to their owning test record. `calls` are
reconstructed from checked source, not a new runtime call trace. The modeled
count must match an independent integer comparison with a passing witness.

`source-checked` means accepted by the versioned `node-sync-console-count-v2`
model, not formally verified application behavior. The model assumes unmodified
native Node assertion/mock APIs, standard JavaScript built-ins (including array
iteration) and the standard global console. It accepts empty console-mock
replacements and a bounded synchronous source subset: constant bindings,
fresh plain objects/arrays, returned closures, primitive-input production branches,
own-property parameter binding, defaults, rest parameters and fresh-array spreads.
Module factories must be source-checked as pure: setup side effects are never
replayed under the test's mock. Shared module objects remain unresolved because
`const` does not rule out mutation by earlier callers. Opaque calls, escaped
mocks, getters, missing own properties, mutable bindings, test branching,
unsupported initialization and async suspension leave explicit reasons. Evidence
inside a rejected assertion witness is not a passing observation.

For a closed top-level `for (const { ... } of cases)` table using native
`node:test`, `countEvidence.rowBinding` can retain the selected source row and
its scalar bindings. The table must be a private literal with no aliases or
other reads/writes. Supercov reproduces each registration title from the source
values and requires a unique exact match to the archived title; it does not
guess from title prefixes or record order. The existing original-source passing
assertion witness is still required. Duplicate titles, transformed titles,
mutable/escaping tables, custom registration helpers and unsupported loops stay
unresolved. A source-checked row binding alone grants no count or value credit;
for example, the producer's shared-object history may still be unknown.

Count evidence does not grant general site, payload or pragma credit, even for
listed calls: an aggregate number does not pin each call's arguments or rule out
compensating changes. Such value relationships remain operand-shape limits.
This analysis is post-run and requires no new probes, pragmas or contract files.

Native Node equality observations also retain both operand source locations and
their comparison relation. Comparing a `const` value with itself, or with an
immutable alias, checks no property of that returned value. Such a comparison
cannot supply value credit or validate a hint; an independent assertion still
can. Node's strict equality uses `Object.is`, so even `NaN` compared with itself
passes. Repeated calls, property reads, mutable aliases and `await` are not
assumed to be identical evaluations. This bounded source check needs no pragma,
contract file or additional runtime probe; it is not general relational analysis.

For native equality comparisons, bounded source analysis also follows `const`
aliases and `await`. If both operands lead to the same stable input through an
await, `comparison.relation` is `shared-input-through-await`; each operand's
`input` records the binding and await source locations. This does **not** mean
the resulting values are equal or the predicate is a tautology: a Promise differs
from its resolved value, and a stateful thenable can resolve differently on two
awaits. The passing witness remains visible, but this comparison alone cannot
supply producer-value, absence, control-flow or pragma credit. A relevant site
reports `limit:predicate-dependence` unless a more specific existing limitation
or known execution gap applies. Independent assertions still contribute normally.
The traversal is bounded, does not equate calls/getters or follow mutable aliases,
and does not infer independence when no shared input was found. It runs only
during querying; the test-time instrumentation is unchanged.

For a narrow direct-call shape, `decision.primitive` retains literal branch
results and the witnessed native strict equality/inequality predicates. The
`js-primitive-decision-v1` model checks whether forcing either outcome would
still satisfy each modeled assertion. Both branches returning the same value,
or different values both accepted by `notEqual`, do not establish decision
sensitivity. Numeric values retain signed zero and remain distinct from strings.
The model requires synchronous, literal-only branches and direct boolean-input
calls in simple Node tests; it rejects transformations, effects and mutable
function bindings. It assumes normal module loading and unmodified native
assertion semantics. Unsupported shapes retain the existing candidate analysis,
not a sensitivity proof. These query-time checks add no probes, and their
bounded results remain unverified candidates, not a global assertion score.

Child-exit observations retain `processExit` source evidence instead of assuming
that every Promise containing an exit listener checks the covered process. The
bounded `node-child-exit-source-v1` model follows const projections and simple
local/imported function returns by declaration identity. It records helper call
sites, the native spawn call, Promise, exit/close listener and selected field.
An accepted `resolution` describes which event argument the callback passes to
the resolver, directly or in a fresh plain object's field (`code` or `signal`).
This is a source-checked resolver mapping under unmodified native Node APIs and
global Promise semantics, not a checked end-to-end assertion link. Calls from
project-defined Promise constructors, transformed fields, competing settlements,
non-child emitters and unsupported shapes retain explicit reasons.

The optional `consumer` check follows the selected read's local const bindings
and checks their other uses, including nested closures. It can establish that
the fresh resolver field reaches this read unchanged under the model's
unmodified built-in/prototype assumptions. Aliases that escape, extra Promise
consumers, mutations, reflective access and nonlocal result carriers remain
unresolved. Simple discarded awaits (including native `Promise.race`) and
independent arrow readers on a fresh helper return are supported. The traversal
is bounded and happens after the test run. A source-checked consumer is not a
claim that the assertion distinguishes every change, or that the event came
from the covered producer.

In particular, a different child can run the same source. `processExit.status` remains
`unresolved`; even a source-checked `resolution` supplies no production-value,
absence, control-flow or pragma credit. Relevant candidates report
`limit:process-exit-link` unless a more specific existing limitation or known
execution gap applies. Assertion witnesses remain visible. A bare `exit` channel
without these source details is not an escape hatch for credit. This source
analysis adds no test-time probes and makes no claim about complete pipe capture.

Fallback native assertion locations prefixed `runtime-stack:` identify runtime
stack coordinates, which a compiler or loader may have shifted. They are not
original-source witnesses and cannot select a static test or validate a hint.
Lexical assertion markers retain their exact original-source locations.

For statically identified native Node assertions with awaited arguments, Supercov
binds that source location to the call before evaluating the arguments. It
records the phase only when the assertion is invoked: a rejected operand creates
no assertion witness, and a caught assertion failure remains a failed witness.
Earlier async work is not attributed to that invocation phase. This preserves
the original `await` expressions without relying on a shared current-statement
marker. Optional calls, generators, matcher calls with awaited operands, constructed
`Assert` instances and awaited arguments to `rejects`/`doesNotReject` still use
qualified runtime-stack fallback where the runner supports it. Exact source
identity alone does not establish an operand's relationship to production code.

## Requirements

Use the **npm Supercov launcher**, which supplies the installed analyzer assets.
The current JS/TS adapter is exercised with Node's test runner and Vitest.
Browser, background, merged, retried and ambiguously attributed records have
explicit limitations; other language assertion analyzers are not enabled here.

Assertion analysis requires a source file for every accepted passing test attempt.
If the runner or a custom stack formatter prevents source attribution, the query
fails rather than treating omitted tests as execution gaps. Restore attribution
and recapture the suite; ordinary coverage queries remain available.

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

`assertions` analyzes source behaviors and their assertion evidence; it does not
just count or list assertion calls. Use `npx supercov docs assertion-evidence`
to read this guide from the installed package.

```sh
npx supercov -- npm test
npx supercov runs latest assertions --limit 5
npx supercov runs latest assertions --file src/core.ts --json
npx supercov runs latest assertions --site '<site-id>' --json
```

The summary reports candidate counts and the site denominator, without claiming
a verified assertion percentage. Each ordinary site row contains its source,
candidate classification, available facts, and an `evidence.pointer` for full
details. Global tests, attempts, execution links, diagnostics and source-scope
limits are referenced under `evidence`, not repeated inside every site page.

```sh
npx supercov runs latest assertions --evidence /tests --limit 5 --json
npx supercov runs latest assertions --evidence /diagnostics --json
npx supercov runs latest assertions --evidence /sites/0/facts --json
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
npx supercov runs '<run-id>' assertions --evidence '<returned-pointer>' \
  --offset 0 --limit 4000 --analysis '<analysisId>' --json
```

Every page carries an `analysisId` hashing the complete derived document and its
provenance. Pass the first page's id as `--analysis` on follow-ups to reject mixed
analyses if the compiler, analyzer, or derived results change. Use a fixed run id
instead of `latest` while paging. Source/run freshness checks still run on every
query. Derived results are not cached or written into the archive.

JSON reports use `reportSchema: 2`. Shared evidence is accessed through the
evidence pointers above.
Success and error JSON envelopes identify this query as `coverage.assertions`.

## Optional assertion hints

```ts
// observes: src/core.ts#compute return value
assert.equal(compute(), 4);
```

The comment must precede a statement containing exactly one recognized assertion.
The project-relative file, exact function/owner name, and optional literal source
substring must select one inventory site. Optional `via ...` is explanation only.

```sh
npx supercov runs latest assertions --pragmas --json
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
