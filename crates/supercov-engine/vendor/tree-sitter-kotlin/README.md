# Kotlin grammar

Supercov's own copy of
[tree-sitter-grammars/tree-sitter-kotlin](https://github.com/tree-sitter-grammars/tree-sitter-kotlin)
1.1.0 (published as `tree-sitter-kotlin-ng`), MIT licensed (see LICENSE). Its
last release is from January 2025, and Supercov fixes it here.

- `grammar.js` is the grammar, and `src/parser.c` is what it generates:
  `node scripts/kotlin-grammar.mjs` regenerates it with tree-sitter-cli 0.24.7,
  which reproduces the 1.1.0 parser byte for byte from the 1.1.0 grammar, and
  `--check` fails when the two differ.
- `src/scanner.c` is the hand-written external scanner: where statements end,
  soft keywords, and raw string content.
- `node scripts/kotlin-corpus.mjs` parses every Kotlin file of six pinned real
  projects with the grammar and fails on a file that does not parse unless
  `scripts/kotlin-corpus-failures.txt` names it. A file the grammar cannot read
  is a file Supercov leaves unmeasured.

From upstream pull requests not merged there:

- [#13](https://github.com/tree-sitter-grammars/tree-sitter-kotlin/pull/13) (with
  [#11](https://github.com/tree-sitter-grammars/tree-sitter-kotlin/pull/11)): a class
  body or statement ending on its own line, and two members on one line.
- [#14](https://github.com/tree-sitter-grammars/tree-sitter-kotlin/pull/14): an
  annotation cut off at the end of the input no longer loops forever.
- [#15](https://github.com/tree-sitter-grammars/tree-sitter-kotlin/pull/15): `when`
  guards (`is X if cond ->`).
- [#16](https://github.com/tree-sitter-grammars/tree-sitter-kotlin/pull/16): an
  annotation before an accessor or constructor.
- [#17](https://github.com/tree-sitter-grammars/tree-sitter-kotlin/pull/17): `where`
  as a name.
- [#20](https://github.com/tree-sitter-grammars/tree-sitter-kotlin/pull/20): a name
  that starts with a keyword (`in1`, `set_value`) at the start of a line.
- [#21](https://github.com/tree-sitter-grammars/tree-sitter-kotlin/pull/21): an
  annotated declaration is a declaration.
- [#22](https://github.com/tree-sitter-grammars/tree-sitter-kotlin/pull/22): `get` as
  an infix function.
- [#23](https://github.com/tree-sitter-grammars/tree-sitter-kotlin/pull/23):
  multi-dollar strings (`$$"..."`).
- [#24](https://github.com/tree-sitter-grammars/tree-sitter-kotlin/pull/24): a `get(`
  or `set(` call after a local property is a call, not an accessor.

Supercov's own, each found in the corpus:

- context parameters and receivers (`context(scope: Scope)`), on declarations and
  function types, in either order with `suspend`;
- `suspend { }`, and `context`, `dynamic`, `open`, `suspend` and `enum` as names;
- `suspend` before a parenthesized type, and `String?::plus`;
- a raw string ending in more than three quotes;
- `actual constructor`, and a primary constructor on a later line after an annotation;
- an annotated setter after an expression-bodied getter, and annotations whose
  arguments span lines or are followed by line comments;
- `set(a, b)` as a call, and `get` on a new line never read as infix;
- two file annotations on one line, and `::ref` starting a line.
