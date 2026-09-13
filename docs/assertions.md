# Understanding assertion coverage

Assertion coverage helps you see which parts of your JavaScript or TypeScript
code are checked by tests. After a normal run, your coding agent records what
each assertion checks in `assertions.json`. Supercov combines that map with the
run's execution evidence and shows an **Assertions** percentage alongside Lines,
Branches and MC/DC.

## Start with your existing tests

```sh supercov
npx supercov -- npm test
npx supercov runs latest assertions
```

Use your project's test command after `--`. Each run creates its own map and
reuses compatible work from earlier runs of the same command. The assertions
list shows the map's path and the assertions that still need attention.

Copy the printed run ID before asking an agent to edit the map. This keeps its
work on the same run even if another test run finishes. You can give it this
prompt, replacing `<run-id>` with that ID:

```text
Read the instructions from npx supercov docs assertion-agent. Build or update
assertion coverage for run <run-id>. Investigate the tests and current source,
then edit that run's assertions.json. Preserve reusable flows and explain
uncertainty. Do not change application code or tests. Validate the map and
show the assertion percentage, remaining gaps, and any missing evidence.
```

You can use your usual coding agent. Supercov does not require a particular
model or make model calls itself.

## Read the percentage

```sh supercov-example
npx supercov runs <run-id>
```

An illustrative report might show:

```text
Assertions 62.50% (50/80) — agent-assessed statements, whole run
  18 assertions with flows; 7 without; 29 current flows; 3 stale; 2 draft
```

Here, 50 of the run's 80 measured statements have a current explanation linking
them to a passing assertion, with execution recorded in the same test. Each
statement counts once, even when several assertions check it. Unexecuted code
stays in the denominator. TypeScript imports that are known to disappear during
compilation are excluded.

The score describes the whole run. Filtering Lines or MC/DC by test kind does
not change the assertion percentage. You do not need to rerun tests just to
recalculate a map: save it and query the run again.

| Report state | What to do |
| --- | --- |
| Not assessed | Ask your agent to start explaining the assertions. |
| Pending | Inspect changed inputs, draft flows or stale flows before using a percentage. |
| Unavailable | Check for a failed run, invalid map or source that no longer matches the run. |
| Not applicable | The run has no measured statements. |

A partially written map can have a useful percentage. Check the counts beside
it: having some flows does not mean every relevant flow has been found.

## Check what an assertion actually protects

Suppose `src/shipping.js` contains:

```js
export function shippingCost() {
  return 4;
}
```

These tests all call the function, but they check different things. Here,
`assert` comes from `node:assert/strict`:

| Assertion | What it checks |
| --- | --- |
| `assert.equal(shippingCost(), 4)` | The cost is exactly `4`. Returning `5` would fail. |
| `assert.ok(shippingCost())` | The cost is truthy. Returning `5` would still pass. |
| `shippingCost(); assert.equal(7, 7)` | Nothing about the returned cost. The assertion compares constants. |

Line coverage can be the same in all three cases. Even a credited statement can
change without failing a test when the new value still satisfies the assertion.
Read the mapped observation when deciding whether a test protects your change.

## Inspect a gap

```sh supercov-example
npx supercov runs <run-id> assertions --needs-attention --limit 5
npx supercov runs <run-id> assertion <assertion-id>
npx supercov runs <run-id> assertions report --view statements --file src/shipping.js
npx supercov runs <run-id> source src/shipping.js
```

An assertion detail shows its exact location in the test, its recorded flows,
and why each source node is credited, not credited or context only. Use this to
find a missing scenario, strengthen a weak check or correct an explanation.
See [Investigating assertion evidence](assertion-evidence.md) when execution
or a passing assertion is missing.

An absence check, such as “no message was received,” can be useful without
crediting a message-sending statement that never ran. Its map should explain
what was observed, when observation ended and why that was sufficient.

## Keep the map when code changes

Run the same suite command again after editing source or tests. Supercov copies
reusable mappings into the new run and identifies flows that need another look.
One changed dependency can affect a single flow; changing the assertion itself
can affect all its flows. Your agent can update those parts instead of starting
over.

Investigation uses your current project files. A run retains the map and file
identities, but does not keep a complete source checkout. If the source no
longer matches, rerun tests before continuing the map.

## Use the score to guide changes

Use MC/DC to find missing condition cases and assertion coverage to investigate
what those tests check. Improve the weakest observations and the gaps relevant
to your change, then run the full suite again.

Neither percentage proves that every possible bug will fail a test. A high
score is most useful together with the recorded observations and a review of
the behavior you intend to preserve. See [Assertion map format](assertion-maps.md)
for editing and validation, or [Trusting results](verification.md) for the
broader test-review workflow.
