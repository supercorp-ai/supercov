# Python mutants: mutmut's, weak and strong

`mutants.md` and `strong-mutation.md` describe mutation testing for
JavaScript; `mutation-drivers.md` is the contract every language meets. This
record is Python's side of it: which mutants, the values the measured run
records for them, and the strong pass under pytest and unittest.

The reference tool is mutmut, pinned to 3.8.1 (`mutantSet:
"mutmut@3.8.1"`). Every report names it.

## Which mutants

mutmut walks a file's libcst tree in source order and asks its operators, in
a fixed order, for mutants of each node it enters. It keeps only those inside
a function it copies: a top-level function, or a method of a top-level class
whose body is indented. It numbers them per function (`x_func__mutmut_3`,
`xǁClassǁmethod__mutmut_3`). `python_mutants.rs` walks ruff's tree the same
way and applies the same operators and skip rules:

- Operators, in mutmut's order: numbers, strings, names (`True`, `False`,
  `deepcopy`), assignment values, augmented assignments, `not` and `~`
  removed, `dict(...)` keywords, call arguments (each replaced by `None`, then
  each removed where a call has two or more), string methods swapped, lambdas,
  `is`/`in` and `break`/`continue`, operators swapped, `match` cases removed,
  conditional expressions forced.
- Skipped, as mutmut skips them: annotations; decorators; a function with any
  decorator but `staticmethod` or `classmethod` alone, nested ones too;
  `__getattribute__`, `__setattr__` and `__new__`; an enum class's `__init__`
  and `_generate_next_value_`; calls of `len` and `isinstance`, arguments and
  all; a parameter whose default is not a name, number or string, the
  parameter whole; triple-quoted strings; and what a `# pragma: no mutate`
  comment (bare, `block`, `start` and `end`) keeps.
- Module-level code, class bodies, functions defined inside a module-level
  `if`, and classes on one line have no mutants: mutmut copies none of them.

mutmut's configuration is read as its defaults: no `do_not_mutate_patterns`
and no `mutate_only_covered_lines` (which also drops the lines coverage.py
excludes).

### Ids and descriptors

An id is `file:startLine:startCol-endLine:endCol:Mutator:descriptor`, with
1-based lines and 1-based columns in code points. mutmut reports no columns;
the span is the mutated node's as libcst's `PositionProvider` gives it
(parentheses of its own and whitespace left out), except that a comparison
mutant's span starts at its operator and ends with the operand after it (a
libcst `ComparisonTarget`). StrykerJS's mutator names are used where the
mutation is the same, so the weak rules keyed by them apply:

| mutmut operator | Mutator | Descriptor |
| --- | --- | --- |
| `operator_number` | `NumberLiteral` | the new literal, as Python's `repr` writes it (`2`, `2.5`, `1e+16`, `4j`) |
| `operator_string` | `StringLiteral` | `XX`, `lower`, `upper` |
| `operator_name` | `BooleanLiteral` / `Name` | `True`, `False` / `copy` |
| `operator_assignment` | `AssignmentValue` | `None`, or `""` for a value that is `None` |
| `operator_augmented_assignment` | `AugmentedAssignment` | `=` |
| `operator_remove_unary_ops` | `BooleanLiteral` (`not`), `UnaryOperator` (`~`) | `removed` |
| `operator_dict_arguments` | `DictKeyword` | the new keyword (`aXX`) |
| `operator_arg_removal` | `CallArgument` | `None@<n>`, `removed@<n>`: the argument's 1-based place |
| `operator_symmetric_string_methods_swap`, `operator_unsymmetrical_string_methods_swap` | `MethodExpression` | the new method |
| `operator_lambda` | `Lambda` | `None`, or `0` for a body that is `None` |
| `operator_keywords` | `EqualityOperator` (`is`, `in`), `LoopControl` | `is not`, `in`, ... / `return`, `break` |
| `operator_swap_op` | `ArithmeticOperator`, `UnaryOperator`, `LogicalOperator`, `EqualityOperator`, `AssignmentOperator` | the new operator |
| `operator_match` | `MatchCase` | `removed@<n>` |
| `operator_if_exp` | `IfExpression` | `false` (`(test) and False`), `true` (`(test) or True`) |

A mutant also carries its edit: the bytes of the source it replaces and the
text mutmut's code generator writes there (`a <= b`, `f(a, )`: removing the
last argument keeps the comma before it, as libcst does).

### Parity

`scripts/mutmut-parity.py` runs mutmut's own visitor over Python files and
prints each mutant with mutmut's name, Supercov's span, mutator and
descriptor, and the shortest edit that turns the file into mutmut's mutated
text. `--write` writes the goldens in
`crates/supercov-engine/tests/fixtures/mutmut-parity`; the engine's test
compares its enumeration with them: the same mutants, in the same order, with
the same spans, numbers and edits, byte for byte. `--root` and
`--module-root` print a project's ids beside mutmut's full names, which is
how mutmut's verdicts join Supercov's. On the validation corpus's 21 source
files (5,438 mutants) the enumeration and mutmut's agree on every mutant
(`SUPERCOV_MUTMUT_PARITY=<dir> cargo test -p supercov-engine --lib
python_mutants -- --ignored`).

`1e20` plus one is `1e20` as a float: such a number mutant is `equivalent`
(`same-number`), the only static rule Python has.

## Reach

A mutant is linked to the innermost statement and function points of the
run's manifest that hold it, with the instrumenter's ids. A Python mutant's
statement is always inside its function, so the plan does not widen a test's
reach to a statement it did not run (JavaScript's rule for an exported
object's method). A mutant of a parameter's default is reached by the
function's first statement, which runs whenever the function is called: the
default is used then, not where the `def` ran.

A run lists its mutants from its sources the first time they are asked for,
as JavaScript's does (`mutant_list::list_from_sources` routes `.py` files to
`python_mutants::list_mutants`).

## Values: `SUPERCOV_MUTANTS=1`

With `SUPERCOV_MUTANTS=1` a run lists its mutants as it measures, archives
them as `mutants.json`, and places a value probe where one decides a
mutant's infection. The engine writes each probe into the file's plan
(`mutantProbes`: a site, a kind and the expression's span); the runtime wraps
that expression, before the coverage probes, in a call that hands the program
the expression's own value, evaluated once and in its own order, and records
what the mutant's value would have been beside it. Each outcome is written
once per site and context, as a `cmp` record:

| Kind | Mutants | Outcomes |
| --- | --- | --- |
| `compare` | `<`, `<=`, `>`, `>=` swapped, in a comparison that is not a chain | the operands' relation: `lt`, `eq`, `gt`, `unordered` (NaN), or `other` |
| `arith` | `ArithmeticOperator` | `same` or `differs` |
| `none` | `AssignmentValue` (`None`), `CallArgument` (`None@<n>`) | `same` where the value was `None` |
| `sign` | `UnaryOperator` (`-x` and `+x`) | `same`, `differs` |
| `method` | `MethodExpression` | `same`, `differs` |
| `iffalse`, `iftrue` | `IfExpression` | `same` where the test already had that truth |

`same` is decided only where nothing of the program's runs: both operands of
an operator are built-in values (numbers, strings, bytes, tuples, lists,
sets, `None`), a method's receiver is a `str` or `bytes`, a test's value is a
built-in one whose truth is read without code. It means the same type and
value, a float's sign of zero included; NaN is never the same. Anything else
records `other`, which leaves the mutant `unknown`. Where the original
expression raises, the probe records `differs` (or `other` for a comparison)
and raises as the program would.

The weak rules are JavaScript's: a boundary mutant whose operands were never
equal is `not-infected`; a mutant whose probe only saw `same` is
`not-infected`; a new literal, name, keyword or dictionary keyword is
`infected` wherever it ran; a removed `break` or `continue` is a proxy.

## The strong pass

### Switches

A mutmut mutant is mutmut's text of its function: mutmut copies the function
once per mutant and calls the copy while the mutant is on. Supercov switches
the same way, so a mutant behaves as mutmut's does, precedence included (`a
and b and c` with its first `and` swapped is `a or b and c`, reparsed). For
each function that holds mutants of a switched file, the engine writes where
the function is and each mutant's edit (`python_strong.rs`, `schemata.json`).
As the file is imported, the runtime (`supercov_strong.py`) parses each
mutant's copy of the function from the function's source with the edit
applied, and replaces the function's body with one switch:

```python
def f(...):
    """The docstring, kept first."""
    global names   # every copy's `global` and `nonlocal`, once
    if _scv_strong.a in {17, 18, 19}:
        ...   # a balanced test of `a` down to the mutant's copy of the body
    else:
        ...   # the original body
```

The copies keep the file's line numbers. A mutant of a parameter's own
default is switched by the function's defaults: a statement after the `def`
registers the mutated default, and switching the mutant on sets it in the
function's `__defaults__` or `__kwdefaults__`, as mutmut's copy carries it.
Each file binds its switch state from the runtime once (`_scv_strong =
strong_slot("pkg/mod.py")`), after its docstring and `__future__` imports.

A mutant whose edit does not parse, or whose file changed since the run (its
size and CRC-32 are in the schemata), gets no switch and is an `error`.

Each copy's loops count their iterations in one call of the function; past
the hit limit (1,000,000) the copy raises `MutantTimeoutError` and appends the
mutant to `<results>.timeouts.jsonl`, which makes it a `timeout`. A count per
process, as JavaScript's, made benchmark tests that call a function a million
times read as timeouts on humanize.

### The driver

The command runs as the user wrote it, with `sitecustomize` arming the
switches instead of the coverage runtime. Once its runner has collected the
tests, the driver takes over: pytest's `pytest_runtestloop` (the
`supercov_strong_pytest` plugin, through `PYTEST_PLUGINS`) or unittest's
`TestProgram.runTests` (`python -m unittest`, `discover` included). What
the command prints is held aside and shown with Supercov's diagnostics only
where the command fails (or with `SUPERCOV_VERBOSE`), so a `--json` answer
stays Supercov's alone.

1. Every switched file is compiled before anything forks, so each fork
   shares the code, and a mutant that could not be switched is known.
2. A baseline: one fork runs, without a mutant, every test some mutant runs,
   in collection order. A test that fails there is no kill.
3. Each mutant runs in a fork of the collected process with its mutant on,
   against its candidate tests in collection order (mutmut's order),
   stopping at the first that fails: `killed`, `killedBy` that test. A fork
   that ends before its tests did is a kill by the test it was running.
4. A kill where the mutant's run did not run the same tests before the
   killing one as the baseline did is confirmed: those tests run again
   without the mutant, and a test that fails that way too is no kill (the
   mutant runs again without it). A confirmation is kept by the tests it ran,
   so mutants with the same candidates share it.
5. A mutant whose candidates include code that ran while the modules loaded
   (`module-load`, or work no test was credited with: a test module's import
   that calls the function, a decorator applied at import) runs in a fresh
   interpreter of the same command with `SUPERCOV_ACTIVE_MUTANT` set from its
   start, as mutmut's runs import the test modules with the mutant on. A
   candidate test that interpreter does not collect (its module failed to
   import with the mutant) is a kill.
6. A mutant's time limit is three times its tests' baseline time and 10 s (5 s
   more in a fresh interpreter); past it the fork is killed and the mutant is
   a `timeout`, unless a test had already failed.

`SUPERCOV_STRONG_WORKERS` sets how many forks run at once (one per CPU). A
test the measured run skipped or failed runs too: the baseline says whether
it passes, and a mutant can turn a skip into a failure (a subtest skipped
before an assertion that fails). pytest-xdist is turned off in the pass (its
workers would collect elsewhere), and so is pytest-cov's measuring, which
changes no test's outcome and costs every fork its tracing. A test's own
`pytest` or `python -m unittest` subprocess runs its tests, not the pass.

`results.jsonl` ends with the driver's summary: forks, fresh interpreters,
confirmations, kills overturned, and the baseline's time.

## Validation

Four public projects (`benchmarks/mutation-python/README.md`): python-slugify
9.2.0, cachetools 8.0.2 (its `unittest.TestCase` suite run both by pytest
and by `python -m unittest`), parse 1.22.4 and humanize 4.17.0. The ground
truth is mutmut's full kill matrix: every test run against every one of
mutmut's 5,438 mutants (`kill_matrix.py`), on the same CI machines (16
CPUs) as Supercov. Supercov measured the same sources mutmut mutates
(`SUPERCOV_SOURCE_ROOTS`) with a plain run, and planned from reach alone.

| Project | Mutants | Both detect | Both leave | Matrix detects, Supercov not | Supercov detects, matrix not |
| --- | --- | --- | --- | --- | --- |
| python-slugify | 1,124 | 936 | 186 | 0 | 2 |
| cachetools (pytest) | 1,660 | 1,521 | 136 | 0 | 3 |
| cachetools (unittest) | 1,660 | 1,521 | 136 | 0 | 3 |
| parse | 1,306 | 1,160 | 146 | 0 | 0 |
| humanize | 1,348 | 1,238 | 110 | 0 | 0 |

"Detects" is killed or timeout, as both tools score them; "leaves" is
survived or no coverage. Every mutant the matrix detects, Supercov detects.
The five it detects and the matrix does not were each applied to the source
by hand and the suite run as written; each fails it:

- python-slugify `__main__.py:44:5-45:108:CallArgument:None@2` and
  `removed@2`: `test_algorithm_cli` runs `python -m slugify --algorithm
  invalid` and expects argparse to refuse the choice. mutmut's trampoline
  switches a mutant only where the function's module is the mutant's, and a
  module run with `-m` is `__main__`, so mutmut never switches them on there;
  Supercov's import hook does.
- cachetools `__init__.py:877:77-877:82:BooleanLiteral:True` (`cachedmethod`'s
  `info=False`), `__init__.py:888:21-888:59:IfExpression:true` and
  `_descriptor.py:21:13-21:33:AssignmentValue:None` (`__set_name__`): the test
  modules apply `@cachedmethod` to their classes as they are imported. The
  matrix imports them once, without a mutant, before it forks; mutmut runs
  only the tests that called the function while its statistics were
  gathered, which a decorator run at import is not. Supercov runs such a
  mutant in a fresh interpreter with the mutant on from its start, and
  `test_decorator_attributes` and `test_class_access_pickle` fail.

The same validation on GitHub's 4-CPU runners (`.github/workflows/mutation-python.yml`,
the matrix in shards under ten minutes) agrees mutant for mutant, but for one
more cachetools mutant Supercov detects and the matrix does not:
`func.py:87:23-87:47:CallArgument:removed@2`, `RRCache(maxsize, choice)` with
`choice` dropped. `test_rr_choice` expects `min` to pick the key evicted; the
mutant evicts at random, and the test fails 16 runs in 20 with the mutant
applied by hand. The matrix and mutmut drew a passing run there, and the
16-CPU matrix a failing one.

Measured with the mutants' values (`SUPERCOV_MUTANTS=1`) the verdicts are the
same, mutant for mutant. The probes decided 19 survivors without running
them (a boundary whose operands were never equal, a value that was already
the mutant's: 4 on python-slugify, 5 on parse, 10 on humanize), and the kill
matrix detects none of them; they also take from each mutant the tests that
reached it without infecting it. `scripts/python-probe-spans.py` checks that
every probe the engine plans names an expression CPython parses at that span:
all 2,535 of the corpus's do.

Earlier passes disagreed in more places, each fixed: a test the measured run
skipped (a subtest skipped before the assertion that would fail) was no
candidate (python-slugify, 2); a mutant whose module a test module imports
lost its kill when the fresh interpreter could not collect the test (6 on
python-slugify, 24 on cachetools); and a count of evaluations per process made
humanize's benchmark tests read as timeouts (3); and with values, a value
made `None` whose evaluation raised read as never evaluated, and a generator
that is a call's only argument was probed at the wrong span (12). The first matrix was also
made on another machine, where a locale and gettext's tools changed what
humanize's tests do (4), and without parse's README in mutmut's tree, whose
doctests the suite runs (8): the matrix above was made again on the CI machines, with
the README copied.

### Speed

Wall time on 16 CPUs, mutmut with one child per CPU, Supercov's forks one
per CPU:

| Project | mutmut run | per mutant | Supercov measured run | strong pass | per mutant |
| --- | --- | --- | --- | --- | --- |
| python-slugify | 10.4 s | 9.2 ms | 1.3 s | 9.7 s | 8.6 ms |
| cachetools (pytest) | 27.0 s | 16.3 ms | 1.5 s | 18.4 s | 11.1 ms |
| cachetools (unittest) | | | 1.0 s | 13.3 s | 8.1 ms |
| parse | 269.2 s | 206 ms | 1.4 s | 14.1 s | 11.0 ms |
| humanize | 65.8 s | 48.8 ms | 5.2 s | 14.6 s | 10.9 ms |

On GitHub's 4-CPU runners, each tool on a runner of its own, with parse's
pytest-cov off for mutmut too (as a strong pass has it):

| Project | mutmut run | per mutant | Supercov measured run | strong pass | per mutant |
| --- | --- | --- | --- | --- | --- |
| python-slugify | 36.7 s | 32.6 ms | 1.3 s | 40.5 s | 36.1 ms |
| cachetools (pytest) | 65.9 s | 39.7 ms | 1.9 s | 45.1 s | 27.1 ms |
| cachetools (unittest) | | | 1.5 s | 39.9 s | 24.1 ms |
| parse | 70.5 s | 54.0 ms | 2.3 s | 75.8 s | 59.6 ms |
| humanize | 211.9 s | 157.2 ms | 6.3 s | 54.1 s | 40.4 ms |

GitHub's runners vary from one to the next: an earlier run of the same
workflow took mutmut 63.6 s on python-slugify (Supercov 40.8 s) and 293.1 s on
humanize, and parse's strong pass with values took 47.2 s against 75.8 s
without. On these runners Supercov is faster on cachetools and humanize, and
about mutmut's speed on python-slugify and parse (whose 16-CPU mutmut time
above has pytest-cov on).

Per mutant is the run's time over the mutants it ran (Supercov's strong pass
alone; with its measured run, python-slugify is 9.7 ms a mutant, about
mutmut's). parse's suite turns pytest-cov on in its own configuration: every
mutmut worker builds a coverage report over the mutated tree, which a strong
pass does not. On python-slugify, whose CLI tests start `python -m slugify`
in subprocesses, keeping each switched file's compiled code beside the
schemata took the pass from 147 s to 9.7 s: every subprocess parsed every
mutant's copy before.

### The measured run's cost

The suite alone, under `supercov --`, and under `supercov --` with
`SUPERCOV_MUTANTS=1`, each the best of three (`overhead.sh`; humanize's
pytest-benchmark tests run once, since they size their rounds to the speed
they measure), on 16 CPUs and on GitHub's 4:

| Project | Tests alone | Measured | Measured with values | 4 CPUs: measured | with values |
| --- | --- | --- | --- | --- | --- |
| python-slugify | 0.63 s | 0.85 s (1.33×) | 0.95 s (1.50×) | 1.35× | 1.62× |
| cachetools | 0.52 s | 0.98 s (1.88×) | 1.92 s (3.65×) | 1.84× | 3.46× |
| parse | 0.58 s | 0.72 s (1.24×) | 0.83 s (1.43×) | 1.24× | 1.44× |
| humanize | 1.05 s | 1.25 s (1.19×) | 1.33 s (1.27×) | 1.25× | 1.44× |

These include the interpreter's start and pytest's collection, which the
probes do not slow. cachetools' values cost has not been profiled yet.

## Not yet

- Windows: the driver forks.
- A suite that runs unittest through its own script (`TextTestRunner().run`)
  rather than `unittest.main`: its tests are not driven, and its mutants are
  `error`.
- mutmut's `do_not_mutate_patterns` and `mutate_only_covered_lines`
  configuration is not read.
