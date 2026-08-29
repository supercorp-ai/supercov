# Synthetic string/byte match binding — 2026-08-29

## Problem (found by R3 dogfood on supercov-contracts)

serde-derive visitors contain `match value { "contractVersion" => …, … }`
(visit_str) and byte-string equivalents (visit_bytes). Pre-borrow synthetic
match binding failed with "0 structurally valid arm chains" for two distinct
reasons, each proven from `mir_built` dumps and wrapper debug output:

1. **Pattern spans.** serde spans generated string patterns at the authored
   field/variant identifiers, so chain FalseEdges do not carry the collapsed
   group source. Fixed: `MatchArmSelectionObligation.pattern_source` captures
   each arm pattern's owned stable range at HIR time, and an arm's chain edge
   may match either the group source or its own pattern source (strictly
   stronger per-arm binding).

2. **Test-tree lowering.** With several same-length byte-string candidates,
   MIR lowers pattern tests into a shared multiway first-byte switch. The
   candidates' FalseEdge blocks then live in sibling subtrees: no FalseEdge
   reaches the others, and reachability from a test region can drop several
   candidates at once. Source arm order is structurally unrecoverable — any
   order-based chain walk (including the reachability-maximal rule) fails
   closed here.

## Decision: literal-value correspondence

For a collapsed group whose non-wildcard arms are all string/byte-string
literal patterns, bind arms to FalseEdges by the exact literal each edge
accepts, recovered from the MIR tests themselves:

- str patterns: the `<str as PartialEq>::eq` call whose success switch edge
  enters the FalseEdge's region carries the literal as a const operand.
- byte-string patterns: walk the unique predecessor path from the FalseEdge
  back through `switchInt((*scrutinee)[i of N])` blocks, collecting
  `index → byte` from each taken edge value (shared-prefix blocks are linear;
  the diverge switch contributes our path's edge value); require the
  collected indices to cover exactly `0..N`.

The arm↔edge assignment must be a perfect bijection of equal literals, or the
group fails closed exactly as today. The wildcard arm's entry is the
imaginary target of the last non-wildcard source arm's FalseEdge (pattern
matrix order guarantees this regardless of test-tree shape). Guarded arms are
not supported in literal mode (serde generates none); a guarded literal group
falls through to the chain walk and otherwise fails closed. Groups that are
not literal-shaped keep the existing chain walk (with per-arm pattern-source
matching and the reachability link rule for linear test interposition).

HIR side: `MatchArmSelectionObligation.pattern_literal: Option<Vec<u8>>`
captures the literal bytes (str as UTF-8) for string/byte-string literal
patterns. Neither new arm field is serialized into manifests; both are
wrapper-internal binding aids.

## Implemented alongside (same root cause family)

- Desugared matches (`?`, `while let`) are excluded from FalseEdge candidacy
  by span desugaring kind: serde's `tri!`-style expansions emit real MIR
  FalseEdge structures whose spans collapse to the same callsite but are
  never authored groups.
- Groups capture their scrutinee ADT (typeck at HIR); a candidate edge
  qualifies only when its discriminant switch tests that ADT, so an
  `Option`-match group can never bind a `Result` structure.
- Same-source sibling structures are ordered by a strict `semantically_before`
  relation — one-way reachability first, dominance to break the
  mutual-reachability tie inside loops — used by the match-assignment
  recursion and by the try-operator/condition/let-else ranking sites.

With all of the above, `visit_str`, `visit_bytes`, `visit_seq` and every
`visit_map` match group in supercov-contracts bind exactly.

## Second wave (all implemented and verified on supercov-contracts)

- Try operators in parallel match arms bind through demand-driven arm
  scoping: obligations carry their exact lexical arm
  (`BranchObligation.parent_match_arm`), each bound arm entry claims only the
  ControlFlow selections it dominates (excluding deeper bound entries) when
  it has obligations, and the unscoped remainder ranks sequentially.
  Domination alone cannot express lexical containment (a diverging sibling
  arm makes the surviving arm dominate everything after the match), which is
  why scopes are demand-driven rather than structural.
- The assignment search precomputes the strict before-ness matrix and
  ancestor pairs; sibling ordering skips nesting-related pairs (a
  scrutinee-nested child executes before its parent despite the later HIR
  visit order).
- Binding-free integer matches (serde visit_u64) lower to one multiway
  switchInt with no FalseEdges at all; arms bind directly to their exact
  value edges via `pattern_int`, the wildcard to the otherwise edge.
- Nested patterns test several discriminants (visit_enum's `Ok((Field, v))`
  arms switch on Result and Field), so the edge type constraint is the set of
  every ADT in the arm patterns (`pattern_adts`, unioned across aggregated
  recordings), not a single scrutinee type.

## Remaining boundary (next work item)

With all match/try binding complete, the supercov-contracts dogfood build
fails closed at a different subsystem: derived `PartialEq::eq` logical
selections ("rustc did not retain branch mappings for logical-selection
function"). rustc omits native branch mappings for derive-generated code, so
the logical-selection binder needs the pre-borrow structural-marker fallback
that CTFE owner kinds already use. That is the next R3 item.

## Gates

- Corpus fixture additions: proc-macro-generated visit_str-like and
  visit_bytes-like matches, including three same-length byte candidates to
  force the multiway tree, with exact selected/not-selected vectors.
- The supercov-contracts dogfood build (the original failure) must compile
  and attribute; the full corpus and workspace gates must stay green.
