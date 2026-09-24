---
name: supercov
description: Measures test coverage, code quality and security in a repository with the supercov CLI, and turns what it finds into small, focused tests or fixes. Use when the user asks to add or improve tests, find untested code or raise coverage, find what to refactor, or scan code for security problems. Works with JavaScript, TypeScript, Python, Ruby, Rust, Go, Java and Kotlin projects.
license: MIT
---

# Supercov

Supercov runs the project's own test command and changes no source, tests or configuration. `npx supercov docs <topic>` prints the guide for the installed version; read it for anything not shown here. Topics: `agent-loop`, `quality`, `security`, `supported-suites`, `troubleshooting`.

## Add a test

1. Measure the full suite: `npx supercov -- <test command>`, for example `npx supercov -- npm test`.
2. List targets with `npx supercov runs latest gaps --limit 5`, pick one, and inspect it with `npx supercov runs latest file <path>`.
3. Write one focused test. Change only tests unless the user asks otherwise.
4. Rerun the same command and report the test and the coverage before and after.

## Quality and security

- `npx supercov quality` ranks files by code smells. `npx supercov security` flags risky code by line.
- For a change, use `npx supercov quality patch` or `npx supercov security patch`.
- Both send source files to TypeSafe and read the key only from the `TYPESAFE_API_KEY` environment variable. If it is not set, ask the user to set it.

Without Node.js 22 or newer, install Supercov as the language guide at https://supercov.com/docs says, and run `supercov` instead of `npx supercov`.
