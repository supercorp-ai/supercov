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

// One native package per platform the release publishes.
const NATIVE_PACKAGES = 8;

// Every quoted occurrence in these files is Supercov's own version.
const EXPECTED = {
  "package.json": 9,
  "package-lock.json": 18,
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
    //
    // Compared position by position, never as two sets with the release's own
    // version filtered out. A dependency already sitting on the version being
    // released is indistinguishable from a bumped entry once both are filtered,
    // so that comparison refused every bump whose target collided with a
    // dependency: six packages in the lockfile are at 1.0.0, which blocked the
    // 1.0.0 release outright. Position keeps them apart, and still catches the
    // thing this guards against, a pin that shares the release's prefix being
    // rewritten to a version that does not exist.
    const pins = (text) => text.match(/(?<=")[0-9]+\.[0-9]+\.[0-9]+(?=")/g) ?? [];
    const pinsBefore = pins(before);
    const pinsAfter = pins(after);
    const intended = pinsBefore.map((pin) => (pin === from ? to : pin));
    if (pinsAfter.length !== intended.length
      || pinsAfter.some((pin, index) => pin !== intended[index])) {
      throw new Error(`${name}: a version other than Supercov's own would change; refusing`);
    }
    planned.push({ path, after, name, expected });
  }
  for (const { path, after } of planned) writeFileSync(path, after);

  // The lockfile records a tarball URL beside each native package's version,
  // and npm installs what the URL says. Those two drifted apart for twenty
  // releases -- the entries read 0.0.38 while the URLs still fetched 0.0.18 --
  // and every plain `npm ci` then installed binaries the launcher refused.
  // A quoted-version replacement cannot fix them, because the version in the
  // URL is not the one being bumped; name them after the release directly.
  const lockPath = resolve(root, "package-lock.json");
  const lock = readFileSync(lockPath, "utf8");
  const urls = /("resolved": "https:\/\/registry\.npmjs\.org\/@supercov\/(cli-[a-z0-9-]+)\/-\/\2)-[0-9.]+(\.tgz")/g;
  const matches = lock.match(urls) ?? [];
  if (matches.length !== NATIVE_PACKAGES) {
    throw new Error(
      `package-lock.json: ${matches.length} native tarball URL(s), expected ${NATIVE_PACKAGES}; refusing to bump`,
    );
  }
  writeFileSync(lockPath, lock.replace(urls, `$1-${to}$3`));

  return planned
    .map(({ name, expected }) => `${name}: ${expected} -> ${to}`)
    .concat(`package-lock.json: ${NATIVE_PACKAGES} native tarball URL(s) -> ${to}`);
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
