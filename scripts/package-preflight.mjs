#!/usr/bin/env node

import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { existsSync, readFileSync, readdirSync } from "node:fs";
import { resolve } from "node:path";
import { releaseNotes } from "./release-notes.mjs";

const repository = resolve(import.meta.dirname, "..");
const manifest = JSON.parse(readFileSync(resolve(repository, "package.json"), "utf8"));
const launcher = readFileSync(resolve(repository, "bin/supercov.js"), "utf8");
const runtime = resolve(repository, "runtime/javascript");
const forbiddenProductOracle =
  /(?:\bllvm-(?:cov|profdata)\b|\bgcov\b|\blcov\b|\bcoverage\.py\b|-Cinstrument-coverage)/;

function sourceFiles(root, extension) {
  return readdirSync(root, { withFileTypes: true }).flatMap((entry) => {
    const path = resolve(root, entry.name);
    if (entry.isDirectory()) return sourceFiles(path, extension);
    return entry.isFile() && entry.name.endsWith(extension) ? [path] : [];
  });
}

assert.deepEqual(manifest.files, ["bin", "runtime/javascript", "docs", "README.md"]);
assert.equal(manifest.dependencies, undefined, "the npm launcher must have no engine dependencies");
assert.equal(manifest.exports["./vite"], undefined, "the legacy Vite engine API must not ship");
assert.doesNotMatch(launcher, /SUPERCOV_ENGINE|dist\/cli|process\.execPath/);
assert.match(launcher, /resolveNativeBinary/);
assert.equal(existsSync(resolve(repository, "src")), false, "legacy TypeScript engine still exists");
assert.equal(existsSync(resolve(repository, "dist")), false, "legacy compiled engine still exists");
assert.equal(existsSync(resolve(repository, "tests/unit")), false, "legacy engine tests still exist");
assert.equal(existsSync(resolve(repository, "tsconfig.json")), false);
assert.equal(existsSync(resolve(repository, "tsconfig.build.json")), false);
assert.equal(existsSync(resolve(repository, "crates/supercov-engine/runtime")), false);
assert.equal(existsSync(resolve(runtime, "esmInterceptor.js")), false);

const runtimeFiles = readdirSync(runtime)
  .filter((name) => /\.[cm]?js$/.test(name))
  .sort();
assert(runtimeFiles.length > 0, "no JavaScript runtime shims were found");
for (const name of runtimeFiles) {
  const path = resolve(runtime, name);
  const source = readFileSync(path, "utf8");
  assert.doesNotMatch(source, /@babel\//, `${name} imports the removed Babel engine`);
  const checked = spawnSync(process.execPath, ["--check", path], {
    encoding: "utf8",
  });
  assert.equal(checked.status, 0, `${name} is invalid JavaScript:\n${checked.stderr}`);
}

// The Python frontend names coverage.py because it is differentially tested
// against it; that file is exempt from the oracle scan. The exemption is by
// file name, not by a "/"-joined suffix: the first Windows build failed on
// exactly that, when D:\a\...\python_frontend.rs did not end with
// "/python_frontend.rs" and the scan ran on a file it was never meant to.
function isOracleDifferential(path) {
  // Not path.basename: on a POSIX host it does not split on "\", so a check
  // that must hold for a Windows path has to split on either separator itself.
  return path.split(/[\\/]/).at(-1) === "python_frontend.rs";
}
assert(isOracleDifferential("D:\\a\\supercov\\crates\\supercov-engine\\src\\python_frontend.rs"));
assert(isOracleDifferential("/w/crates/supercov-engine/src/python_frontend.rs"));
assert(!isOracleDifferential("/w/crates/supercov-engine/src/python_project.rs"));

// Independent coverage implementations are development oracles only. A user
// run must never shell out to them or enable compiler-native coverage. Keep
// this audit over every product Rust source and every shipped executable shim;
// oracle harnesses live in scripts/tests or behind the explicitly non-default
// `oracle-harnesses` feature.
const productSources = [
  resolve(repository, "bin/supercov.js"),
  ...runtimeFiles.map((name) => resolve(runtime, name)),
  ...sourceFiles(resolve(repository, "crates/supercov-cli/src"), ".rs"),
  ...sourceFiles(resolve(repository, "crates/supercov-engine/src"), ".rs").filter(
    (path) => !isOracleDifferential(path),
  ),
  ...sourceFiles(
    resolve(repository, "crates/supercov-engine/runtime-assets"),
    ".rs",
  ),
];
for (const path of productSources) {
  assert.doesNotMatch(
    readFileSync(path, "utf8"),
    forbiddenProductOracle,
    `${path} invokes or embeds a development-only coverage oracle`,
  );
}
const engineLibrary = readFileSync(
  resolve(repository, "crates/supercov-engine/src/lib.rs"),
  "utf8",
);
assert.match(
  engineLibrary,
  /#\[cfg\(any\(test, feature = "oracle-harnesses"\)\)\]\s*pub mod python_frontend;/,
  "the coverage.py importer must remain unavailable to normal product builds",
);
const engineManifest = readFileSync(
  resolve(repository, "crates/supercov-engine/Cargo.toml"),
  "utf8",
);
assert.match(
  engineManifest,
  /\[features\]\s*default = \[\]\s*oracle-harnesses = \[\]/,
);

for (const [subpath, path] of Object.entries(manifest.exports)) {
  assert(existsSync(resolve(repository, path)), `missing npm export ${subpath}: ${path}`);
}
for (const [name, version] of Object.entries(manifest.optionalDependencies ?? {})) {
  assert.match(name, /^@supercov\/cli-/);
  assert.equal(version, manifest.version);
}

// The release publishes this section verbatim, so a version cannot be tagged
// without notes, and the notes stay short enough for a reader to scan.
const notes = releaseNotes(manifest.version);
assert(notes !== null, `CHANGELOG.md has no "## ${manifest.version}" section`);
assert(
  notes.split(/\s+/).length <= 200,
  `the ${manifest.version} changelog section is ${notes.split(/\s+/).length} words; release notes are bullets, not prose`,
);
for (const line of notes.split("\n")) {
  assert(
    line === "" || line.startsWith("- ") || /^\*\*[A-Z][a-z]+\*\*$/.test(line) || line.startsWith("  "),
    `unexpected changelog line for ${manifest.version}: ${JSON.stringify(line)}`,
  );
}
// A Windows runner checks the changelog out with CRLF endings. The first
// Windows build failed right here, on "**Fixed**\r", before a line of Rust was
// compiled; the notes must read identically however the file was checked out.
const changelogPath = resolve(repository, "CHANGELOG.md");
const crlfNotes = releaseNotes(
  manifest.version,
  readFileSync(changelogPath, "utf8").replace(/\r?\n/g, "\r\n"),
);
assert.equal(crlfNotes, notes, "release notes must not depend on the checkout's line endings");

console.log(
  `[package-preflight] Rust-only launcher, ${runtimeFiles.length} target-language shims, no legacy engine or product-oracle dependencies`,
);
