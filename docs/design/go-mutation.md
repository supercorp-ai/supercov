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

A mutant carries StrykerJS's mutator name where the mutation is one of
StrykerJS's, because the weak rules are keyed by those names; Gremlins' name
is worked out again from the mutator, the token and the replacement
(`go_mutants::gremlins_name`). `INVERT_NEGATIVES` keeps Gremlins' name:
its `+` for a `-` is also `ARITHMETIC_BASE`'s, and the two are two mutants
there, as Gremlins lists them. `INVERT_BITWISE` and `INVERT_BWASSIGN`, off by
default in Gremlins, are not listed; the rest are, including the four
Gremlins leaves off by default (its flags for the same set:
`--invert-assignments --invert-logical --invert-loopctrl
--remove-self-assignments`).

A mutant's id is its token's span (1-based line, 1-based byte column, as Go
and Gremlins report positions), its mutator and the token it becomes:
`calc.go:6:7-6:8:EqualityOperator:>=`. Gremlins' table maps `%=` to itself
under `INVERT_ASSIGNMENTS`; that mutant is the program as written and is
`equivalent` (`identical-token`).

`scripts/gremlins-parity.mjs` writes Gremlins' own list of a module from a
dry run, and the tests compare the enumeration with it: same mutants, same
positions and mutators, on the fixture in
`crates/supercov-engine/test-assets/go-mutants` (81 mutants) and, through
`SUPERCOV_GREMLINS_PARITY`, on the validation modules below (995 mutants).
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
3. Runs each package's baseline: its candidates by exact name
   (`-test.run '^(?:TestA|TestB)$'`), no mutant. A candidate that fails
   there is no candidate; where the candidates fail together and the
   package passes whole, the package runs whole.
4. Runs each mutant, one process per package, its candidates by exact name,
   the cheapest package first, `-test.failfast`, with
   `SUPERCOV_ACTIVE_MUTANT=<file>#<index>`, until a test fails. Workers run
   mutants side by side (`SUPERCOV_STRONG_WORKERS`, default every CPU).
5. Confirms a kill whose tests up to the failing one are not the baseline's:
   they run again without the mutant, and a kill they fail without it too
   is overturned and the mutant runs with every candidate of its package.
6. A run's time limit is three times its package's baseline and ten
   seconds. Past it the process group is killed: `timeout`. The runtime
   counts the active mutant's evaluations; past the hit limit it panics
   (`SUPERCOV_MUTANT_TIMEOUT`) and appends the mutant to the results'
   `.timeouts.jsonl`, so a recovered panic is still a timeout. The Go driver
   sets the limit to a billion unless `SUPERCOV_MUTANT_HIT_LIMIT` says
   otherwise: btree's `TestBTree` evaluates one comparison more than a
   million times in a quarter of a second, and with the contract's million
   three of its mutants Gremlins calls survived were timeouts.
7. A mutant with no switch gets builds of its own: `go test -c -overlay`
   with its file replaced by the source with only its token changed.

Examples are candidates: a command that passed passed each of its
Examples, whose own outcome no record sees (it is checked after the
function returns). btree's `ExampleBTreeG` is the only test that kills four
of its mutants.

## Validation

Every mutant's verdict is compared with Gremlins run with every test per
mutant (`--integration --coverpkg ./...`), `GOFLAGS=-count=1` and
`--timeout-coefficient 10`. Without `-count=1` Gremlins' coverage run is a
cached result of a few milliseconds, its time limit three times that, and
every mutant timed out. `scripts/go-mutation-corpus.sh` clones each module
at its tag and runs either tool; `scripts/gremlins-compare.mjs` joins the
verdicts by file, line, column and Gremlins mutator.

RESULTS

## Fixed in the Go frontend on the way

- Two packages' tests of one name (golang-lru's `TestLRU`, in its root
  package and in `simplelru`) shared one record's evidence: the root
  package's code read as run by no test, and six mutants Gremlins kills
  were `no-coverage`. Evidence is now keyed by package and name.
- An Example ran `unknown`, and so was never a strong candidate.

## Not yet

- `INVERT_BITWISE` and `INVERT_BWASSIGN`.
- Value probes for arithmetic and assignments (they are `unknown`, so every
  test that reaches them is a candidate).
- `go vet`'s checks: Gremlins' `go test` refuses a mutant vet flags (`a != 1
  || a != 2`) and counts it killed; the strong pass runs it.
- A test binary built by a command other than `go test` (a Makefile that
  passes its own flags): the pass reads `-tags` and the rest from a `go
  test` in the command, and builds with none where there is none.
