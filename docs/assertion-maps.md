# Agent-authored assertion maps

Run the normal test command through Supercov first. It collects structural
coverage and freezes source inputs. An external coding agent can then edit one
run-owned `assertions.json`; Supercov has no embedded model and does not infer or
reconstruct its semantic edges. The same Rust implementation serves the npm,
Cargo, Python and Ruby distributions.

```sh
supercov -- npm test
supercov runs latest assertions init --json
supercov runs latest assertions inventory --limit 20 --json
supercov runs latest assertions source --file src/example.ts --limit 100 --json
# Edit the returned map path, then acknowledge the reviewed work:
supercov runs latest assertions review --all
supercov runs latest assertions
supercov runs latest
```

Pin a concrete run ID while editing. The map lives at
`.supercov/runs/<run>/assertions.json`; its containing directory identifies the
run. `assertions.state.json` holds fingerprints and review state. Neither file
replaces the immutable archive or MC/DC evidence. Source is stored once in the
compressed archive's `assertion-inputs.json`, not once per flow. Old runs that
lack this input snapshot need one regular test run with this version.

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

`countsAsAsserted` explicitly names credited nodes. Anchor the complete statement
being claimed, not an arbitrary enclosing file. A flow can optionally restrict
its run evidence with `appliesTo`, an array of exact test names. Omitting it uses
all passing tests that observed the specific assertion. Case names are reusable;
phase/event IDs from a previous run are not carried forward.

`watch` records the broader context the agent relied on: guards, setup, helpers,
the containing test and alternate branches. It accepts whole files, or
`{"kind":"span","at":{...}}` for exact reviewed function/test spans. These
inputs are essential for detecting edits outside the small credited expression.
Paths must be project-relative. Agents may register custom assertion sites
missing from the syntax inventory; they still need matching runtime identity
before they earn execution-backed credit.

## Maintaining a map after edits

```sh
supercov -- npm test
supercov runs latest assertions init --from <previous-run>
supercov runs latest assertions --view assertions --json
# Repair only affected explanations/anchors in the new map, then:
supercov runs latest assertions review --flow a_example/return-value
# After investigating and assigning any unowned changes:
supercov runs latest assertions review --ack-scope
```

Initialization is optional and explicit; it never overwrites an existing map or
state. The previous run remains intact. Unchanged, unambiguous source fragments
relocate and retain their IDs. Unique exact file renames work too. New assertions
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
multiple separated edits can produce extra scope-review work. Changed run
commands, dependencies, configuration, instrumenter, language or captured
environment digest dirty every flow. Environment values are not stored. Inputs
outside the project's discovered source/configuration scope, external services
and undeclared fixtures require the agent's attention; this is not dependency
discovery.

## What the number means

The regular summary includes `assertionCoverage` when a map exists. The dedicated
query also pages assertions, credited lines and unasserted lines:

```sh
supercov runs latest assertions --view creditedLines --json
supercov runs latest assertions --view unassertedLines --json
supercov runs latest assertions validate --json
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

## Language boundary

All storage, matching, review state, aggregation and CLI behavior are Rust.
Language adapters contribute syntax ranges and normalized run evidence:

| Language | Initial source inventory | Execution-backed assertion identity |
| --- | --- | --- |
| JS/TS | Existing Rust/oxc recognizer for Node assertions and supported expect bindings | Exact location from assertion phases; some runner-only phases lack it |
| Rust | Standard assert/debug_assert macro spellings | Exact recorded Rust assertion locations where available |
| Python | `assert` and attribute calls beginning with `assert` | Existing aggregate test-level phases are insufficient for a specific-site claim |
| Ruby | assert/refute spellings and expectation terminal calls | Existing aggregate test-level phases are insufficient for a specific-site claim |

Inventories describe recognized syntax, including unexecuted sites; custom
wrappers, macros, dynamic calls, some awaited JS arguments and generated sources
can remain outside them. Python/Ruby call spellings are candidates, not binding
proofs. Their maps and incremental reviews work now, but exact-site runtime
adapters must supply occurrences before those claims earn score credit. Extending
an adapter does not require a new semantic analysis engine.

The removed analyzers and calibration results are preserved in local checkpoint
`57b7f9e` on `safety/assertion-experiments-2026-09-12`; research prototypes have
their own repository checkpoint `41e668c` and verified local Git bundles.
