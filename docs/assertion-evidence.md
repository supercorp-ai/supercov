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
An installed empty mock, or a saved reference to it, can also count calls carrying
source-modeled plain objects, arrays or closures without inspecting their payload.
Argument-evaluation calls still participate in the history in evaluation order.
Fresh dense arrays support source-defined synchronous `map` callbacks, including
their element, index and array parameters. Receiver and callback expressions are
evaluated before callback invocations. Own source methods named `map` or `slice`
are evaluated as those methods, not treated as native array operations. Sparse
arrays, callback mutation, opaque callbacks and a `map` this-argument remain limits.
Fresh arrays and native mock-history snapshots also support `slice` with bounded
integer indices. History slices retain `historySelections`: each selection's
source location, input count and effective half-open `[from, to)` range. Selected
calls remain tied to the original instance and snapshot across later resets;
index-argument effects occur after the snapshot is taken. An empty selection is
not evidence that the original mock had no calls. Coercing indices remain limits.
This does not pin payload fields or grant production-value credit. Unmocked
console calls with nonprimitive arguments remain unsupported because formatting
can invoke user code; getter-bearing or otherwise opaque payloads remain limits.
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
Within an accepted primitive model, `stuckTrueCaught` and `stuckFalseCaught`
describe rejection of those two forced outcomes only. A rejected or incomplete
model omits these fields: unknown is not a known non-rejection. Outside this
model, the same fields are legacy branch-observation heuristics, not checked
counterfactuals. Neither form predicts arbitrary edits such as changing a regex
anchor or a comparison boundary merely because the containing site is `evident`.

Decision candidates expose `sensitivityBasis` in JSON and as `sensitivity:` in
text output:

- `bounded-source-model`: the supported source model evaluated both forced
  outcomes against the witnessed predicates, within its stated assumptions.
- `branch-observation-heuristic`: the flags come from observed branch evidence,
  not evaluation of the alternative behavior. Do not use them as verified
  change-detection results.
- `unavailable`: forced-outcome results are not available. Missing flags are not
  `false` and must not be counted as known non-rejections.

Effect candidates omit this decision-only field. These labels describe the
analysis method, not accuracy percentages or global safety; `analysisCertainty`
stays `unverified`. Evidence pagination preserves the same labels.

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

The comment must precede a statement containing exactly one recognized assertion
or a supported directly awaited observation helper.
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

### A checked missing-call question

```ts
// observes: src/logger.ts#stdout console.log; check missing call
assert.equal(log.mock.callCount(), 1);
```

The optional `; check missing call` suffix selects a bounded source-analysis
recipe. It targets a console emission, excluding a containing callback-return
inventory entry. Existing `via` text remains explanatory, not executable guidance.
The question is precise: replace the selected expression-bodied arrow callback
with `() => undefined`. This removes its body on **every invocation**, not just
one recorded call. It says nothing about changing argument values or arbitrary
edits in the enclosing function.

The `node-first-test-call-omission-v1` model checks a closed test-file prefix and
the first synchronous native Node test (or first row of a checked registration
table). Only native test/assert imports and the selected production module may
initialize that test scope. The production module's initialization must be pure
under the existing bounded source interpreter. The supported source prefix has
no earlier registrations, hooks, async suspension, opaque operations or mutations.
This is what allows the model to read the module's initial shared objects; a
comment never disables the ordinary shared-history guard for other analysis.

The model interprets the supported prefix twice after the run, with and without
that callback body, through the selected independent integer count assertion.
The original modeled count must match the expectation and the assertion must
have its own exact passing witness. The named call must participate in the
selected history. Wrong histories, resets that erase the evidence, later rows,
unsupported source and absent witnesses stay unresolved. Source-driven helper,
receiver and variable names are not fixed by this recipe.

`--pragmas` returns `hint.callOmission` with the source locations, mock instance,
original/omitted/expected counts and an outcome of `rejected` or `not-rejected`.
A sliced history can hide an omitted call when another call fills its slot;
`not-rejected` concerns **this assertion only**, never whole-suite survival.
Support uses `validation: analyzer-supported`, with no site `strength` and no
change to ordinary candidate verdicts or the global assertion score.

These conclusions depend on normal isolated Node test-file loading and serial
registration execution, the matching compiler, unmodified native assertion/mock
APIs and built-ins, and finite ordinary execution. The source checker does not
prove those environmental assumptions or implement arbitrary JavaScript semantics.
Do not apply its result to custom preloads, non-isolated module histories or
different scheduling contracts. The existing source-freshness checks still apply.
No mutation is executed during querying and no new test-time probe is installed.
Invalid or unsupported hints only affect analysis; they never fail a test.

### A checked count/routing question

```ts
// observes: src/logger.ts#selectLogger mode === 'quiet'; check count
assert.equal(log.mock.callCount(), 1);
```

`; check count` selects a decision or a return containing a conditional expression.
The bounded `node-closed-count-sensitivity-v1` model checks three explicit changes
to the selected primitive equality: replace it with true, replace it with false,
or invert it. Each change applies on every evaluation of that expression in the
modeled prefix. This is not arbitrary-change or whole-site protection.

Unlike the first-test omission recipe, this rule can handle later source-checked
registration rows. It checks a closed synchronous test module, the sole imported
production factory, source closure origins, nonescaping receivers and enumerated
shared object allocations. Only those allocations receive permission; captured
arrays/objects do not inherit a blanket shared-state exemption. Setup effects,
hooks, native aliases, dynamic escape and unsupported calls remain unresolved.

For call counts, `util.inspect` on bounded fresh plain data can be represented as
an opaque string. Native TTY state remains opaque too. The model does not guess
formatted bytes, and an opaque comparison or unsupported earlier assertion stops
the selected variant. Earlier rejecting assertions are not credited to a later
count assertion.

The owning count assertion must have an exact passing witness and an independent
integer expectation matching its modeled original count. `hint.countSensitivity`
reports the checked source locations, allocations, mock instance, original and
expected counts, and separate `condition-true`, `condition-false` and
`condition-inverted` variants. Every variant has its own status and, when checked,
count and `rejected`/`not-rejected` outcome. Partial results retain unknown variants.

`validation: analyzer-supported` applies to these bounded results only. It never
promotes ordinary observations, site strength or a global assertion score.
`not-rejected` concerns the selected assertion, not the whole suite. The native
API/compiler/pristine-environment and isolated-loading assumptions above still
apply; this source checker is not a formally verified JavaScript semantics.
Hints are inert comments. All additional work occurs during querying, with no
new test-time probes or external user-maintained contract files.

### A checked selected-argument question

```ts
// observes: src/logger.ts#formatData args.map; check value
assert.equal(infoCalls[0].arguments[1], 'hello');

// observes: src/logger.ts#selectLogger mode === 'verbose'; check value
assert.deepEqual(infoCalls[0].arguments[2], { a: 1 });
```

`; check value` uses the same closed synchronous scope, but follows a selected
mock call and argument to its existing native strict equality or deep-strict
equality assertion. `hint.payloadSensitivity` retains the mock instance, ordered
call index, emission source, history slices, argument index, original value,
independent literal expectation and per-change outcome. A call index is relative
to the selected history; recorded slices describe how that history was obtained.

For a primitive equality target, the changes are force true, force false and
invert. For a native `map` call with an inline block-bodied arrow callback, the
single `map-callback-empty` change empties that callback body on every invocation;
its parameters and the surrounding call still evaluate. Native array origin is
checked, not inferred from a method name. This does not model arbitrary edits.

The current value domain contains primitives, fresh plain objects/dense arrays,
and opaque strings returned by the restricted native `util.inspect` summary.
An opaque string can be distinguished from an expected object without guessing
formatted bytes. Two opaque strings cannot be assumed equal, and comparing one
with a fixed string stays unresolved. Coercion, substring matching, arbitrary
transformations, identity comparisons between objects and nonliteral expected
operands remain outside this recipe. Earlier rejecting or unsupported assertions
stop a variant; they cannot provide credit to a later assertion.

The bounded interpreter can derive a history-alias relationship that the older
boundary analyzer did not recognize. Such a result does not require a legacy
boundary observation, but still requires native assertion identity, a successful
original source evaluation and the exact owning passing assertion witness.
The engine checks value/outcome consistency and variant scope; the versioned
source analyzer and documented native-environment assumptions remain trusted.
This is not a formally verified JavaScript implementation or a general assertion
score. `not-rejected` concerns only the selected predicate, not the suite.

No comments create observations or promote ordinary site strength. No application
execution, mutations, runtime value sampling or additional probes occur during
the query. Invalid hints affect analysis only; they cannot fail native tests.

### Awaited child-process capture helpers

For a bounded Node `test` pattern, a hint can attach to an awaited helper that
polls a regex over append-only UTF-8 child-process output and throws on exit or
timeout. Supercov checks the actual imports, bindings, capture callbacks,
predicate and polling loop—not a method name such as `ready`. The receiver must
be a local `const`, with one factory launch immediately followed by the awaited
call. Escaped receivers, transformed buffers, stateful regexes, caught polling
failures and unsupported control flow do not satisfy this source model.

`--pragmas` reports recognized source structure in `hint.awaitedObservation`,
including the factory, capture and predicate locations and the regex literal.
This is **source support, not a recorded read**. The result remains `unresolved`
with reason `observation-capture-unavailable` and `witness: unavailable`: current
archives do not supply a supported read receipt for this model. A passing test
or similarly named assertion phase cannot substitute for that receipt.

This query-only analysis adds no probes, changes no test result and supplies no
presence, value or whole-site assertion credit. A bad or unsupported pragma never
adds a test failure. Native API behavior and no external monkey-patching remain
model assumptions; source recognition does not prove end-to-end pipe provenance.

## Agent workflow

Read coverage first, then inspect assertion candidates and their evidence before
writing another test. An execution gap may need a reachable scenario; an assertion
gap may need a stronger check. An analysis limit needs investigation—not a test
written just to satisfy the analyzer. Keep the original denominator and unresolved
work visible. Never automatically treat an `evident` candidate as permission to
change application behavior.
