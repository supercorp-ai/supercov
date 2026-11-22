# Mutation testing for Rust

Rust implements the contract in `docs/design/mutation-drivers.md`: the
mutants list, the weak report and `supercov mutants test`, for tests run by
`cargo test` (libtest and doctests) and `cargo nextest run`. The reference
tool is cargo-mutants, pinned at 27.1.0 (`mutantSet: "cargo-mutants@27.1.0"`).

| Contract | Rust |
| --- | --- |
| enumerator | `rust_mutants.rs` (cargo-mutants' operator set), linked to the run's points by `rust_mutants::list` |
| schemata | `rust_schemata.rs`, written into a copy by `rust_project::prepare_rust_strong_project` |
| runtime switch | `runtime-assets/rust-strong-runtime.rs` (`on(file, index)`) |
| driver | `rust_strong.rs` (`run_strong_rust`), dispatched by `strong_driver` for `rust` |
| value probes | none yet: weak verdicts read reach, decisions and arm selections |

## Which mutants

`rust_mutants::enumerate` lists cargo-mutants' mutants of a file in its
order, with its spans (1-based lines, 1-based char columns, end exclusive)
and its replacement code:

| Genre | Mutant | Descriptor in the id |
| --- | --- | --- |
| `FnValue` | a function body replaced by a value of its return type (`0`, `1`, `-1`, `true`, `None`, `Ok(Default::default())`, `vec![]`, ...) | the replacement code |
| `BinaryOperator` | `==` and `!=` swapped, `&&` and `||` swapped, `<` to `==`, `>`, `<=`, arithmetic, bitwise, shift and compound assignment operators replaced | the new operator |
| `UnaryOperator` | `!` or `-` deleted | `removed` |
| `MatchArm` | an arm deleted where a `_` arm catches what it matched | `removed` |
| `MatchArmGuard` | a guard made `true`, and `false` | `true`, `false` |
| `StructField` | a field of a struct literal with `..base` deleted | `removed` |

It reads the file with the parser the instrumenter uses (`ra_ap_syntax`),
where cargo-mutants reads it with `syn`, so it repeats what `syn` sees: a
macro's arguments are tokens and hold no mutant; a node starts at its first
attribute or doc comment and never at a plain comment; a function body is
compared with each replacement token by token, as cargo-mutants'
`ToPrettyString` prints both, and a replacement that is the body as written
is left out. What cargo-mutants skips is skipped: `#[cfg(test)]` (only where
the `cfg` is a list of bare names, as `syn`'s `parse_nested_meta` reads it),
any attribute whose path ends in `test`, `#[mutants::skip]` and
`#[cfg_attr(.., mutants::skip)]`, `unsafe fn`, a method named `new`, an
`impl Default`, and calls of `with_capacity` with their arguments.

The goldens in `crates/supercov-engine/tests/fixtures/cargo-mutants-parity`
are cargo-mutants' own list (`scripts/cargo-mutants-parity.mjs` writes
them), and the engine's list is held to them: same mutants, order, spans and
replacement code. On the three validation crates below the lists are the
same, id for id (363, 104 and 365 mutants).

`rust_mutants::list` links each mutant to the run's evidence, only to ids
the run measured: the innermost statement and function around it; a
function body to its function's own point; a deleted arm to its
`match-arm` branch's `selected` alternative; a forced guard to its
`match-guard` decision. A mutant in code the run cannot measure (a `const
fn`) has no link and is reached by every test. A file whose recorded points
its source no longer has changed since the run, and is left out.

## Weak verdicts

`js_weak_mutation::verdict` has a rule per genre (`rust_operator`):

- `FnValue` with `()` is infected by proxy where its function ran; any other
  value is `unknown` there: no probe records what the function returned.
- `MatchArm` is infected by proxy where its arm was selected.
- `MatchArmGuard` reads the guard's decision: infected where it had the other
  outcome, not infected where every evaluation had the forced one, not
  reached where it never ran.
- `BinaryOperator` `==` to `!=` and back is infected wherever it ran; every
  other operator is `unknown` where it ran.
- `UnaryOperator`: deleting `!` is infected where it ran; deleting `-` is
  `unknown` (it changes nothing where the operand is 0).
- `StructField` is `unknown` where it ran.

So a Rust run's weak report says mostly what no test reached, and the strong
pass runs every reaching test for an `unknown` mutant. That is always sound;
comparison and return-value probes would take tests away (see What is left).

## Schemata

The strong pass switches the plan's mutants into a copy of the project
(`.supercov/strong/rust/switched`), only in the files that hold one, and
appends the strong runtime to the root of each crate that compiles such a
file. A mutant is addressed as the contract says, by its file and its index
in the file's list: `crate::<runtime>::on("src/a.rs", 17)` is true only in a
process started with `SUPERCOV_ACTIVE_MUTANT=src/a.rs#17`.

- A function body: `if on(..) { return <value>; }` after its `{` and its
  inner attributes. `return` gives the value the declared return type, as
  the body cargo-mutants writes would.
- An operator, a guard, a struct literal: `(if on(..) { <the expression with
  this mutant only> } else if .. else { <the expression as written, with the
  switches inside it> })`.
- A deleted arm: `<pattern> if !on(..) =>`, so its values fall through to
  the arms after it, as they would with the arm gone.

Rust type-checks every branch. A mutant that does not compile breaks the
build, and the build is read for it (`rust_strong::build_switched`): the
compiler's JSON errors carry byte ranges in the switched files, and the
switcher recorded each mutant's own code and each switch there.

- An error inside one mutant's own code, of a kind that says that code does
  not type-check (E0308, E0277, E0369, E0368, E0599, ...), is that mutant's:
  it is `unviable`, as cargo-mutants reports a mutant whose build fails.
- Any other error at a switch may be the switch's doing (two branches of
  different types, a borrow the duplicated code makes): its mutants leave the
  switched build and are built on their own.
- An error at no switch is found by halving: the switched mutants of its
  file are added back half by half until the one that breaks the build is
  found, and it is built on their own.

The build is then repeated without them, so a pass costs a handful of
incremental builds, not one per mutant.

Some mutants are never switched (`rust_schemata::placeable`): code the
compiler evaluates (`const`, `static`, `const fn`, array lengths, enum
discriminants), a body whose type is an opaque `impl Trait` (a second value
would be a second type), an operator over a `let` chain (an `if` around it
ends the chain), allocator code (the runtime reads the environment once,
which allocates), and files of proc-macro crates (the compiler runs them).
Each is written into a plain copy (`.supercov/strong/rust/plain`) and built
there on its own, as cargo-mutants builds every mutant: unviable where that
build fails.

Lints are capped in both builds (`--cap-lints`), as in a measured run. A
crate that denies a lint cargo-mutants' build would fail on (an overflowing
literal after `-` is deleted, a variable left unused) has such a mutant
unviable under cargo-mutants and run here.

## The runtime

`on(file, index)` reads `SUPERCOV_ACTIVE_MUTANT` once, compares the index
first and the file only where the index matches, and counts the active
mutant's evaluations. Past `SUPERCOV_MUTANT_HIT_LIMIT` (1,000,000 where it
is unset, none where it is 0) it appends the mutant to
`<SUPERCOV_STRONG_RESULTS>.timeouts.jsonl`, writes `SUPERCOV_MUTANT_TIMEOUT`
to standard error and aborts the process, which no `catch_unwind` in the
code under test can carry on from. The module brings in `std` itself, so a
`#![no_std]` crate or one without the prelude builds.

The driver sets the limit to 0 unless the user set one. Each test runs in a
process of its own and is stopped at its time limit, so the limit could
only stop a loop sooner, and could not tell a loop the mutant made endless
from code that evaluates the mutant many times and ends: such a test would
read as a timeout where cargo-mutants, which has no limit of the kind, runs
it to its end.

## The driver

`rust_strong::run_strong_rust` reads `plan.json`, finds each mutant again
in its file by its index (a file whose mutant there has another id changed
since the run: `error`), and builds the switched copy with
`cargo test --no-run --message-format=json`: the command's own Cargo
options, without its test name filters; for `cargo nextest run`, its Cargo
options given to `cargo test`. Then:

- **Baselines.** Every test a mutant needs runs once with no mutant, in the
  switched build. One that fails there is no candidate; a mutant whose
  candidates all fail is `survived`, with that reason. Its time sets the
  test's limit: 3 times the baseline and 10 s.
- **Libtest and nextest tests** run by exact name through the test binary
  (`<binary> --exact <name> --test-threads=1`), each in its own process,
  through the target runner Cargo would use, in the package's directory, as
  a measured run runs them. A mutant's candidates run cheapest first and stop
  at the first that fails: `killed`, `killedBy` that test. Past its limit:
  `timeout`. All passing: `survived`. Mutants run side by side, half the
  CPUs by default (`SUPERCOV_STRONG_JOBS`).
- **Doctests** are compiled by rustdoc against the switched library: a
  mutant's doctests run after its libtest candidates, in one
  `cargo test --doc -p <package> -- <names>` with the mutant on.
  rustdoc's names do not match `--exact`; a name is whole, from its file's
  path to its line, and matches only itself.
- **Mutants built on their own** run after the switched ones, one at a time,
  each against its candidates in its own build.

Tests run in separate processes in the measured run, so a test's evidence is
its own, and running it alone reproduces it: no confirmation of kills is
needed as it is where Vitest runs a file's tests together.

## Validation

Every verdict is checked against cargo-mutants 27.1.0 run over the whole
suite for each mutant (`cargo mutants`, which runs `cargo test` for the
package), with `scripts/cargo-mutants-agreement.mjs`: a mutant cargo-mutants
catches (or times out) that Supercov calls survived or no-coverage is a
soundness violation, and the reverse too. Unviable mutants are counted
apart.

The corpus is under `mutation-corpora/rust/`, cloned at a tag: strsim-rs
v0.11.1, rust-hex v0.4.3, semver 1.0.27. The results are in
"Measured" below.

## Measured

See the release report for the run of 2026-11-22; the figures there are
copied here once the validation runs have finished.

## What is left

- Value probes. Comparison probes on relational operators (recording each
  evaluation's `lt`, `eq`, `gt` as hit points) and return-value probes on
  functions returning `bool` and integers would make most `BinaryOperator`
  and `FnValue` mutants infected or not infected, and take tests away from
  the strong pass. They are not built.
- Rust compiled to wasm and loaded by JavaScript tests, and `wasm-pack test`:
  `wasm32-unknown-unknown` has no environment, so the switch cannot read the
  active mutant. Their mutants are listed and weak-judged; a strong pass over
  a command that builds for wasm records them `error` ("a strong pass cannot
  switch mutants in wasm tests yet") rather than run tests that would pass
  with every mutant.
- nextest's own configuration (per-test environment, setup scripts, its
  timeouts) is not applied: the pass runs nextest's tests through their
  binaries as libtest would.
