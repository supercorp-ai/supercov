# Compiler frontend findings — 2026-09-09

## TS7-001 — Windows canonical path rejected by Node's module resolver

Reproduced and fixed in review commit `408a3f2`. Both Windows architectures
reproduced EISDIR from `realpathSync` while resolving TypeScript from a `\\?\C:`
path. File-URL lookup works on the same installation. Probe and product lookup
regressions are in CI. Installed-package matrix verification remains required.
This is separate from the formerly unsupported TypeScript 7 API.

## TS7-002 — aliased compilers still compete for the same executable name

During development, installing the `typescript-native` alias changed which
package supplied `.bin/tsc`. That accidentally built the analyzer with 7.0.2,
whose default ambient types differ. Fixed by naming the pinned TypeScript 5
build executable explicitly. This is tooling ambiguity, not a sample-code bug.

## TS7-003 — edited virtual config did not expose an added root source file

A native 7.0.2 API probe parsed the first source, then failed to expose the next
after updating the virtual config's file list and invalidating the snapshot.
The exact upstream cause is not established. The frontend now uses distinct
in-memory parser configs, checks exact source text, and has a multiple-file
regression. It writes no files into the application. Do not claim an upstream
fix or silently accept a missing source tree.

## TS7-004 — native resolver lacks a general helper-specifier resolution adapter

Known explicit limitation, not fixed by guessing or by using another compiler.
Actual import/export nodes can be resolved by the native checker. A string only
passed to `vi.mock` / `vi.importActual` may not have such a reference. Unresolved
cases are reported in compiler/public limitations; the existing candidate model
and null assertion score remain. Broader support needs its own calibration.

## REVIEW-005 follow-up — installed README links

The two links to omitted historical research notes now point to the shipped
public assertion guide, so both the repository and npm artifact have valid
targets. Historical notes were not copied into the distribution.
