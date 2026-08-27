# Reviewed-exception contract v1

The optional project-root file is `supercov.waivers.json`:

```json
{
  "version": 1,
  "waivers": [
    {
      "file": "src/example.ts",
      "decision": "ready && enabled",
      "line": 12,
      "condition": "enabled",
      "reason": "No satisfiable independence pair because ..."
    },
    {
      "kind": "line",
      "file": "src/generated.ts",
      "line": 18,
      "reason": "The generated state machine proves this line unreachable because ..."
    },
    {
      "kind": "statement",
      "id": "statement-stable-id",
      "file": "src/example.ts",
      "reason": "The upstream protocol excludes this state because ..."
    }
  ]
}
```

`kind` is one of `mcdc` (the backward-compatible default), `line`,
`statement`, `function`, or `branch`. Every entry requires `file` and a
non-blank rationale in `reason`.

For `mcdc`, `condition` is required; `decision` may be the stable decision ID
or whitespace-normalized decision source, and `line` may disambiguate it.
Positional `C<n>` requires `decision`. A `line` exception requires a positive
`line`. Statement, function, and branch exceptions require either their stable
`id` from a Supercov query or both `line` and `column`; optional `source` and
`alternative` selectors can make the reviewed claim narrower.

Reviewed exceptions never mutate measured totals. Supercov always displays raw
coverage and labels the separate policy-adjusted view. An uncovered matched
obligation is `applied`, a covered matched obligation is `contradicted`, and a
record matching nothing is `unmatched`. All three categories remain visible.
The first entry for an obligation owns its annotation. A platform/environment
gap, missing evidence, failed transport, or merely difficult test is not proof
of unreachability and must not be recorded as an exception.

Malformed JSON or shape is `INVALID_ARGUMENT`. Absence means no waivers.
