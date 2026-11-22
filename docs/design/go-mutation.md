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
   package's baseline and a second. A run past it is stopped and run again
   at once, on the same worker, with three times the baseline and three
   seconds; past that the process group is killed: `timeout`. A test that
   failed before the limit makes the mutant killed either way. At first the
   second attempts waited for a round of their own after every other
   mutant, with ten seconds: that round lasted a full limit and was most of
   the pass on small modules (8.4 of go-humanize's 12.5 s, 11 of uuid's
   15.9). No mutant of the validation modules that got past its first
   limit ever finished: each hung, or passed its hit limit. The runtime
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
assignment of an `atomic` call to its own operand. A mutant is vetted only
where it may give either something to report: an assignment holding an
`atomic.` call, or a chain whose operands, mutated, hold one twice (`a < b
|| a <= b` to `a < b || a < b`) or compare one expression by `!=` in an
`||` (`==` in an `&&`) with two that may be constants (literals, names,
qualified names). Each is vetted alone (`go vet -atomic -bools -overlay`),
the package with only that mutant in place, and vet decides. At first every
mutant in a chain was vetted: 21 `go vet` runs on go-humanize for its 3
findings, now 3. A command that turns `go test`'s vet off (`-vet=off`)
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
| dustin/go-humanize v1.0.1 | 332 | 234 / 211 | 84 | 15.5 / 12.3 | 61.2 / 24.7 |
| google/btree v1.1.3 | 367 | 140 / 134 | 21 | 11.3 / 11.4 | 41.1 / 29.0 |
| hashicorp/golang-lru v2.0.7 | 165 | 120 / 104 | 11 | 31.7 / 17.9 | 165.3 / 59.3 |
| google/uuid v1.6.0 | 205 | 130 / 145 | 1 | 15.3 / 13.6 | 90.0 / 43.7 |
| spf13/cast v1.7.1 | 190 | 129 / 122 | 7 | 9.4 / 8.8 | 59.4 / 22.9 |

16 CPUs: one c8a.4xlarge machine per tool, with the retries and vetting
of this note (counts of that run; golang-lru's and uuid's move by their
flaky and time-dependent mutants, below). 4 CPUs: GitHub's
`ubuntu-latest` (`.github/workflows/mutation-go.yml`), measured before
them, when a mutant past its first limit waited for a round of its own with
ten seconds; they would take less now. Gremlins runs one `go test` per
mutant, as many at once as there are CPUs, and pays for each on four;
Supercov builds each package's binary once and runs the candidates its weak
verdicts leave.

Where Supercov's time goes, on 16 CPUs (`[supercov] Go pass timings`):

| Module | Measured run | Type check | Vet | Binaries | Mutants |
| --- | --- | --- | --- | --- | --- |
| go-humanize | 6.6 | 0.6 | 0.6 | 0.3 | 4.1 |
| btree | 8.2 | 0.5 | 0.6 | 0.6 | 1.3 |
| golang-lru | 8.8 | 0.4 | 0.1 | 0.9 | 7.5 |
| uuid | 7.6 | 0.4 | 0.6 | 0.3 | 4.6 |
| cast | 7.4 | 0.3 | 0.6 | 0.3 | 0.05 |

Seconds. The measured run is most of it, and most of that is Go compiling
the module and the standard library's packages it needs on a machine that
never had (a second run of go-humanize takes 0.3 s), which Gremlins'
coverage run pays as well. Before, go-humanize took 19.1 s and uuid 21.0:
the round of retries with ten seconds was 8.4 and 11 s of their passes, and
21 `go vet` runs on go-humanize found the 3 that 3 now find.

Value probes, by the median of two runs of `supercov run -- go test ./...`
on 16 CPUs after a first, without probes, with them, with them after a
source changed, and the suite alone (each run starts with a fresh
workspace and keeps `.supercov/cache`, as a project's next run does):

| Module | Without / with / with, edited / `go test` s | Unknown mutants | Candidate runs | Untestable |
| --- | --- | --- | --- | --- |
| go-humanize | 0.29 / 0.30 / 0.40 / 0.20 | 249 to 145 | 2636 to 2557 | 0 |
| btree | 1.19 / 1.17 / 1.20 / 0.35 | 115 to 37 | 2629 to 2229 | 0 |
| golang-lru | 0.85 / 0.86 / 0.90 / 0.71 | 82 to 38 | 1393 to 1252 | 1 |
| uuid | 0.32 / 0.33 / 0.40 / 0.21 | 129 to 29 | 1551 to 1449 | 1 |
| cast | 0.38 / 0.40 / 0.49 / 0.23 | 67 to 16 | 477 to 420 | 0 |

The probes' own cost is within the noise. What they need beside them is
the type checker's facts of each operation's type: asked of operations
only (a comparison's probe infers its type), of the packages without their
tests (`go list` without `-test` compiles no test dependencies: 0.06 s
rather than 0.19 s on go-humanize), and kept in
`.supercov/cache/go-facts.json` under a digest of the run's sources, its
module's requirements, the command's build tags and the environment that
picks the build. The type checker is asked with the tags the command gives
`go test` (`-tags`, or those a wrapped command's last run passed, which
the recording `go` saw), and `GOFLAGS` reaches it as it reaches `go test`:
at first it was asked without them, and a file behind a tag got no
operation probes. A run of
the same sources reads them; one after an edit asks again, 0.04 to 0.11 s.
The helper that answers is built once into `.supercov/cache/go-mutants`
(5 MB, built `-trimpath -ldflags=-s -w`), 3-4 s on a machine's first run.
Both live in the project's `.supercov`, not the run's workspace: a first
version kept them in the workspace, which each run makes afresh, so every
run relinked the helper and asked again, +0.24 s (+92%) on go-humanize.
No weak verdict a probe gave is contradicted by a mutant run by hand.

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

- Ctrl-C during a strong pass left its test binaries running: each runs in
  a process group of its own (so a timeout stops what its tests started),
  which a terminal's Ctrl-C, sent to Supercov's group, does not reach. The
  pass now registers each binary's group, and every `go` it runs, with the
  signal guard (`child_signal_guard`): SIGHUP, SIGINT or SIGTERM to Supercov
  sends SIGTERM to each before Supercov exits.

## Not yet

- Value probes for floating-point operations.
- Value probes in files behind a build tag that a wrapped command (`make
  test`) passes to `go test`: its tags are known from the run before, so
  the first run with mutants asks without them, and those operations' mutants
  stay `unknown` (they run).
- The recording `go` on Windows: there the pass reads flags only from a `go
  test` the command names itself.
