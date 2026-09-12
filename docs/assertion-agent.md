# Assertion mapping instructions for a coding agent

Use one ongoing investigation and one run-owned `assertions.json`. Supercov runs
no model. You supply the semantic reasoning; its Rust CLI checks the file and
tracks evidence and changes. Read this guide with `supercov docs assertion-agent`.

## Start from an existing run

1. Run the project's regular tests through `supercov -- <test command>` if there
   is no suitable fresh run. Use the full suite when you want a suite-wide score.
2. Run `supercov runs --json` and pin a concrete run ID. Do not keep using `latest`
   while editing, because another run can change its meaning.
3. Run `supercov runs <run> assertions --json`. The run already created its map,
   automatically carrying the newest available map for the same command and
   language. Edit `data.map`; read `data.inheritance` for the source run and any
   skipped-map errors. Keep the old run and its map.
4. Read `supercov docs assertion-maps` and page all of:

   ```sh
   supercov runs <run> assertions --limit 100 --json
   supercov runs <run> assertions report --view statements --limit 100 --json
   supercov runs <run> assertions report --view tests --limit 100 --json
   ```

   Follow `data.pagination.nextOffset` using `--offset` until it is null.
   Pages from the same report must have the same `data.revision`; restart a
   multi-page read if it changes. Do not interpret one page as the whole suite.

## Investigate each assertion

Read archived code with
`supercov runs <run> source <path> --offset 0 --limit 100`.
This prints source code with line numbers from the run's archive. Add `--json`
only when you need structured `{line, text}` items. It reads the archived file
as it was when the tests ran, even when today's checkout has changed or the
assertion map has a syntax error. Use `assertions files` to discover frozen paths.

`supercov runs <run> assertions --file <test-path>` lists all assertion sites,
including unmapped ones, with review and passing-execution status. An entry with
`inMap: false` was found in archived code but is missing from the authored map;
restore that entry before treating the investigation as complete. There is no
separate `inventory` command.

Use `supercov runs <run> assertion <id>` to inspect one assertion's property,
full authored flows, nodes, edges, watches and current review/evidence status.
The detail response uses the same map snapshot for its graph and assessment.
Add `--json` to read these under `data.assertion` and matching test names under
`data.tests`. IDs must match exactly.

Add `--file <path>` to assertion/statement report pages to focus an investigation;
summary counts still cover the whole run. Paths are project-relative. Read test
setup, input values, mocks, callbacks, called functions, branches and any helpers
that matter to this assertion.

For each assertion:

- Preserve its ID and exact `at` anchor from the automatically created map. It identifies the
  complete assertion call, usually without a trailing semicolon. Do not replace
  it with a test declaration or another expression at the same coordinate.
- State the property actually checked in `observes`: equality of a field, array
  length, rejection type/message, absence of a call, etc. An existence check
  does not imply checking the whole object's contents.
- Trace the values and control choices manually. Write one or more flows with
  stable IDs, a concrete explanation and exact nodes. Edges can describe data,
  control, calls or absence; edge labels are your explanation, not proof rules.
- Use the `statements` view's exact `at` anchors for production statements you
  judge asserted. Put only their node IDs in `countsAsAsserted`. Other nodes can
  explain setup, transport, parsing, branches or test helpers without earning
  credit. A counted node must exactly match a measured statement anchor. A
  guard/block does not automatically credit nested statements; add those nodes
  explicitly if your reasoning supports them.
- Record broader review inputs in `watch`: the containing test, relevant
  functions, setup, guards, alternate branches and helpers. Whole files are a
  simple conservative starting point. Span watches can reduce review work once
  you understand the complete relevant context. Do not watch just a return
  expression while relying on an unwatched guard.
- For parameterized or shared assertions, describe each relevant case. Use
  `appliesTo` with exact names from the `tests` view when flows differ by case.
  Omission allows all passing tests that observed that assertion. Test IDs in
  reports are run-specific and are not `appliesTo` values.
- An absence check can have an empty `countsAsAsserted` array. Explain why an
  event is absent. Unexecuted bodies receive no execution-backed credit. Credit
  an executed guard only when the predicate really observes its behavior.
- Keep `analysis: "partial"` for unresolved reasoning and `"unmapped"` for
  untouched sites. `"mapped"` means your investigation is complete, not that all
  code is asserted. A complete explanation may intentionally claim no statement.

Inspect sites with empty `observedPassingTests`, flows with `blockers`, and
statements with `at: null`. They may represent skipped tests, expected failures,
unsupported syntax, generated-source mismatches or missing evidence. Do not
fabricate an occurrence, change an anchor to borrow another assertion's
identity, delete inconvenient sites, or edit review state to make the gate pass.
Custom sites can be documented, but JS/TS credit currently requires the exact
inventoried expression and operation in passing runtime evidence.

## Validate, acknowledge and verify

Save the JSON, then use these separate steps:

```sh
supercov assertions validate --file <map-path> --json
supercov runs <run> assertions validate --json
supercov runs <run> assertions review --flow <assertion-id>/<flow-id> --json
supercov runs <run> assertions check --require-complete --require-observed --json
supercov runs <run> assertions report --view statements --limit 100 --json
```

Repeat `--flow` or use `review --all` after reviewing every flow. Syntax validation
checks JSON shape; run validation also checks IDs, edges, references and state
binding. Review records **your acknowledgement**. `check` does not acknowledge
anything or repair the file. The commands return exit 2 on an unmet check; read
`errors` or `failures`. Add `--min <percentage>` when the project has a chosen
assertion coverage target. Do not lower the target to hide a regression.

A strict evidence check may legitimately remain blocked by skipped or unsupported
sites. Report those limits explicitly. Basic `check` allows an incremental map;
`--require-complete` adds inventory completion and `--require-observed` adds
passing evidence for every entry and flow restriction. None proves the semantic
claims. A green check is meaningful only alongside honest authored reasoning.

## Continue after code or test changes

Run the same test command again. Its new map automatically inherits prior work. Read the new
summary, assertion rows, new sites and `retiredAssertions`. Retain unchanged
explanations. Repair dirty flow anchors and reasoning; resolve ambiguous identity
suggestions using the old explanation and the new test. A changed assertion can
keep an ID as a dirty suggestion, so retaining an ID is not evidence of equivalence.

Investigate each `scopeReview` file. Add missing watches/flows or explain why its
changes do not affect existing claims, then `review --ack-scope`. Acknowledge only
flows actually reviewed. Dirty flags persist across runs and reverts until that
acknowledgement. Do not hand-edit `assertions.state.json` or reuse a map by copying
its state into another run.

Finish with the pinned run ID, primary statement percentage and counts,
inventory completion, unobserved/dirty items, relevant limitations and exact
verification commands used. Keep MC/DC and structural coverage separate. A score
is an agent's assessment backed by occurrence/execution evidence; it does not
promise that every change to a credited statement will fail a test.
