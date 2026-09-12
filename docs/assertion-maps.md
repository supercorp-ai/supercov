# Assertion maps

Each normal test run creates `assertions.json` in its run directory. Any coding
agent can edit that file to describe what specific assertions observe and which
statements contribute to those observations. Rust handles syntax, references,
input freshness, reuse and reporting. Supercov does not run an embedded model or
reconstruct the agent's semantic reasoning with a static analyzer.

## Run, investigate, save

```sh
supercov -- npm test
supercov runs latest assertions --json
# Pin the returned run ID; edit data.map using matching current project files.
supercov runs <run> assertions report --view changes --json
supercov runs <run> assertions validate --json
# After investigating, copy expectedBasis tokens into the corresponding entries.
supercov runs <run> assertions check --require-mappings --require-observed --json
supercov runs <run>
```

There is no `init` or `review` command. `validate` is read-only: it returns the
expected acknowledgements, and the agent saves them in the one editable file.
A token says that the author acknowledges these particular inputs and this
particular claim. It is not proof, a signature, or a model confidence score.

## File format, version 2

```json
{
  "schemaVersion": 2,
  "assertions": [{
    "id": "a_example",
    "at": {
      "file": "tests/value.test.ts", "line": 5, "column": 3,
      "text": "assert.equal(value(), 1)"
    },
    "observes": ["The returned number equals one."],
    "flows": [{
      "id": "return-value",
      "basis": null,
      "appliesTo": [{ "file": "tests/value.test.ts", "name": "value" }],
      "explanation": "The function's returned number reaches the equality assertion through value().",
      "nodes": [{
        "id": "return",
        "at": { "file": "src/value.ts", "line": 2, "column": 3, "text": "return 1;" }
      }],
      "edges": [{ "from": "return", "to": "$assertion", "kind": "data" }],
      "countsAsAsserted": ["return"],
      "watch": []
    }]
  }]
}
```

This is an illustrative draft; its anchors and test selector must match a real
run before it can receive credit. `basis: null` explicitly means unacknowledged.
`supercov assertions schema` exports the schema generated from the same Rust
types as the parser. The published editor schema is `schemas/assertions.schema.json`.
`supercov assertions validate --file <path> --json` checks JSON shape without a run.

| Field | Meaning |
| --- | --- |
| `id`, `at` | Persistent assertion identity and exact assertion expression. |
| `observes` | What the predicate distinguishes; an existence check does not check every field. |
| `flows` | Recorded explanations. An empty array means none are recorded; there is no known total. |
| `questions` | Optional assertion-level investigation questions. They do not establish completeness. |
| Flow `id` | Stable within its assertion; `assertion-id/flow-id` identifies the claim. |
| Flow `basis` | Null, or the opaque `scov2:<64 lowercase hex digits>` token supplied by validation. |
| `appliesTo` | Explicit project-relative test file and exact displayed test name. Empty means no execution credit. |
| `nodes`, `edges` | Exact source anchors and authored relationships ending at the reserved `$assertion` sink. |
| `countsAsAsserted` | The node IDs judged asserted. Context nodes earn no automatic credit. |
| `watch` | Additional whole-file dependencies as path strings. |
| Flow `questions` | Optional unresolved questions; any entry blocks this flow's credit. |

Anchors use project-relative `/` paths, one-based lines and one-based UTF-8 byte
columns. Preserve exact text, including multiline expressions. IDs use letters,
digits, `_`, `-`, or `.`. Edges have `from`, `to`, `kind`, and optional explanatory
`basis` text; that edge text is distinct from the flow's acknowledgement token.
Every counted node needs a path through the authored edges to `$assertion`.
The checker traverses this JSON graph only; it does not infer source dependencies.
A counted node must match one measured statement exactly to earn credit. A block
or guard does not implicitly credit its nested body.

A zero-credit explanation is useful for a fixture-only check or an absence check:
explain the observation and use `countsAsAsserted: []`. An absent event cannot
credit an unexecuted body. An executed guard can receive credit only when the
author judges that the predicate observes its behavior.

## Percentage and report states

The regular `supercov runs <run>` report reads the map on demand. Its primary
percentage is the union of eligible, explicitly counted measured statements,
divided by **all measured statements** in the run. Unexecuted and unanchored
measured statements remain in the denominator. Duplicate claims count once.
Line credit requires all measured statements on that line to be credited.

Eligibility requires valid references, current input acknowledgement, no open
flow questions, a passing run, an exact passing assertion occurrence, and
statement execution attributed to the same selected passing test. File + name
selectors must resolve unambiguously. A missing or unobserved selector contributes
nothing; an observed sibling selector can still contribute. Coexecution supports
the agent's claim but does not establish causality or temporal ordering.

| `summary.status` | Public percentage |
| --- | --- |
| `available` | Numeric, including a meaningful 0 when a current observed explanation credits nothing. |
| `notAssessed` | Null: no flow explanations have been recorded. |
| `pending` | Null: change impact remains outstanding, or no eligible current explanation exists. |
| `unavailable` | Null or an unavailable report: failed run, invalid identities, mismatching source or unreadable map. |
| `notApplicable` | Null: no measured statements exist. |

For example:

```text
Assertions 62.50% (50/80) — agent-assessed statements, whole run
  18 assertions with flows; 7 without; 29 current flows; 3 stale; 2 draft
Assertions not assessed — No recorded flow explanations
Assertions pending — Source changes need impact assessment
```

A partial map can produce a useful numeric score. Counts show assertions with
and without flows, current/draft/stale/invalid flows, unobserved assertions,
questions and pending changes. There is no `mapped` or `complete` status. The
number of missing semantic flows is unknown. `current` means the claim has no
freshness/reference/question blockers; selector evidence is reported separately.
Diagnostic statement counts remain inspectable while change impact is pending;
the public percentage stays null.

JSON regular reports expose this under `data.assertionCoverage.summary`; detailed
assertion reports use `data.summary`. `assertionCoverage.available` indicates
whether the report could be read, while `summary.status` describes whether its
percentage is available. Structural filters do not change the assertion score:
it describes the whole run, as its label says. MC/DC remains a separate metric.
`revision` binds each response to the map, managed state and run evidence.

## Resource commands

| Command | Shows |
| --- | --- |
| `runs <run> assertions` | Recognized and authored assertions, including sites missing from the map. |
| `runs <run> assertions --needs-attention` | Assertions without flows, with open questions, or with flows needing investigation/evidence. |
| `runs <run> assertion <id>` | One exact assertion and its graph, freshness, selectors and evidence. |
| `runs <run> source <path>` | Matching current source, printed as code with line numbers. |
| `runs <run> assertions files` | Input paths, byte sizes and SHA-256 hashes; works even with a stale checkout. |
| `runs <run> assertions report --view <view>` | `summary`, `assertions`, `statements`, `tests`, `changes`, `creditedLines`, or `unassertedLines`. |
| `runs <run> assertions validate --json` | Shape/reference errors, changes and flow `expectedBasis` tokens. Read-only. |
| `runs <run> assertions check` | Read-only policy gates. |

`--file <path>` filters assertion/statement/line items, never the summary.
List views support `--offset <n> --limit <1..1000>` and `--json`. Follow
`pagination.nextOffset`; restart a multi-page read if `revision` changes.
Source defaults to 20 lines and prints `34 │ code here`. Source JSON uses
`{line, text}` items for integrations. It is usable with malformed map JSON but
requires current files matching the run. There is no separate inventory command.

Large validation responses can be paged with `validate --view flows|changes|errors`
and `--offset`/`--limit`. Entries appear under `items`; overall `valid` and
`errorCount` still cover the whole map. Default validation returns all entries.

`validate` returns exit 2 for syntax/reference errors, not merely null/stale
acknowledgements. `check` returns exit 2 for invalid references, failed runs,
unacknowledged/stale/invalid flows, open questions, pending impact assessments,
or an unverifiable checkout. An incremental map with untouched empty sites is
allowed by basic `check`. Optional gates:

- `--require-mappings`: every recognized assertion observed passing has at least
  one eligible current explanation. A zero-credit explanation qualifies. This
  is not a claim that every flow is known.
- `--require-observed`: every listed assertion and every explicit selector has
  matching passing evidence. Empty selectors fail this gate.
- `--min <0..100>`: the public statement percentage must be numeric and meet the target.

## Reuse after source or test changes

Run the same test command again. Before publication, Supercov selects the newest
usable map for that exact command and language. A malformed newer candidate is
reported under `inheritance.skipped`; fallback claims require fresh acknowledgement.
Publication writes evidence, map and managed state together atomically. Previous
runs stay untouched. Finish editing the newest map before starting its successor.

The run stores a hash manifest and assertion identities, not complete source
files. Current project files supply the source for investigation. Managed
`assertions.state.json` (version 3) binds evidence and input identities and stores
invalidation generations, outstanding change records and inheritance metadata.
It contains no second semantic graph. Only `assertions.json` is agent-editable.
No query changes either file.

A flow depends on the assertion file, each selected test file, every node file,
and extra `watch` files. Any byte change in those files, including comments or
blank lines, invalidates that flow. Other sibling flows can remain current.
Changes to the assertion/observation affect all its flows. Context/dependency/
instrumenter changes invalidate inherited claims conservatively. Flow tokens
bind the claim (excluding their own token), context, sorted dependency hashes,
and effective invalidation generation; they exclude run IDs and runtime events.

Exact identities retain IDs. Unique snippets or unique identical-file renames
can relocate as suggestions. A sole unmatched old/new assertion in the same file
can retain its ID as a changed candidate. Ambiguous/removed assertions remain
under `retiredAssertions` with explanations. Unresolved changes stay stale across
reruns and reverts until acknowledged. Every new run supplies fresh evidence.

Every added, edited or removed file in the captured analysis scope enters the
change queue, even if a flow already watches it. Known dependencies are a starting
point, not proof that other flows are unaffected. Read `--view changes` and edit:

```json
{
  "changeAssessments": [{
    "id": "c_copy_from_changes_view",
    "basis": null,
    "affectedFlows": ["a_example/return-value"],
    "explanation": "The edit affects the returned value; the independent sibling computation is unchanged."
  }]
}
```

This is an excerpt. Include all `knownFlows` that still exist, plus any additional
claims affected by the change. Repair missing watches when appropriate. An empty
list needs an explanation of why existing claims are unaffected; it does not
mean the changed code is tested. Save responses, validate, copy examined change
tokens, save, then validate again for final flow tokens. Impact assignment can
invalidate another flow, so this order matters. Tokens have no circular dependency.
Deleting a response does not clear its managed change record.

On the next run, resolved changes are folded into per-flow generation baselines
and retired. Unchanged equivalent runs preserve current tokens. Pending records
survive and their responses must match the current input manifest. The scope is
finite: uncaptured external state is not automatically monitored.

Version-1 maps remain importable during carry; graphs and IDs are preserved,
span watches become file watches, and all flow tokens start null. Former test
names are retained in investigation questions until file-qualified selectors are
provided. Missing sink edges are not invented. Legacy evidence may contain full
source, used only to recover hashes. No migration writes into the old run.

## Limits and validation

The JS/TS verification matrix covers Node ESM/CommonJS/native TypeScript, Vitest
TypeScript, Jest CommonJS and Playwright's Node-side TypeScript assertions,
including aliases, shared parameterized sites and async rejection matchers.
Unsupported/custom sites can be documented, but JS/TS credit requires the exact
recognized expression and operation in runtime evidence. This does not infer
browser assertion identity or data flows through external services.

The map is an agent assessment. A credited statement can be changed without
failing a test when the change preserves the observed property. Neither 100%
assertion coverage nor 100% MC/DC proves mutation resistance or safety for every
change. Use the observation and graph to select tests and identify missing
checks; sample mutation audits can independently evaluate the authored claims.
See [the agent workflow](assertion-agent.md), [assertion evidence](assertion-evidence.md),
and [verification](verification.md).
