# Coverage model

Supercov turns source structure into a fixed set of obligations before the test
run. Tests can cover those obligations, but they cannot silently change what
100% means.

## What Supercov measures

| Obligation | Question |
| --- | --- |
| Line | Did execution reach this source line? |
| Statement | Did this executable statement run? |
| Function | Was this function entered? |
| Branch | Did each alternative execute? |
| Decision vector | Which combinations of conditions were observed? |
| MC/DC witness | Was each condition shown to affect its decision independently? |
| Value path | Did language constructs such as defaults, optional chains, and logical assignments take each meaningful path? |

The exact obligations depend on the language and source construct. Query one
file or decision to see the concrete missing behavior:

```sh
npx supercov runs latest file app/checkout/session.ts
npx supercov runs latest decision app/checkout/session.ts:64
```

## Why a line percentage is not enough

A line can execute while an important outcome remains untested. For example,
this decision has two conditions:

```js
if (user.isAdmin || user.ownsDocument) allowEdit();
```

Executing the line proves very little by itself. Useful tests should show the
admin condition matters, the ownership condition matters, and the denied path
still works. Decision-vector and MC/DC queries expose the missing cases directly.

The same principle applies to Rust boolean expressions and control flow.

## Evidence confidence

Where the runner exposes exact test boundaries, Supercov can show which test and
attempt covered an obligation. Where it does not, execution is recorded as
aggregate background evidence rather than assigned to a guessed test.

Use attempt filters to choose the evidence included in a view:

```sh
npx supercov runs latest --filter all
npx supercov runs latest --filter passed
npx supercov runs latest --filter failed
```

Use `--kind` when the project distinguishes test levels:

```sh
npx supercov runs latest gaps --kind e2e
```

A filtered view recomputes obligations from the selected evidence. It does not
filter a percentage that was already calculated from something else.

## Complete, uncovered, and blocked

An ordinary uncovered obligation can be closed by a test. A completeness
blocker means Supercov cannot honestly claim the source was fully measured.
Common blockers are:

- ambiguous first-party source scope;
- source that must remain uninstrumented because code observes its own text;
- dynamically created source without a stable pre-run denominator; and
- execution that crossed an unsupported or unattributed runner boundary.

Inspect source scope with:

```sh
npx supercov runs latest scope
```

If automatic scope is ambiguous, declare the authoritative roots with
`SUPERCOV_SOURCE_ROOTS`. Other blockers remain visible with their location and
reason. Supercov does not round them away to produce a comfortable 100%.
