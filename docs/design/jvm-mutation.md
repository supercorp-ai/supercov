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
| `NegateConditionals` | every comparison; every other condition (a name, a call, `instanceof`, `is`, Kotlin's `x in names` but not a range's `x in a..b`) where the code branches on it: an `if`, a loop, `?:`, `&&`, `\|\|`, `!`, a bare `when` branch; the whole condition of a Kotlin `require` or `check` of `&&` or `\|\|` (its inlined `if (!value) throw`) | `negated` |
| `Math` | `+ - * / % & \| ^ << >> >>>` on numbers and booleans (Kotlin's `and` `or` `xor` `shl` `shr` `ushr`), `x op= y`, and `x++` on anything but a local `int` | the new operator |
| `Increments` | `i++`, `i--`, `i += c`, `i -= c` on a local `int` (`IINC`) | the new operator |
| `InvertNegs` | `-x` | `removed` |
| `VoidMethodCall` | a call statement whose method returns nothing | `removed` |
| `PrimitiveReturns`, `BooleanTrueReturnVals`, `BooleanFalseReturnVals`, `NullReturnVals`, `EmptyObjectReturnVals` | each returned value, by the method's declared (or, for a Kotlin expression body, inferred) type, as PIT chooses among them | the replacement |

Where the bytecode depends on a type the source does not spell out, the type
is inferred from the file's declarations and an index of every method the
project declares: `s + 1` is a string concatenation (no mutant) where `s` is
a `String`, a call statement is a `void` call where the project or the JDK
declares it so, `i++` is `IINC` where `i` is a local `int`. A lambda returns
what javac declares its synthetic method to return: the functional
interface's return type where the source names it, the body's type where
the interface's is a type variable inferred from it, and `Object` (so
`NullReturnVals`) where the body is a value of the caller's own type
variable (`() -> defaultValue` of a `T`). A Kotlin property's `get()`
returns the property's type, under the name kotlinc gives the method
(`getArea`, `isOpen`). A Kotlin local is incremented with `IINC`
(`Increments`) unless a lambda reads it, inlined or not: then kotlinc
loads, adds and stores (`Math`).

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
type; no return of a try-with-resources body. An anonymous class declared in
a static initializer is a class of its own: its methods are mutated.

PIT's filters match bytecode patterns, and a few source forms compile to
the pattern only sometimes. On commons-csv PIT keeps the counter increment
ending two `while (pos < len)` loops whose body has an `if` before it, and
filters it in four others of the same source shape; on commons-cli it
filters the `hasNext()` of one explicit `for (Iterator it = …; it.hasNext();)`
loop and keeps it in another written the same way. Supercov applies each
filter to its source shape every time: those three mutants are listed by
one tool only.

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
a project, PIT-only mutants by these categories. It pairs a mutant with
PIT's by file, mutator and what the mutant does, then by line: the same
line, then the lines javac gives an operator on a continuation line, then
the statement's lines. kotlinc puts every jump of a multi-line condition on
its last line; where PIT has more of a kind on a line than Supercov does,
those are paired in code order (PIT's instruction index, Supercov's source
position), not by line.

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

- **Ant.** In an Ant build the patched tasks name Supercov's formatter and
  listeners by class; a strong copy compiles drivers under those names:
  `<junit>`'s formatter starts the pass with the JUnit 4 driver in each JVM
  the task forks (the claims share the work), `<junitlauncher>`'s listener
  and `<testng>`'s listener the other two.
- **Getting in.** On the JUnit Platform (Jupiter, JUnit 4 through Vintage,
  Kotest, Spock) `SupercovStrong` is a discovery filter that leaves the
  build's own discovery empty, and a listener that, told the empty plan
  starts, runs the pass with a launcher of its own. Under TestNG
  `SupercovStrongTestNG` is a method interceptor that leaves the build's run
  empty and an execution listener that runs the pass when it ends. Both are
  registered through the frameworks' service files in the copy.
- **One test at a time, by its name.** A test is found by the name the
  measured run recorded: the platform driver discovers its class and takes
  the identifier with that name. A parameterized or dynamic test's
  invocations are not in a discovered plan (they appear as it runs), so an
  invocation is selected by its template's id and its index
  (`…/[test-template-invocation:#3]`), as PIT selects it. A JUnit 4
  parameterized test whose parameter prints as `Object.toString` does
  (`[com.example.Builder@6c0d7c83]`) carries an identity hash that differs
  from one JVM to the next, and from one discovery to the next, unique id
  included: such a test is found by its name with every `@hash` left out,
  and run by its class with a discovery filter on that name. Two tests of a
  class that read alike without their hashes are not found (`error`). The
  TestNG driver
  runs the class and method (a data provider's invocations together); the
  JUnit 4 driver of Ant's `<junit>` runs `JUnitCore` on the method. A
  candidate the driver cannot find leaves its mutant `error`, never
  `survived`.
- **Fresh classes for each mutant.** Each mutant runs in a class loader of
  its own that loads the project's classes afresh (every directory on the
  test class path, a surefire booter jar's manifest read for them) and the
  libraries as the JVM already has them. A cache a baseline filled, or a
  static a previous mutant changed, does not carry over: on typetools a
  type cache filled without the mutant hid seven mutants PIT kills. The
  runtime's switches are set in that loader's copy and in the JVM's own, for
  code a packaged jar holds. The loader keeps the build's assertion setting
  (Surefire enables assertions on its own loader, not with `-ea`), and a
  minion JVM gets `-ea` where the build had them: typetools' tests check
  with `assert`.
- **Order.** A mutant's candidates run one at a time until one kills it:
  first the tests of the test file named after the mutant's file
  (`JvmAnnotationsTest` for `JvmAnnotations.kt`), then the fastest. On
  kotlinpoet a mutant in a Kotlin expression body (no statement point of
  its own, so every test is a candidate) was killed only by its 812th
  candidate when they ran in name order: about 50 s each.
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

A probe's opening ranks inside the coverage wrappers at its start and its
closing between their parentheses (5, 20, 21) and the braces and `else`
arms statements close there (60 and up). The instrumenter braces a Kotlin
`if`, `when` or `try` expression's branches and its braces nest by the
order they were added, which no rank can reproduce from outside, so a
Kotlin probe is not placed around an expression that ends in one.

## Reach that is not where the code is

A lambda's body runs when the lambda is called. An expression lambda (and a
Kotlin lambda a function that is not `inline` keeps) has no statement point
of its own, so the statement around it records only that the lambda was
made: `map.put(Character.class, s -> …)` runs once, as the class loads, and
the tests that convert a character later run the body. A mutant in such a
body is linked to no point: every test is its candidate, unless its value
probe recorded where it ran. commons-cli's `TypeHandler` had a violation of
this kind before.

## Validation

`benchmarks/mutation-jvm/` runs PIT 1.17.1 with `fullMutationMatrix` (every
test against every mutant), PIT as a team runs it, and Supercov's measured
run and strong pass, on public projects pinned to tags
(`projects.json`), and joins the results with `supercov runs latest
mutants pit`.

A project with many files is split into shards by file size (largest first,
each to the lightest shard), and PIT and Supercov each run a shard's files
in a job of their own: kotlinpoet in 12. The Ant project (Apache Ant's own
`util` package) runs PIT through its command line over the classes Ant
compiled, since PIT's Ant task launches the same.

### Results

Measured on 16-CPU Linux machines (c8a/m8a/r8a), 16 threads for PIT and
16 JVMs for the strong pass, PIT 1.17.1; times are seconds, summed over a
project's shards. "PIT" is PIT as a team runs it (it stops at a mutant's
first kill); "PIT matrix" runs every test against every mutant, the ground
truth the verdicts are compared with.

| project | PIT mutants | Supercov mutants | matched | agree | violations | PIT s | PIT matrix s | measured run s | strong pass s |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| commons-cli 1.9.0 (Java, JUnit 5) | 882 | 882 | 881 | 880 | 1 | 35 | 78 | 13 | 24 |
| commons-csv 1.12.0 (Java, JUnit 5) | 720 | 719 | 717 | 717 | 0 | 46 | 131 | 17 | 27 |
| typetools 0.6.3 (Java, TestNG) | 216 | 216 | 216 | 214 | 2 | 19 | 20 | 9 | 18 |
| picnic 0.5.0 (Kotlin, JUnit 4) | 632 | 330 | 306 | 304 | 1 | 34 | 39 | 14 | 24 |
| kotlinpoet 1.14.2 (Kotlin, JUnit 4, 12 shards) | 2,114 | 1,155 | 1,041 | 1,033 | 8 | 1,041 | 3,567 | 728 | 794 |

The Kotlin enumeration changed after this run (explicit getters' returns,
`x in names`, a local a lambda reads incremented by `Math`, pairing in the
order the compiler emits code): joined with the same PIT reports, kotlinpoet
matches 1,100 of PIT's mutants and picnic 313. Their verdicts were not run
again.

On the Java projects the measured run and the strong pass together take
about as long as PIT as a team runs it, and a third to a half of PIT's
full matrix. On kotlinpoet the strong pass (794 s) is faster than PIT
(1,041 s); with the measured run it is slower, because each of the 12
shards runs the whole suite measured (about 60 s a shard) where one run
would do. Before this round the strong pass on kotlinpoet took longer than
PIT (658 s against 574 s on 4 of 6 shards): three quarters of one shard's
time went to mutants of another Gradle module the measured run never ran
(every test their candidate; the benchmark now mutates the module's own
files only), and to mutants every test may reach, tried in name order (now
the mutant's own test file first, see **Order**).

Every violation, checked:

- **commons-cli `OptionBuilder.java:141` (NullReturnVals):** Supercov
  survived, PIT killed by `testGroovy`. Applied by hand, that test and the
  whole suite pass: PIT's kill comes from static state an earlier test left
  in its JVM.
- **typetools `TypeResolver.java:72` and `:102` (VoidMethodCall):** Supercov
  killed, PIT survived. The calls (`setAccessible`, `Unsafe.putBoolean`) are
  in anonymous classes the static initializer runs; without them the
  initializer's lambda support fails and `LambdaTest` gets `Unknown` for
  `UUID`. PIT swaps a mutated class into a JVM whose static initializer
  already ran, so the mutant never runs there.
- **picnic `textBorder.kt:5` (NegateConditionals):** Supercov killed, PIT
  survived. The code runs only as the companion object's statics are set
  up; applied by hand, 42 of 48 tests fail. Same cause as typetools'.
- **kotlinpoet `CodeWriter.kt:41`, `Taggable.kt:73`:** Supercov killed, PIT
  no coverage. Both are in Kotlin `inline` functions: their code runs only
  as copied into callers, compiled before PIT mutates anything, so PIT's
  mutant of the function itself never runs.
- **kotlinpoet `Taggable.kt:28`, `:63`:** Supercov killed, PIT survived or
  no coverage. Only `TaggableTest` reaches them, a JUnit 4 parameterized
  test whose names carry identity hashes (`AnnotationSpec$Builder@6c0d7c83`).
  PIT selects a test by its JUnit description in a fresh JVM, where the
  hashes differ, so it never runs them; Supercov finds them by their names
  without the hashes (see **One test at a time**).
- **kotlinpoet `PropertySpec.kt:236`, `:241` (two each):** a pairing error,
  fixed after the run: `require(a || b)`'s own jump was paired with the
  operand `a`'s and the other way round. Paired in the order the compiler
  emits them, the four agree.
- (earlier, fixed) kotlinpoet `TypeSpec.kt:834`: paired with PIT's
  surviving `?.` check on line 833 instead of its own line's.

Mutants one tool lists and the other does not:

- **Java:** commons-csv 3 PIT-only (a lambda passed to a library's generic
  method, whose return type the source does not show; two counter
  increments PIT's loop filter keeps, see **Which mutants**), 2
  Supercov-only (`CSVPrinter.java:421`, `:423`, a branch PIT lists nothing
  for); commons-cli 1 and 1 (a lambda converting a character, stored in a
  map whose value type is a wildcard; the iterator loop PIT filters once
  and not once); typetools none. Before this round typetools had 4
  PIT-only (the static initializer's anonymous classes) and commons-cli 3
  (lambdas returning a type variable).
- **Kotlin, PIT-only:** code kotlinc writes that has no source of its own,
  by `mutants pit`'s categories on kotlinpoet (1,073) and picnic (326):
  inlined standard-library code reported past the file's end (202, 32),
  property getters (154, 102), null checks kotlinc adds (107, 3) and those
  of `?.`, `?:` and `!!` (52, 77), a `require` or `check`'s own jump where
  its condition is one comparison (the same mutant as the comparison's;
  111, 2), `for` loop counters (62, 20), `when` subject comparisons and
  `is` branches (55), inlined `isNotEmpty()` and the like (45), range checks
  (9), an array constructor's filling loop (8), and 276 and 82 others not
  yet put down one by one.
- **Kotlin, Supercov-only:** kotlinpoet 114, picnic 24. Of kotlinpoet's,
  17 are `+`, `+=` or `-=` on a type the source does not show (a
  collection, a string) that the compiler refuses: `unviable`. picnic's
  include `debug { }` lambdas whose code kotlinc removes.

### Ant

Apache Ant 1.10.15's own `util` package (`src/main/org/apache/tools/ant/util/*.java`,
its tests `org/apache/tools/ant/util/*Test.java` through the build's
`junit-tests` target, JUnit 4 under `<junit>`), on the same 16-CPU machines:

| | PIT mutants | Supercov mutants | matched | agree | violations | PIT s | PIT matrix s | measured run s | strong pass s |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| Apache Ant `util` | 2,083 | 2,056 | 2,039 | 2,016 | 22 | 119 | 160 | 11 | 29 |

PIT runs through its command line over the classes Ant compiled, with the
system properties the build's `<junit>` sets. The violations, with the
mutant applied to the source and the build's own `junit-tests` run whole
where marked "checked":

- **Supercov killed, PIT survived (6):** `JAXPUtils.java:105`, `:122`,
  `:247` (a parser factory cached in a static field; checked for `:105`:
  `XMLFragmentTest` errors), `JavaEnvUtils.java:529`, `:530` (a static
  package list built once) and `SymbolicLinkUtils.java:55` (read into a
  test class's static field). PIT's JVM already holds the cached value, so
  the mutant changes nothing there; Ant forks a JVM per test class, as
  Supercov's fresh class loader starts afresh.
- **Supercov survived, PIT killed (16):** 13 in `FileUtils`, killed in
  PIT's run by `XMLFragmentTest`, `JavaEnvUtilsTest`, `ResourceUtilsTest`
  or `LoaderUtilsTest`, and `DeweyDecimal.java:214`. Checked
  `FileUtils.java:1374` and `:1523`: the whole suite passes with them, so
  PIT's kills come from its environment (one JVM, tests sharing a
  `FileUtils` cache and files), not from the project's tests as they run.
  The others are not checked one by one.
- **Supercov wrong (1, checked):** `LazyFileOutputStream.java:128`
  (`write(b, 0, b.length)` removed). `testByteArray` fails with it, as PIT
  says; the strong pass called it survived. The test asserts a file
  `test.txt` exists in the working directory, which the class's other tests
  create and delete: with 16 JVMs in one directory, another JVM's
  `testOpen` made the file while the mutant ran. The strong pass does not
  yet isolate tests that share files in the working directory, and a
  survivor is not confirmed by running its candidates again alone.

`FileUtils.java:1204` (`createNewFile` returns `true`) is decided without
running: every test saw it return `true`. The pass's one `error` is the
same file's `createNewFile` mutant, whose run ended the formatter's JVM
before its verdict (Ant's own classes are the ones under test).
