# Verification

Supercov rewrites your source before running it. That is a strong claim to make
about someone else's production code, so the burden of proof sits with the
instrumenter: it has to demonstrate, on every release, that the rewritten
program behaves exactly like the original and that the coverage it reports is
arithmetically correct.

Seven independent gates block publication. A failure in any one of them stops
the trusted-publishing workflow.

## 1. Semantic differential execution

Original and instrumented programs are executed in isolated scopes and compared
on three axes: return values, thrown errors, and the observable order of side
effects.

The fixtures deliberately target the places where a naive transform breaks:
getters, proxies, optional calls and `this` binding, computed logical
assignments, parameter defaults, `try`/`catch`/`finally`, iterator closing,
switch fallthrough, labelled loops, async functions and generators.

## 2. Deterministic generated corpus

A generated corpus exercises 160 nested combinations of short-circuiting,
ternaries, coercion and thrown expressions on every run. It is deterministic, so
a regression reproduces exactly rather than appearing once in CI and never
again.

## 3. Property testing

Seeded `fast-check` properties generate a further 500 nested expressions and 300
control-flow executions per run, with shrinking and a reproducible seed printed
on failure.

## 4. Coverage oracles

Behaviour equivalence is not enough — the numbers have to be right too. Separate
oracles assert exact decision vectors, MC/DC witnesses and branch alternatives
independently of what the program does.

## 5. An independent MC/DC implementation

The same three-condition masking-MC/DC golden cases must produce identical
verdicts under Supercov and under Clang/LLVM source-based MC/DC: 100% for a
complete witness set and 33.33% for an incomplete one.

This is the gate that matters most. MC/DC has enough subtlety — masking versus
unique-cause, short-circuit evaluation, compound conditions — that agreement
with an independently implemented, widely audited toolchain is far stronger
evidence than any self-consistent test suite.

## 6. TC39 Test262

Release CI shards the pinned Test262 corpus across 16 workers, runs the official
harness against original and instrumented sources, and rejects any scenario that
passes originally but fails after transformation.

Some categories are excluded by construction, with reason counts printed for
every shard:

| Excluded | Why |
| --- | --- |
| Module, async and raw tests | Not comparable under the source-rewrite harness |
| Parse and resolution negatives | The transform never runs on unparseable input |
| Annex B sloppy-script extensions | Does not apply to the application modules Supercov instruments |
| `Function.prototype.toString` and function-source coercion | Exact source reflection necessarily observes a source transform |

The last category is handled in the product, not hidden: when application code
directly coerces or observes a function's source, Supercov leaves that body
uninstrumented and records a visible `semantic-safety` completeness blocker.
Dedicated differential fixtures cover the async and generator cases that Test262
cannot compare.

## 7. Performance budgets

Transform latency, transactional workspace preparation, output expansion and
runtime probe overhead are each checked against explicit budgets. A change that
makes instrumentation correct but unusably slow fails the same way a wrong
answer does.

## Cross-platform and compatibility gates

Alongside the seven correctness gates, the compatibility workflow runs Node 22,
24 and 25, Playwright 1.55 and current, Vite 5 and current, Vitest 2 and
current, Chromium, Firefox and WebKit, and modern JavaScript, JSX, TypeScript
and TSX syntax fixtures.

Filesystem publication, symlink handling, copy fallback, `ENOSPC`, failed
rename and forced-termination recovery are all exercised on Ubuntu, macOS and
Windows.

A clean-room gate packs the npm tarball, invokes it through `npx` in a project
with no build step, and asserts that not a single source or configuration file
changed.

## Running the gates yourself

```sh
npm test
npm run test:clang-mcdc
npm run benchmark:check
TEST262_DIR=/path/to/test262 npm run test:test262
```

The Clang/LLVM oracle requires `clang` and `llvm` to be installed. The Test262
gate requires a checkout of the pinned corpus.
