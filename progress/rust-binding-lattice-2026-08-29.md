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
