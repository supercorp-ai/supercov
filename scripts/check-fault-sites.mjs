#!/usr/bin/env node

// Tests fail Supercov's own calls through `SUPERCOV_TEST_FAULT`, addressing
// a call by the name a `// fault-site: NAME` comment above it gives
// (crates/supercov-engine/src/fault.rs). They once addressed it by its file
// and line, `fs.rename#lifecycle.rs:1194`, and asserted the line in the
// message: every edit above such a call broke tests in other files, often
// only on the last platform a CI run finished. This holds the line to names:
//
// - no test, script or source addresses a call or matches a message by a
//   source file and line (`lifecycle.rs:1194`, `lifecycle\.rs:1194`,
//   `lifecycle.rs:{line}`, or a site's file alone, `fs#lifecycle.rs`);
// - every name is given once, across the crates (each crate's build fails on
//   a repeat within it), and two crates name no place by the same path;
// - every `site#NAME` a test or script writes is a name some comment gives
//   (but in fault.rs, whose own tests name a site no comment gives).

import { execFileSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { basename, resolve } from "node:path";

const repository = resolve(import.meta.dirname, "..");
const tracked = execFileSync("git", ["ls-files", "crates", "scripts"], {
  cwd: repository,
  encoding: "utf8",
})
  .split("\n")
  // This script, whose examples are the pins it refuses, is not checked.
  .filter((file) => /\.(rs|mjs|js)$/u.test(file) && !file.includes("/fixtures/"))
  .filter((file) => file !== "scripts/check-fault-sites.mjs");
const read = (file) => readFileSync(resolve(repository, file), "utf8");
const failures = [];

// The crates' own sources, by file name: a test that names one with a line
// pins that line. A path before the name (`src/lib.rs:5`) is a project's
// file, not Supercov's.
const sources = tracked.filter((file) => /^crates\/[^/]+\/src\/.*\.rs$/u.test(file));
const names = [...new Set(sources.map((file) => basename(file, ".rs")))].map((name) =>
  name.replace(/[^\w]/gu, ""),
);
const pin = new RegExp(
  String.raw`(?<![\w/\\.-])(?:${names.join("|")})(?:\\?\.)rs:(?:\d|\{|\$\{)(?!\d*:\d)|#(?:${names.join("|")})(?:\\?\.)rs\b`,
  "u",
);
// The site comments, and every `site#NAME` written to fail one.
const marker = /^\s*\/\/ fault-site:\s*(\S+)\s*$/u;
const reference = /\w#([a-z][a-z0-9]*(?:[-.][a-z0-9]+)*\.[a-z0-9]+(?:[-.][a-z0-9]+)*)/gu;
const named = new Map();
const placed = new Map();
const referenced = [];

for (const file of tracked) {
  const lines = read(file).split("\n");
  const crate = file.match(/^crates\/([^/]+)\/src\/(.*)$/u);
  lines.forEach((line, index) => {
    const where = `${file}:${index + 1}`;
    if (pin.test(line)) {
      failures.push(`${where}: a call or message addressed by its source line; name the call with \`// fault-site: NAME\` and use the name\n    ${line.trim()}`);
    }
    const site = line.match(marker);
    if (site && crate) {
      const [, name] = site;
      if (named.has(name)) failures.push(`${where}: \`${name}\` names a site already, at ${named.get(name)}`);
      else named.set(name, where);
      const owner = placed.get(crate[2]);
      if (owner && owner !== crate[1]) {
        failures.push(`${where}: ${crate[1]} and ${owner} both name places in src/${crate[2]}, which a test could not tell apart`);
      }
      placed.set(crate[2], crate[1]);
      return;
    }
    if (file.endsWith("/src/fault.rs")) return;
    for (const [, name] of line.matchAll(reference)) referenced.push([where, name]);
  });
}

for (const [where, name] of referenced) {
  if (!named.has(name)) failures.push(`${where}: no \`// fault-site: ${name}\` names a site`);
}

if (failures.length > 0) {
  console.error(failures.join("\n"));
  console.error(`[fault-sites] ${failures.length} problem(s)`);
  process.exit(1);
}
console.log(`[fault-sites] ${named.size} named sites, no line pins`);
