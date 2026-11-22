---
description: Check that the tests fail when the code is wrong, by running them against mutants with Supercov, and write tests for the survivors.
argument-hint: "[--diff base] [-- test command]"
disable-model-invocation: true
---

Run `npx supercov mutants $ARGUMENTS`. With `--diff <base>` it keeps only the mutants on lines changed since `<base>`; with `-- <test command>` it measures that command first. Without either it tests every mutant of the latest run, measuring the project's full suite first if there is no run.

1. Read the surviving mutants: `npx supercov runs latest mutants <path>` lists a file's by line, with what each changes and why; with `--json`, each mutant's `strong.status` says whether it `survived`.
2. Write a focused test that fails on one survivor. Change only tests.
3. Run the same `npx supercov mutants` command again and report which mutants the new test kills and which still survive.

Do not change source code to kill a mutant. If no test can tell a mutant apart from the original, say why. `npx supercov docs mutation` has every option for the installed version.
