# Portable HTML reports

Supercov's terminal queries are deliberately small for coding agents. The HTML
report is the human view over the same immutable local evidence.

```sh
npx supercov report
```

This writes `supercov-report.html` in the project root and opens it in your
default browser. The file contains its application, styles, and compressed
evidence. It works after the file is moved, attached to a pull request, or
opened on a machine that does not have Supercov installed. It does not need a
server, extraction step, file picker, external font, CDN, or network request.

## What the report answers

The first screen is ordered around four questions:

1. Did the measured command pass?
2. Is the measurement complete and still current?
3. What changed from the comparison run?
4. What is the next concrete line, branch, or MC/DC obligation to test?

The **Review** view ranks open gaps and explains their evidence beside source.
**Explore** lists every measured file. **Tests** shows runner, kind, outcome,
retry, and attributed source information. **Scope** shows which files entered
or stayed outside the denominator.

## Choose runs and output

Up to ten recent runs are included by default. The selected run is compared with
the previous saved run automatically:

```sh
npx supercov report
npx supercov report <run-id>
npx supercov report latest --compare <older-run-id>
```

Include more local history or choose a destination with:

```sh
npx supercov report --runs 5
npx supercov report --output artifacts/coverage.html --no-open
```

`--runs` accepts 1 through 20. The generated file is capped at 24 MB, leaving
room under GitHub's normal 25 MB attachment limit. A report that exceeds the
cap is not written; retry with fewer runs.

## Source integrity

Stored evidence always supplies immutable source locations and snippets. Full
authored source is added only when its current source fingerprint is identical
to the selected run. A test-only change can therefore mark a run stale while
its source remains safe to show. If authored source changed, the report labels
that run as stale and uses the stored snippets instead of displaying newer code
beside older coverage.

Individual source files over 1 MB and total source beyond 16 MB are omitted.
The report names its source mode and any omitted paths rather than silently
substituting content.

## Privacy and sharing

The report includes source, test names, commands, and coverage evidence. Treat
it like any other development artifact and share it only with intended
reviewers. Supercov does not include environment variables or raw test output,
and the report has a content-security policy that blocks outbound connections.

For an ordinary pull request, attach `supercov-report.html` to the description
or a comment. GitHub serves HTML attachments as downloads. The reviewer clicks
the attachment, downloads the single file, and opens it locally.
