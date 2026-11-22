// cargo-mutants' own mutants for a Cargo package, normalized so Supercov's
// enumeration (crates/supercov-engine/src/rust_mutants.rs) can be compared
// with them entry by entry.
//
//   node scripts/cargo-mutants-parity.mjs [--root DIR]
//   node scripts/cargo-mutants-parity.mjs --write-goldens [--root DIR]
//
// It runs `cargo mutants --list --json` in DIR (the default is the parity
// fixture, crates/supercov-engine/tests/fixtures/cargo-mutants-parity),
// which lists mutants without building or testing anything, and prints one
// JSON entry per mutant:
//
//   { file, genre, start: [line, column], end: [line, column], replacement }
//
// Lines are 1-based and columns 1-based in chars, as cargo-mutants reports
// them; `replacement` is cargo-mutants' replacement code, empty where it
// deletes. --write-goldens writes FILE.cargo-mutants.json beside each source
// FILE instead, in the order cargo-mutants lists them.
//
// The version is the installed cargo-mutants'; the goldens name it, and the
// engine's MUTANT_SET must say the same.

import { execFileSync } from "node:child_process";
import { writeFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const here = path.dirname(fileURLToPath(import.meta.url));
const args = process.argv.slice(2);
const write = args.includes("--write-goldens");
const rootAt = args.indexOf("--root");
const root =
  rootAt >= 0
    ? path.resolve(args[rootAt + 1])
    : path.join(here, "../crates/supercov-engine/tests/fixtures/cargo-mutants-parity");

const version = execFileSync("cargo", ["mutants", "--version"], { encoding: "utf8" })
  .trim()
  .replace(/^cargo-mutants\s+/, "");
const listed = JSON.parse(
  execFileSync("cargo", ["mutants", "--list", "--json"], {
    cwd: root,
    encoding: "utf8",
    maxBuffer: 1 << 28,
  }),
);
const byFile = new Map();
for (const mutant of listed) {
  const entry = {
    file: mutant.file,
    genre: mutant.genre,
    start: [mutant.span.start.line, mutant.span.start.column],
    end: [mutant.span.end.line, mutant.span.end.column],
    replacement: mutant.replacement,
  };
  if (!write) {
    console.log(JSON.stringify(entry));
    continue;
  }
  if (!byFile.has(mutant.file)) byFile.set(mutant.file, []);
  byFile.get(mutant.file).push(entry);
}
if (write) {
  for (const [file, mutants] of byFile) {
    const lines = mutants.map((mutant) => `  ${JSON.stringify(mutant)}`).join(",\n");
    writeFileSync(
      path.join(root, `${file}.cargo-mutants.json`),
      `{\n"mutantSet": "cargo-mutants@${version}",\n"file": ${JSON.stringify(file)},\n"mutants": [\n${lines}\n]\n}\n`,
    );
  }
}
