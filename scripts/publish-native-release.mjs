#!/usr/bin/env node

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { spawnSync } from "node:child_process";

const directory = resolve(process.argv[2] ?? "native-release");
const releaseSet = JSON.parse(
  readFileSync(resolve(directory, "release-set.json"), "utf8"),
);

assert.equal(releaseSet.schemaVersion, 1, "unsupported native release-set schema");
assert(Array.isArray(releaseSet.packages) && releaseSet.packages.length > 0);

for (const entry of releaseSet.packages) {
  const tarball = resolve(directory, entry.tarball.file);
  const result = spawnSync(
    "npm",
    ["publish", tarball, "--ignore-scripts", "--access", "public"],
    { encoding: "utf8", stdio: ["ignore", "pipe", "pipe"] },
  );
  if (result.status !== 0) {
    process.stderr.write(result.stdout);
    process.stderr.write(result.stderr);
    throw new Error(`failed to publish ${entry.package}@${entry.version}`);
  }
  process.stdout.write(result.stdout);
}
