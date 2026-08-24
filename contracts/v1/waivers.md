# Reviewed-waiver contract v1

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
    }
  ]
}
```

`file`, `condition`, and a non-blank `reason` are required strings. `decision`
may be the stable decision ID or whitespace-normalized decision source. `line`
is an optional positive integer disambiguator. `condition` may be normalized
condition source or positional `C<n>`; positional form requires `decision`.

Waivers never mutate measured totals. An uncovered matched condition is
`applied`, a covered matched condition is `contradicted`, and a record matching
nothing is `unmatched`. All three categories remain visible. The first waiver
for a condition owns its annotation. A platform/environment gap is not an
impossibility and must not be waived.

Malformed JSON or shape is `INVALID_ARGUMENT`. Absence means no waivers.
