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
| value probes | `rust_value_probes.rs`, read by `js_weak_mutation::rust_valued` |

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

- `MatchArm` is infected by proxy where its arm was selected.
- `MatchArmGuard` reads the guard's decision: infected where it had the other
  outcome, not infected where every evaluation had the forced one, not
  reached where it never ran.
- `BinaryOperator` `==` to `!=` and back, and deleting `!`, are infected
  wherever they ran.
- `FnValue` with `()` is infected by proxy where its function ran.

A run measured with `SUPERCOV_MUTANTS=1` also places value probes
(`rust_value_probes`), and lists its mutants as it ends, linked to them
(`mutant_list::list_rust_run`, `mutants.json` in its evidence). A probe
records one outcome per evaluation, as a hit of `<site>#<outcome>` that the
snapshot turns into a comparison record, never a covered point:

- **Comparisons** (`<`, `<=`, `>`, `>=`): the operands' relation (`lt`,
  `eq`, `gt`, `unordered`). Each replacement's value follows from it, so the
  mutant is infected where some relation gives the two operators different
  values, and not infected where none does (`a < b` to `a <= b`, where the
  operands were never equal).
- **Arithmetic, bitwise and shift operators**, plain and compound: for each
  of the operator's mutants, whether it gives the original's value
  (`same`, `differs`). An operation that panics in a test build (overflow,
  division by zero, a shift past the width) is a value of its own, so `+`
  that overflows and `*` that does not differ.
- **`-x`**: whether `x` is zero (an integer's `0`; any float counts as
  differing, as `-0.0` is not `0.0` to every operation).
- **Functions returning `bool` or a built-in number**: what they returned
  (`0`, `1`, `-1`, other) and whether they ended by a panic. A value other
  than the replacement's infects. One that matches proves nothing: the
  replacement also skips everything the body does, so it stays `unknown`.

Each probe site keeps a state of its own (a `static` the site declares,
of the runtime's `State` type, so it names no path that only some editions
have). Once more evaluations cannot change the weak verdict of any of the
site's mutants in this process (a comparison whose operands were less, equal
and greater; an operator each of whose mutants differed once; a negated
non-zero; a function that returned two different values; any operand the
probe cannot read), the probe returns at once there. A test runs in a
process of its own, so the state is that test's.

The runtime reads an operand by its type's name (`std::any::type_name`),
and skips without comparing names any type named in more than five
characters:
a built-in integer, float, `bool` or `char`; anything else records
`unknown`. No probe has a trait bound, so it compiles wherever the operands
can be borrowed. An operator's probe evaluates its operands a second time,
so it goes only where they are pure: paths, fields, indexes, literals,
casts, dereferences, parentheses and arithmetic of those, never a call. A
literal takes the other operand's type, as the operator gives it. Probes go
only where the instrumenter places probes at all: not in macros, test code,
const contexts or an allocator. A returned value's wrapper goes around every
wrapper the instrumenter placed at its range, and inside every one around it
(`outer_scope`); where no nesting is possible it is left out.

A plain run has no probes and lists its mutants when asked, linked to
nothing but its points: its weak verdicts are those above, and every other
mutant that ran is `unknown`.

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
- An error at no switch is found by halving: the switched mutants are
  added back half by half until the one that breaks the build is found, and
  it is built on its own. One such error: deleting an arm of a `match` whose
  only `_` arm has a guard (cargo-mutants deletes arms wherever a `_` arm
  exists) leaves the match without the arm that made it exhaustive, and the
  compiler's error is at its scrutinee. That error is in the mutant's own
  file; no error at no switch is known in another file, so the halving does
  not narrow its suspects to the errors' files. Where the build fails with
  no mutant switched, the project itself does not build, and the pass ends
  saying so.

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

On 16-CPU machines (one per job), 2026-11-22:
cargo-mutants 27.1.0 with `-j 8`, in shards of a crate's mutants; Supercov
built from this tree, one job per crate, the strong pass with its default 8
workers, from a run measured with `SUPERCOV_MUTANTS=1`. Every mutant of the
three crates, 832 in all:

| Crate | Mutants | Agree | Soundness violations | Viability disagreements | cargo-mutants (s, shards summed) | Supercov strong pass (s) |
| --- | --- | --- | --- | --- | --- | --- |
| strsim-rs v0.11.1 | 363 | 363 | 0 | 0 | 60.3 (4 shards) | 15.7 |
| rust-hex v0.4.3 | 104 | 102 | 0 | 2 | 74.2 (2 shards) | 22.5 |
| semver 1.0.27 | 365 | 362 | 0 | 3 | 43.0 (4 shards) | 15.8 |

Weak statuses, without and with the value probes (no mutant the probed run
calls not-infected or not-reached is one cargo-mutants catches: 0 weak
soundness violations on all three):

| Crate | Plain run: infected / not-infected / not-reached / unknown | Probed run: infected / not-infected / not-reached / unknown |
| --- | --- | --- |
| strsim | 35 / 0 / 17 / 311 | 279 / 2 / 17 / 65 |
| hex | 7 / 0 / 8 / 89 | 36 / 0 / 8 / 60 |
| semver | 65 / 0 / 31 / 269 | 182 / 13 / 31 / 139 |

The measured run's cost, each from a cold build of its own copy (seconds,
16-CPU machines, 2026-11-22, before the probes' length check and the
`open` gate below):

| Crate | `cargo test` | Supercov | Supercov with value probes |
| --- | --- | --- | --- |
| strsim | 0.44 | 0.72 (1.66×) | 0.78 (1.80×) |
| hex | 6.37 | 6.13 (0.96×) | 6.47 (1.02×) |
| semver | 0.63 | 1.15 (1.84×) | 3.41 (5.44×) |

The same, warm: each kind run once to build, then seven times, the median
(seconds, 16-CPU machines, 2026-11-22, this tree):

| Crate | `cargo test` | Supercov | Supercov with value probes | Probes over Supercov |
| --- | --- | --- | --- | --- |
| strsim | 0.083 | 0.276 (3.34×) | 0.308 (3.72×) | +11% |
| hex | 0.120 | 0.395 (3.31×) | 0.411 (3.44×) | +4% |
| semver | 0.110 | 0.564 (5.12×) | 0.627 (5.69×) | +11% |

Warm, `cargo test` does no build, so a measured run's fixed cost (the copy,
the instrumentation, a process per test) is nearly all of what it adds; on
hex, cold, the build dominates and Supercov costs what `cargo test` does.
The probes add 4-11% to a measured run. Cold, they had nearly tripled
semver's, whose tests call probed comparisons and returns in hot loops; two
changes since bring that to 11%: a probe decides an operand's type by its
name's length first, and a site's probe is called behind `rt::open(site)`,
one relaxed atomic load, so a site whose verdicts no further evaluation can
change reads no operands at all (`returned` checks the same load first).

Where semver's evaluations go, from the runtime patched to log each site's
count at every power of two (so each is a lower bound): 24 sites reach 16
evaluations in one test process; together 13,696 evaluations, 8,720 of them
on a site already decided. The hottest, `identifier.rs:175`, a
`returned::<bool>` of `is_empty`, is evaluated 3,136 times across 22 test
processes, 3,104 of them after it decided. The three hottest that never
decide, `identifier.rs:361` (1,040) and `parse.rs:164` (944) and `:177`
(784), read their operands at every evaluation: no test process saw the
outcomes that settle them. A plain run places no probe.

The five viability disagreements, each applied by hand:

- hex `src/lib.rs:177:36` and `178:36`, `+` to `*` in `c - b'A' + 10`:
  `b'A' * 10` overflows `u8`, and rustc's deny-by-default
  `arithmetic_overflow` lint fails cargo-mutants' build. The pass caps lints,
  as a measured run does, so the mutant builds and panics in the tests:
  killed.
- semver `src/identifier.rs:180:55`, `-` to `+` and `/` in
  `PTR_BYTES * 8 - 1`: the shift by a constant past the width is the same
  lint.
- semver `src/identifier.rs:94:67`, `!0` to `0` in a `const` inside a
  function no test calls: cargo-mutants' build fails on const evaluation
  (a null `NonNull`); the plan records it no coverage without building it.

None is a mutant one tool kills and the other calls survived.

## nextest

Under `cargo nextest run`, each test runs through nextest, one test per
invocation (`-E 'binary_id(=..) & test(=..)'`), with the switched build
reused (`--binaries-metadata` and `--cargo-metadata` from a
`cargo nextest list` of it): the project's nextest configuration applies,
its profile, setup scripts and the environment they export, timeouts and
retries. A test nextest ends at its own timeout is a `timeout`.

## wasm

A `cargo test` for `wasm32-unknown-unknown` through
`wasm-bindgen-test-runner`, and `wasm-pack test --node` (learned once as
that `cargo test`), run each test module in Node. A wasm module reads no
environment, so each switched crate's runtime exports a buffer for the
active mutant's file and a function that switches it on
(`__supercov_strong_buffer_<crate>`, `__supercov_strong_activate_<crate>`).
The Node hook the launcher gives every wasm test process
(`rust-wasm-node.mjs`, which also reads coverage regions) calls them as the
module is instantiated, before any test runs. Under WASI (`wasmtime`) the
runtime is given `SUPERCOV_ACTIVE_MUTANT` by name (`--env`). Tests run in a
browser (`wasm-pack test --chrome`) are not run against mutants yet; their
mutants are `error`.

A strong pass records `error`, never `survived`, for a mutant whose tests
passed in the measured run and do not pass without a mutant in the pass's
build: such a pass cannot tell anything of it.

## What is left

- Tests run in a browser (`wasm-pack test --chrome`): their mutants are
  `error`. The page would need the mutant before its module loads, through
  the WebDriver proxy that reads its coverage.
- Rust compiled to wasm and loaded by JavaScript tests (a Vitest or
  Playwright suite over `wasm-pack build` output): its mutants are not run.
  They would need the JavaScript drivers to switch them on through the
  module's exports, as the Node hook does.
- The value probes on `&&` and `||`, struct fields and non-number returns:
  those mutants stay `unknown` where they ran.
