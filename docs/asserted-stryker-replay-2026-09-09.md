# Original vs current assertion analysis: frozen Stryker replay

## Answer to the regression concern

The original empirical result is reproducible. Essential SEO still retains
almost all of its measured agreement: **714/827 (86.3%) → 710/827 (85.9%)** under
the prototype's unchanged mutation-prediction rules. Its drop from 563 to 518
contractual `evident` sites was not a corresponding collapse in mutation
agreement. Site counts and mutant outcomes have different denominators.

Supergateway is a different problem. Its historical input contains **zero
assertion-phase files**. The newly imposed exact-passed-witness requirement
rejects every observation, including useful ones the prototype recognized.
All 100 mutant cases involve analysis limits under the current analyzer. This
is a real compatibility/capability regression on that input, not evidence that
the suite asserts nothing. Calling the abstention an accuracy improvement would
be misleading.

The broad safety changes did fix separate adversarial cases, but on this frozen
benchmark they did **not** establish improved overall accuracy. Essential SEO
still has exactly the same 49 false kill predictions, by mutant identity, and
loses four true kill predictions. The earlier gap/limit table did not answer
this question; this replay does.

## What was held constant

- Original prototype at `7d838fa`, including its own `predict` and
  `predictMutant` function source. Those functions are imported into diagnostic
  modules, not replaced with a newly designed score.
- The same saved Stryker fixtures, converted coverage, site inventories, source
  trees and tests for both analyzers. No application tests or mutations ran.
- The exact original comparable cohorts: 100 Supergateway mutants and 827
  Essential SEO mutants. Current unknowns do not leave those cohorts.
- All 661/1,813 sites and 81/527 linked test identities. Source slices were
  checked against every inventory site's text, and source/test and capture
  hashes were checked before/after.
- The unchanged original binary convention: a site with no inferred protection
  predicts survival. This convention is useful for reproducing the historical
  number, but it misrepresents an analysis limit as a confident negative. A
  separate availability view below exposes that problem.

The old extracted analyzer plus the current Rust join reproduces **every
original mutant prediction**, not merely the aggregate percentage. The original
prototype's separately generated report also confirms all four confusion-matrix
cells. This rules out the extraction/port as the cause of these observed losses.

The current original-reference replay has Supergateway cells 88/4/2/6. An older
step-3 document recorded 89/5/1/5. Both sum to 94/100; they are different
prototype revisions, not evidence that the individual predictions never changed.
This comparison consistently uses the available handover reference and records
its source hash.

## Unchanged binary scoring policy

“False kill” means the analyzer predicts that the mutation is detected but the
stored Stryker outcome is survived. “Missed kill” means the opposite. These are
mutation predictions, not a percentage of code proven safe to change.

| Sample / analyzer | Correct / fixed cohort | True kills | False kills | Missed kills | True survivals |
| --- | ---: | ---: | ---: | ---: | ---: |
| Supergateway original | 94/100 (94.0%) | 88 | 4 | 2 | 6 |
| Supergateway current, **forcing limits into survival** | 10/100 (10.0%) | 0 | 0 | 90 | 10 |
| Essential SEO original | 714/827 (86.3%) | 595 | 49 | 64 | 119 |
| Essential SEO current | 710/827 (85.9%) | 591 | 49 | 68 | 119 |

The second row must not be presented as “current assertion accuracy is 10%.”
Its apparent correct survivals are merely a consequence of forcing every
unknown into a negative answer. The current analyzer has no usable positive
signal for this legacy Supergateway capture.

Essential SEO kill precision is 92.39% → 92.34%; kill recall is 90.29% → 89.68%.
For context, an always-predict-killed baseline gets 90/100 and 659/827 (79.7%)
agreement respectively, but falsely claims detection of every survivor. Neither
the baseline nor a single agreement percentage is an adequate assertion metric.

## Unknowns stay visible

For this additional diagnostic view, retain positive predictions, but turn a
negative into unknown if its selected site or a relevant contained removal site
reports an analysis limit. This is a conservative reporting convention, not a
new proof of mutant semantics or a new product score. Positive predictions are
still fallible; a test-gap reason is not itself a certified survival proof.

| Sample / analyzer | Answered | Unknown | False kills | Missed kills among answers | Actual kills left unknown | Correct / original cohort |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Supergateway original | 99/100 | 1 | 4 | 1 | 1 | 94/100 |
| Supergateway current | 0/100 | 100 | 0 | 0 | 90 | 0/100 |
| Essential SEO original | 767/827 | 60 | 49 | 44 | 20 | 674/827 |
| Essential SEO current | 764/827 | 63 | 49 | 45 | 23 | 670/827 |

There is no accuracy-among-answers percentage for the current Supergateway row:
there are no answers. Removing unknowns would make Essential SEO look like
87.7% agreement among answers; that is not an improvement over 85.9%. It is a
different denominator. The result JSON preserves both numbers and the fixed
denominator explicitly.

## Which restrictions caused the loss?

These are **ordered diagnostic ablations**, not independent additive causal
estimates. Modified analyzer modules existed in memory only. No safeguard was
removed from the product.

| Analyzer variant | Supergateway correct / 100 | Essential SEO correct / 827 |
| --- | ---: | ---: |
| Original | 94 | 714 |
| Original without execution-to-observation enrichment | 94 | 711 |
| Above, also without helper-name whitelist | 87 | 711 |
| Current source-flow rules, bypassing only exact witness gate | 87 | 710 |
| Current, all restrictions active | 10 (100 unknown in availability view) | 710 |

Specific losses before the witness gate:

- **Helper-name removal, Supergateway:** seven previously correct kill
  predictions disappear: five in `getLogger.ts`, one in `getVersion.ts`, one
  in `headers.ts`. Their mutant IDs are 41, 42, 43, 44, 60, 80 and 113 in the
  corresponding files. This identifies capability worth recovering through
  declaration- and instance-specific analysis, not a reason to reinstate a
  spelling-based contract.
- **Runtime enrichment removal, Essential SEO:** three disappear, all around
  the missing-table constructor guard in `PrismaSessionStorage.ts:26–27`,
  mutants 315, 317 and 318. The old rule supplied a useful answer here, but its
  general “executed during assertion” shortcut failed the separate discarded
  result controls. A source-grounded constructor/throw path is a candidate for
  recovering this family safely.
- **Other source-flow restrictions, Essential SEO:** one more disappears,
  `app/routes/app.plans/route.tsx:50`, mutant 1345. This ablation groups the
  expression/initializer restrictions; it does not isolate an individual AST
  rule and should not be described as doing so.
- **Exact assertion witness gate:** it removes the remaining 81 true and four
  false kill predictions in Supergateway because the legacy capture cannot
  satisfy it. In Essential SEO it drops observations (1,242 → 1,019) without
  changing any of these 827 binary predictions. Observations and sites outside
  a mutant's effective evidence path need not change its prediction.

This is not a case for restoring all old rules. It is evidence that safety fixes
need **both** their negative regression controls and positive capability checks.
Passing the negative controls while removing most useful observations does not
by itself meet the product goal.

## Scope and oracle limitations

- The fixtures omit original source text and source/test hashes. Current
  inventory/source alignment was verified, but that cannot reconstruct full
  provenance for the historical mutation runs.
- The broad Essential SEO fixture predates subsequently added tests. A false
  kill against that fixture is a disagreement with the frozen historical
  oracle, not automatically proof of a bug in today's dependency analysis.
  Source/test provenance needs checking before fixing any of the 49 cases.
- The Essential SEO fixture has 871 executed mutants; 44 had no original
  prediction and remain separately listed, not silently deleted. If included
  as unknown in an all-executed denominator, correct predictions are 714/871
  and 710/871. The 827 denominator is retained solely to reproduce the
  published comparison. Supergateway has no such outside-cohort cases.
- Excluded statuses remain recorded: Supergateway 56 ignored and 43 compile
  errors; Essential SEO 529 ignored. These were not executed comparable cases.
- The historical convention counts timeouts as kills: five in Supergateway,
  eight in the Essential SEO cohort. Excluding timeouts gives Supergateway
  original 89/95 and Essential SEO original/current 709/819 → 705/819.
- These samples were already used to develop the prototype. They are regression
  calibration, not a held-out estimate of accuracy on arbitrary codebases.
- The original prediction rules themselves are coarse: e.g. a value-level
  link is treated as detecting a value mutation without proving that the
  particular mutation changes the asserted value for a tested input. Matching
  Stryker empirically does not establish arbitrary-change safety.

No global assertion score or perfect-accuracy claim follows from these results.
The public `assertionScore` remains null; candidates remain unverified.

## Artifacts and verification

[Full replay, per-mutant predictions, transitions, exclusions and hashes](asserted-stryker-replay-2026-09-09.json).

Added only calibration/reporting files in Supercov:

- `scripts/asserted-stryker-replay.mjs`: frozen-oracle comparison and in-memory
  diagnostic ablations; refuses to overwrite an existing output.
- `scripts/asserted-stryker-metrics.mjs` and its tests: fixed-denominator
  confusion/availability reporting, including all-unknown behavior.
- `crates/supercov-engine/examples/asserted_join.rs`: internal stdin/stdout
  adapter to the existing Rust join, not a new public command.

Reproduce from the main Supercov checkout, with a new output path:

```sh
cargo build -p supercov-engine --example asserted_join
node --test scripts/asserted-stryker-metrics.test.mjs
node scripts/asserted-stryker-replay.mjs \
  --prototype /Users/domas/Developer/supercorp/supergateway/.claude/worktrees/asserted-coverage-prototype \
  --previous-analyzer /Users/domas/Developer/supercorp/supercov-asserted-typescript-analyzer/analyzers/typescript \
  --second-project /Users/domas/Developer/supercorp/essential-seo \
  --output /absolute/path/to/new-replay.json
```

Node 24.18.0; source/compiler/input hashes and compiler version are saved in the
JSON. The backup extraction must remain the old analyzer: its per-mutant parity
check fails if it stops reproducing the reference. Prototype outputs are
regenerated only in a temporary directory, whose path is recorded in the JSON.

The elapsed fields are diagnostic query costs, **not a speed benchmark**. In
particular the original variant's resolution is preloaded after separately
running the prototype; its tiny per-variant elapsed value is not comparable
with analyzer execution. No test-time instrumentation changed and no runtime
overhead claim was measured here. Nothing was committed or pushed; application
source/tests and existing unrelated work were preserved.

## Next

First recover a meaningful Supergateway measurement: capture its existing full
suite with the **existing** assertion/statement probes, verify successful
assertion records are actually exported, and compare the analyzer on that fresh
input. That is one ordinary test run, not a new mutation campaign. Do not
disable the witness gate to make legacy inputs produce a reassuring score.

Then choose one source-resolved helper or constructor/throw path from the
identified losses, and require recovered true positives alongside the existing
discarded/transformed/unrelated-helper negative controls. Keep this frozen
Stryker replay as a regression check for every change. Audit the provenance and
cause of the unchanged false predictions before calling them new analyzer bugs.
Avoid another broad removal or blanket restoration justified only by site counts.
