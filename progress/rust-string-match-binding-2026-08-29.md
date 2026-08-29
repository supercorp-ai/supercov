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

## Remaining boundary (next work item)

`visit_map` still fails closed at try-operator selection binding:
serde generates one `next_value()?`-style try per field arm of the key
dispatch, all collapsed to one source, living in PARALLEL match arms — no
CFG order exists among them, so the flat same-source ranking cannot assign
them. The fix is arm-scoped assignment: partition try candidates by the
dominating arm entry of the already-bound enclosing match group and match
them to obligations by their HIR parent arm (the obligation ordinals were
recorded during arm visitation). The same scoping likely applies to the
condition and let-else ranking sites. Until then the dogfood build of
supercov-contracts stays fail-closed at visit_map.

## Gates

- Corpus fixture additions: proc-macro-generated visit_str-like and
  visit_bytes-like matches, including three same-length byte candidates to
  force the multiway tree, with exact selected/not-selected vectors.
- The supercov-contracts dogfood build (the original failure) must compile
  and attribute; the full corpus and workspace gates must stay green.
