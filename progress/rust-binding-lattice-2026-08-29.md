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

## Match bindings get the same treatment

Two arms of one group are alternatives: they cannot both be entered, so they
cannot share an entry block, and an arm cannot list its own entry as a
selection source. Those post-conditions now run on every compile alongside the
decision ones, pass on the fixture and on bytes, once_cell, proc-macro2, http
and either, and are proven able to fire by the same fault injection.

Both halves of the automatic check are gated in strict and lattice directions.
Strict names the duplicated edge or shared block; lattice declines the body, so
a *suspected* misbind never produces numbers — which is the point, since a
misbind is the one failure mode that yields confident wrong numbers rather
than none.

What these checks cannot catch is a misbind that is locally plausible: one
binding, no collision, no self-reference, but attached to the wrong switch.
Only the execution differential against `-C instrument-coverage` counts
reaches that, and it remains the last piece of this invariant.

## Marker widening, targeted correctly

The earlier attempt marked every match group and regressed proc-macro2, so it
was reverted with the finding written down. Returning with that data, the
right cut was narrower: mark **macro-expanded** groups regardless of coverage
eligibility (synthetic-expansion and authored-expansion alike), and leave
plain authored matches to the span planner.

That is principled rather than tuned. Span degeneracy is a property of macro
expansion — collapsed bodies, derives, generated code — while plain authored
code has real distinct arm spans that the span planner handles correctly and
the chain walk cannot. The pre-borrow binder never needed to replace the span
planner wholesale; it needed to cover exactly the cases where spans collapse.

| crate | before | after |
|---|---|---|
| proc-macro2 | 94.59% | 95.54% |
| http | 90.32% | 92.64% |
| bytes | 99.68% | unchanged |
| once_cell | 98.99% | unchanged |
| log | 90.24% | unchanged |
| either | 94.57% | unchanged |

No crate regressed, and the fixture stays strict-green. The general lesson is
that the failed wide experiment was worth more than a success would have been:
it produced the measurement that showed where the real boundary lay.

## Declining is scoped to what the failed phase actually cost

The per-crate baseline made a defect visible that no single-crate run could:
`zmij` declined 368 of 593 obligations — 37.94% exact — from **eleven**
limitation entries, and `either` declined 79 obligations from about five
bodies. The bodies were large and the failures were narrow. Whole-body
declining, the conservative rule adopted when the lattice was introduced, had
become the dominant cost of exactness in the whole corpus, larger than every
binder fix in this file recovered.

The conservatism was not justified. Reading the call sites rather than
reasoning about them shows two distinct failure shapes:

- a **bind** phase that fails degrades its own plan list to `Vec::new()` and
  falls through — the body still instruments its other kinds, and those probes
  still fire;
- an **inject** phase that fails returns the pristine body, so nothing is
  instrumented at all.

The distinction is a property of the call site and not of the phase name: two
`bind ...` sites abandon the body, and two `inject Rust CTFE ...` sites
continue with a partially instrumented one. So `DeclineScope` is passed
explicitly at all 46 call sites rather than derived from the phase text, and a
new call site has to state what its failure costs.

Ownership within a body follows the plan that instruments it. A decision owns
the branches carrying its outcome, its loop back edge and its logical
selections; a match group owns its arms' branches and guard decisions. Those
are declined with their owner and stay measured without it.

| crate | before | after |
|---|---|---|
| zmij | 37.94% | 63.07% |
| either | 77.43% | 96.86% |
| build_script_build | 17.14% | 50.71% |
| tracing_attributes | 88.56% | 96.72% |
| serde_json | 91.98% | 98.71% |
| http | 90.32% | 93.25% |
| proc_macro2 | 94.59% | 97.49% |
| syn | 94.01% | 96.32% |
| serde_core | 95.13% | 97.65% |
| serde | 95.79% | 97.76% |
| tracing_core | 97.72% | 98.99% |
| tracing | 95.62% | 95.89% |

Twelve crates gained, none regressed, across all 18 measured.

This change declines *less*, which is the direction that can turn an
unmeasured obligation into a reported-uncovered one — the wrong number the
whole design exists to prevent. So it is proven rather than argued.
`SUPERCOV_RUST_FORCE_UNBOUND_DECISIONS` fails only the decision bind phase of
a chosen body, reproducing the exact shape that dominates the corpus, and the
lattice gate requires that the same body's statements, match group and arm
branches stay measured while the decision and the one branch it owns are
declined. Two obligations declined out of eleven, where the old rule declined
all eleven.

The first run of that gate passed for the wrong reason — a stale cargo target
directory served a cached rlib, so the wrapper never ran and the manifest was
the previous one. Same false-green mechanism as earlier in this work; a gate
that cannot recompile must fail loudly rather than read whatever is on disk.

## Why zmij's decisions decline: counters are not the coverage graph

zmij is the worst-exactness real crate at 63.07%, and all seven of its
decision-bind failures read

    coverage block N for rs:decision:... maps to 0 MIR blocks

The binder indexes coverage basic blocks by scanning the body for
`CoverageKind::VirtualCounter` statements. Extending the diagnosis to list
which BCBs *do* carry a counter settled the cause immediately: the missing one
is always a lone interior gap in an otherwise dense run — bcb 25 missing from
`[0..24, 26, 27]`, bcb 7 from `[0..6, 8, 9]`, bcb 43 from `[0..36, 50..59]`.

Nothing was optimised away and the mapping logic is sound. rustc minimises
physical counters: a BCB whose count follows arithmetically from other
counters carries no counter statement at all. Counter statements are a lossy
projection of the coverage graph, and the binder was treating them as the
graph itself. These decisions are compiled and do execute, so this is not the
unmeasurable class — it is exactness being lost to a wrong assumption.

The obvious repair does not survive contact. Deriving the uncounted side from
the unique two-way switch that reaches the known target — requiring the
recovered block to be entered by that edge alone — gains nothing and produces
wrong bindings: three of the seven then failed the misbind post-condition with
`condition 1 bound the same switch edge (1, 2, 3) as condition 0`. A
decision's conditions may legitimately share a target, since `a && b` sends
both false edges to the same block, so a whole-body search finds a switch
belonging to a different condition. The post-condition caught it, which is the
invariant earning its keep, but plausible is not provable and it was reverted.

The first-principles direction is to reconstruct the coverage graph the way
rustc builds it, from the MIR CFG, rather than infer it from counters. That
yields every BCB, not just the counted ones, and it comes with its own proof:
every BCB that does carry a counter must land on that counter's block. Making
that agreement a hard post-condition — decline the body if the reconstruction
disagrees anywhere — is what turns the recovery from plausible into provable.

## Cost, not frequency, decides what to fix next

Two waves in a row ranked work by how often a limitation fires and were wrong
both times. The `either` shape — `match arm entry bbN has no external incoming
edge` — fires 66 times and looks like the largest remaining blind spot, but it
costs 11 obligations: the generic `AsMut<Target>` impl is instantiated for many
targets that share one authored source, so all 66 failures collapse onto a
handful of obligation identities.

Attributing declined obligations to limitations by definition name gives the
ranking that matters. Cost first, occurrences second:

| cost | count | family |
|---|---|---|
| 1107 | 114 | bind pre-optimization match probes |
| 1010 | 40 | bind Rust decision probes |
| 673 | 15 | bind Rust statement probes |
| 645 | 18 | bind Rust logical-selection probes |
| 587 | 108 | NOT_COMPILED statements (legitimately unmeasurable) |
| 368 | 49 | inject pre-optimization match probes |
| 259 | 3 | inject Rust decision probes |

Three `inject Rust decision probes` failures cost more than the entire either
family, because an inject failure returns the pristine body and declines
everything in it.

## Injection phases rewrite the edges later phases recorded

Those three failures are one shape, and it is not a binding defect.
`Punct::new` matches a 22-alternative or-pattern arm, and its decision plan
recorded the true edge `bb0 -> bb2`. By the time decisions are injected:

    bb0 now targets [bb8 x22, bb1], whose own successors are
    [(bb1, [bb4]), (bb8, [bb7])], reaching bb2

Twenty-two identical targets is a match-arm bridge — match injection correctly
installs one block recording "this arm was taken" for all 22 entry edges — and
the original target now sits behind a chain of our own bridges. Decisions are
injected after match arms and loop frames, so the phase looks for an edge that
an earlier phase already rewrote, and gives up.

The first hypothesis, a single intervening block, was tested and refuted: the
one-hop reachability set came back empty. The diagnosis was extended a second
time rather than a fix guessed at, which is what turned "edge was not found"
into a cause.

The repair needs no heuristic, because we create every bridge ourselves: keep a
map from each inserted bridge to the edge it replaced, and have later
injections resolve recorded edges through it, requiring the resolved chain to
consist only of blocks we inserted and to terminate at the recorded target.

## The injection phase that rewrote its own edge

Three inject-decision failures cost 259 obligations — the most per occurrence
of any family, because an inject failure returns the pristine body and declines
everything in it. Finding the cause took four diagnostic rounds, and rounds
three and four each moved the fix to a different function than the previous
round implied. That is worth recording, because every round in between looked
like enough to act on.

1. "condition 0 true edge from bb0 was not found" — no cause, just a symptom.
2. Testing for a single intervening block returned an empty reachability set,
   refuting the obvious reading.
3. Following the bridge showed a chain, `bb0 -> bb8 -> bb7`, and 22 identical
   targets — the signature of a match-arm bridge.
4. Snapshotting the CFG after each phase showed the edge surviving both match
   arms and loop frames untouched:

       [("before injection", [bb2 x22, bb1]),
        ("after match arms", [bb2 x22, bb1]),
        ("after loop frames", [bb2 x22, bb1])]

`instrument_runtime_decisions` rewrites it itself. Two decision obligations
legitimately claim the same CFG edge; the first injected redirects it, and the
second finds nothing to replace and declines the whole body.

A first repair was built on round three's reading — a bridge map populated in
`instrument_runtime_matches` — and reverted: it compiled, changed nothing, and
was keyed in a function that turns out to be uninvolved. The working fix is
local to one function: record `(source, replaced_target) -> bridge` at the
existing replacement site, and when a later plan finds nothing to replace, walk
that map until the source targets the result, bounded by the map size.

This is semantically right rather than merely effective. When two obligations
observe one edge, both probes *should* fire, and a keyed chain makes them fire
in order on exactly that edge and no other. The unkeyed alternative — walking
appended blocks until the chain reaches the target — was rejected: a bridge
deliberately collects every edge between its pair, so it would place a probe
where edges the decision never claimed also arrive. It would have worked in
`Punct::new`, where all 22 edges share an outcome, and been wrong in general.

| crate | before | after |
|---|---|---|
| syn | 96.32% | 97.88% |
| proc_macro2 | 97.49% | 97.77% |

No crate regressed across the 18 measured. The gain landed with the exactness
ratchet as its guard rather than a dedicated fixture: which source construct
makes two decision plans share an edge is not yet established, and encoding
that guess as an assertion would be worse than recording the gap. Task #32
carries the method for establishing it.

## A regression that was the report becoming honest

Statement-probe binding aborted on its first failure. That looked like a
tidiness question and was a wrong-number defect: when the first failing
statement was unmeasurable, `runtime_statement_plans` returned `Err`, so the
caller emptied the plan list and *no* statement probe was injected anywhere in
that body, while `degrade_unbound_obligations` saw the UNMEASURABLE marker,
declined only the obligation it named, and returned before applying
`DeclineScope::Statements`. Every other statement in the body was therefore
uninstrumented, never fired, and was reported as **uncovered**. Live
behaviour: tracing_attributes carried 19 such statements, serde_core 10,
bytes 8.

Binding every statement that can be bound and declining only the failures
repairs it, and is worth a great deal besides — zmij 63.07% to 77.57%,
build_script_build 50.71% to 70%, http, tracing_core, tracing and three more.

The defect was found only because the fix was reverted first. The ratchet
reported bytes, serde_core and tracing_attributes going backwards, and the
change was withdrawn on that signal. Diffing the limitation families showed
every added decline was `RUST_OBLIGATION_NOT_COMPILED`: aborting on the first
failure meant only the *first* uncompiled statement per body was ever
discovered, so the exact fraction fell precisely because the report had become
more honest. All three crates carry zero `UNBOUND` statement declines, so no
part of those decreases can be a binder failure.

The lesson is about the gate, not the binder. The ratchet exists to enforce
the north star's first property, and it could not distinguish *losing
exactness* from *gaining honesty* — so it vetoed a correctness fix and would
have vetoed the next one. It now records per-crate uncompiled counts and
permits a decrease only when newly declared uncompiled constructs account for
it, reporting `honesty:` rather than `REGRESSION:`. Until the baseline stored
those counts the exemption compared against zero and was permissive by
construction; the re-baseline made it real.

When a gate blocks a change, the gate is a hypothesis too.

## One rewrite, two stages, five wrong turns

Logical-selection binding aborted on its first failure, declining every branch
obligation in the body — 645 across the crate set. The failures themselves are
correct: `cfg!(feature = "full") && input.peek(..)` has a constant left
operand, rustc folds it, and no branch region exists. Only the collateral was
wrong.

Declining per branch instead gained syn and http and collapsed
build_script_build from 70% to 37.86%, through a single new
`inject Rust decision probes` limitation — an inject failure returns the
pristine body, so one limitation cost 45 of 140 obligations.

Finding out why took five attempts, and every wrong turn came from reading
part of a diagnostic that was complete on the first run:

    condition 0 true edge from bb86 to bb87 was not found;
    bb86 targeted [("before injection", [bb88, bb87]),
                   ("after match arms", [bb139]), ...];
    bb86 now targets [bb139], whose own successors are
    [(bb139, [bb138, bb137])], reaching bb87

Edge invalidation in general, refuted by inspection. Match bridges, refuted by
measurement when a cross-phase bridge map changed nothing. The two
`mem::replace` relocation sites, refuted by checking their enclosing functions
— one is never called from this path, the other runs after the snapshot that
already shows the change. Then a relocation map alone, which also changed
nothing.

`instrument_runtime_matches` applies **two** rewrites, and a recorded edge can
be subject to both. Arm bridging interposes a block on an edge. The
selection-start split then takes the whole terminator out of the block and
pushes it into a fresh one, leaving a call behind. So `bb86 -> bb87` became
`bb86 -> bb139 -> bb13x -> bb87`: resolving only the relocation stops at
`bb139` where `bb87` is still not a successor, and resolving only the bridges
never leaves `bb86`, which no longer holds a switch at all.

The order matters and is not symmetric. Arm bridging runs first, so its keys
are stated in terms of the block as it was *before* the split — the target
resolves against the original source. The split runs second, so the source
resolves last. Reversed, the lookup finds nothing.

| crate | before | after |
|---|---|---|
| build_script_build | 70.00% | 71.43% |
| syn | 97.92% | 98.43% |
| http | 94.86% | 95.05% |

The trailing `reaching bb87` clause named the second stage from the very first
run. It was read past three times. The lesson is not about this binder: a
change that measures neutral on its own may be half of a fix rather than a
wrong one, and the cross-phase bridge map was exactly that.

## FAST, measured for the first time

The north star's third property had no numbers attached to it at any point in
this work. It does now, and they are a long way from the target.

Cold `cargo build -j 4` of a 13-dependency probe crate (serde_json, serde,
syn, quote, proc-macro2, tracing, bytes, http, either, once_cell, log, itoa,
memchr), fresh target directory on both sides:

| wrapper | wall | user | vs baseline |
|---|---|---|---|
| none | 4.38s | 8.81s | — |
| debug build | 28.58s | 68.98s | 6.5x |
| release build | 21.35s | 52.05s | **4.9x** |

The target is ≤1.10x. Optimising the wrapper moved 6.5x to 4.9x, so the
unoptimised driver explained part of the gap but nowhere near all of it.

User time grows faster than wall time — 8.81s to 52.05s against 4.38s to
21.35s — so the cost is CPU spent in the wrapper rather than serialisation.
Parallelism is not the bottleneck; the per-body analysis is.

What this does **not** measure, and none of it is known: test execution
overhead with probes firing, the warm content-addressed cache path that the
north star relies on for "warm runs in seconds", a large workspace, the
release profile, or the cost of per-test attribution across threads and
subprocesses. Four separate budgets, one of them now measured.

The honest position is that ≤1.10x cold compile is not close, and it is not
yet known whether it is reachable or whether the gate wants restating in terms
the architecture can meet — cold build overhead X, warm build near zero,
runtime overhead Y. That is a decision to make on evidence once the warm path
and runtime overhead exist, not before.

### Warm and incremental

| scenario | baseline | instrumented | overhead |
|---|---|---|---|
| cold, everything from scratch | 4.38s | 21.35s | **4.9x** |
| warm, no-op rebuild | 0.05s | 0.04s | none |
| incremental, local crate edited | 0.06s | 0.08s | *not measured* |

The no-op result matters: cargo's own caching means an unchanged rebuild pays
nothing, so 4.9x is a once-per-clean-build cost rather than a per-invocation
tax. That is a materially different thing from a 4.9x tool.

The incremental row is honest about being empty. The probe crate's `lib.rs` is
a one-line stub, so editing it recompiles almost nothing and instruments
almost nothing — the numbers are real and mean nothing. What a developer
actually feels is recompiling *their own substantial crate* while its
dependencies stay cached, and that measurement does not exist yet. It is the
one that decides whether the overhead is tolerable in practice, and it needs a
crate with real code in it.

Nor does any of this cover test execution with probes firing, which is a
separate budget again.

### Own-code cost: the number that matters, and it is bad

The 4.9x cold figure was measured on a crate whose own source is a one-line
stub, so it is dominated by dependency compilation. A developer feels the cost
of instrumenting *their own* code. Measured on a generated crate of 3,600
lines — 400 functions, each with an if/else chain, a loop with a match, and a
guarded match — single crate, no dependencies:

| | baseline | instrumented | overhead |
|---|---|---|---|
| wall | 0.46s | 37.75s | **82x** |
| user | 0.41s | 22.23s | |
| sys | 0.05s | 15.50s | |

Two things stand out. The overhead scales with obligation density, not with
line count, so the dependency-heavy measurement understated it badly. And 15.5
seconds of *system* time against 22.2 of user time points at per-body I/O
rather than analysis alone — something is hitting the filesystem far more than
the work requires.

The generated code is deliberately branch-dense and real code will be less so,
so 82x is an upper bound rather than a typical figure. But the earlier 4.9x
was a lower bound for the same reason, and the truth for a normal crate sits
between them, unmeasured. What is now clear is that the cost is not a fixed
per-crate constant: it is proportional to how much there is to measure, which
is exactly the code a user cares about.

This reframes FAST from "document the cold cost" to a genuine engineering
problem. The sys-time share is the first thing to look at, because I/O per
body is the kind of overhead that is usually structural rather than
algorithmic.

### Where the time goes

Artefacts written for that 3,600-line crate:

| artefact | size |
|---|---|
| `manifest-*.json` | 8.5 MB (~2.4 KB per source line) |
| `*.jsonl` trace | 0.9 MB |
| `sources-*.json` | 0.17 MB |
| target directory | 49 MB, against 8.4 MB baseline |
| `libsupercov_runtime.a` | 17 MB, linked into **every** instrumented crate |

The 17 MB static archive is the first suspect: it is linked per crate, so a
workspace pays it once per member, and it plausibly explains both the system
time and the 6x target bloat. The 8.5 MB manifest is the second — that is real
serialisation work per crate, and 2.4 KB of manifest per source line invites
the question of whether obligations carry redundant fields or the JSON is
pretty-printed. The 22 seconds of user time is third, and only worth attacking
once the I/O is understood.

None of this is optimisation work yet. It is the first time the cost has been
attributed to anything at all.

### The 82x, decomposed

Running the wrapper with instrumentation off, and again with instrumentation
on but no static runtime directory, splits the cost cleanly:

| configuration | wall | user | sys |
|---|---|---|---|
| no wrapper | 0.46s | 0.41s | 0.05s |
| wrapper, instrumentation off | 0.56s | 0.27s | 0.10s |
| instrumented, no runtime archive | 26.67s | 16.62s | 10.01s |
| instrumented, archive linked | 37.75s | 22.23s | 15.50s |

| stage | cost | share |
|---|---|---|
| wrapper process overhead | ~0.10s | negligible |
| instrumentation analysis (user) | 16.6s | 44% |
| instrumentation I/O (sys) | 10.0s | 26% |
| linking the 17 MB runtime archive | 11.1s | 29% |

The wrapper itself is free — being a rustc driver costs nothing measurable.
The cost is three roughly comparable pieces, and two of them are not analysis:
linking a 17 MB static archive into the crate, and 10 seconds of filesystem
work during instrumentation, against an 8.5 MB manifest for 3,600 lines of
source.

That is an encouraging shape. Better than half the overhead is I/O and
linking, which are usually structural — a smaller or dynamically linked
runtime, and a leaner manifest format — rather than algorithmic work that
would need the binder rewritten. The 16.6 seconds of analysis is the part that
would be genuinely hard, and it is the minority.

### One HIR walk per body, not twelve

Profiling the 400-function crate put 86% of samples under the pre-optimization
phase, with `runtime_body_obligations` -> `HirManifestCollector::visit_expr`
the dominant frame beneath it. That function re-runs the entire HIR walk on
every call, and twelve call sites ask for it — every plan builder and every
degrade path.

Memoising it per body was an immediate 3.5x, and the ratchet rejected it:
`log` fell 90.24% to 89.16%, with smaller moves in four other crates. Diffing
`log`'s manifest found `set_logger_racy` gaining `match arm ... has no
authored MIR`.

The cause was a design flaw the cache merely exposed. `collect_body_obligations`
ended with `prune_unreachable_match_arms`, which reads `UNREACHABLE_MATCH_ARMS`
— a set that *grows* as later bodies are bound. So the function was never a
pure function of the body: it mixed a stable HIR walk with a step whose result
depends on when it runs. Caching froze whichever view existed at the first
call.

Separating the two fixes it properly. The walk is cached; pruning happens
fresh on every call, against the current set. `log` returns to byte-identical
output — 36 declined, zero limitations added or removed — and the ratchet
reports exactness held or improved across all 18 crates.

| fns | before | after |
|---|---|---|
| 100 | 4.17s | 1.74s |
| 400 | 24.85s | 7.93s |

With the archive fix, cold overhead on the 400-function crate goes 82x to 26x.

The lesson is not about caching. A function that reads mutable global state is
not a function of its arguments, and nothing in its signature says so. The
cache did not introduce the bug; it made an existing ambiguity observable.

### The quadratic: a file's identity, verified once per obligation

Profiling the 800-function crate was unambiguous where four rounds of
reasoning had not been:

    619  stable_source_range
    177  normalized_path
    124  ExactSourceSnapshot::eq
    117  ExactSourceSnapshot::clone

`stable_source_range` runs for every recorded span. On each call it
materialised the whole file's text, cloned it into an `ExactSourceSnapshot`,
and compared that snapshot against the stored one — three full passes over the
source, per obligation. With tens of thousands of obligations against a single
file that is quadratic in crate size, and it accounted for both the growth in
user time and a large share of the system time.

A file's identity needs verifying once, not once per span.

| fns | before | after | user before | user after |
|---|---|---|---|---|
| 400 | 7.85s | 4.16s | 4.67s | 1.20s |
| 800 | 23.14s | 7.30s | 17.56s | 2.37s |

The scaling result is the important one. Analysis time now doubles when the
code doubles — 1.98x from 400 to 800 functions, against 3.9x before — so
overhead is roughly 13x and *flat* in crate size, where it previously grew
32x, 53x, 83x. A tool whose overhead grows with the codebase is unusable on
the codebases that most need it.

Four hypotheses were wrong before this: memory (peak RSS 232 MB, 76 page
faults), manifest writing (8 MB is milliseconds), the static archive, and a
clone of the unreachable-arm set (empty in this workload, so it early-returns).
`sample` found the answer in a single run. On this codebase, measurement has
beaten reasoning every time they disagreed.

### The number that matters: 1.08x on a real dependency tree

Every figure above came from generated crates: one crate, no dependencies,
400 or 800 functions each carrying an if/else chain, a loop with a match and a
guarded match. That is a stress shape, not a workload. Measured on a real tree
— syn with `full`, serde_json and regex, `cargo build -j 4`, fresh target
directories both sides:

| | baseline | instrumented | ratio |
|---|---|---|---|
| wall | 4.97s | 5.37s | **1.08x** |
| user | 7.16s | 12.12s | 1.69x |
| sys | 0.86s | 2.41s | 2.80x |

Wall clock is 1.08x, inside the north star's ≤1.10x gate. The synthetic 13x
and the real 1.08x are both true and measure different things: a single
branch-dense crate compiled alone has no parallelism to hide the analysis,
while a real dependency tree compiles many crates at once and rustc's own work
dominates the critical path.

Two caveats that must travel with the number. CPU time is 1.81x combined, so a
machine with no spare cores — a constrained CI runner — would feel closer to
that than to 1.08x. And this measures compiling *dependencies*, which a user
pays once; their own crate is nearer the dense case, though real code is far
less branch-dense than the generator.

The honest summary is three numbers, not one: about 1.08x wall on a realistic
cold build, about 1.8x CPU, and warm rebuilds free. Publishing a single
multiplier would repeat exactly the merged-number mistake this project refuses
everywhere else — and the first figure measured tonight, 4.9x, was already
misleading for that reason.

## Wave: negated condition chains, and the decomposition invariant (#36)

`flatten_decision_expression` had no arm for `Unary(Not, ..)`, so `!(a || b)`
fell through to the atomic case and was recorded as a *single* condition. That
is a live wrong MC/DC number: a decision with two operands reported as one.

The naive fix — recurse through the negation — made it worse, and the exactness
ratchet caught it as −0.42 on `build_script_build`. Diffing obligation ids
rather than trusting the percentage showed what the percentage could not: two
obligations **vanished** (140 → 138) while the declined count stayed at 40.
They left the manifest without being declined.

The cause is at main.rs:1361. A decision is dropped whole (`return None`) when
any of its atomic conditions is an external-macro expansion — sound for the
case it was written for, hidden control flow inside `assert!` or `println!`.
But decomposing

```rust
!(err.kind() == ErrorKind::NotFound
    || (cfg!(target_os = "linux") && err.raw_os_error() == Some(ENOTEMPTY)))
```

exposes the `cfg!` operand, so an **authored** `if` was taken out of the
denominator because a sub-expression came from a macro. A vanished branch is
strictly worse than a declined one: it is a silent coverage hole, and we would
report 100% on code containing a real, unmeasured branch. This is the same
family as the uncovered-vs-unmeasured defect fixed in 413d2e2.

Falling back to atomic fixed the vanish but left one decline, which exposed a
second finding worth more than the fix: **there are two independent collectors
of short-circuit operators.** `flatten_decision_expression` claims the ones it
owns in `decision_logical_expressions`; a separate HIR visitor at main.rs:2511
records a logical-selection branch for every `&&`/`||` *not* so claimed. The
fallback discarded its selections without claiming them, so the visitor
resurrected them as orphans — and a `cfg!`-folded operand has no MIR switch to
bind to, so one could never bind. The two collectors disagreed, and the
disagreement was the decline.

The fix keeps both consistent. The drop predicate is extracted as
`external_macro_condition` and shared by both sites. The `Not` arm decomposes
into scratch vectors and splices only if the result survives that filter;
otherwise it falls back to atomic *and* records the discarded short-circuits in
a `subsumed` list the caller inserts into `decision_logical_expressions`.

The invariant this establishes, which outranks the decomposition itself:

> **Decomposing must never cost a decision.** A better MC/DC number is never
> worth losing a branch. When decomposition cannot bind, record the decision
> atomically — a merged number is honest and measurable; an absent one is not.

Evidence, `scratchpad/not-chain` under strict binding, 0 declined:

| function | conditions | selections |
| --- | --- | --- |
| `negated_or` | 2 | 1 |
| `negated_and` | 2 | 1 |
| `negated_chain_with_cfg` | 1 | 0, decision-outcome branch preserved |

Ratchet: 18 crates, zero regressions, `proc_macro2` +1.18, `syn` +1.00,
`build_script_build` +0.31 — the crate that regressed under the naive fix now
gains, because claiming subsumed short-circuits also retired pre-existing
orphan declines.

Two process notes. The percentage said "−0.42, regression"; only the id diff
said "two obligations disappeared", which is a different and more serious
claim. Diff identities, not aggregates. And the chain script reported exit 0
while dying at stage four: zsh treats an unmatched glob as a fatal error, so
`rm -f target/debug/*.libtest.json` aborted the script when no bundle was
present. Only the `CHAIN COMPLETE` check caught it — the exit code was from the
backgrounding wrapper, not the chain. Replaced with `find -delete`.

## Wave: match arms entered at function entry (bb0)

A match arm entered unconditionally on function entry has its body dominated by
`bb0`, the MIR entry block, which has no incoming edge. The binder places arm
probes on the edge *into* the arm, found none, and returned `Err` for the whole
body — declining every match obligation in it, not just that arm. It was the
largest family of unbound diagnostics: 98 of 295 across the 18-crate set,
concentrated in `either` (66) and `syn` (21).

The instrumenter already knew the move. Its function-entry probe clones `bb0`
into a fresh block and replaces `bb0` with a call targeting it — but at the
injection stage, long after planning has failed. Doing the same split *before*
planning gives the arm exactly the external incoming edge the binder wants.
Only bodies that need it are split: the entry block is load bearing elsewhere,
since observation kind is keyed off whether a block is `bb0`.

Family eliminated, 98 → 0, no crate regressing. Worth stating plainly:
**declined counts did not move**, because the affected bodies also fail for
other reasons that remain. This retires a false failure and makes the surviving
diagnostics legible; it is not an exactness win on its own.

## Investigation: per-invocation obligation identity (#22, not landed)

The defect is real. `scratchpad/twice` invokes one `macro_rules` body three
times; at HEAD that is a single obligation with `defs=[first,second,third]`, so
exercising `first` alone credits `second` and `third` — coverage reported for
code that never ran.

Three identities were measured, each against two repros and the 18-crate
ratchet. The second repro mattered most: `scratchpad/inbody` expands one macro
*twice inside a single body*.

| identity | twice | inbody | serde_json |
| --- | --- | --- | --- |
| full expansion chain | 3/3 bound | 2 obligations, both declined | 61.09% |
| def path + owner ordinal | 3/3 bound | still split | — |
| **def path only** | 3/3 bound | merges, matches HEAD | 98.74% |

The first variant failed for a reason worth recording: **the binder matches
obligations to MIR constructs by source range**, and two expansions inside one
body share that range exactly. Handed two indistinguishable obligations it
fails, and scope degradation then declines the body's *whole* scope — which is
why 755 `authored-source` obligations declined in serde_json, obligations the
change never touches. That also explains the arithmetic that first looked
impossible: declines grew more than obligations did (+1763 vs +1378). Pure
splitting cannot do that; collateral scope damage can.

Def-path identity is the correct one, and it still cannot land: `either` goes
96.86% → 67.75%. That regression is honest exposure, and the signature proves
it — obligations 350 → 645 (+295) against declines 11 → 208 (+197). Declines
growing *less* than obligations is what real splitting looks like, the exact
inverse of variant one. 136 of the declines are `authored-expansion` and only 5
`authored-source`, so it is not scope collateral.

What blocks it: 67 of `either`'s 72 unbound messages are "bind pre-optimization
Rust match probes". Macro-expanded match arms bind when one obligation stands
for every invocation and fail when each body must bind its own. Fix that first,
then land def-path identity — the same sequencing that worked for bb0. Then
honesty improves *and* the exact fraction does not regress, instead of trading
one for the other.

## Wave: match arms that share one macro body fragment (#28)

A macro that writes its body fragment once and expands it into several arms
gives every arm the identical body span. `either`'s `for_both!` is the case in
the wild — `$result` appears twice, once per arm. The binder selects an arm's
MIR blocks by that body range, so both arms resolved to the *same* block set,
computed the same entry block, and the misbind post-condition correctly refused
two arms entering one block, declining the whole body's match plans. It was 67
of `either`'s 72 unbound diagnostics, and http's `$body` case is the same
family.

The first fix attempt was to refine each arm's block set to those dominated by
blocks carrying the arm's own *pattern* range. Instrumenting it killed the idea
cleanly: the pattern block set comes back **empty**, because pre-optimization
MIR carries the scrutinee span on the test blocks, not the arm patterns. Spans
cannot separate these arms at all — only the CFG can.

So arms now carry `pattern_variant`, derived exactly as `if let` decision
conditions already derive theirs, and when an arm shares its body span with a
sibling the binder keeps only the blocks reached by that arm's `SwitchInt`
target. Arms with distinct body ranges stay on the existing path.

Deliberately rejected: matching arm order to switch-target order. Enum variant
order need not follow arm order, and that assumption is precisely what the
misbind check exists to catch.

No crate regressed: http 95.05% → 97.29% (+2.24), either 96.86% → 98.29%
(+1.43), tracing_attributes +0.34, syn +0.26. Corpus obligation-weighted
exactness 98.19% → 98.54%, declined 689 → 557.

### What it unblocked

#22's def-path identity was held at `either` 67.75% (−29.11) entirely by this
shape. With it fixed, `either` now *gains* +0.62 under per-definition identity,
and the remaining regressions are small: serde_core −3.80, tracing −0.65, syn
−0.51, proc_macro2 −0.44, http −0.25, serde_json −0.03.

serde_core is the one that matters, and its shape says honest splitting rather
than breakage: obligations 1787 → 7768 (+5981) against declines 44 → 486
(+442). Declines growing far slower than obligations is the inverse of the
full-expansion-chain variant, where declines *outran* obligation growth because
scope degradation was taking unrelated authored code down with it.

That leaves #22 as a judgement rather than a bug: the exact fraction falls
because the denominator became more honest, so the before and after percentages
measure different things and the ratchet cannot adjudicate between them. The
recommendation is to keep closing the newly exposed gaps first — 46 of
serde_core's messages are "inject pre-optimization Rust match probes" — and
land the identity change with no regression at all, which is exactly how this
wave unblocked `either`.
