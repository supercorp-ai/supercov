# Agent workflow for assertion maps

You are the semantic author. Supercov supplies source identities, execution
records, file hashes and a JSON validator. It does not infer or prove your
explanations. Use your ordinary source-reading tools and edit one run-owned
`assertions.json`; no model is embedded in Supercov.

## Start with a real run

1. Run the intended suite through `supercov -- <test command>` if no matching
   current run exists. Use the full suite for a suite-wide result.
2. Read `supercov runs --json`, pin its concrete run ID, then read
   `supercov runs <run> assertions --json`. Edit `data.map`; the run creates it
   automatically and inherits compatible prior work. Check `data.inheritance`
   for fallback errors. Keep old runs and their maps.
3. Read `supercov docs assertion-maps`. Page the assertion, statement, test and
   change views. Do not mistake one page for the whole suite:

   ```sh
   supercov runs <run> assertions --limit 100 --json
   supercov runs <run> assertions report --view statements --limit 100 --json
   supercov runs <run> assertions report --view tests --limit 100 --json
   supercov runs <run> assertions report --view changes --limit 100 --json
   ```

Follow `pagination.nextOffset` with `--offset`; restart if `revision` changes.
Use a single map writer. Save edits atomically where your editor supports it.
The outer object contains `schemaVersion: 2` and `assertions`; do not insert run IDs.

## Investigate precise claims

Read current test setup, inputs, mocks, callbacks, branches, callees, helper
modules and relevant configuration. Current files must match the pinned run.
Supercov retains hashes and identities, not a source checkout. Optional
`runs <run> source <path> --offset 0 --limit 100` prints matching code with line
numbers; `assertions files` lists input hashes even when files no longer match.

Use `runs <run> assertion <id>` for the exact site and graph. Preserve its ID and
full `at` anchor, usually the complete assertion call without a trailing semicolon.
Use one-based UTF-8 byte columns, not character counts. An `inMap: false` site
was recognized but is missing from the map; restore it when investigating.

For each assertion:

- Describe exactly what its predicate distinguishes in `observes`. Truthiness,
  existence, length or substring checks do not imply equality of every field.
- Write independently maintainable flows with stable IDs, `basis: null`, a
  concrete explanation, exact nodes and authored edges to `$assertion`.
- Use explicit `appliesTo: [{file, name}]` from the test view. File and name
  must resolve unambiguously. Empty means no credit. Runtime IDs never belong
  in selectors. Shared/parameterized sites may need multiple cases or flows.
- Take production statement anchors from the `statements` view when possible.
  Put only nodes you judge asserted in `countsAsAsserted`. Each counted node
  needs an authored path to `$assertion`; a block does not credit nested code.
- Put additional dependency paths in `watch`, as whole-file strings. Assertion,
  selected-test and node files are already dependencies. Include setup, guards,
  alternate paths and helpers your explanation relies on, even without nodes.
- Preserve uncertainty in `questions`. Flow questions block that flow's credit.
  Assertion questions record unfinished exploration without implying a known
  total. There is no `analysis`, `mapped`, or `complete` flag.
- A fixture-only or absence explanation can have `countsAsAsserted: []`. Explain
  the absence, including ordering/barriers and the observation window. Never
  credit an unexecuted body just because its execution would violate a check.

Inspect `selectors`, `blockers`, `reasons`, empty `observedPassingTests`, and
statement `at: null` entries. They may represent skipped tests, unsupported
syntax, ambiguous test names or missing evidence. Do not borrow another site's
identity, fabricate events, delete inconvenient entries or edit managed state.
Zero-credit explanations are useful; invented credit is not.

## Account for changes before finalizing flow tokens

When inheriting a map, inspect **every** item in `--view changes`, including files
already watched by some flows. Dependencies may be missing. In root
`changeAssessments`, write one response per managed change ID:

```json
{
  "id": "c_copy_from_changes_view",
  "basis": null,
  "affectedFlows": ["assertion-id/flow-id"],
  "explanation": "What changed, which claims it affects, and why other existing claims remain valid."
}
```

Include all known dependent flows still present, plus any others affected.
Repair watches if the change reveals a missing dependency. An empty list needs
an actual explanation. Removing a response does not clear the outstanding change.
A changed test/assertion can affect all its flows; a production file used by one
sibling may affect only that flow. No machine can tell you an expected flow total.

Save the responses and graph edits. Run:

```sh
supercov assertions validate --file <map-path> --json
supercov runs <run> assertions validate --json
```

For large maps, page `validate --view changes`, `validate --view flows`, or
`validate --view errors` with `--offset` and `--limit`; tokens appear under
`items` and overall validity still covers the whole map. Pin `revision` across pages.

Validation checks shape, IDs, graph links, anchors and state binding. It returns
`changes[].expectedBasis` and `flows[].expectedBasis`; null/stale tokens alone
are unfinished work, not syntax errors. Copy change tokens **only after inspecting
those impact assessments**, then save. Validate again and copy flow tokens only
for claims you have examined. Save the map again. Change acknowledgements can
invalidate additional flows, so obtain final flow tokens after change tokens.
Changing a graph, selector, observation or watch after obtaining its token makes
that token stale. Do not implement the hashing algorithm or manufacture tokens.

There is no `review` command. All commands are read-only; acknowledgement is your
explicit file edit. Passing reference validation does not prove your reasoning.

## Check and report

```sh
supercov runs <run> assertions check --require-mappings --require-observed --json
supercov runs <run> assertions report --view statements --limit 100 --json
supercov runs <run>
```

Basic `check` permits untouched sites without flows, but fails on invalid or
unfinished authored claims, open questions and outstanding change impact.
`--require-mappings` requires at least one current observed explanation for every
recognized passing site, including legitimate zero-credit explanations.
`--require-observed` checks every site and selector. Neither means every semantic
flow was found. Use `--min <percentage>` only for the project's chosen target.
Do not lower a target or hide unknowns to make a check pass.

The normal report includes the percentage immediately from the edited file.
`notAssessed`, `pending`, `unavailable` and `notApplicable` have null percentages,
not fabricated zeroes. A numeric partial-map result still reports counts of
assertions without flows and claims needing work. Describe the score as
agent-assessed statements; successful same-test coexecution is evidence, not
proof of causality or mutation resistance.

## Continue after edits

Run the same suite command again. The new run carries IDs, explanations and
unchanged acknowledgements using prior file hashes, without needing old source.
Use `assertions --needs-attention` and the change view. Repair only affected
claims; keep current siblings. Any dependency byte edit, including comments,
requires rechecking. Unique relocation preserves identity as a suggestion;
ambiguous/removed sites remain in `retiredAssertions`. Old v1 flows are imported
as drafts with questions about selectors and graph paths; inspect them explicitly.

Do not edit `assertions.state.json`. Dirty generations persist across reruns and
reverts until current tokens are recorded. Resolved change responses are folded
and retired on the next publication. Runtime evidence never carries forward.
If source differs from the pinned run, rerun tests before continuing.

Finish with the pinned run ID, status, numeric percentage/counts when available,
assertions without flows, remaining questions/stale or unobserved claims, and
verification commands used. Explain remaining limits. The property in `observes`
matters: changing a credited line while preserving that property can still pass.
