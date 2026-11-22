# Mutation testing for Java and Kotlin

Supercov's mutants list, weak report and strong pass
(`docs/design/mutants.md`, `docs/design/strong-mutation.md`) for Java and
Kotlin, through the driver contract of `docs/design/mutation-drivers.md`. The
reference tool is PIT 1.17.1 and its `DEFAULTS` mutator group; reports name
the set `pitest@1.17.1`.

The code: `crates/supercov-engine/src/jvm_mutants.rs` (enumeration and value
probes), `jvm_schemata.rs` (switches), `jvm_strong.rs` (the list, the probes'
slots, PIT's report and the join), `jvm_run.rs` (`run_strong_jvm`), the
runtime's switches and probes in `runtime/jvm/.../Supercov.java`, the
drivers `SupercovStrongPass.java`, `SupercovStrong.java` (JUnit Platform)
and `SupercovStrongTestNG.java`, and the CLI's `mutants pit` command.

## Which mutants

PIT mutates bytecode: each mutator changes one instruction, and the compiler
decides what a line of source becomes. Supercov enumerates from source, so
each operator is mapped to the source the compiler turns into that
instruction, and PIT's default filters are applied in the same terms.

| Mutator (PIT's, less `Mutator`) | Source form | Descriptor |
| --- | --- | --- |
| `ConditionalsBoundary` | `<` `<=` `>` `>=` wherever they are (always a jump) | the new operator |
| `NegateConditionals` | every comparison; every other condition (a name, a call, `instanceof`, `is`) where the code branches on it: an `if`, a loop, `?:`, `&&`, `\|\|`, `!`, a bare `when` branch | `negated` |
| `Math` | `+ - * / % & \| ^ << >> >>>` on numbers and booleans (Kotlin's `and` `or` `xor` `shl` `shr` `ushr`), `x op= y`, and `x++` on anything but a local `int` | the new operator |
| `Increments` | `i++`, `i--`, `i += c`, `i -= c` on a local `int` (`IINC`) | the new operator |
| `InvertNegs` | `-x` | `removed` |
| `VoidMethodCall` | a call statement whose method returns nothing | `removed` |
| `PrimitiveReturns`, `BooleanTrueReturnVals`, `BooleanFalseReturnVals`, `NullReturnVals`, `EmptyObjectReturnVals` | each returned value, by the method's declared (or, for a Kotlin expression body, inferred) type, as PIT chooses among them | the replacement |

Where the bytecode depends on a type the source does not spell out, the type
is inferred from the file's declarations and an index of every method the
project declares: `s + 1` is a string concatenation (no mutant) where `s` is
a `String`, a call statement is a `void` call where the project or the JDK
declares it so, `i++` is `IINC` where `i` is a local `int`.

PIT's filters, in source terms: no mutant in a static initializer, a static
field's initializer, an enum's constructor, an `object`'s or companion's
initialization, or a private method only they call (`FSTATI`); none of a
`for` loop's counter or a `while` loop's last statement incrementing the
counter its condition reads first (`FFLOOP`); none in `assert` (`FASSERT`), a
`switch` on a string or an enum (`FSTRSWITCH`, `FESWITCH`), a for-each loop's
iterator (`FFEACH`), on a line that calls a logging framework (`FLOGCALL`),
in code a constant condition leaves out, or of an expression the compiler
folds (`60 * 60`, `CONST + 1`); no return mutant that returns what the code
already returns (`return 0`, `return a < b`'s `false`, `return ""`,
`Collections.emptyList()`: `FRETEQUIV`); no `NullReturnVals` of a method
annotated `@NotNull`, which is every Kotlin function with a non-null return
type; no return of a try-with-resources body.

### Ids

`file:startLine:startCol-endLine:endCol:Mutator:descriptor`, lines and
columns 1-based, columns in characters.

### Parity

`tests/fixtures/pit-parity` holds Java and Kotlin fixtures and PIT's own
list of their mutants; `jvm_strong`'s tests join the two as `mutants pit`
does. On the Java fixtures (315 PIT mutants) Supercov lists the same mutants
but one: a lambda passed to `Stream.map` whose body's type is not in the
source. On the Kotlin fixture (173) it lists 150 of PIT's and none of its
own; the 23 others are code kotlinc writes that has no source of its own:

- a property's generated getter (`val name: String` returns `""`);
- a `for` loop over a range: its counter and bounds check;
- a `when (x)` branch's comparison with its subject;
- the null checks of `?.`, `?:` and `!!`;
- a standard-library function inlined into the method, reported at a line
  past the file's end.

`supercov runs <id> mutants pit <mutations.xml>` reports the same split for
a project, PIT-only mutants by these categories.

## Schemata

In strong mode the files with a mutant the pass runs are written with every
such mutant behind a switch reading `Supercov.on(file, index)`
(`jvm_schemata.rs`). Switches nest as the code does.

- **Java** calls typed helpers wherever an operator is replaced
  (`Supercov.lt(f, k, a, b)`, `Supercov.add(f, k, a, b)`), so each operand
  is evaluated once and overload resolution keeps javac's numeric promotion.
  A negated condition is `Supercov.n(f, k, c)`; a returned value
  `Supercov.rZ(f, k, v)` and the like; a removed call `{ if (!on) call(); }`;
  an increment or compound assignment both forms behind a conditional,
  through `Supercov.v(...)` where Java requires a statement expression (a
  `for` update).
- **Kotlin** has no implicit widening for overloads to follow, so an
  operator's switch binds its operands with `let` and applies the operator
  itself: `(a).let { l -> (b).let { r -> if (on) l <= r else l < r } }`. A
  negated condition goes through `supercovNegate(f, k, c)`, whose contract
  says what the condition says, so the smart casts the condition made still
  compile (`if (x != null) x.length`); with the mutant on the branch runs
  without them and fails where it relies on them, as PIT's negated jump
  does.

**Unviable mutants.** A switch the compiler refuses (a `+` that was a
string's after all, an operator a Kotlin type does not have) is reported by
the build at a position inside its region. The pass maps the compiler's
errors (Maven's and Gradle's javac, kotlinc's old and new formats) to the
innermost switch there, drops those mutants as `unviable`, writes the files
again and builds again. An error outside every switch stops the pass. A
pass builds once where nothing is refused, twice where something is.

## The driver

The build compiles the strong copy once and starts its test JVM as it always
does; the pass runs inside it, as PIT's minion does, switching mutants
between test executions (`SupercovStrongPass.java`).

- **Getting in.** On the JUnit Platform (Jupiter, JUnit 4 through Vintage,
  Kotest, Spock) `SupercovStrong` is a discovery filter that leaves the
  build's own discovery empty, and a listener that, told the empty plan
  starts, runs the pass with a launcher of its own. Under TestNG
  `SupercovStrongTestNG` is a method interceptor that leaves the build's run
  empty and an execution listener that runs the pass when it ends. Both are
  registered through the frameworks' service files in the copy.
- **One test at a time, by its name.** A test is found by the name the
  measured run recorded: the platform driver discovers its class and takes
  the identifier with that name (or, for a parameterized invocation whose
  arguments print differently in another JVM, with that invocation index);
  the TestNG driver runs the class and method. Each runs alone.
- **Baselines and confirmations.** A test runs once without a mutant before
  any mutant runs against it; one that fails then is no candidate. A kill is
  confirmed by running the killing test again with no mutant: a mutant that
  left the JVM so that the test fails without it (a static cache filled
  with a mutated value) gets no verdict there, and the JVM stops taking
  mutants.
- **Time.** A test with a mutant gets the longer of its measured time and
  its baseline, × 3, and 10 s. The switch counts the active mutant's
  evaluations and throws `MutantTimeoutError` past 1,000,000
  (`SUPERCOV_MUTANT_HIT_LIMIT`), a `timeout`. A test past its time leaves a
  thread that will not stop: the verdict is written and the JVM takes no
  more mutants. The engine starts the build again for what is left, up to
  twelve starts in all.
- **Several JVMs.** `SUPERCOV_STRONG_JVMS` (by default half the machine's
  processors, at most eight) starts that many JVMs in all, the test JVM's own command again with the
  driver as entry; each claims a mutant before running it, so none runs
  twice. Several modules each run their own tests' share; a mutant whose
  candidates span modules is decided from each module's part.

## Weak mutation: value probes

A run measured with its mutants listed (`SUPERCOV_MUTANTS=1`, which
`supercov mutants` sets) places a value probe at each mutant that can take
one without changing what compiles, and archives `mutants.json`. A probe is
a call around the code that returns exactly what the code computes and
records, per test, what decides the mutant:

| Probe | Records | Decides |
| --- | --- | --- |
| a comparison (`Supercov.wlt(slot, a /*<*/, b)`) | `lt`, `eq`, `gt`, `unordered` | `ConditionalsBoundary` (infected only where the operands were equal); `NegateConditionals` (whether it ran) |
| an operation (`wadd`, `wneg`, ...) | `same` or `differs`: the mutant's value computed beside the original's | `Math`, `InvertNegs` |
| a returned value (`wr`) | `false`/`true`, `zero`/`nonzero`, `null`, or that very empty value (`""`, `Collections.emptyList()`, a cached boxed zero, ...) | every return mutant |

The probes are insertions only, so they compose with the coverage probes'
insertions: an operator is kept in a comment between its operands
(`a /*<*/, b`), and a probed file is rewritten without the position
comments the shared rewrite leaves at each edit. Java probes go wherever
the operator takes only numbers (every comparison and arithmetic operator
but `+`, whose operands must be known numbers); Kotlin probes only where
both operands are known primitives of the same type. Elsewhere a mutant
has no probe and is decided by reach alone. A probe that recorded nothing
says its code never ran, which is what decides reach in a Kotlin
expression body, where no statement point is.

## Validation

`benchmarks/mutation-jvm/` runs PIT 1.17.1 with `fullMutationMatrix` (every
test against every mutant), PIT as a team runs it, and Supercov's measured
run and strong pass, on public projects pinned to tags
(`projects.json`), and joins the results with `supercov runs latest
mutants pit`.

(Results: see the report section below.)
