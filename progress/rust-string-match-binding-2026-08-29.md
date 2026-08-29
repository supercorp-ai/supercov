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
failed closed at derived `PartialEq::eq` logical selections: rustc treats
`#[automatically_derived]` (and `#[coverage(off)]`) functions as
coverage-ineligible (`rustc_mir_transform::coverage::query`), so their
decisions can never bind through native branch mappings. The wrapper now
mirrors that exact predicate: decisions in functions where
`tcx.coverage_attr_on` is false carry structural markers, like CTFE owner
kinds.

The predicate landed after the full corpus proved every derive fixture binds
structurally with identical exact vectors.

That unmasked visit_map's structural conditions, now fully bound by two
further extensions (verified on the repro; corpus verdict pending at this
note's commit):

- Structural PATTERN binding for `while-let` conditions: a refutable pattern
  condition selects through a two-way discriminant switch, not a typed
  Boolean switch. The pairing collects those switches into a separate class,
  and the post-borrow marker resolution accepts them for while-let decisions,
  discriminating true/false by the loop back edge (the matching variant's
  edge reaches the switch again; the refuted edge exits).
- Arm-scoped structural conditions: the eight duplicate-field
  `Option::is_some` checks live in parallel key-dispatch arms with no CFG
  order, exactly like the try operators. Decisions now carry
  `parent_match_arm`, and the pairing applies the same demand-driven claiming
  (bound arm entries claim dominated switches when they have conditions;
  leftovers rank sequentially).

## Third wave: value-position selections (implemented; supercov-contracts clear)

Derived `PartialEq::eq` is a value-position `&&` chain: no control decision
exists, so its logical selections are standalone branches that previously
bound only through rustc's native branch mappings — which coverage-ineligible
functions never receive. The selection binder now falls back structurally
exactly when `function_coverage_info` is absent AND `coverage_attr_on` is
false: each selection's left operand IS a typed Boolean switch findable by
the selection's exact `mapping_source` (span or callsite), its value edges
are the evaluated/short-circuited alternatives per the and/or discriminator,
and same-source groups rank under `semantically_before` with strict counts.

With that, `supercov-contracts` compiles fully instrumented — every serde
visitor shape, every derived impl, all decisions, matches, tries and
selections bound exactly.

## Fourth wave (repro-verified; corpus verdict pending at this note)

- The Serialize boundary at `AgentError` was `skip_serializing_if`: two tries
  in parallel if/else branches, not match arms. `semantically_before` gained
  a third relation: parallel branches of one switch follow MIR lowering
  order, which mirrors source order (then before else, arms in order) —
  implemented as successor-block-index order under the nearest common
  dominator switch, which is inversion-proof.
- A one-field struct's serde key dispatch is itself a two-variant
  discriminant switch and polluted the while-let pattern pool. Conditions now
  capture their let pattern's ADT (`DecisionCondition.pattern_adt`), and a
  pattern-class switch qualifies only when it tests one of the source's
  condition pattern ADTs.

## Remaining boundary (next work item)

The dogfood now advances past all of the above and stops in a NEW PHASE:
derived `PartialOrd::partial_cmp` for `coverage_report::SourceLine` fails in
the pre-OPTIMIZATION match probe binder — "match arm … entry bb0 has no
external incoming edge". This is the post-borrow instrumentation of a
structurally-bound match group whose arm entry degenerates (optimizes/merges
to the function entry) in optimized MIR. Diagnose with the standalone repro
loop (`#[derive(PartialOrd)]` struct) and the pre-optimization plan builder
before changing anything.

## Gates

- Corpus fixture additions: proc-macro-generated visit_str-like and
  visit_bytes-like matches, including three same-length byte candidates to
  force the multiway tree, with exact selected/not-selected vectors.
- The supercov-contracts dogfood build (the original failure) must compile
  and attribute; the full corpus and workspace gates must stay green.
