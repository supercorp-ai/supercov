# Recorded evidence

These files are the actual stdout/JSON and, where present, stderr from
`npm run record`. The script verifies the results before writing them.

- `before-*`: the original two tests, their summary, and decision query.
- `after-*`: all three tests, their summary, and decision query.
- `diff.*`: the coverage gain and absence of losses.
- `regression-before.tap`: the original tests pass with the expiry guard removed.
- `regression-after.tap`: the additional expired-session test fails on that same broken copy.
- `result.json`: the checked results, CLI/Node versions, platform, and recording time.

Run IDs, local temporary paths, and timings are retained as emitted. The
temporary workspaces are removed when the reproduction script finishes; run
`npm run demo` for a fresh verification, or the manual coverage commands in the
example README to retain queryable runs locally.

The assertion-linked MC/DC counts are real evidence fields, not a fabricated
standalone assertion-coverage percentage. No AI conversation is claimed by
these recordings.
