#!/usr/bin/env node

import assert from "node:assert/strict";
import { readFileSync, writeFileSync } from "node:fs";
import { resolve } from "node:path";

const repository = resolve(import.meta.dirname, "..");
const mappings = [
  ["contracts/v1/contract.json", "crates/supercov-contracts/assets/v1/contract.json"],
  ["contracts/probe-v2/contract.json", "crates/supercov-contracts/assets/probe-v2/contract.json"],
  ["contracts/frontend-v2/contract.json", "crates/supercov-contracts/assets/frontend-v2/contract.json"],
  ["contracts/frontend-v2/examples/javascript-mixed-runners.json", "crates/supercov-contracts/assets/frontend-v2/examples/javascript-mixed-runners.json"],
  ["contracts/frontend-v2/examples/python-pytest-xdist.json", "crates/supercov-contracts/assets/frontend-v2/examples/python-pytest-xdist.json"],
  ["contracts/python-coverage-v1/contract.json", "crates/supercov-contracts/assets/python-coverage-v1/contract.json"],
  ["contracts/evidence-v3/contract.json", "crates/supercov-contracts/assets/evidence-v3/contract.json"],
  ["contracts/coverage-model-v1/contract.json", "crates/supercov-contracts/assets/coverage-model-v1/contract.json"],
  ["contracts/rust-coverage-v1/contract.json", "crates/supercov-contracts/assets/rust-coverage-v1/contract.json"],
  ["contracts/rust-compiler-companion-v1/contract.json", "crates/supercov-contracts/assets/rust-compiler-companion-v1/contract.json"],
  ["contracts/rust-probe-transport-v1/contract.json", "crates/supercov-contracts/assets/rust-probe-transport-v1/contract.json"],
  ["contracts/coverage-model-v1/vectors.json", "crates/supercov-engine/test-assets/coverage-model-v1/vectors.json"],
  ["contracts/probe-v2/vectors.json", "crates/supercov-engine/test-assets/probe-v2/vectors.json"],
  ...[
    "pytest-basic.json",
    "pytest-concurrency.json",
    "pytest-outcomes.json",
    "pytest-paths.json",
    "pytest-retry.json",
    "pytest-worker-crash.json",
    "pytest-xdist.json",
  ].map(name => [
    `contracts/python-coverage-v1/examples/${name}`,
    `crates/supercov-engine/test-assets/python-coverage-v1/${name}`,
  ]),
  ...["agent-error.json", "agent-page.json", "agent-success.json"].map(name => [
    `tests/golden/${name}`,
    `crates/supercov-engine/test-assets/agent/${name}`,
  ]),
];
const check = process.argv.includes("--check");

for (const [source, destination] of mappings) {
  const expected = readFileSync(resolve(repository, source));
  const destinationPath = resolve(repository, destination);
  if (check) {
    assert.deepEqual(
      readFileSync(destinationPath),
      expected,
      `${destination} differs from ${source}; run npm run sync:rust-assets`,
    );
  } else {
    writeFileSync(destinationPath, expected);
  }
}

console.log(
  check
    ? `[rust-package-assets] ${mappings.length} packaged assets are exact`
    : `[rust-package-assets] synchronized ${mappings.length} packaged assets`,
);
