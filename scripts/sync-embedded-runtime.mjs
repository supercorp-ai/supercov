#!/usr/bin/env node

import assert from "node:assert/strict";
import { readFileSync, writeFileSync } from "node:fs";
import { resolve } from "node:path";

const repository = resolve(import.meta.dirname, "..");
const runtimeFiles = [
  "atomic.js",
  "launchSupervisor.js",
  "nodeAssert.js",
  "nodeAssertAdapter.js",
  "nodeAssertStrict.js",
  "nodeTest.js",
  "playwright.js",
  "playwrightReporter.js",
  "provenance.js",
  "register.mjs",
  "resolve-loader.mjs",
  "runnerEvidence.js",
  "runtime.js",
  "transport.js",
  "types.js",
  "vitest.js",
  "vitestReporter.js",
];
const check = process.argv.includes("--check");

for (const name of runtimeFiles) {
  const built = readFileSync(resolve(repository, "dist", name));
  const embeddedPath = resolve(repository, "crates/supercov-engine/runtime", name);
  if (check) {
    const embedded = readFileSync(embeddedPath);
    assert.deepEqual(
      embedded,
      built,
      `${name} differs; run npm run sync:runtime and commit the generated shim`,
    );
  } else {
    writeFileSync(embeddedPath, built);
  }
}

console.log(
  check
    ? `[embedded-runtime] ${runtimeFiles.length} generated shims are exact`
    : `[embedded-runtime] synchronized ${runtimeFiles.length} generated shims`,
);
