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

cargo-mutants replaces an operator's text, so the compiler reads its mutant
afresh: where the new operator binds otherwise than the old, the expression
around it groups otherwise (`a - b * c` with `*` made `+` is `a - b + c`,
`(a - b) + c`; `p && q && r` with its first `&&` made `||` is
`p || (q && r)`). Such a mutant's site is the whole expression of operators,
casts and ranges it sits in (up to parentheses, a call, a statement), which
its switch replaces with that text, and its weak verdict reads no operand
probe; one whose text groups as the expression with only the operator
changed stays at its own operator (`rust_mutants::regrouped`).

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
literal takes the other operand's type, as the operator gives it. An
operand of literals alone (`1 << 83`) has no probe: it takes its type from
where it stands, and evaluated apart it is an `i32`, which can overflow where
the original does not (itoa's `n < 1 << 83`, `n` a `u128`, failed every
`u128` test of the measured run). Probes go
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
discriminants), a body whose type holds an opaque `impl Trait` anywhere
(`Result<impl Iterator, E>`; a second value would be a second type), an operator over a `let` chain (an `if` around it
ends the chain), allocator code (the runtime reads the environment once,
which allocates), and files of proc-macro crates (the compiler runs them).
So is a deleted arm whose `match` is left with arms that all diverge
(`_ => return None`): the `match` is then of type `!`, and the code that
uses its value may not compile, as cargo-mutants' build finds (regex-syntax
has four); a switch keeps the arm, and its type.
Each is written into a plain copy (`.supercov/strong/rust/plain`) and built
there on its own, as cargo-mutants builds every mutant: unviable where that
build fails.

Both copies and their build directories are removed when the pass ends,
whether it ended well or in an error: they hold no verdict, and a switched
build is a gigabyte or more (rust-hex's was 1.1 GB). Only the driver's plan
and results stay. The next pass builds afresh. A JavaScript pass over Rust
wasm removes its scratch copy (`.supercov/strong/rust-wasm`) the same way
once it has found which mutants compile.

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

## What the plan decides, and what still compiles

The plan decides a mutant no test reaches (`no-coverage`) or none infects
(`survived`) without running it. cargo-mutants builds every mutant first, and
reports one that does not compile as `unviable` whatever the tests would do.
So the CLI gives the Rust driver its plan-decided mutants too (`decided` in
`plan.json`), and the driver switches those a switch can hold into its
build, only to find whether they compile: one that does not is `unviable`
(a body replaced by `Default::default()` where the type has no `Default`);
one that does keeps the plan's verdict, and nothing runs it. One a switch
cannot hold is not built on its own for this: it keeps the plan's verdict
(semver's unreached `const`, below).

**Reached in mutated form.** A process of the switched build appends
`<file>#<index>` to `<results>.reached` the first time it evaluates its
active mutant (`rust-strong-runtime.rs`, once per process). A mutant whose
candidates all pass and that no process reached is `no-coverage`, reason
`not-reached-mutated`: the tests ran code the pass never switched, as a
Worker whose wasm `wrangler dev` built from the sources, or a subprocess
started with a cleared environment, and could not have failed. A merge keeps
such a verdict where no run killed the mutant or let it survive. Not judged
so: a mutant built on its own (no switch), a `wasm-pack test` pass (wasm has
no file to append to), and a mutant resumed from candidates an earlier pass
ran (that pass's reach is not kept).

**Rust Workers.** A Worker a `cargo test` suite starts with `wrangler dev`
is built by wrangler, from the `[build] command` of its configuration
(`worker-build`, or a script that runs it, read one level deep), not by the
pass's `cargo` build. The pass serves it as the measured run does, through
Supercov's `wrangler` on the tests' PATH, and switches it as the switched
build does:

- after the native switched build, the Worker packages are built for
  `wasm32-unknown-unknown` from the same switched copy; a mutant that does
  not compile there leaves the switched build and is built on its own,
  where the test's real `wrangler dev` builds it, and fails, as it would;
- the pass then runs each Worker's `[build] command` once, in the switched
  copy, with Cargo's output in the check's target, and marks it built for
  that switched build. Every test's `wrangler dev` gets a copy of its
  configuration with no `[build]`, whose entry first loads a module that
  switches the test's mutant on in each wasm instance as it is made (the
  runtime's `__supercov_strong_activate_*` exports, as the Node hook does
  for wasm-pack), so a mutant costs no Worker build. A Worker that does not
  build, or whose wasm fails to build through no mutant's fault, is served
  as the tests' own `wrangler` builds it, with no mutant switched on;
- after each call into the module, that module reads the runtime's
  `__supercov_strong_hits_*` export and, the first time the mutant ran,
  logs it; a poller on wrangler's inspector, which the stand-in starts
  beside it, appends the mutant to `<results>.reached`, as a native process
  does. A test that starts a Worker some other way runs it unswitched, and
  its mutant, never reached in mutated form, is `no-coverage`.

A mutant built on its own is served by the real `wrangler`, which builds the
plain copy's mutated source itself.

A mutant in a `const fn` is linked to no statement: the instrumenter
declares a `const fn`'s body (`unmeasured`) and cannot probe it, so its
points are in the run's record and never hit. Linked to one, the mutant read
as code no test ran, though the program calls the function at run time.

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
  test's limit, cargo-mutants' rule: 5 times the baseline, at least 20 s.
  (It was 3 times and 10 s: on fastrand 2.3.0 a mutant that makes rejection
  sampling loop longer ended its test in 14 s, a `timeout` here and missed
  by cargo-mutants.)
- **Libtest and nextest tests** run by exact name through the test binary
  (`<binary> --exact <name> --test-threads=1`), each in its own process,
  through the target runner Cargo would use, in the package's directory, as
  a measured run runs them. A mutant's candidates run cheapest first and stop
  at the first that fails: `killed`, `killedBy` that test. All passing:
  `survived`. Mutants run side by side, half the CPUs by default
  (`SUPERCOV_STRONG_JOBS`), each test process in a process group of its
  own, which its limit or the pass's stop ends whole (nextest's and
  rustdoc's test processes with it).
- **Timeouts, at the machine's speed.** A test past its limit beside the
  pass's other tests decides nothing yet: once every switched mutant has
  run, each such mutant runs again alone, one at a time. Its test first runs
  again without the mutant, and its limit there is 5 times the slower of
  that run and its baseline, at least 20 s; the mutant then runs against the
  candidates it has not passed. Only past that limit is it a `timeout`
  (`ran past 10120 ms, alone, after its test ran again without the mutant`).
  A baseline measured while the machine was quieter (the baselines run
  first, side by side) does not make a slow test a timeout once a CI runner
  is busier; a test that does not pass without the mutant then is an
  `error`. A mutant that never ends costs its limit twice and one run of
  its test. A test stopped at the hit limit counts evaluations, not time,
  and is a `timeout` at once.
- **Stopping.** The CLI writes `stopped-at` beside the pass's results before
  it signals the pass, for `--budget` or a signal alike. From then the pass
  starts nothing, ends the process groups it runs (a build among them), and
  judges nothing they said: their mutants are left for the next pass. It
  ends at once, with the signal's exit code (143 for the budget, which the
  CLI reads as a finished pass). Each candidate a mutant passes is written
  as it passes (`{"progress": true, "id": .., "passed": [..]}`), so the next
  pass runs that mutant against the rest (`docs/mutation.md`, partial
  progress): each test runs in a process of its own, so its passing holds
  whatever runs beside or after it. A test process ended by SIGINT,
  SIGTERM or SIGHUP waits up to 2 s for the stop to show before it is
  judged. Up to this release, the Rust pass did not look for the stop: a
  budget ran 8 s past and ended the command with exit 143, leaving its test
  processes running; a Ctrl-C ended each test process of the terminal's
  group, and the pass saved each of those mutants as `killed`.
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

The corpus is `benchmarks/mutation/rust/corpus.json`: eleven public crates
at a tag (strsim, hex, semver, itoa, smallvec, unicode-width, bitflags,
anyhow, base64, fastrand under cargo-nextest, regex-syntax as a workspace
member), run by `.github/workflows/mutation-rust.yml` on GitHub's runners:
cargo-mutants in shards, `supercov mutants --shard k/n` in shards merged by
`supercov merge`, joined by `benchmarks/mutation/rust/summarize.mjs`. The
table and every disagreement are in `docs/mutation.md` ("Rust, against
cargo-mutants 27.1.0"). Its first runs found the gaps 4.29.18 fixed: value
probes that failed itoa's `u128` tests (33 soundness violations), `const fn`
mutants read as never run (12), a regrouping operator mutant (1), mutants of
`Result<impl Trait, E>` bodies called unviable, unviable mutants no test
reaches listed as gaps, and diverging arms. "Measured" below is the
original three crates' record, on 16-CPU machines.

## Measured

On 16-CPU Linux machines, one per job, 2026-11-22:
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
16-CPU Linux machines (c8a/m8a.4xlarge), 2026-11-22, before the probes'
length check and the `open` gate below):

| Crate | `cargo test` | Supercov | Supercov with value probes |
| --- | --- | --- | --- |
| strsim | 0.44 | 0.72 (1.66×) | 0.78 (1.80×) |
| hex | 6.37 | 6.13 (0.96×) | 6.47 (1.02×) |
| semver | 0.63 | 1.15 (1.84×) | 3.41 (5.44×) |

The same, warm: each kind run once to build, then seven times, the median
(seconds, 16-CPU Linux machines (c8a/m8a.4xlarge), 2026-11-22, this
tree):

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

## After an edit

`supercov mutants` on a new commit keeps the verdicts no change could move
(`docs/design/strong-mutation.md`, "After an edit"). Up to this release a
Rust test kept a verdict only where no code but tests' changed: any edit to
a crate's source ran every mutant again. A Rust test is now bounded by
names (`strong_reach.rs`), as a JavaScript one is:

- **What it loads.** A test binary links every crate it depends on, so
  every Rust file the earlier run recorded counts, and every file that
  changed. A changed file that is not Rust (a fixture a test reads, a file
  `include_str!` takes) is read whole: any change there reaches the test.
- **Where it starts.** From its own function (`tests::it_works` in its
  file), not its whole file: libtest runs that function alone, in a process
  of its own. A test whose function is not found there (one a macro
  writes) starts from its whole file.
- **What reaches.** A function is reached where reached code names it, and
  a function nested in one where it runs. Modules, implementations and
  traits hold items and run nothing themselves: an item in one is reached
  only by its name. A method of a trait implementation is reached by its
  trait's or its type's name too, since the language calls it unnamed: an
  operator calls `Add::add`, a value going out of scope `Drop::drop`,
  `format!` `Display::fmt`, a `for` loop `Iterator::next`, `sort` `Ord::cmp`.
  A type a test never names has no value in its code: a value of it comes
  from code that names it, in its signature at least. An implementation's or
  a trait's header names nothing that runs. A derive's code calls the
  methods of the types its item names, which those names reach.
- **What gives up.** An item that carries an attribute other than the
  compiler's own (`derive`, `inline`, `cfg`, `allow`, `doc`, `repr`, ...) is
  reached with what holds it: `#[no_mangle]` hands it to code that never
  names it, and a procedural macro's attribute may register it. A file whose
  macros build names (`paste!`'s `[<..>]`, `concat_idents!`) gives up the
  bound wherever it is loaded. A change to a file's top level (a `use`, a
  `mod`), a new file, and a function added give it up too: an added `fn`
  may shadow what a glob import brought in. A doctest keeps its verdict only
  where no code changed: its code is a comment of a source file, which the
  run counts among its test files.

`rust_verdicts_carried_across_commits_are_those_a_fresh_pass_reaches`
(`crates/supercov-cli/tests/rust_mutation.rs`) carries a crate across seven
commits and compares each with a pass from nothing on the same commit:
every verdict the same each time. Of its ten mutants a pass runs, one is
killed by a doctest, whose name holds its line and whose code is a comment
of the source: it runs again wherever code changed.

| commit | kept | ran |
| --- | --- | --- |
| a comment above every function (the doctest's line moved) | 9 | 1 |
| `double`, which one test names | 6 | 4 |
| `audit`, which only a mutant of `refund` sends a test into | 6 | 4, the survivor now killed |
| `Display::fmt`, which `format!` calls and no code names | 6 | 4 |
| a function nothing names | 9 | 1 (the doctest's) |
| a comment after every function (no line moved) | 10 | 0 |
| `Cargo.toml` | 0 | 10 |

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
- Rust compiled to wasm and loaded in Vitest Browser Mode's pages: not
  measured, so its mutants are not planned. Under Node and in Playwright's
  pages the JavaScript strong pass runs them (`mutation-drivers.md`).
- The value probes on `&&` and `||`, struct fields and non-number returns:
  those mutants stay `unknown` where they ran.
- Equivalent mutants by compiled code, as JavaScript's pass finds them
  (Trivial Compiler Equivalence): a Rust mutant would need a build of its
  own to compare, which is what cargo-mutants pays per mutant and the
  switched build exists to avoid. Every mutant runs; none is skipped.
- A stalled attempt ended early, as Playwright's are: a libtest process
  shows no wait a pass could read, so a test runs to its limit.
- On Windows, a test's limit or the pass's stop ends the test process, not
  processes it started (nextest's, rustdoc's): Unix ends the process group.
- A slow mutant's time limit is per test (5 × the test's baseline, at least
  20 s); cargo-mutants' is per suite run. A mutant that slows one test past
  20 s but leaves the suite within 5 × its time is a `timeout` here (fastrand).
