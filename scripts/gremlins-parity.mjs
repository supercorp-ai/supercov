#!/usr/bin/env node
// Writes the goldens Supercov's Go mutant enumeration is tested against:
// Gremlins' own list of mutants for a module, from a dry run with the
// mutators Supercov lists (`go_mutants::MUTATORS`).
//
//   node scripts/gremlins-parity.mjs <module dir> [<golden path>]
//
// The golden is a sorted JSON list of `file:line:column MUTATOR`, Gremlins'
// position of the token (1-based line and byte column). With no golden path
// it is written to `<module dir>/gremlins.json`. Gremlins is found on PATH or
// at $GREMLINS; it is pinned at 0.6.0 (go install
// github.com/go-gremlins/gremlins/cmd/gremlins@v0.6.0).
//
// Gremlins reads the module's name from the first line of go.mod, so a
// go.mod that starts with a comment leaves every mutant "NOT COVERED"; the
// list of mutants is the same either way.

import { execFileSync } from "node:child_process";
import { mkdtempSync, readFileSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";

const [directory, goldenArgument] = process.argv.slice(2);
if (!directory) {
  console.error("usage: gremlins-parity.mjs <module dir> [<golden path>]");
  process.exit(2);
}
const module = resolve(directory);
const golden = resolve(goldenArgument ?? join(module, "gremlins.json"));
const report = join(mkdtempSync(join(tmpdir(), "gremlins-parity-")), "report.json");
execFileSync(
  process.env.GREMLINS ?? "gremlins",
  [
    "unleash",
    "--dry-run",
    "--silent",
    "--invert-assignments",
    "--invert-logical",
    "--invert-loopctrl",
    "--remove-self-assignments",
    "--invert-bitwise",
    "--invert-bwassign",
    "-o",
    report,
    ".",
  ],
  { cwd: module, stdio: ["ignore", "ignore", "inherit"] },
);
const parsed = JSON.parse(readFileSync(report, "utf8"));
const mutants = parsed.files
  .flatMap((file) =>
    file.mutations.map(
      (mutation) => `${file.file_name}:${mutation.line}:${mutation.column} ${mutation.type}`,
    ),
  )
  .sort();
writeFileSync(golden, `${JSON.stringify(mutants, null, 2)}\n`);
console.log(`${mutants.length} mutants -> ${golden}`);
