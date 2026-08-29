# The binding lattice — 2026-08-29

## Problem

R3 dogfooding found eight distinct generated-code shapes the pre-borrow
binder could not bind (waves 1–8 in
`progress/rust-string-match-binding-2026-08-29.md`). Each one stopped the
build, because an obligation that cannot be bound exactly used to be a
`fatal`. That is the right default for Supercov's own gates and the wrong
default for a user's codebase: a single unknown shape anywhere in a
dependency-shaped macro expansion makes the tool unusable, and the shape
space of generated Rust is effectively open.

The waves also shared one root cause worth stating plainly: the binder
*reconstructs* the HIR→MIR correspondence after the fact (spans, dominators,
literals, discriminants) wherever identity-carrying markers were not already
injected. Every reconstruction heuristic has a blind spot, and each blind
spot is a wave. Marker-first binding is the long-term elimination of the
class; the lattice is what makes the remaining blind spots non-fatal.

## Decision

An obligation now degrades instead of failing the build:

- **exact** — bound through injected identity, exact vectors as today;
- **structural** — bound through the structural marker path (coverage-off
  bodies, CTFE owners), still exact;
- **unbound** — the body is left uninstrumented and the obligation is
  recorded as an explicit `RUST_OBLIGATION_UNBOUND` limitation naming the
  phase, the exact definition and the binder's own diagnosis.

The guarantee changes from "every number is exact or the build fails" to
"every number is exact or explicitly marked unmeasured". That is not a
weaker honesty claim: nothing is ever silently approximate, and an unbound
obligation must never be reported as *uncovered* — an unmeasured branch and
an uncovered branch are different facts and conflating them would be the
lie this design exists to prevent.

Degradation is per body: the failing body loses instrumentation, every other
body in the crate keeps exact measurement. `mir_built` returns the original
untouched body, so a degraded body carries no partial markers.

## Strict mode

`SUPERCOV_RUST_STRICT_BINDING=1` restores the old behavior and every corpus
compile sets it. Supercov's own gates must keep failing hard on an
unbindable shape, or the corpus would silently degrade instead of proving
exactness, and we would lose the discovery signal that produced waves 1–8.
An empty value counts as unset so a single gate can opt out.

## Why the degradation path is itself gated

`SUPERCOV_RUST_FORCE_UNBINDABLE=<substring>` treats every matching body as
unbindable. The corpus uses it to prove, in one crate, that:

- the lattice build succeeds and records the exact unbound limitation;
- an unrelated body in the same crate keeps its obligations;
- the strict build fails and names the same obligation.

An untested degradation path would be precisely the silent wrongness the
lattice is meant to prevent, so it is proven on demand rather than assumed.

## Mechanics

`BINDER_LIMITATIONS` collects degradations during MIR passes.
`after_analysis` forces `optimized_mir`/`mir_for_ctfe` for every body while
collecting obligations, so all degradations are recorded before the manifest
candidate is serialized; the merge happens after that loop (merging before
it silently produced empty limitations — the first version of this change
had exactly that bug).

## Consequences for iteration speed

Lattice mode turns discovery from serial into parallel: a probe run no
longer stops at the first unbindable shape, it compiles everything and
reports *every* remaining shape at once. That makes an overnight shape
miner over external crates worthwhile — one run enumerates the remaining
shape space instead of one wave per attempt.

## Follow-ups

- Per-obligation unmeasured buckets in the analyzer and report (three-way
  covered / uncovered / unmeasured) so one degraded body does not flip a
  whole run to `coverage_complete = false`. Today any limitation does.
- Degrade per obligation rather than per body, so eight bound match groups
  survive when the ninth cannot bind.
- Report the exact fraction as a released metric so the shape space is
  visibly ratcheting toward total exactness.

## Bug found by gating the lattice: symlinked source roots measured nothing

Adding the lattice gate to the corpus surfaced a real product defect. The
gate crate lives in the corpus scratch directory, which macOS reaches through
the `/var` -> `/private/var` symlink, and it produced no manifest at all:
every obligation was recorded as
`RUST_SOURCE_IDENTITY_UNRESOLVED: unowned external source`.

Cause: source ownership compared paths lexically. `normalized_path` only
removes `.` and `..`; it never resolves symlinks. rustc reports the physical
file path (`/private/var/...`) while `SUPERCOV_RUST_SOURCE_ROOT` carried the
symlinked spelling (`/var/...`), so `strip_prefix` failed and every authored
file was classified external. `package_identity` already canonicalized for
exactly this reason (see the `RCV-GENERATED-1` traceability row), so
ownership disagreed with itself depending on which check ran.

Impact: any project reached through a symlinked path — macOS `/tmp` and
`/var`, symlinked worktrees, some network mounts and home directories —
measured nothing, while still compiling and reporting successfully.

Fix: `root_relative` keeps the lexical comparison as the fast path and falls
back to comparing canonical physical paths, matching `package_identity`.
The lattice gate now doubles as the regression test, because owning that
crate's obligations at all requires physical containment to work.

## What the shape miner found (first runs)

The miner compiles a shape-dense dependency set in lattice mode with the
source root pointed at the registry checkout, so every downloaded crate's own
source is owned and reaches the binder. Three findings in the first runs:

1. **The lattice was incomplete.** Only the pre-borrow sites had been
   converted, so a real shape in `proc-macro2`'s build script still stopped
   the build. All binder and injection sites are converted now; the surviving
   `fatal`s are I/O and rustdoc-catalog integrity, where measurement is
   genuinely impossible.
2. **A defect introduced by the lattice itself.** Degrading try-operator
   binding to an empty map left downstream marker code indexing that map,
   panicking with `no entry found for key`. Degraded phases now skip their
   obligations instead of assuming an entry exists. Worth noting the failure
   mode: a compiler *panic* is not covered by the lattice at all, so
   degradation must never leave a partially-populated data structure behind.
3. **Four distinct shapes enumerated in one crate in a single pass** — the
   parallel discovery the lattice was built for, against one shape per
   multi-hour dogfood before it.

## A second category: invariant violations, not blind spots

The miner then surfaced a different class, and it should not be degraded
reflexively:

- `Rust branch aggregation mismatch` fires when two obligations hash to the
  same stable ID but carry different content (kind, discriminator,
  alternatives or parent arm). That is an *identity* defect: if it were
  ignored, two distinct branches would merge under one ID and report a
  single wrong number. It fired on `bytes`, so the ID derivation has a real
  collision case in real code, and that is worth fixing rather than
  degrading.
- `has no Rust decision kind for X` is the opposite — an authored control
  shape we do not model yet, which is an ordinary lattice case.

The rule to apply when converting the rest: degrade *shape* problems (we
cannot bind this construct), fail on *environment* problems (our own runtime
or evidence path is broken), and treat *identity* problems as bugs to fix
first — degrading them only as a safety net, and never by merging the
colliding obligations.

## The `bytes` aggregation mismatch: visit metadata treated as identity

The miner's `Rust branch aggregation mismatch` on `bytes` turned out to be a
regression, and the first hypothesis was wrong in an instructive way.

An authored obligation's canonical ID deliberately excludes the def path and
the owner-local ordinal, so one macro's body aggregates into a *single*
obligation across all of its invocations. The natural suspicion was therefore
`parent_match_arm` (added in wave 2), which genuinely does describe the
callsite rather than the obligation. Making the error self-diagnosing settled
it immediately: the differing field was `alternatives`.

`StableObligationIdentity` carries `owner_local_ordinal`, a visit counter that
advances on every recorded obligation. Two invocations of the same macro
therefore produce identical IDs and canonical strings but different counters,
and aggregation compared whole identity structs — so it reported a mismatch
between two recordings of the very same alternative. Aggregation now compares
semantic identity only (each alternative's stable ID and label).

The `parent_match_arm` change was kept anyway on its own merits: it is a
binding *hint* that narrows a search, so when two invocations disagree the
honest response is to drop the hint and fall back to sequential ranking, not
to fail.

Fixture: one macro invoked twice in the same function pins the aggregation
path. Writing it also surfaced an unrelated unbindable shape — a macro
invocation forming an entire match-arm body degenerates the span-located
planner exactly like the wave-5 derived `PartialOrd` case. That is tracked
separately rather than being absorbed by the lattice, because real code hits
that pattern constantly and it deserves an exact binding.

## Miner run 4: 11 crates, 22 shapes, and one model inconsistency

With the lattice covering every binder and injection site and the branch
aggregation fixed, the miner reached 11 crates and enumerated 22 distinct
unbindable shapes in a single pass (`bytes` alone contributes decision-probe,
try-operator and statement-probe shapes). That is the intended behavior:
shapes are now a ranked worklist rather than a sequence of build failures.

The run also stopped on a `decision aggregation mismatch` in `itoa`, and the
self-diagnosing message named the differing fields immediately:
`assertion-source,outcome-branch`.

The cause is a model inconsistency rather than a binder blind spot. A
decision's identity is derived from the assertion condition's own span, which
for macro-expanded code is the macro body and therefore invocation
independent. Two of its recorded parts are not: `assertion_source` comes from
`expression.span.source_callsite()`, and the outcome branch is recorded at a
callsite-dependent span. One `assert!`-bearing macro invoked at two callsites
therefore produces a single decision ID carrying two different callsite links,
and neither recording is wrong.

Interim behavior: the conflict degrades instead of failing. The first
recording is kept — never merged with the second, since merging is exactly the
silent wrongness to avoid — and a `RUST_OBLIGATION_AGGREGATION_AMBIGUOUS`
limitation records it. The obligation's condition set and vectors are
identical either way; only its callsite link is ambiguous, which is what the
limitation says. Strict binding still fails hard.

The real fix is a semantic decision, tracked separately: either every recorded
part becomes invocation-independent to match the ID, or macro-expanded
obligations get per-invocation identity. The choice determines whether a macro
body reports one aggregated number or one per expansion site, and it changes
obligation IDs and the `repeated_expansions` corpus expectations, so it is not
a patch to make in passing.

## Closing the honesty gap the lattice opened

The lattice let unbindable code compile, but it left a defect behind that the
north star names explicitly: a declined obligation still appeared in the
manifest with no hits, so the analyzer counted it as **uncovered**. That is a
measurement gap reported as a coverage gap — a wrong number, and wrong numbers
get trusted. It was introduced by the lattice itself.

The fix runs end to end:

- The wrapper records the exact obligation IDs it declines
  (`UNMEASURED_OBLIGATIONS`) and publishes them as `unmeasuredObligations`;
  the manifest candidate schema moves to v4.
- Declining is per body, not per phase. Once any phase of a body fails to bind
  we cannot prove which of its probes still fire, so the whole body is
  declined. Over-declining only under-reports coverage; under-declining would
  recreate exactly the defect above.
- The analyzer removes declined obligations from the covered/uncovered
  denominator and reports `unmeasuredObligations` alongside
  `exactFractionPct` — the share measured exactly, which is the number that
  must ratchet toward 100 and never regress.
- Both fields are omitted when nothing was declined, so a fully exact run
  serializes byte for byte as before and no existing consumer sees a change.

A unit test pins all four properties, including the byte-identical
serialization of a fully measured summary. The compiler route carries the
declined set through `normalize`; the legacy source-rewriting route has none
by construction.

Still owed: an end-to-end corpus gate proving a wrapper-declined obligation
arrives in the report as unmeasured rather than uncovered. The unit test
covers the analyzer and the lattice gate covers the wrapper, but the seam
between them is exactly where a defect would hide.

## What the seam gate found immediately

Writing the end-to-end gate (wrapper declines an obligation -> it arrives in
the manifest as unmeasured) failed on its first run, and the failure was real:
the declined body's `rs:function:` obligation was never declined. Function
identity occupies owner-local ordinal zero and is recorded outside the
per-body collector, so the marking missed it, and every uninstrumented body
would still have reported its function-entry obligation as uncovered — the
same defect the change had just fixed, surviving in the one corner not
checked.

Both sides of that seam were green: the analyzer had a passing unit test and
the wrapper had a passing lattice gate. Only the assertion that crossed
between them found the gap. Declining a body now covers all five obligation
kinds.

## Note on the wave-9 relaxations and the misbind invariant

Every wave-9 fix made binding more permissive: containment as a fallback when
a terminator span collapses, accepting either the expanded or callsite span
for a try operator, and accepting a pattern switch in either lowering shape.
Each relaxation is a potential misbind source, so each keeps the property that
matters: candidates must still resolve to exactly one match, and ambiguity
still fails closed rather than picking one. The pattern-switch binder
explicitly declines when the recorded variant is not among the tested values,
rather than guessing an edge.

That reasoning is currently held by review, not by a machine check. A
differential oracle against rustc's own branch mappings would catch a misbind
automatically wherever both exist, and is tracked separately.

## Precision of declining, and the first real exactness number

Declining a whole body is the correct conservative rule for a binder failure:
once a phase cannot be bound we cannot prove which of that body's probes still
fire. It is the wrong rule for an uncompiled construct, which is identified
exactly. Splitting the two took `bytes` from 98.68% to 99.68% exact — 29
declined obligations down to exactly 7, one per cfg-eliminated statement, with
no collateral. The unmeasurable marker carries its obligation ID through an
explicit constructor/parser pair rather than ad-hoc string parsing.

`bytes` is now the first real third-party measurement: 2199 obligations, zero
binder blind spots, and the entire remainder explained as code this target
does not compile.

Two traps worth remembering from taking that measurement. `Finished` is not
evidence — cargo had not rebuilt the crate at all on the first attempt. And
`SUPERCOV_RUST_COMPILER_OUTPUT` must be absolute: cargo runs a dependency with
its own directory as the working directory, so a relative path sends that
crate's manifest into the registry checkout and leaves an empty output
directory that reads exactly like "nothing to report".

## Misbind detection: the obvious oracle is vacuous

Diffing Supercov's bindings against rustc's own branch mappings sounds like
the natural check for the never-misbind invariant, but it only applies where
rustc supplies mappings — and on that path Supercov's binding is derived from
those mappings, so it compares a thing to itself. The misbind risk lives in
the fallback paths, which exist precisely because rustc gives no mapping
there.

What would reach it: structural post-conditions on each binding (the chosen
switch dominated by the decision's entry and dominating its outcome, distinct
true/false targets, no two obligations sharing a switch edge), cheap enough to
run on every compile; and an execution differential against
`-C instrument-coverage` counts for the same run, which is the strongest
available check and does cover the fallbacks.

## A crash the lattice could not catch

Converting the injection sites to degradations was done by rewriting many call
sites at once, and in `mir_drops_with_structural_probes` that produced
`return body;` paths *after* `body.steal()`. Returning an already-stolen
`Steal` makes rustc panic with "attempt to steal from stolen value", which
killed `proc-macro2` outright.

Two things this is worth remembering for:

A panic is not covered by the lattice at all. There is no limitation, no
degradation and no report — just a dead build. Every degradation path must
therefore leave the compiler in a state it can still use, which is a stronger
requirement than merely returning an error.

Returning the partially instrumented body would have been worse than the
crash. Half-applied markers produce evidence we cannot justify, and wrong
numbers outrank broken builds in the priority order. The fix keeps a pristine
copy taken immediately after the steal, so a decline returns exactly the
uninstrumented body.

This is the second defect tonight introduced by mechanically rewriting many
call sites (the first left a half-populated map that panicked on lookup), and
both were found only by compiling real third-party crates rather than by the
type checker or the corpus.

## Unmeasurable, second case: an arm that cannot complete

`once_cell` uses the `match void {}` idiom on an uninhabited type, and such an
arm lowers to no blocks of its own. The first attempt reused the span-overlap
test from the cfg-eliminated case and silently did not fire: the enclosing
match's spans overlap the arm's range, so "no MIR overlaps it" was simply not
true.

Rather than widen the heuristic until it passed, the arm obligation now
records a fact about the program instead of a guess about spans: whether the
arm body's type is uninhabited, taken from typeck at HIR time. `match void {}`
has type `!`, so the arm provably cannot complete, and an arm that cannot
complete is unmeasurable rather than unbindable. A loosened span rule could
have misfired on a real binder miss; a type cannot.

once_cell: 493 obligations, 98.99% exact, the five declined being exactly
those arms.

## Measured crates so far

| crate | obligations | exact | declined |
|---|---|---|---|
| bytes | 2199 | 99.68% | 7 cfg-eliminated statements |
| once_cell | 493 | 98.99% | 5 uninhabited match arms |
| proc-macro2 | 2108 | 94.59% | 6 bodies, half from one aggregation question |

proc-macro2's largest remaining family is not a binder defect: `next_ch!`
contains a match and is invoked twice in the same function, so one aggregated
obligation faces two independent MIR structures and only one can be bound.
That is the aggregation semantics decision, recorded with evidence rather than
guessed at.

## Containment must be a fallback, not an equal alternative

`http` exposed a defect in the wave-9 collapsed-span fix. That change admitted
a switch whose range is contained in the condition's range, to handle spans
that collapse to a point — but it admitted containment *simultaneously* with
exact matches. In `HeaderMap::find` the condition's own switch sits at
13349..13402 and a nested switch sits at 13349..13357, so both matched and the
pair failed as ambiguous.

Exact matches now win outright; containment applies only when nothing matches
exactly. The uniqueness requirement still fails closed on real ambiguity. This
is worth remembering as a general rule for the binder: a relaxation added for
one shape has to be ordered below the precise rule it backs up, or it competes
with it.

http: 3657 obligations, 90.32% exact.

## Marker-first has a prerequisite

Marking every match group pre-borrow — the direction that would eliminate the
degenerate-span family (28 occurrences in syn, derived PartialOrd, macro-bodied
arms) — was attempted and reverted. Two findings:

`synthetic_groups` feeds guard-condition markers as well as arm markers, so
widening both records a guard condition twice. Only arm marking should widen;
that part works.

The blocker is that the pre-borrow binder is weaker than the span planner for
macro-generated authored groups. `generated_match` binds today by span but
reports "0 structurally valid arm chains" once marked, because its spans
collapse to the macro body and chain walking cannot recover arm order. So
marking everything currently makes some groups worse. The prerequisite is for
`synthetic_match_candidates` to bind collapsed authored groups as reliably as
the span planner does.

## Miner run 8: 157 crates, and what the ranking says

Unblocking http, log and the CTFE marker path let the miner reach 157 crates
in one pass, up from 23. At that scale the ranking is decisive rather than
anecdotal, and it says the remaining work is concentrated in two families:

- **350 occurrences** of "match arm entry edge was not found", across axum,
  axum_core, h2, hyper_util and tracing. This is the macro-aggregation
  question: a macro containing a match, invoked more than once in one function
  body. All invocations are one obligation, they lower to independent MIR
  structures, and only the first can be bound. proc-macro2's `next_ch!` was
  the first instance; it is plainly a common idiom.
- **59 occurrences** in `either` of the degenerate-span family, which needs
  the marker-first prerequisite.

Everything else is a long tail: winnow decision probes at 39, tracing
statement probes at 31, and so on.

The useful conclusion is that the biggest remaining obstacle to universality
is not a binder defect at all. It is a semantic decision about what a
twice-expanded macro's branch means — one number covered when either
expansion takes it, or one number per expansion site — and no amount of
binder work substitutes for making it.

## Marker-first, measured

The dual-span candidate fix (candidates offering both the expanded span and
the callsite, mirroring the wave-9 try-operator defect) unblocked the
marker-first experiment, and two further prerequisites fell out of trying it:
guard conditions must keep the narrow marking rule or they get recorded twice,
and marker survival must be required only for arms that still exist in the
pruned obligations, because markers are placed before rustc prunes the arms it
proves unreachable.

With all three in place the widening was measured rather than assumed:

| crate | before | after |
|---|---|---|
| either | did not compile | 350 obligations @ 94.57% |
| once_cell | 98.99% | 100.00% |
| bytes | 99.68% | unchanged |
| log | 90.24% | unchanged |
| proc-macro2 | 94.59% | 86.62% |

It was reverted for the last row. The regression is six authored match groups
that the span planner binds today and the pre-borrow chain walk cannot, so
marking them makes them decline. Marker-first remains the right direction —
it eliminates a 59-occurrence family and takes once_cell to 100% — but the
pre-borrow binder has to be a full replacement for the span planner first, and
the exact fraction must never regress for any crate.

Those six groups are now the specification for the next step, and they are
reproducible in seconds via the pm2-repro crate.

## The never-misbind invariant gets its first automatic check

Until now the invariant that outranks everything was held by reasoning and by
fail-closed uniqueness at each binding site. That is not enough on its own: a
misbind produces confident wrong numbers rather than no numbers, so the
fail-closed paths never exercise it, and every relaxation added for one shape
widens the space where one could hide.

Decision bindings now carry structural post-conditions. No condition may select
the same block for both outcomes, and no two conditions may claim the same
switch edge — two conditions can only share a switch if they are the same
condition, so a repeat means at least one binding is wrong. They run on every
compile, including the corpus.

They pass on the fixture and on bytes, once_cell, proc-macro2, http, log and
either. A clean pass is exactly the kind of result that can be vacuous, so the
check is proven by fault injection (`SUPERCOV_RUST_FORCE_MISBIND`) and gated in
both directions: strict binding fails naming the duplicated edge, and lattice
mode declines the body, so a *suspected* misbind never produces numbers.

Chasing that also exposed a latent trap in the older fault injection: an empty
`SUPERCOV_RUST_FORCE_UNBINDABLE` made `definition.contains("")` true for every
body, silently declining everything. Empty now means unset for both injections,
matching the strict-binding flag.

Still open for this invariant: the execution differential against
`-C instrument-coverage` counts, which is the only check that would catch a
misbind whose structure is locally plausible.
