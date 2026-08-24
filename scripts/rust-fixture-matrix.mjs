#!/usr/bin/env node

import { spawnSync } from "node:child_process";
import { resolve } from "node:path";

const npm = process.platform === "win32" ? "npm.cmd" : "npm";
const binary = resolve(
  "target/debug",
  process.platform === "win32" ? "supercov.exe" : "supercov",
);
const rustEnvironment = {
  ...process.env,
  SUPERCOV_ENGINE: "rust",
  SUPERCOV_RUST_BINARY: binary,
};

function run(arguments_, extraEnvironment = {}) {
  const result = spawnSync(npm, arguments_, {
    stdio: "inherit",
    env: { ...rustEnvironment, ...extraEnvironment },
  });
  if (result.error) throw result.error;
  if (result.status !== 0)
    throw new Error(
      `${npm} ${arguments_.join(" ")} failed with exit ${result.status ?? "signal"}`,
    );
}

// Chromium exercises every adapter/build fixture. Firefox and WebKit rerun
// the mixed Vitest + two-worker Playwright fixture, including user contexts,
// popup frames, service workers, WebSockets, and request attribution.
run(["run", "test:fixture"]);
for (const browser of ["firefox", "webkit"]) {
  run(
    ["--prefix", "tests/fixtures/generic-playwright", "run", "test:coverage"],
    { SUPERCOV_BROWSER: browser },
  );
}

console.log(
  "[rust-fixture-matrix] Rust engine passed the adapter matrix in Chromium, Firefox, and WebKit",
);
