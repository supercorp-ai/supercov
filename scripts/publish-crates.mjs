#!/usr/bin/env node
// Publish the workspace's crates to crates.io in dependency order, skipping
// the versions that are already there, so a release that stopped halfway can
// be run again. `cargo publish` waits for each crate to appear in the index
// before returning, which is what lets the next crate resolve it. The token
// comes from CARGO_REGISTRY_TOKEN, which the workflow obtains through
// crates.io's trusted publishing for the duration of the job.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { spawnSync } from "node:child_process";

const repository = resolve(import.meta.dirname, "..");
const version = readFileSync(resolve(repository, "Cargo.toml"), "utf8").match(
  /^\[workspace\.package\][^[]*?^version = "([^"]+)"/ms,
)[1];
// Leaves first: each crate's dependencies must be on the registry before it.
const crates = ["supercov-contracts", "supercov-engine", "supercov"];
// --dry-run reports what would be published and publishes nothing.
const dryRun = process.argv.includes("--dry-run");
assert(dryRun || process.env.CARGO_REGISTRY_TOKEN, "CARGO_REGISTRY_TOKEN is not set");

const onRegistry = (crate) =>
  fetch(`https://crates.io/api/v1/crates/${crate}/${version}`, {
    headers: { "user-agent": "supercov-release (https://github.com/supercorp-ai/supercov)" },
  });

for (const crate of crates) {
  const response = await onRegistry(crate);
  if (response.ok) {
    console.log(`[crates] ${crate}@${version} already exists; skipped`);
    continue;
  }
  assert.equal(response.status, 404, `crates.io answered ${response.status} for ${crate}@${version}`);
  if (dryRun) {
    console.log(`[crates] would publish ${crate}@${version}`);
    continue;
  }
  // A crate's dependency can be on crates.io and still missing from the index
  // cargo resolves against: 3.0.0 stopped at `supercov` a second after
  // `supercov-engine` was published. Try again while the index catches up.
  for (let attempt = 1; ; attempt++) {
    const published = spawnSync("cargo", ["publish", "-p", crate, "--locked"], {
      cwd: repository,
      encoding: "utf8",
      stdio: ["ignore", "inherit", "inherit"],
    });
    if (published.status === 0 || (await onRegistry(crate)).ok) break;
    if (attempt === 5) throw new Error(`failed to publish ${crate}@${version}`);
    console.log(`[crates] ${crate}@${version} failed to publish; trying again in 30 s`);
    await new Promise((settle) => setTimeout(settle, 30_000));
  }
  console.log(`[crates] published ${crate}@${version}`);
}
