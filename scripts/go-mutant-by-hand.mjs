#!/usr/bin/env node
// Applies each disputed Go mutant by hand and runs the suite as written,
// whole, several times: what the module's own tests say of the mutant,
// whatever either tool's selection or timing did.
//
//   node scripts/go-mutant-by-hand.mjs <module dir> <compare.json> [runs] [kinds]
//
// <compare.json> is `gremlins-compare.mjs --json`'s output. Each mutant of
// the listed kinds (default `soundness,other`) is applied to a copy of the
// module (its token, from its id, replaced) and `go test -count=1 ./...`
// runs `runs` times (default 5). Prints one JSON line per mutant.

import { spawnSync } from "node:child_process";
import { cpSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";

const [moduleArgument, compareArgument, runsArgument = "5", kindsArgument = "soundness,other"] =
  process.argv.slice(2);
const module = resolve(moduleArgument);
const runs = Number(runsArgument);
const kinds = new Set(kindsArgument.split(","));
const compared = JSON.parse(readFileSync(compareArgument, "utf8"));
for (const entry of compared.list.filter((entry) => kinds.has(entry.kind) && entry.id)) {
  const match = entry.id.match(/^(.*):(\d+):(\d+)-(\d+):(\d+):([A-Za-z_]+):(.*)$/);
  const [, file, line, column, endLine, endColumn, , replacement] = match;
  const copy = mkdtempSync(join(tmpdir(), "go-mutant-"));
  cpSync(module, copy, {
    recursive: true,
    filter: (source) => !source.includes("/.supercov") && !source.includes("/.git/"),
  });
  const path = join(copy, file);
  const bytes = readFileSync(path);
  const offset = (lineNumber, columnNumber) => {
    let at = 0;
    for (let index = 1; index < lineNumber; index += 1) at = bytes.indexOf(10, at) + 1;
    return at + columnNumber - 1;
  };
  const start = offset(+line, +column);
  const end = offset(+endLine, +endColumn);
  writeFileSync(
    path,
    Buffer.concat([bytes.subarray(0, start), Buffer.from(replacement), bytes.subarray(end)]),
  );
  let failed = 0;
  let firstFailure = null;
  for (let run = 0; run < runs; run += 1) {
    const result = spawnSync("go", ["test", "-count=1", "./..."], {
      cwd: copy,
      encoding: "utf8",
      timeout: 120_000,
    });
    if (result.status !== 0) {
      failed += 1;
      firstFailure ??= (result.stdout + result.stderr)
        .split("\n")
        .find((text) => /--- FAIL|panic:|build failed|\.go:\d+:\d+:/.test(text)) ?? `exit ${result.status}`;
    }
  }
  rmSync(copy, { recursive: true, force: true });
  console.log(JSON.stringify({ ...entry, byHand: { runs, failed, firstFailure } }));
}

