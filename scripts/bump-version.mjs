#!/usr/bin/env node
// Move Supercov's own version across the four files that carry it.
//
// The match is anchored on the closing quote, never a bare substring. A
// dependency pin can share the release's prefix -- `ra_ap_syntax` is pinned at
// 0.0.349, which starts with 0.0.34 -- and a substring bump rewrites it to a
// version that does not exist. That fails nowhere locally and everywhere on the
// release runners, after the tag is already pushed. Every file has a known
// number of Supercov references; any other count means the world changed and
// the bump stops rather than guessing.
import { readFileSync, writeFileSync } from "node:fs";
import { resolve } from "node:path";

const repository = resolve(import.meta.dirname, "..");

// Every quoted occurrence in these files is Supercov's own version.
const EXPECTED = {
  "package.json": 9,
  "package-lock.json": 12,
  "Cargo.toml": 3,
  "Cargo.lock": 3,
};

export function bump(from, to, root = repository) {
  const quoted = (version) => `${version}"`;
  const planned = [];
  for (const [name, expected] of Object.entries(EXPECTED)) {
    const path = resolve(root, name);
    const before = readFileSync(path, "utf8");
    const count = before.split(quoted(from)).length - 1;
    if (count !== expected) {
      throw new Error(
        `${name}: ${count} reference(s) to ${from}, expected ${expected}; refusing to bump into a file that does not look as expected`,
      );
    }
    const after = before.replaceAll(quoted(from), quoted(to));
    // A third-party pin that shared the prefix must read exactly as it did.
    const pinsBefore = before.match(/(?<=")[0-9]+\.[0-9]+\.[0-9]+(?=")/g) ?? [];
    const pinsAfter = after.match(/(?<=")[0-9]+\.[0-9]+\.[0-9]+(?=")/g) ?? [];
    const untouched = pinsBefore.filter((pin) => pin !== from);
    const survived = pinsAfter.filter((pin) => pin !== to);
    if (untouched.join("\n") !== survived.join("\n")) {
      throw new Error(`${name}: a version other than Supercov's own would change; refusing`);
    }
    planned.push({ path, after, name, expected });
  }
  for (const { path, after } of planned) writeFileSync(path, after);
  return planned.map(({ name, expected }) => `${name}: ${expected} -> ${to}`);
}

if (process.argv[1] === new URL(import.meta.url).pathname) {
  const [from, to] = process.argv.slice(2);
  const semver = /^[0-9]+\.[0-9]+\.[0-9]+$/;
  if (!semver.test(from ?? "") || !semver.test(to ?? "")) {
    console.error("usage: node scripts/bump-version.mjs <from> <to>   e.g. 0.0.35 0.0.36");
    process.exit(2);
  }
  try {
    for (const line of bump(from, to)) console.log(`[bump-version] ${line}`);
  } catch (error) {
    console.error(`[bump-version] ${error.message}`);
    process.exit(1);
  }
}
