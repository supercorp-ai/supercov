# Ruby mutation: the mutant gem's mutants, weak and strong

Ruby gets the mutants list, the weak report and `supercov mutants test`
through the contract in `docs/design/mutation-drivers.md`. The reference tool
is the mutant gem (0.17.1, its default light operators); its mutant set is
named `mutant@0.17.1` in every list and report.

## Which mutants

mutant mutates methods. Every `def` and `def self.x` of a measured file is a
subject, its mutations whole methods with one node replaced. Supercov
enumerates the same set without running mutant (`ruby_mutants/`):

- `tree.rs` converts Prism's tree into the parser gem's, as
  `Prism::Translation::Parser34` builds it with Unparser's modern builder:
  `(send recv :sel args...)`, `(begin ...)` for a statement list,
  `(args (procarg0 (arg :x)))` for a block's lone parameter, a string over
  several lines as one `str` per line. A method holding syntax the port does
  not convert yet (a heredoc, a constant pattern) is skipped whole, and
  named in the enumeration.
- `mutators.rs` is a port of mutant's mutator classes, emitting in their
  order, with their output sets: a mutation equal to its input or to one
  already emitted is dropped. mutant's `parent` is kept exactly, since rules
  read it (`left_op_assignment?`, a ternary under `return`, `super` in a
  method with parameters).
- mutant keeps a mutation only where Unparser prints it and parses it back to
  the same tree. That is emulated: a local variable read before any
  assignment to it (parsed again, it is a method call), a bare call by a
  `retry` outside a rescue clause, `break`, `next` or `redo`
  outside a loop or block, `a || (b || c)`, and a left operand that binds
  more loosely than its operator (`(a + b) * c` prints as `a + b * c`), a
  regexp literal in a condition (a match against `$_` once parsed), and
  `(a || b) && c` anywhere but a statement (Unparser writes it with `and`),
  a pin of anything but a variable or parentheses (`^nil`), `a[] = v`
  without its value, an operator assignment whose target became a plain
  value, a name in `alias` or `undef` made an expression, an interpolation
  of several statements (`"#{a; b}"`), and a regexp part that is neither
  text nor `#{...}` (`/a#@b/`, which Unparser prints `#{@b}`: a method
  holding one has no mutants). A call by a local variable's name is written
  with parentheses (`rule()`). Floats are equal as Ruby holds them equal, so
  `-0.0` made `0.0` is no change. Integers past 64 bits (and past `i128`)
  get mutant's 0, 1 and neighbours; an imaginary's value is `(0+2i)` or
  `(0-2i)`, as `inspect` prints it.
- A regexp's body is parsed as `regexp_parser` parses it (`regexp.rs`) and
  mutated as mutant's regexp mutators do: a quantifier removed or `*` made
  `+`, `\d` and `\D` (and the other character types, `\b`, `^`, `$`)
  swapped, an alternative removed, a capturing group made passive or a named
  one renamed `_name`. An exact interval followed by `?`, any interval
  followed by `+`, and intervals one after another are several quantifiers,
  as Ruby reads them (`a{2}?` is `(?:a{2})?`, `a{2}{3}` is
  `(?:a{2}){3}`), each removed apart; `a{1,2}?` is one reluctant
  quantifier. A mutation that leaves a back reference or subexpression call
  without its group (`(a)\1` made `(?:a)\1`), or a whole-regexp call
  (`\g<0>`) without an alternative that ends it, is not a regexp Ruby
  parses, and is dropped. A pattern with a conditional or a set intersection is not
  read, and gets no body mutations.

`scripts/mutant-parity.rb` prints mutant's own mutants of the fixtures in
`crates/supercov-engine/tests/fixtures/mutant-parity` and writes the goldens
(`FILE.mutant.json`); the engine's tests compare node by node: each method's
tree, and each mutant's located node and replacement, in mutant's order
(7,023 mutants on the fixtures, every one equal; the tests also take a
project's files and goldens from `MUTANT_PARITY_FIXTURES`). Beside parity
the tests check that every fixture method and every mutant of it, printed
from its tree alone, parses back to that tree; that every switch slot
printed in its own place does; that a method holding syntax the port does
not convert, put at any node in turn, is refused whole; and that printing,
mutating and validating malformed trees refuses instead of panicking.

### Ids

A mutation is located at the smallest node whose replacement it is: from the
method down, while exactly one child differs and it is a node on both
sides; a child that became nil in a body slot, or one statement dropped from
a list, is that node removed. Its id is
`file:startLine:startCol-endLine:endCol:Mutator:descriptor`, 1-based lines and
1-based columns in characters, where `Mutator` is mutant's mutator class for
the located node (`Send`, `If`, `Integer`, ...) and the descriptor is
`removed`, or the replacement's code on one line (past 40 characters, its
start and a hash of the whole). Two mutants that would share an id get `#2`,
`#3`.

## Weak mutation

Reach is read from the innermost statement and method points around the
mutant, as for every language. A run measured with `SUPERCOV_MUTANTS=1`
(what `supercov mutants` asks for) also places value probes where mutant
replaces an expression with one a probe can compute beside the original.
Each records per test `same`, `differs` or `unknown` at
`file:L:C-EL:EC#<kind>`, and a mutant is linked to a probe only where the
probe records its kind on every evaluation: no record then means the
expression never ran.

- A binary operator's receiver: `$__supercov.bo(k, (a)) < b`. The receiver
  stands in as an `Operand`, whose operator method computes the original
  value and, from the same operands, another operator (`==`, `eql?`,
  `equal?` for `<`), `!a.eql?(b)` for `!=`, the left or right operand alone,
  and `true`, `false` or `nil` in the operation's place.
- A method call that mutant replaces: another method from mutant's table
  (`any?` for `empty?`, `lstrip` and `rstrip` for `strip`), the receiver
  alone, `true` and `false` for a predicate, and `nil`:
  `$__supercov.mc(k, (s)).strip`. A plain receiver stands in as a `Call`,
  whose `method_missing` makes the call on it and computes the
  replacements; anything else is passed on as it is, and its kinds are
  `unknown`. A call with a block, a splat, keywords, `&.`, a `self`
  receiver, a method with an effect (`!` methods, `push`, `pop`, `send`,
  `sample`) or a name `BasicObject` answers itself is not probed, nor is
  any call in a file that activates a refinement (`using`), which a call
  made from the runtime would not see.
- An `if`, `unless`, `elsif` or ternary predicate that reads a variable (a
  local, instance or global variable, or `self`):
  `if $__supercov.cv(k, (x))`, against mutant's `true` and `false` in its
  place.

A value is compared only where both sides are plain (nil, booleans,
integers, finite non-zero floats, symbols, strings, rationals), and a
replacement is computed only from methods Ruby defines itself (a C method or
one in `<internal:`), on plain values, so computing it has no effect. In a
predicate (of `if`, `unless`, `while`, `until` or a ternary) only the
values' truth is compared, whatever they are. A mutant that leaves out an
operand, a receiver or an argument is linked only where what it leaves out
is a variable read or a literal without interpolation: anything else could
have done something beside giving its value. An original that raises
records `unknown`. `String#<<` appends to its receiver, so its kinds are
`unknown`.

`=~`, `!~` and `===` are not probed: a regexp's match sets `$~` in the frame
that calls it, which would be the probe's. mutant's mutants of `return`
(`return x` made `x`, or `nil`) change where the method returns, not a
value, and are left to the strong pass.

Every other mutant is `unknown` once reached: the plan takes every test
that reached it.

### What the probes cost

A probe compares at most 8 evaluations per test (`VALUE_TURNS`); past them,
or once every kind it records has differed or met a value it cannot
compare, it is done for that test. A done probe, and a condition's probe
given a truth it has already recorded in the test, is handed back by the
receiver (`Probe#bo`, `mc`, `cv`) from one array read, without a call into
the runtime. The array is filled only while every thread runs in one test,
and emptied at each test's start and end. Outcomes are kept as bits per test
and written with the test's hits, one record per test (`cmps`, ids
numbered as `hits` numbers them): a record per outcome had cost
dry-inflector's suite a third of its time. The mutants themselves are
listed on a thread of their own while the tests run.

## Strong mutation

### Schemata

A strong pass writes its Ruby mutants into copies of their files
(`ruby_mutants/schemata.rs`). A switch goes around the smallest node holding
the mutation that takes parentheses without changing the program: a
statement, an expression in a slot that holds any expression (a condition,
an operand, a receiver, an assigned or returned value, an argument in
parentheses, an element), a method body's statements, a body with its rescue
clauses (inside `begin ... end`), or before the `end` of an empty method:

```ruby
(if ::Supercov::Strong::MUTANT == 1000017 then ::Supercov::Strong.h; (copy) elsif ... else (original) end)
```

Each copy is the node with one mutant applied, printed on one line: no line
after a switch moves, so backtraces and `__LINE__` stay those of the file.
Every edit inserts text; the original stays between the switch's halves. A
mutant whose only change is a method's parameters (an optional parameter
made required) is switched in its default, which then raises the
`ArgumentError` the required parameter would, where a call leaves it out.
A receiver that is or becomes `self` is never switched alone: `(x).open`
calls a private method only where `x` is written `self.`, so the switch goes
around the call (dotenv's `File.open` made `self.open` was a kill that
mutant, applying the mutant as written, rightly calls a survivor).

### The runtime and the driver

`runtime/ruby/supercov_strong.rb` loads through `RUBYOPT` in place of the
coverage runtime. It compiles each switched file from its copy through the
`load_iseq` hook, counts the active mutant's evaluations (`h`) and past the
hit limit raises `Supercov::Strong::MutantTimeout`, an `Exception` that a
`rescue => e` does not catch, noting the mutant in the timeouts file. It
reads `SUPERCOV_ACTIVE_MUTANT` at start. A switch reads the active mutant
from a constant, `Supercov::Strong::MUTANT`, which code running in a Ractor
can read (a global it cannot: ostruct's Ractor tests failed under every
mutant); a Ractor's evaluations are not counted against the hit limit.

When the suite has loaded and RSpec (`Runner#run_specs`), Minitest
(`Minitest.run`), test-unit (`AutoRunner#run`) or Cucumber (`Runtime#run!`,
once its step definitions have loaded) is about to run it, it runs the plan
instead, as mutant does: one `fork` of the loaded process per mutant, as
many at once as `SUPERCOV_STRONG_JOBS` (default: the machine's CPUs).

- A baseline fork runs every candidate test without a mutant first: a test
  that fails there is no kill, and its duration sets each mutant's time
  limit (3× its tests' time and 10 s).
- A mutant's fork switches it on and runs its candidates in the suite's
  order, stopping at the first failure. RSpec runs the chosen examples
  through its own `run_specs`, with its reporter, formatters and
  `before(:suite)` hooks, output thrown away: code a suite hook calls
  (dotenv's `Dotenv.save`) runs with the mutant on, as under mutant, and a
  failure outside any example kills. A first version ran the groups
  itself: 35 of dotenv's 195 examples failed without a mutant, because
  nothing had loaded `StringIO` as RSpec's formatters do. Minitest runs each
  test by class and method, after its seed and plugins are set up as
  `Minitest.run` sets them. test-unit runs the chosen tests through its own
  mediator, a suite per class so the class's `startup` and `shutdown` run
  around them; a data-driven test runs every data set under its one id.
  Cucumber compiles the feature files through its own filters (tags, names,
  lines, hooks, the `BeforeAll` hooks) with one more that passes on only the
  chosen scenarios, into its own test runner; a scenario's id is its
  location (`features/a.feature:3`).
- A kill is confirmed: the same tests up to the failing one run again
  without the mutant. Where they fail that way too, the candidates' files
  run whole with and without the mutant, and only a test that fails only
  with it kills.
- A fork past its limit is killed: `timeout`. A fork that ends without
  reporting (the mutant crashed the process) is `killed`. A fork ends with
  `exit!`, so the suite's `at_exit` hooks (an autorun) do not run in it, and
  its outcome is read while it runs, so a large one cannot fill the pipe and
  hold it.
- A Ruby process a test starts (`assert_separately`, a CLI it runs)
  inherits `RUBYOPT`: the pass sets `SUPERCOV_ACTIVE_MUTANT` as it switches
  a mutant on, so that process runs the same mutant, as it would load the
  mutated file from disk, and `SUPERCOV_STRONG_SWITCHES_ONLY`, so it runs
  its own tests and never a pass. mutant's mutations live in its own
  process only; the validation against it found no disagreement from this.
- A switched file that was loaded before the runtime (a gemspec that
  requires the library, which `bundle exec` loads before `RUBYOPT`'s own
  files) has no switches: its mutants are `error`, never `survived`.

`ruby_run::run_strong_ruby` is the driver's Rust half: it checks each
mutant's id against its file as it is now (a changed file makes its mutants
`error`), writes the switched copies and the runtime's plan, and runs the
command once.

## Validation

`benchmarks/mutation-ruby/run-job.sh` runs, on one public project, mutant
with every test as a candidate for every subject (RSpec's
`mutant_expression` metadata and Minitest's `cover` set to the subjects),
then Supercov's measured run and strong pass, and joins the two:
`supercov runs latest mutants --stryker .mutant/results` reads mutant's
session results and matches each mutation to Supercov's mutant by the method
it makes. `summarize.rb` counts every disagreement.

mutant has no test-unit or Cucumber integration. For those,
`benchmarks/mutation-ruby/by-hand-job.sh` runs Supercov's strong pass and
then `by-hand.rb`, which runs every mutant the pass ran again on its own:
the whole suite, in a fresh process, with only that mutant switched on
(`SUPERCOV_STRONG_SWITCHES_ONLY` and `SUPERCOV_ACTIVE_MUTANT`), killed when
the suite fails. Nothing of the pass's runner is used: no candidate tests,
no forks, no stopping at the first failure. That strong pass runs on a run
measured without the value probes, so no weak verdict decides a mutant and
every mutant a test reached is run both ways; the weak verdicts come from a
second run with the probes, and one of not infected for a mutant the suite
killed by hand counts as a violation.

Both jobs also time five runs each of the plain suite, Supercov without the
value probes and Supercov with them, and take the medians.

### Results

On a 16-CPU Linux runner (`c8a.4xlarge`, Ruby 3.4.13, 16 jobs for both tools),
mutant run with every test a candidate for every subject and
`process_abort` counted as a kill:

| Project (suite) | mutant's mutants | joined | Supercov only | strong disagreements | weak violations | mutant | Supercov (measured + strong) |
| --- | --- | --- | --- | --- | --- | --- | --- |
| dry-rb/dry-inflector 1.3.1 (RSpec, 1,138 examples) | 2,061 | 2,061 | 0 | 0 | 0 | 32.2 s | 1.5 + 51.7 s |
| bkeepers/dotenv 3.2.0 (RSpec, 195 examples) | 1,345 | 1,345 | 987 | 0 | 0 | 21.4 s | 1.0 + 29.6 s |
| jeremyevans/erubi 1.13.1 (Minitest, 100 tests) | 1,571 | 1,571 | 84 | 0 | 0 | 13.4 s | 0.5 + 4.1 s |

Every mutation mutant made was matched to one of Supercov's. Supercov's own
are methods mutant did not load as subjects (dotenv's Rails integration and
CLI, which `require "dotenv"` does not load; erubi's methods with no test)
and are not part of the comparison. Per mutant, the strong pass took 25 ms
on dry-inflector against mutant's 16, 14 ms on dotenv against 16, and 2.6 ms
on erubi against 8.4.

Disagreements found on the way, each applied by hand and fixed:

- dotenv's `Dotenv.save` runs in a `before(:suite)` hook: the pass skipped
  suite hooks and missed 127 kills; it now runs RSpec's own `run_specs`.
- `File.open` made `self.open` was killed by Supercov alone: a switched
  receiver `(…).open` cannot call the private `Kernel#open`, which `self.`
  can; the suite as written passes with the mutant (now the switch goes
  around the call).
- 38 of erubi's `src << x` made `src` were called not infected: `String#<<`
  appends to its receiver, which the value comparison did not see.
- A mutant whose test calls `exit` ends the test process: mutant counts it
  only with `process_abort` on.
- Running first the tests that had killed a mutant nearby, to stop sooner,
  made 9 of dotenv's mutants kills that mutant calls survivors: a test that
  fails on its own with the mutant passes after the tests before it. The
  pass keeps the suite's order.
- ostruct's suite runs test-unit through `ruby -rhelper` (as Rake's test
  task does), which loads test-unit before Supercov's runtime: its tests ran
  after the runtime had closed its evidence, and a child started with
  `--disable-gems` found no `RbConfig`. Both are fixed in the runtime.

## Not yet

- The test-unit and Cucumber drivers have run under test-unit 3.7 and
  Cucumber 12 only. The Cucumber one uses cucumber-core's filters and event
  bus, which releases before 3 do not have.
