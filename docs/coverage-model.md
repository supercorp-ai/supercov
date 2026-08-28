# Coverage model

Supercov derives coverage obligations from code structure. It reports which
obligations ran and the quality of the available evidence, including line,
branch, value-path, control-flow, and MC/DC coverage.

## Obligations

An obligation is one thing the code structure requires a test to demonstrate.
The denominator is fixed before the run from the source itself, so a percentage
cannot drift when tests are added or removed.

| Family | Obligation |
| --- | --- |
| Lines | Each executable line executes |
| Statements | Each statement executes |
| Functions | Each function is entered |
| Branches | Each alternative is taken: `true`, `false`, switch fallthrough, and the implicit no-match arm |
| MC/DC | Each atomic condition is shown to independently determine its decision |
| Value selection | Optional-chain short-circuits, logical assignments, and parameter or destructuring defaults each resolve both ways |
| Control flow | `try` versus `catch`, and zero-iteration versus entered `for-in` / `for-of` |

The value-selection and control-flow families are the ones most tools omit.
`a?.b`, `x ??= y` and `function f(a = 1)` each hide a decision that never
appears as a branch in a conventional report, and a `for-of` that never runs
with an empty collection is an untested path even though every line inside it
is green.

## MC/DC in one example

Modified condition/decision coverage asks more than "was this condition true and
false at some point". It asks whether each condition was shown to *independently
change the outcome*, which requires a pair of executions differing in that one
condition and producing different decisions.

For `isAdmin || (total > limit && !locked)`:

| Vector | `isAdmin` | `total > limit` | `!locked` | Decision |
| --- | --- | --- | --- | --- |
| v1 | F | T | T | true |
| v2 | F | F | T | false |
| v3 | F | T | F | false |
| v4 | T | F | T | true |

- v1 and v2 differ only in `total > limit` and disagree, so that condition is
  proven.
- v1 and v3 do the same for `!locked`.
- v4 and v2 do the same for `isAdmin`.

Remove v2 and two of the three proofs collapse, even though every condition has
still been observed both true and false, and every line is still green. That is
the gap MC/DC exists to catch, and it is why the criterion is required for the
highest software assurance levels in avionics.

Supercov stores **vector-level provenance**: which test produced each observed
vector, not just which tests touched the decision. A filtered query therefore
recomputes valid witness pairs for the tests it selected, rather than filtering
a percentage computed for a different set. A witness assembled from one unit
vector and one end-to-end vector counts for the combined suite and for neither
level alone — and Supercov reports it that way.

## Quality of evidence

Not all coverage is equally convincing. Each line, branch alternative, vector
and condition records how it was reached:

| Level | Meaning |
| --- | --- |
| Unexecuted | No evidence |
| Executed | Reached during a test, with no explicit causal link |
| Action-linked | Reached inside a recognised browser action such as `locator.click()` |
| Assertion-linked | Reached inside an `expect()` matcher, or in the code path an assertion depends on |

Only an explicit browser or server event can raise confidence to
assertion-linked. Where Supercov has to fall back on timing correlation — an
early cross-origin iframe probe, for example — the evidence stays
execution-only and is labelled as such. Code reached outside a recognised
action, such as setup work or a helper making its own HTTP requests, still has
exact test attribution but may carry no action phase at all.

In Playwright, the phase travels with the request: an action opened in the
browser is still the active phase inside the server route it triggers, so a
chain of `click → application decision → visible assertion` is queryable.

## Provenance

Every test carries two independent labels.

**Runner** is the process that executed it — `playwright`, `vitest`, `jest`,
`node`.

**Kind** is its semantic level — `e2e`, `integration`, `component`, `unit`.
Kind is resolved in descending confidence from an explicit `SUPERCOV_TEST_KIND`,
then the Playwright project name, then the test path, then the runner default
(Playwright is end-to-end, Vitest is unit). Queries preserve how the label was
established, so an inferred kind is never presented as a declared one.

Vitest module-import and setup execution is retained as a separate setup scope
rather than being attributed to whichever test happened to run first.

## Attempts and filters

Evidence records attempt status, so a test is classified as passed, failed,
flaky, skipped, timed out, interrupted, unknown, or selected but unstarted
after fail-fast. `--filter` selects which attempts contribute to a view:

- `all` — every executed attempt, including attempts that later failed. This is
  the default and matches conventional coverage tools.
- `passed` — successful attempts of tests that ultimately passed.
- `failed` — failed attempts only, including failed retries of a flaky test.

Passed and failed views are derived from the same immutable archive rather than
duplicated into separate report files, so they cannot disagree.

## When completeness is blocked

A verdict is only useful if it refuses to be complete when it cannot be:

- **Ambiguous scope.** Every candidate source file is retained as included,
  excluded, or ambiguous. Ambiguity blocks a complete verdict and is
  inspectable with `coverage scope`. Set `SUPERCOV_SOURCE_ROOTS` to declare the
  authoritative scope.
- **Semantic-safety blockers.** When application code coerces or observes a
  function's own source, Supercov leaves that body uninstrumented and records
  the blocker rather than transforming code whose text is being read.
- **Unknowable denominators.** Direct `eval` and `Function` source cannot
  receive a stable pre-run denominator. Their exact locations are recorded as
  completeness blockers instead of being silently excluded.
- **Unattributed evidence.** Execution that arrives without a carrier is stored
  under a first-class background scope, visible in the all-attempt view and
  excluded from per-test passed-only coverage.

None of these are rounded away. A blocked verdict is more useful than a
comfortable 100%.
