# Agent-authored assertion maps

Run the normal test command through Supercov first. It collects structural
coverage, freezes source inputs and creates an assertion map. An external coding agent can then edit one
run-owned `assertions.json`; Supercov has no embedded model and does not infer or
reconstruct its semantic edges. This release work targets JavaScript and TypeScript. Format validation,
review tracking and reporting all run in Rust.

```sh
supercov -- npm test
supercov runs --json
# Choose a concrete run ID from the response:
supercov runs <run> assertions --json
supercov runs <run> assertions source --file src/example.ts --limit 100 --json
# Edit the map path returned as data.map:
supercov runs <run> assertions validate --json
supercov runs <run> assertions review --all --json
supercov runs <run> assertions check --require-complete --require-observed --json
supercov runs <run> assertions
```

Read `supercov docs assertion-agent` for a complete single-agent workflow.


Pin a concrete run ID while editing. The map lives at
`.supercov/runs/<run>/assertions.json`; its containing directory identifies the
run. `assertions.state.json` holds fingerprints and review state. Neither file
replaces the immutable archive or MC/DC evidence. Source is stored once in the
compressed archive's `assertion-inputs.json`, not once per flow. Old runs that
lack this input snapshot need one regular test run with this version.

## Where the percentage appears

`supercov runs <run>` (or `runs latest`) automatically shows an **Assertions**
row beside Lines, Branches and MC/DC. New normal runs always have
`assertions.json`; a fresh unmapped map starts at 0% when statements are measured.
For example, a reviewed map crediting 32 of 40 measured statements displays:

```text
  Assertions 80.00% (32/40) — agent-assessed statements, whole archived run
```

The numerator is the union of explicitly credited measured statements, so
multiple assertions checking the same statement do not inflate the score.
Unmapped, dirty or execution-ineligible flows do not count. The denominator is
all measured statements, including statements no test reached. A run with no
measured statements shows `n/a`, never 100%. Invalid JSON shows an unavailable
message. Incomplete maps and pending review are visible below the row.

JSON consumers read
`data.assertionCoverage.summary.statements.{asserted,total,percentage}`.
`revision` changes with map, review state or evidence. The detailed
`runs <run> assertions` command exposes the same summary and pageable statement
and assertion views. All assertion scores describe the whole archived run,
independently of structural query filters.

The CLI recalculates from local map, state and archived evidence on each query.
It does not rerun tests or invoke a model. This keeps edits visible immediately;
there is no saved percentage to regenerate. Query time scales with archive and
map size. The existing stale-source warning still applies to old runs; use
`assertions check` to gate development against the current checkout.

## Optional inspection commands

The normal workflow needs no inventory-building command. The automatically
created map already lists the recognized assertion sites.

- `assertions inventory`: the raw list of assertion expressions recognized in
  the saved test source, with exact locations and operation names. Use it to
  compare the authored map against discovery; it does not trace flows or imply
  the assertion executed. `report --view assertions` adds mapping/review status
  and passing execution witnesses.
- `assertions source --file src/example.ts`: numbered lines from the source
  archived with this run, even if that file has since changed or been deleted.
  This lets the agent trace the exact code that ran. `--offset` is zero-based.
- `assertions files`: the available archived paths and their byte sizes.

These read-only helpers are optional. The assertion map remains an ordinary JSON
file that the agent edits directly.

## File format

This example shows a return-value observation. Every `text` must match that
run's source exactly; locations use **one-based lines and one-based UTF-8 byte
columns in all languages**. `source` and `inventory` queries read archived
inputs, even if the checkout has since changed.

```json
{
  "schemaVersion": 1,
  "assertions": [{
    "id": "a_example",
    "at": {"file": "tests/core.test.js", "line": 5, "column": 3, "text": "assert.equal(value(), 1)"},
    "analysis": "mapped",
    "observes": ["value returns one"],
    "flows": [{
      "id": "return-value",
      "explanation": "The returned integer is compared to one by this assertion.",
      "nodes": [{
        "id": "return",
        "at": {"file": "src/core.js", "line": 2, "column": 3, "text": "return 1;"}
      }],
      "edges": [],
      "countsAsAsserted": ["return"],
      "watch": [
        {"kind": "file", "file": "src/core.js"},
        {"kind": "file", "file": "tests/core.test.js"}
      ]
    }]
  }]
}
```

`schemaVersion` defaults to 1. Assertion and flow IDs are persistent names, not
line numbers; IDs use letters, digits, `_`, `-` or `.`. `analysis` is `unmapped`,
`partial` or `mapped`. Keep partial investigations partial. Nodes can have `role`
and `meaning` text; edges use `{from, to, kind, basis?}` with agent-defined labels.
They explain the reasoning, and do not automatically earn coverage credit.

`countsAsAsserted` explicitly names credited nodes. Use the exact complete anchor from the `statements` view. Each counted node
credits only that measured statement; a guard or block does not implicitly
credit its nested body. Add explicit nodes for any nested statements you claim. A flow can optionally restrict
its run evidence with `appliesTo`, an array of exact test names. Omitting it uses
all passing tests that observed the specific assertion. Case names are reusable;
phase/event IDs from a previous run are not carried forward.

`watch` records the broader context the agent relied on: guards, setup, helpers,
the containing test and alternate branches. It accepts whole files, or
`{"kind":"span","at":{...}}` for exact reviewed function/test spans. These
inputs are essential for detecting edits outside the small credited expression.
Paths must be project-relative with `/` separators and no `.`/`..` components.
Custom sites can be documented, but JS/TS credit requires an exact inventoried
assertion expression and operation that matches a passing runtime occurrence.

## Maintaining a map after edits

```sh
supercov -- npm test
supercov runs <run> assertions --view assertions --json
# Repair only affected explanations/anchors in the new map, then:
supercov runs <run> assertions review --flow a_example/return-value
# After investigating and assigning any unowned changes:
supercov runs <run> assertions review --ack-scope
```

Map creation is automatic, including for failed test runs. The map and its
Rust-owned review state are published together with the run, so an interrupted
publication cannot expose a half-created map. Authoring the semantic flows
remains optional; there is no `assertions init` command.

Supercov selects the newest available map in this project's run store with the
same test command (exact argument list) and language. A focused test command
keeps a separate history from the full suite. Without an earlier compatible map,
all recognized assertions start unmapped. A mapless older run is skipped. The
previous run remains intact; new runs never modify its map or state. Finish
reviewing the newest map before rerunning; later edits to older maps are not
automatically merged into the newest history.

`data.inheritance.from` identifies the selected run, or is null for a fresh map.
If a newer candidate is malformed or has invalid state/evidence binding,
Supercov tries an older one and records the errors in `data.inheritance.skipped`.
Any inherited flows then require explicit review before credit can return.
Existing dirty flags remain latched. If no candidate can be read, the new map
starts unmapped and earlier files stay available for manual recovery.

Only the map's reasoning and review metadata carry forward. Assertion
occurrences and statement execution always come from the new run's evidence;
a failed run never inherits a passing score. Merged runs have multiple source
snapshots and are excluded; investigate their individual runs instead.

Unchanged, unambiguous source fragments relocate and retain their IDs. Unique exact file renames work too. New assertions
start unmapped. A sole changed/replacement assertion in a file retains its ID and
explanation as a dirty review suggestion. Deleted or ambiguously matched assertions are retained
with their full explanations in `retiredAssertions`; the agent can reuse those
explanations and IDs when resolving the new inventory.

Changed watched inputs mark the affected flows dirty. Dirty state persists
through subsequent runs and reversions until an explicit review acknowledgement.
Editing the map also requires acknowledgement. Reviewing one flow does not
clear its siblings. A failed reference check leaves the review state unchanged.

Unassigned source changes enter `scopeReview`, which blocks score credit until
the agent has classified them. Span comparison is deliberately conservative:
multiple separated edits can produce extra scope-review work. Changed
dependencies, configuration, instrumenter or captured environment digest dirty
every inherited flow. A different command or language starts a
separate map history. Environment values are not stored. Inputs
outside the project's discovered source/configuration scope, external services
and undeclared fixtures require the agent's attention; this is not dependency
discovery.

## What the number means

The regular summary includes `assertionCoverage` when a map exists. The dedicated
query also pages assertions, exact statement anchors, test names, credited lines
and unasserted lines:

```sh
supercov runs <run> assertions --view statements --file src/example.ts --limit 100 --json
supercov runs <run> assertions --view tests --json
supercov runs <run> assertions --view creditedLines --json
supercov runs <run> assertions --view unassertedLines --json
supercov runs <run> assertions validate --json
```

The primary score is **agent-assessed asserted statements / measured statements**
for the archived run, separate from structural coverage. Only explicitly credited, current flows count. They
must have an exact passing assertion site and same-test statement execution in
the current run's successful attempts. Failed runs, unreviewed flows, invalid
references and unresolved scope changes cannot add credit.

The additional line view is conservative: its numerator
requires every statement point on a line to be credited; the denominator is the
run's measured executable lines. Function-entry-only lines receive no statement
credit in this first version. `declared` includes syntactically valid authored
claims even when runtime evidence or review is missing; `asserted` is the stricter
count. A zero denominator yields a null percentage. The primary statement
percentage counts measured statements, independently of function-entry points.
`workingTree.stale` reports changes since the archived run; an old score never
becomes evidence for changed code. Structural query filters do not silently
change this whole-run assertion metric.

Absence checks can describe a prevented call or empty collection in the graph.
An unexecuted statement itself receives no execution-backed line credit; the
agent can instead credit the executed guard/statement whose behavior the check
observes. A flow may explain an absence without crediting any line.

`inventoryMappingComplete` means every recognized assertion has a mapped,
current entry. It is not proof that every possible assertion form or dependency
was recognized. No percentage guarantees mutation detection or safe changes:
different implementations can satisfy the same assertion. `validate` checks
format, IDs, edges and source anchors, not semantic correctness. A future sampled
mutation audit can calibrate the agent's claims separately.

## Syntax, editor support and CI

```sh
supercov assertions schema > assertions.schema.json
supercov assertions validate --file .supercov/runs/<run>/assertions.json --json
supercov runs <run> assertions validate --json
supercov runs <run> assertions check --require-complete --require-observed --min 60 --json
```

The schema is generated from Rust's deserialization types using
[Schemars](https://docs.rs/schemars/latest/schemars/), and also ships in the npm
package at `schemas/assertions.schema.json`. CI checks the published copy against
the compiled CLI. Schema output is raw JSON by default; `--json` wraps it in the
normal response envelope. Associate the schema with `**/assertions.json` in your
editor's JSON settings; `$schema` is not a map field.

| Command | What it verifies | What it changes |
| --- | --- | --- |
| `assertions validate --file` | JSON syntax, types, required/unknown/duplicate fields, supported schema version | Nothing |
| `runs <run> assertions validate` | Above, plus IDs, node/edge links, frozen anchors and state binding | Nothing |
| `runs <run> assertions review` | Selected references are valid; records your review of the explanation | Review state |
| `runs <run> assertions check` | Valid references/state, passed run, current flows, no scope queue, current working tree | Nothing |

Both validation commands work before flow review. Empty/unmapped flows can be
valid JSON. `check --require-complete` additionally requires a fully mapped
recognized inventory with no parse failures. `--require-observed` additionally
requires a passing occurrence for every map entry and every `appliesTo` filter.
`--min N` requires at least N percent asserted statements; it fails on a null
percentage. Basic `check` supports incremental work and does not demand all
assertions be mapped. `--archived` explicitly checks an old run without requiring
today's working tree to match. All gates use exit 0 for success, 2 for unmet
requirements or invalid input. Reporting alone never acts as a CI gate.

Malformed JSON returns a diagnostic such as
`/assertions/0/flows/1/countsAsAsserted`, with its JSON line and column. These are
file syntax locations, separate from an anchor's source coordinates. Run-level
reference errors identify assertion/flow IDs. Flow report rows provide
`blockers`, `reasons`, `matchingTests` and credited statement lines. Statement
rows provide the complete source `at`, `declared`, `asserted` and crediting flow
IDs. `at: null` means the frozen source could not resolve that measured statement;
it stays in the denominator and cannot earn credit.

Use `assertions files` to list frozen input paths. `inventory --file <path>` and
`report --view assertions|statements|creditedLines|unassertedLines --file <path>`
filter items; summary metrics remain whole-run. Array views use
`--offset`/`--limit`, with `pagination.nextOffset`. Do not use
pagination on a summary. `revision` identifies the map/review/evidence version
for report pages; restart a paged read if it changes. Mutating a map while another
process reviews it is unsupported: use one writer per run. A review never edits
the map and a later map edit invalidates the recorded fingerprint.

## JavaScript and TypeScript boundaries

The automated assertion-map matrix exercises Node ESM and CommonJS, Node's native
TypeScript loading, Vitest with TypeScript, Jest with CommonJS named `expect`
imports, and Playwright with TypeScript. It checks exact Unicode locations,
values calculated before assertions, parameterized test names, promise rejection matchers, statement
credit, syntax errors, review, thresholds and checkout freshness. The Playwright
map fixture uses Node-side tests; browser transport and bundled application
source maps have their own structural coverage gates and need representative
assertion-map trials before broader claims.

Node `assert` bindings and supported Jest/Vitest/Playwright `expect` bindings use
the same Rust syntax recognizer for inventory and instrumentation. Discovered
Playwright fixture modules are included. Lexical binding recognition distinguishes
an imported assertion from a shadowed local function; it does not trace values.

Known boundaries remain visible:

- Awaited operands such as `assert.equal(await value(), 1)` and
  `expect(await value()).toBe(1)` preserve their original evaluation position.
  Supercov binds the matcher/receiver and opens its phase when the matcher is
  called. No extra `await` is inserted; rejected operands do not create a passing
  assertion. Async rejection matchers also have phases.
- Optional assertion calls are inventoried but are not directly wrapped, because
  a short-circuited call must not manufacture a passing occurrence. Runner/native
  adapters may still supply identity for calls that actually execute. Nested
  assertions can lack separate occurrences while an outer assertion phase is
  active. Inspect `observedPassingTests` and use the evidence gate.
- Skipped, unreached or expected-failing assertions have no passing witness.
  `inventoryMappingComplete` can be true while `--require-observed` fails.
- Custom wrappers, dynamically selected matchers, aliases not supported by the
  recognizer and generated tests may be absent from inventory. Completeness
  applies only to recognized syntax, never every conceivable assertion.
- Transformed or generated statement locations that cannot be resolved against
  frozen inputs stay uncredited. Inspect `unanchoredStatements` and statement rows.
- Assertions and statements must share a successful test identity, but the tool
  does not reconstruct an exact dynamic slice or distinguish every loop iteration.
  The agent supplies that causal judgment. Test-level co-execution alone is not
  proof that the statement affected the checked value.

Other language adapters do not change this JS/TS contract and are outside this
release validation scope. Exact-site support can be extended later without
introducing a semantic static analysis engine.
