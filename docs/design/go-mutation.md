# Go mutation testing

The mutants list, the weak report and the strong pass
(`docs/design/mutants.md`, `docs/design/strong-mutation.md`) for Go, through
the contract in `docs/design/mutation-drivers.md`. The reference tool is
Gremlins (`go-gremlins/gremlins`), pinned at 0.6.0; reports name the set
`gremlins@0.6.0`.

```sh
npx supercov mutants -- go test ./...
npx supercov runs latest mutants
npx supercov runs latest mutants test
```

## Which mutants

Gremlins mutates one token at a time and walks the syntax tree in order
(`ast.Inspect`), trying each token's mutators in its table's order
(`internal/engine/mappings.go`). `go_mutants.rs` keeps that table and that
order over tree-sitter's tree:

| Gremlins mutator | Tokens | Supercov's mutator |
| --- | --- | --- |
| `CONDITIONALS_BOUNDARY` | `<` `<=` `>` `>=` to the boundary's other side | `EqualityOperator` |
| `CONDITIONALS_NEGATION` | `==` `!=` `<` `<=` `>` `>=` negated | `EqualityOperator` |
| `ARITHMETIC_BASE` | binary `+` `-` `*` `/` `%` | `ArithmeticOperator` |
| `ARITHMETIC_BASE` | unary `-x` `+x` | `UnaryOperator` |
| `INVERT_NEGATIVES` | `-` (binary and unary) to `+` | `INVERT_NEGATIVES` |
| `INCREMENT_DECREMENT` | `++` `--` | `UpdateOperator` |
| `INVERT_LOGICAL` | `&&` `||` | `LogicalOperator` |
| `INVERT_LOOPCTRL` | `break` `continue` | `INVERT_LOOPCTRL` |
| `INVERT_ASSIGNMENTS` | `+=` `-=` `*=` `/=` `%=` | `AssignmentOperator` |
| `REMOVE_SELF_ASSIGNMENTS` | every `op=` to `=` | `AssignmentOperator` |
| `INVERT_BITWISE` | `&` `|` `^` `&^` `<<` `>>`, unary `&x` `^x`, a type union's `|` | `INVERT_BITWISE` |
| `INVERT_BWASSIGN` | `&=` `|=` `^=` `&^=` `<<=` `>>=` | `INVERT_BWASSIGN` |

A mutant carries StrykerJS's mutator name where the mutation is one of
StrykerJS's, because the weak rules are keyed by those names; Gremlins' name
is worked out again from the mutator, the token and the replacement
(`go_mutants::gremlins_name`). `INVERT_NEGATIVES` keeps Gremlins' name:
its `+` for a `-` is also `ARITHMETIC_BASE`'s, and the two are two mutants
there, as Gremlins lists them. The bitwise mutators have no StrykerJS twin
and keep Gremlins' names. Every Gremlins mutator is listed, the six Gremlins
leaves off by default among them (its flags for the same set:
`--invert-assignments --invert-logical --invert-loopctrl
--remove-self-assignments --invert-bitwise --invert-bwassign`). Gremlins
mutates a type union in a constraint (`~int | ~float64`, which go/ast reads
as a binary expression) and `&x` to `|x`, and neither mutant is a program Go
accepts: both are `unviable`.

A mutant's id is its token's span (1-based line, 1-based byte column, as Go
and Gremlins report positions), its mutator and the token it becomes:
`calc.go:6:7-6:8:EqualityOperator:>=`. Gremlins' table maps `%=` to itself
under `INVERT_ASSIGNMENTS`; that mutant is the program as written and is
`equivalent` (`identical-token`).

`scripts/gremlins-parity.mjs` writes Gremlins' own list of a module from a
dry run, and the tests compare the enumeration with it: same mutants, same
positions and mutators, on the fixture in
`crates/supercov-engine/test-assets/go-mutants` (81 mutants) and, through
`SUPERCOV_GREMLINS_PARITY`, on the validation modules below (1,124 mutants
of four of them).
Gremlins also mutates files no `go test ./...` builds: a nested module
(`golang-lru/arc`) and files behind `//go:build ignore` (`btree_mem.go`).
Supercov lists the files the run measures.

## Reach and infection

Each mutant links to the statement point and the function point coverage
gives the code around its token, so a mutant no test reached is
`not-reached`. Package-level code (a `var` initializer) has neither, and is
`unknown`: every test is a candidate.

A run measured with mutants (`supercov mutants`, or `SUPERCOV_MUTANTS=1`)
also probes each relational comparison in a function body: `a < b` becomes
`(__supercov.Cmp(<slot>, 0, a, b) == true)`, which evaluates each operand
once, in order, and is the untyped boolean the comparison was. The slot is
one more entry of the package's probe array; the runtime ORs the relation
it saw into it (`lt` 1, `eq` 2, `gt` 4, `unordered` 8 for a NaN), and the run
reads it as the site's outcomes. A boundary mutant is then `not-infected`
where its operands were never equal, and a comparison never evaluated is
`not-reached`. The probe is a generic call (`cmp.Ordered`), so it is left
off where Go needs a constant, where an untyped shift takes its type from
the comparison, and where both operands are literals; a file that has one is
compiled as Go 1.21 (its `//go:build` line gains `go1.21`, and an old `//
+build` line becomes `//`), whose inference of untyped arguments the call
needs, and no later, so a loop variable keeps the meaning it had.

It also probes the integer operations a mutant changes, where the module's
type checker (below, run once before instrumenting) says the operation is
not a constant and names its type: `x + y` becomes
`__supercov.Arith[int](<slot>, 0, x, y)`, `-x` `__supercov.Unary[int](...)`,
`x <<= n` `x <<= __supercov.AssignShift[uint](<slot>, 0, x, n)`. The
program's own operation runs first (it panics where the program does), then
the mutant's (a division by zero counts as a value that differs), and the
slot records whether they gave the same value: `<site>#0` for the operator's
swap, `<site>#1` for `x op= y` to `x = y`. An assignment's left operand is
read again by the probe only where reading it does nothing (a name, a field,
an element at a name or a literal). Floating-point operations are not
probed: `-0` and `+0` compare equal and a program can tell them apart. A
mutant no evaluation gave another value is `not-infected`.

A comparison whose operand is worked out from a clock or random reading in
its function (`Now()`, `UnixNano()`, `Since(`, `Until(`, `rand.`, or a name
an assignment there gives such a value, as far as assignments carry it)
gets an `untestable` note (`clock` or `random`). One run's values never
being equal says nothing of the next run's: uuid's `if now <= lastV7time`
(`now` from `timeNow().UnixNano()`) never saw equal operands in the measured
run, and its `<` mutant fails the suite 10 times in 10. A `not-infected`
mutant with such a note is still a strong candidate and runs.

## Which mutants compile

A Go mutant can fail to compile: `+` of strings has no `-`, `continue` needs
a loop, `x += y` to `x = y` can leave `x` declared and not used, a constant
can overflow. Before anything is built, the strong pass asks the module's
own type checker. `runtime/go/mutants` (stdlib only, built once into
`.supercov/cache/go-mutants`) loads every package as `go vet` does (`go list
-test -deps -export` for the dependencies' export data, go/types for the
package with its `_test.go` files, as `go test` compiles it), swaps each
mutant's token in the syntax tree, as Gremlins does, and checks the package
again. Only mutants whose viability no rule settles are checked again
(constants, assignments, `break`/`continue`, a constant divisor, strings);
the rest are viable by rule. A mutant that does not compile is `unviable`,
with the compiler's message, and gets no switch. Gremlins counts such a
mutant `KILLED`: `go test` exits 1 when the build fails, and Gremlins reads
only exit 2 as not viable.

The same check answers, for each site, whether the expression is a
constant, the largest constant expression around it, and the expression's
type as the file can write it.

## Schemata

In strong mode every mutant the pass runs is written into its file behind a
switch (`go_instrumenter::strong_rewrite`), with no coverage probe. A file
reads its record once, as its package initialises (`var <slot> =
__supercov.StrongSlot("a/b.go")`), and the runtime reads the active mutant
from `SUPERCOV_ACTIVE_MUTANT` before any package of the binary initialises,
so package-level code is mutated too. Go type-checks every switch, so each
form is a program Go accepts whichever way it goes, and with the switch off
is the program as written, evaluated as written:

- `==`, `!=`: `((x == y) != s.On(i))`.
- `&&`, `||`: `(((bool(x) != s.Is(i)) && (bool(y) != s.Is(i))) != s.On(i))`,
  which short-circuits as each operator does.
- relational, arithmetic and unary operators: a generic call of the
  runtime's (`Lss`, `Add`, `Sub`, `Neg`, ...) taking the operands once,
  instantiated with the expression's own type where the file can write it.
- a constant expression: `Pick` among the largest constant expression around
  it as written and each mutant inside it written whole, so the compiler
  still evaluates every alternative as a constant. A switch inside it would
  make the original a run-time computation, which overflows where the
  constant does not.
- `x += y`, `x++`, `break`: the statement once per mutant behind `if
  s.On(i) { ... } else { ... }`; in a `for` clause's post statement or an
  `if`'s initializer, inside `func() { ... }()`.

Where Go needs a constant (a `const` declaration, an array length) nothing
can switch: such a mutant is built alone. The copy is then built with `go
build -gcflags=-e`; an error is laid at the innermost switch whose bytes
hold it, and that switch's mutants are built alone too. On the validation
modules, 8 of 301 mutants of go-humanize and 6 of 146 of uuid were built
alone, all constant declarations; no switch was refused.

## The driver

`go_strong.rs` runs the plan's Go mutants:

1. Copies the project into the pass's directory, writes the runtime into
   each module (`_supercov_runtime`), and the switched files.
2. Builds each candidate package's test binary once: `go test -c -vet=off`
   (vet would read the switches), with the run's `-tags`, `-race`, `-mod`,
   `-ldflags` and `-trimpath`.
3. Runs each package's baseline: the package whole, no mutant. A test that
   fails there (one the measured run saw fail, or a flaky one) is left out
   of every run of the package (`-test.skip`), and the baseline is the rest.
4. Runs each mutant, one process per package that holds a candidate, the
   cheapest package first, the package whole (`-test.failfast`), with
   `SUPERCOV_ACTIVE_MUTANT=<file>#<index>`, until a test fails. A package's
   tests share its process: a test after a candidate can fail on what the
   mutant left behind, and one before it set up what the candidate reads.
   On uuid, `version7.go`'s clock state set by earlier tests is what lets
   `TestVersion1` reach the code a mutant changes, and the
   `version6.go` variant mutants fail `TestVersion6` alone but not after
   the tests before it. Running the candidates by name (`-test.run`), as a
   first version did, called the first survived and the second killed;
   the suite as written says the opposite. Which packages run is still the
   plan's: a package with no candidate cannot run the mutant's code.
   Workers run mutants side by side (`SUPERCOV_STRONG_WORKERS`, default
   every CPU).
5. A kill is the mutant's: the package passed whole without it.
6. A mutant's first attempt at a package has one and a half times the
   package's baseline and a second. A run past it is stopped, and the
   mutant runs again after every other has, in a round of retries alone,
   with three times the baseline and ten seconds; past that the process
   group is killed: `timeout`. A test that failed before the limit makes the
   mutant killed either way. A mutant that hangs holds a worker for a second
   or two in the main round, not for ten. The runtime
   counts the active mutant's evaluations; past the hit limit it panics
   (`SUPERCOV_MUTANT_TIMEOUT`) and appends the mutant to the results'
   `.timeouts.jsonl`, so a recovered panic is still a timeout. The Go driver
   sets the limit to a billion unless `SUPERCOV_MUTANT_HIT_LIMIT` says
   otherwise: btree's `TestBTree` evaluates one comparison more than a
   million times in a quarter of a second, and with the contract's million
   three of its mutants Gremlins calls survived were timeouts.
7. A mutant with no switch gets builds of its own: `go test -c -overlay`
   with its file replaced by the source with only its token changed.

## The command's own flags

A Go suite is often behind `make test`, a script or gotestsum, which give
`go test` flags of their own. A run with mutants puts a `go` of its own
first on PATH for the command (on Unix): it notes every `go test` it is
asked for and starts the real one. The notes are kept under
`.supercov/go-invocations/` by the command's digest, and the strong pass
builds and runs with their flags: `-tags`, `-race`, `-mod`, `-ldflags`,
`-gcflags`, `-asmflags`, `-trimpath`, `-asan` and `-msan` for `go test -c`,
`-short`, `-cpu` and `-parallel` for the test binary. A `go test` the
command names itself is read from the command as well.

## `go vet`

`go test` runs a subset of `go vet` (atomic, bools, buildtag, directive,
errorsas, ifaceassert, nilfunc, printf, stdversion, stringintconv, tests)
before it runs any test, and a finding fails the package as a compile error
does. A mutant vet rejects is a program `go test` refuses, so Supercov calls
it `unviable`, as it calls one that does not compile, with vet's finding;
Gremlins calls it killed. Of those checks, only two read what a token
mutation changes: `bools`, of the operands of an `&&` or `||` chain (`x !=
1 && x != 2` to `x != 1 || x != 2`, "suspect or"), and `atomic`, of an
assignment of an `atomic` call to its own operand. Each mutant there is
vetted alone (`go vet -atomic -bools -overlay`), the package with only that
mutant in place. A command that turns `go test`'s vet off (`-vet=off`)
turns this off too.

Examples are candidates: a command that passed passed each of its
Examples, whose own outcome no record sees (it is checked after the
function returns). btree's `ExampleBTreeG` is the only test that kills four
of its mutants.

## Validation

Every mutant's verdict is compared with Gremlins run with every test per
mutant (`--integration --coverpkg ./...`), `GOFLAGS=-count=1` and
`--timeout-coefficient 10`. Without `-count=1` Gremlins' coverage run is a
cached result of a few milliseconds, its time limit three times that, and
every mutant timed out. `benchmarks/mutation-go/corpus.sh` clones each module
at its tag and runs either tool; `benchmarks/mutation-go/gremlins-compare.mjs` joins the
verdicts by file, line, column and Gremlins mutator.

Five modules, at their tags, each tool on its own machine of the same
type. Mutants are those both tools list in the packages `go test ./...`
runs; Gremlins' killed count includes the mutants that do not compile,
which it calls killed. Seconds are the whole of `supercov mutants -- go test
./...` (the measured run, the type check, the copy, the builds and the
strong pass) against Gremlins' whole run.

| Module | Mutants | Killed (Gremlins / Supercov) | Unviable | 16 CPUs: Gremlins s / Supercov s | 4 CPUs: Gremlins s / Supercov s |
| --- | --- | --- | --- | --- | --- |
| dustin/go-humanize v1.0.1 | 332 | 234 / 211 | 84 | 14.6 / 19.1 | 61.2 / 24.7 |
| google/btree v1.1.3 | 367 | 140 / 134 | 21 | 11.1 / 10.2 | 41.1 / 29.0 |
| hashicorp/golang-lru v2.0.7 | 165 | 119 / 107 | 11 | 31.9 / 21.2 | 165.3 / 59.3 |
| google/uuid v1.6.0 | 205 | 127 / 143 | 1 | 15.0 / 21.0 | 90.0 / 43.7 |
| spf13/cast v1.7.1 | 190 | 129 / 122 | 7 | 9.0 / 8.4 | 59.4 / 22.9 |

16 CPUs: one c8a.4xlarge machine per tool. 4 CPUs: GitHub's `ubuntu-latest`
(`.github/workflows/mutation-go.yml`). Counts are the 16-CPU run's; the
4-CPU run's differ only in golang-lru (117 / 105) and uuid (130 / 143), by
the flaky and time-dependent mutants below. Gremlins runs one `go test` per
mutant, as many at once as there are CPUs, and pays for each on four;
Supercov builds each package's binary once and runs the candidates its
weak verdicts leave. On sixteen CPUs both take seconds and Supercov's
fixed costs (the measured run, the type check, the builds) are most of its
time.

Value probes, by the median of two warm runs of `supercov run -- go test
./...` on 16 CPUs, without probes, with them, and the suite alone:

| Module | Without / with probes / `go test` s | Unknown mutants | Candidate runs | Untestable |
| --- | --- | --- | --- | --- |
| go-humanize | 0.29 / 0.55 / 0.13 | 249 to 145 | 2636 to 2557 | 0 |
| btree | 1.44 / 1.67 / 0.42 | 115 to 37 | 2629 to 2228 | 0 |
| golang-lru | 0.87 / 1.12 / 0.62 | 82 to 38 | 1393 to 1252 | 1 |
| uuid | 0.34 / 0.62 / 0.16 | 129 to 29 | 1551 to 1449 | 1 |
| cast | 0.37 / 0.64 / 0.15 | 67 to 16 | 477 to 420 | 0 |

The probes' cost is a quarter of a second, about the same on every
module: most of it is the type checker's facts, which the probes need to
write an operation's type, not the probes themselves (btree, with the
most to probe and the longest suite, pays 0.23 s). On the first run the
type checker's own build adds 4-5 s, once per machine. No weak verdict a
probe gave is contradicted by a mutant run by hand.

Every disagreement, with the mutant applied by hand and the suite run whole
ten times (`benchmarks/mutation-go/by-hand.mjs`):

- **Unviable** (124): Supercov's type check refuses the mutant (`-` of
  strings, a negative array length, a duplicate map key after a constant
  changed, a variable left unused, `x != 1 || x != 2`, which `go vet`
  refuses). Gremlins calls each KILLED: `go test`
  exits 1 on a build failure.
- **Not covered by Gremlins, killed by Supercov** (go-humanize 16, uuid 16):
  constant declarations (`KByte = Byte * 1000`). Gremlins' coverage profile
  has no block there and it never runs them; Supercov builds each alone, and
  the tests kill them.
- **Only Gremlins** (golang-lru 45, btree 10): `golang-lru/arc` is a module
  of its own that `go test ./...` does not run, and `btree_mem.go` is
  `//go:build ignore`.
- **Time-dependent kills** (uuid): mutants of clock arithmetic
  (`version7.go:96`, `version6.go:45`, `time.go:58`, `time.go:63`) make the
  suite as written fail 4 to 9 times in 10; each tool's single run lands
  either way, and which ones disagree changes from run to run. Neither is a soundness violation of the selection: the
  mutant's only killers are its candidates.
- **A flaky test** (golang-lru): `expirable`'s `ExampleLRU` waits 100 ms on
  entries that expire in 10 ms, and with 16 copies of the suite running at
  once, as Gremlins runs them, fails 4 times in 64 with no mutant at all.
  Every golang-lru disagreement is a kill by it or a test like it, on one
  side or the other, where the mutant applied by hand fails 0 or 1 times in
  10; the set changes from run to run. One Gremlins run of the module
  stopped at its coverage step on that failure.

`version7.go:97`'s `now <= lastV7time` to `<` was a fifth: weak mutation
called it survived because the measured run never saw the operands equal,
and the suite fails 10 times in 10 with it. Its operands are worked out from
the clock, so it is now `untestable` (`clock`) and runs; both runs above
agree with Gremlins on it.

No other disagreement remains: 0 soundness violations outside time-dependent
and flaky tests, each checked by hand. Four were real and are fixed (below,
and btree's Example-only kills).

## Fixed in the Go frontend on the way

- Two packages' tests of one name (golang-lru's `TestLRU`, in its root
  package and in `simplelru`) shared one record's evidence: the root
  package's code read as run by no test, and six mutants Gremlins kills
  were `no-coverage`. Evidence is now keyed by package and name.
- An Example ran `unknown`, and so was never a strong candidate.
- The run-wide record (what no test could claim: Examples, parallel tests)
  took a probe's slot only where no test had touched it: a comparison an
  Example alone saw with equal operands read as never equal, and btree's
  `len(n.children) > 0` to `>=` as not infected, though `ExampleBTreeG`
  kills it. It now takes every bit no test claimed.
- A test the plan takes only because its evidence is not its own runs for a
  mutant only where its binary links the mutant's package.

- A compile error in the strong copy was laid at the switch whose bytes
  held it in the file before its runtime import was written in, so every
  switch after the import was off by the import's change in length, and a
  refused switch took the file's others with it. No validation module had a
  refused switch; the tests' does (a shift whose type, from a dot import,
  the file cannot write: the generic call infers `int`). Each switch's
  bytes now move with the import.

## Not yet

- Value probes for floating-point operations.
- Value probes for files behind a build tag: the type checker's facts for
  a measured run are asked without the command's tags, so such a file's
  operations get no probe and their mutants stay `unknown` (they run).
- The recording `go` on Windows: there the pass reads flags only from a `go
  test` the command names itself.
