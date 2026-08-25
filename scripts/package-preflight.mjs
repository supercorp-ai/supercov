#!/usr/bin/env node

import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { existsSync, readFileSync, readdirSync } from "node:fs";
import { resolve } from "node:path";

const repository = resolve(import.meta.dirname, "..");
const manifest = JSON.parse(readFileSync(resolve(repository, "package.json"), "utf8"));
const launcher = readFileSync(resolve(repository, "bin/supercov.js"), "utf8");
const runtime = resolve(repository, "runtime/javascript");

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

for (const [subpath, path] of Object.entries(manifest.exports)) {
  assert(existsSync(resolve(repository, path)), `missing npm export ${subpath}: ${path}`);
}
for (const [name, version] of Object.entries(manifest.optionalDependencies ?? {})) {
  assert.match(name, /^@supercov\/cli-/);
  assert.equal(version, manifest.version);
}

console.log(
  `[package-preflight] Rust-only launcher, ${runtimeFiles.length} target-language shims, no legacy engine dependencies`,
);
