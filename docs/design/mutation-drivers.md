# Mutation drivers: what a language provides

Weak mutation (`docs/design/mutants.md`) and the strong pass
(`docs/design/strong-mutation.md`) were built for JavaScript first. Their core
is language-neutral: one mutant list, one plan built from any run's per-test
evidence, and a file protocol between the plan and the driver that runs it.
This record is the contract a language implements to get the mutants list,
the weak report and `supercov mutants test`.

The code: `crates/supercov-engine/src/mutant_list.rs` (the list),
`strong_plan.rs` (the plan), `strong_driver.rs` (the dispatch and the
environment), `js_weak_mutation.rs` (weak verdicts, for every language), and
the CLI's `mutants_test_command.rs`, which plans, hands each language's
mutants to its driver and saves the verdicts.

## What a language provides

1. An enumerator that writes `mutants.json`.
2. Mutant schemata in its instrumenter, behind strong mode and
   `SUPERCOV_STRONG_FILES`.
3. A runtime switch that reads the active mutant and counts its evaluations
   against a hit limit.
4. A driver that runs `plan.json` and writes `results.jsonl`.
5. For infection (optional): value probes, recorded per test.

Without 5 the plan still works, from reach alone: every test that ran a
mutant's code is a candidate for it.

## 1. The enumerator and its JSON

A run measured with `SUPERCOV_MUTANTS=1` (or in strong mode) carries
`mutants.json` at the top of its evidence archive. It lists every
instrumented file's mutants:

```json
{
  "schemaVersion": 1,
  "mutantSet": "mutmut@3.3.0",
  "mutantSets": { "python": "mutmut@3.3.0" },
  "languages": { "src/cart.py": "python" },
  "files": {
    "src/cart.py": [
      {
        "mutator": "EqualityOperator",
        "replacement": ">",
        "start": [2, 8],
        "end": [2, 17],
        "original": "sum >= 50",
        "statement": "src/cart.py:2:5",
        "function": "src/cart.py:1:1"
      }
    ]
  }
}
```

- `files` keys are paths from the project root, with `/`. A mutant's place
  in its file's list is its **index**: the number its switch carries. The
  order is the enumerator's and must be the same every time the same source
  is enumerated.
- `languages` names each file's language where it is not JavaScript;
  `mutantSets` names each language's mutant set (the reference tool and its
  pinned version) where any language other than JavaScript is listed, and
  then names every language listed. `mutantSet` is the one set, or the sets
  joined with `, `. A list of JavaScript alone writes neither map, as every
  list did before. Language names: `javascript`, `python`, `rust`, `ruby`,
  `go`, `jvm`.
- `id` is left out where it is
  `<file>:<startLine>:<startCol>-<endLine>:<endCol>:<mutator>:<replacement>`,
  which a reader works out again. Lines are 1-based, columns as the
  reference tool reports them, so ids compare with its report.
- `mutator` and `replacement`: the reference tool's mutator name and a
  descriptor of the replacement, unique among the mutants of one span and
  mutator. Use StrykerJS's names (`EqualityOperator`,
  `ConditionalExpression`, `ArithmeticOperator`, `BooleanLiteral`,
  `BlockStatement`, ...) where the mutation is the same: the weak rules
  below are keyed by them.
- `statement` and `function`: the ids of the innermost statement point and
  function point of the run's manifest that contain the mutant. **Reach is
  read from them**: a test ran the mutant's code when its hits include its
  statement (and its function, where one contains it). A mutant with
  neither is reached by every test. A function body, which no statement
  holds, is reached by its function.
- `equivalent` (`{ "rule", "reason" }`): a static rule shows no input makes
  it differ. `untestable`: a hint that its operands are clock or random
  readings.
- `static: true`: its switch is evaluated while the module loads.
  `unplaceable: "<reason>"`: it has no switch; the driver reports it
  `error`.

Merged runs merge lists file by file (`MutantsFile::merge`); a language
listed under two sets does not merge.

**Listing after a run.** `supercov mutants` on a run measured without
mutants lists them from the sources the run measured, so a plain run can be
planned: `mutant_list::list_from_sources` routes each measured file to its
language's enumerator by extension. A language adds its arm there; the
enumerator instruments the file again with nothing written, and gives the
file's mutants only where its point ids are the ones the run recorded (else
the file changed since the run, and is named so).

## 2. Schemata

In strong mode the instrumenter writes every listed mutant into the code
behind a switch and places no coverage or value probe. Strong mode is on
where `SUPERCOV_STRONG=1`, and only the files listed in the JSON array at
`SUPERCOV_STRONG_FILES` get switches; every other file is left as written,
with no runtime import. Both are part of the instrumented copy's cache key.

A switch is an expression or statement that reads the active mutant's index
for its own file each time it is evaluated: the original where the index is
not one of its mutants', a mutant's replacement where it is, counting the
evaluation (below) before the replacement runs. Where a mutant cannot be
wrapped without changing the program (a declaration, a place the language
needs a literal), a switch goes on the nearest enclosing node that can be,
with the mutant applied to a copy of it; where none can, the mutant is
`unplaceable`. Each replacement must be the reference tool's own, byte for
byte where the tool prints it.

## 3. The runtime switch and the environment

| Variable | Meaning |
| --- | --- |
| `SUPERCOV_STRONG=1` | instrument with switches |
| `SUPERCOV_STRONG_FILES` | path of a JSON list: the only files that get switches |
| `SUPERCOV_STRONG_PLAN` | path of `plan.json` |
| `SUPERCOV_STRONG_RESULTS` | path of `results.jsonl` |
| `SUPERCOV_ACTIVE_MUTANT` | `<file>#<index>`: the mutant on in this process from its start; `-` or unset for none |
| `SUPERCOV_MUTANT_HIT_LIMIT` | evaluations of the active mutant allowed in one process (default 1,000,000) |

The runtime reads `SUPERCOV_ACTIVE_MUTANT` before any instrumented module
runs, so module-level code is mutated too, and passes it to child processes
through the environment. A driver that switches mutants inside one process
(between test files whose modules are evaluated afresh) needs the same
state through an API; JavaScript's is `setActiveMutant(file, index)`,
`clearActiveMutant()`, `setMutantHitLimit(n)`, `mutantHitCount()`.

**The hit limit.** Each evaluation of the active mutant's replacement
counts; the count starts again when the active mutant changes. Past the
limit the switch raises the language's timeout error, whose code (or, where
the language has none, message) contains `SUPERCOV_MUTANT_TIMEOUT`, so a
loop the mutant made endless ends where it runs. Because code under test
may catch that error and carry on, the runtime also appends one line to
`<SUPERCOV_STRONG_RESULTS>.timeouts.jsonl`:

```json
{"mutant": "src/cart.py#3", "attemptId": null, "pass": "main"}
```

A driver reads that file: a mutant named there is a `timeout`, whatever its
tests did.

## 4. The driver

`strong_driver::run_strong_pass(language, ...)` runs a language's share of
the plan; `has_driver(language)` says whether there is one. A language adds
its arm to both. Mutants of a language without a driver are recorded
`error`, "no strong mutation driver for <language> yet".

The driver is handed the run's command (or the one given after `--`), the
project root, and two files.

**`plan.json`** (`StrongPlan`):

- `tests`: every test of the run, `{ id, file, name, title, runner,
  durationMs, runnable }`; `id` is the test id the runner's hooks recorded,
  and `runnable` is false where any attempt failed, was skipped or was
  flaky in the measured run.
- `mutants`: the mutants to run, `{ id, file, language, index, mutator,
  weak, candidates, wholeFiles, covering, ... }`. `candidates` are indexes
  into `tests`, cheapest first, each with a reason; `wholeFiles` are test
  files that must run whole for the mutant (its code ran while the file's
  modules loaded). A mutant with `decided` set is not run (the CLI records
  it), and the CLI hands a driver only undecided mutants of its language.

**What the driver does with each mutant**, as `strong-mutation.md` describes
for Vitest:

- Runs it against its candidates only, grouped by test file, the cheapest
  file first, with the mutant switched on before the file's modules load.
  Its other files run only if it is still alive.
- Stops at the first failing test: the mutant is `killed`, `killedBy` that
  test.
- Runs each test file first without a mutant (its **baseline**), with the
  union of the candidates it will run. A test that fails there is no kill.
  A file that fails its baseline with a subset runs whole from then on; a
  file that cannot finish without a mutant makes its mutants `error`.
- **Confirms a kill** where the mutant's run did not run the same tests
  before the killing one as the baseline did: the same tests run again
  without the mutant, and where the killing test fails that way too the
  kill is overturned and the mutant runs again with its file whole.
- Gives each run a time limit from its baseline (3× and 10 s); a run past it
  is stopped, and the mutant is a `timeout` unless a test had already
  failed. A mutant in the timeouts file is a `timeout`.
- Runs candidates under its own runners only. A mutant whose candidates are
  all under another runner is `error`, never `survived`.

**`results.jsonl`**: one line per mutant as soon as it has a verdict,

```json
{"id": "src/cart.py:2:8-2:17:EqualityOperator:>", "status": "killed",
 "killedBy": "test_cart.py::test_free_shipping", "tests": 2,
 "durationMs": 840, "reason": null}
```

with `status` one of `killed`, `survived`, `timeout`, `error`; `tests` how
many tests ran against it; `reason` a short sentence where the status needs
one. A driver may add a line `{"summary": true, ...}` about itself (the CLI
shows it under `passes`). A mutant with no line when the driver returns is
run again in a further pass (at most six), and after that recorded `error`.

The driver returns the command's exit code. Progress lines, the gate and the
saved verdicts are the CLI's, the same for every driver.

## 5. Value probes, for infection

Weak mutation decides, from one ordinary run, which mutants each test
infected. The plan uses the same verdict one test at a time: a test whose
own evidence shows the mutant's value never differed cannot kill it, and is
left out. Without values every reaching test is a candidate; with them the
strong pass runs a fraction of the tests (on the JavaScript corpus, a
quarter fewer).

What `js_weak_mutation::verdict` reads, per test record (`RuntimeSnapshot`
in the run's evidence):

- `hits`: point ids. Reach.
- `comparisons`: `[{ "id": <site>, "outcomes": [...] }]`. Two kinds of site:
  - a **comparison** site, named in the mutant's `compare`
    (`<file>:<line>:<col>`), on a relational operator: each evaluation's
    operand relation, `lt`, `eq`, `gt`, or `unordered`. It decides
    `EqualityOperator` boundary mutants (`>=` to `>`) and
    `ConditionalExpression` mutants of a comparison.
  - an **infection probe** site, named in the mutant's `probe`
    (`<file>:<line>:<col>-<endLine>:<endCol>#<kind>`): the probe evaluates
    the original expression and the replacement side by side, with the same
    operands, and records `same` or `differs`. Any mutator with a `probe`
    is decided by it: `differs` infects, only `same` does not, no record
    means the expression never ran. The probe must not evaluate an operand
    twice where that has an effect; where it cannot, it places no probe and
    the mutant stays `unknown`.
- `decisions` and `logicals`: a decision's condition vectors and a logical
  operator's outcomes, read through the mutant's `decision`, `branch` and
  `chain` links, as JavaScript records them for MC/DC.

A mutant whose mutator has no rule and no probe is `unknown` wherever it was
reached: a candidate for every test that reached it. That is always sound;
values only take tests away.

## JavaScript as the reference implementation

| Contract | JavaScript |
| --- | --- |
| enumerator | `js_mutants.rs` (StrykerJS 10.1.1's mutators), linked by `js_mutant_links.rs` |
| schemata | `js_schemata.rs` |
| runtime switch | `runtime/javascript/runtime.mjs` (`strongSlot`, `setActiveMutant`) |
| drivers | Vitest (`vitestStrong*.mjs`), Playwright (`playwrightStrong*.mjs`), and the other runners (see `strong-mutation.md`) |
| value probes | `js_instrumenter.rs` (comparison and infection probes) |

Rust implements the contract with cargo-mutants 27.1.0 as its reference:
`docs/design/rust-mutation.md`.

## Rust compiled to wasm under a JavaScript host

A Rust crate's mutants can only be told apart by the tests that load its
wasm: a Vitest or Playwright suite over `wasm-pack build` output, or
`wasm-bindgen-test-runner` in Node or Chrome. A wasm module reads no
environment, so a crate switched for a strong pass (`rust_schemata`) exports,
per crate (`<key>` is the crate's):

| Export | Meaning |
| --- | --- |
| `__supercov_strong_buffer_<key>() -> u32` | where in the module's memory to write the active mutant's file (UTF-8, at most 4096 bytes) |
| `__supercov_strong_activate_<key>(length: u32, index: u64)` | switches mutant `index` of the file just written on; the hit limit is off |

A host that knows the active mutant (`<file>#<index>`) calls, for every
instance it sees created, each `_activate_` export after writing the file
into the matching `_buffer_` (`rust-wasm-node.mjs` and the Chrome proxy's
page script do so for Rust's own runners). It must do so before any test
code of the instance runs: right after `WebAssembly.Instance` or
`instantiate` returns.

For Rust wasm loaded by JavaScript tests, the JavaScript strong pass owns
the run: the mutants are listed by the Rust run beside it, and its candidate
tests are JavaScript tests.

1. Dispatch: a Rust mutant all of whose candidates ran under a JavaScript
   runner goes to the JavaScript driver (`strong_driver::driver_of`); one
   with a Rust test among its candidates stays Rust's.
2. Build: in a strong pass, each Cargo workspace of the copy that holds a
   wasm crate gets the plan's Rust mutants behind switches
   (`rust_strong::switch_wasm_workspace`, through
   `rust_project::prepare_rust_strong_project`) instead of probes, before
   the command's own build runs. Which mutants compile is found first in a
   scratch copy with `cargo build --lib --target wasm32-unknown-unknown`, as
   the Rust driver finds it: one whose own code does not type-check is
   `unviable`; one a switch cannot hold, or whose switch breaks the build,
   is `error` (the command builds the wasm once). Those are taken out of the
   plan the JavaScript driver runs.
3. Activation: the test processes get `SUPERCOV_WASM_STRONG`, a JSON list of
   the workspaces' directories in the project (`""` at its root). The
   runtime remembers every wasm instance (`globalThis.__supercovWasm`) and
   hands each the active mutant, its file relative to its workspace, as it
   is created and whenever `setActiveMutant` switches another on; a mutant
   of no workspace (a JavaScript one) switches every crate's off. The Rust
   runtime's active file can change in one instance, as a Vitest worker
   keeps its modules from one mutant to the next.

A wasm mutant's hit limit is off: a loop it makes endless is stopped by the
job's time limit. Not yet: wasm loaded in a browser page (Playwright, Vitest
Browser Mode) gets no mutant, and a mutant a switch cannot hold is not built
on its own.

## Python

| Contract | Python |
| --- | --- |
| enumerator | `python_mutants.rs` (mutmut 3.8.1's operators), parity goldens in `tests/fixtures/mutmut-parity` |
| schemata | `python_strong.rs` writes each switched function's mutants' edits; `runtime/python/supercov_strong.py` switches the function to mutmut's copy of it |
| runtime switch | `supercov_strong.py` (`strong_slot`, `activate`), `SUPERCOV_ACTIVE_MUTANT` from a process's start |
| driver | `supercov_strong.py`: pytest's `pytest_runtestloop`, unittest's `TestProgram.runTests`; a fork per mutant after collection |
| value probes | `supercov_instrument.py` (`_MutantProbes`) and `supercov_runtime.py`, from the plan's `mutantProbes` |

See `python-mutants.md`.
