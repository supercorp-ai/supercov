# Understanding assertion coverage

After a normal test run, a coding agent can map what each JavaScript or
TypeScript assertion checks into the run's `assertions.json`. Supercov reads
that map alongside the recorded execution evidence and displays an assertion
percentage in the regular coverage report.

```sh supercov
npx supercov -- npm test
npx supercov runs latest assertions --json
```

The run creates the map automatically, reusing the newest available map for the
same test command and language. Pin the returned run ID and edit `data.map`.
Ask your agent to follow
`supercov docs assertion-agent`, read the frozen source, and complete the map.
Then validate and acknowledge the reviewed flows:

```sh supercov-example
npx supercov runs <run> assertions validate --json
npx supercov runs <run> assertions review --all
npx supercov runs <run> assertions check --require-complete --require-observed --json
npx supercov runs <run>
```

The **Assertions** row appears beside Lines, Branches and MC/DC. It is the
percentage of measured statements credited by current agent-authored flows,
with exact passing assertion identity and execution in the same test. It uses
the whole archived run even when structural coverage is filtered. Incomplete
maps remain useful; unmapped or dirty flows earn no credit.

## Check what the test asserts

Suppose `src/shipping.js` contains:

```js
export function shippingCost() {
  return 4;
}
```

These checks all call the function, but they test different things. Here,
`assert` comes from `node:assert/strict`:

| Assertion | What it checks |
| --- | --- |
| `assert.equal(shippingCost(), 4)` | The cost is exactly `4`. Returning `5` would fail. |
| `assert.ok(shippingCost())` | The cost is truthy. Returning `5` would still pass. |
| `shippingCost(); assert.equal(7, 7)` | Nothing about the returned cost. The assertion compares two constants. |

Line coverage can be the same in all three cases. The useful test is the one
that checks the behavior you need to preserve.

The same applies to other results: checking the number of log calls does not
check their messages, and checking one substring does not check the whole response.

## Inspect and improve the map

```sh supercov-example
npx supercov runs <run> assertions --limit 5
npx supercov runs <run> assertion <assertion-id>
npx supercov runs <run> source src/shipping.js
npx supercov runs <run> assertions --view statements --file src/shipping.js --limit 20
```

Each assertion keeps its exact test source anchor, the behavior it observes,
and flows to explicit production statements. Follow those links when deciding
whether a test needs another scenario or a stronger assertion. The model owns
the semantic assessment; Supercov validates syntax, source references, evidence
and freshness without reconstructing the reasoning.

After changing code or tests, run the same suite command again. Its new map
automatically carries forward unchanged entries;
changed or ambiguous references stay dirty until reviewed. See the
[map reference](assertion-maps.md) for the format, commands and limitations.

MC/DC measures independent condition effects. Assertion coverage measures the
statements linked to checks by the reviewed map. Maximizing either number does
not prove every edit will fail a test: the exact property in `observes` matters.
