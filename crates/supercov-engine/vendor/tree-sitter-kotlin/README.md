# Kotlin grammar

The generated tree-sitter parser of
[tree-sitter-grammars/tree-sitter-kotlin](https://github.com/tree-sitter-grammars/tree-sitter-kotlin)
1.1.0 (published as `tree-sitter-kotlin-ng`), MIT licensed (see LICENSE),
with fixes to its external scanner that upstream has not merged. Its last
release is from January 2025.

`src/parser.c` and `src/tree_sitter/` are the 1.1.0 files unchanged. `src/scanner.c`
carries these upstream pull requests:

- [#13](https://github.com/tree-sitter-grammars/tree-sitter-kotlin/pull/13), which
  includes [#11](https://github.com/tree-sitter-grammars/tree-sitter-kotlin/pull/11):
  a class body or statement ending on its own line (`class C { val x = 1 }`) or
  two members on one line no longer turn the whole file into an error, which
  made Supercov leave the file unmeasured.
- [#14](https://github.com/tree-sitter-grammars/tree-sitter-kotlin/pull/14): an
  annotation cut off at the end of the input no longer loops forever.
- [#20](https://github.com/tree-sitter-grammars/tree-sitter-kotlin/pull/20): an
  identifier that starts with a keyword (`in1`, `set_value`) at the start of a
  line is no longer read as the keyword.

The scanner changes need no regenerated parser. Moving to a newer upstream
release means replacing these files with its own and keeping whichever of the
fixes it still lacks.
