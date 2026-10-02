#!/usr/bin/env node

import { spawnSync } from "node:child_process";
import { resolve } from "node:path";

const npm = process.platform === "win32" ? "npm.cmd" : "npm";
const binary =
  process.env.SUPERCOV_BINARY ??
  resolve(
  "target/debug",
  process.platform === "win32" ? "supercov.exe" : "supercov",
);
const rustEnvironment = {
  ...process.env,
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

function runNode(arguments_, extraEnvironment = {}) {
  const result = spawnSync(process.execPath, arguments_, {
    stdio: "inherit",
    env: { ...rustEnvironment, ...extraEnvironment },
  });
  if (result.error) throw result.error;
  if (result.status !== 0)
    throw new Error(
      `${process.execPath} ${arguments_.join(" ")} failed with exit ${result.status ?? "signal"}`,
    );
}

// Chromium exercises every currently supported adapter/build fixture.
// Its first attempt at the retry test fails on purpose, so the run prints a
// code frame: it shows the project's line, not Supercov's instrumented copy.
const playwright = spawnSync(
  npm,
  ["--prefix", "tests/fixtures/generic-playwright", "run", "test:coverage"],
  { encoding: "utf8", env: { ...rustEnvironment, FORCE_COLOR: "0" }, maxBuffer: 64 * 1024 * 1024 },
);
process.stdout.write(playwright.stdout ?? "");
process.stderr.write(playwright.stderr ?? "");
if (playwright.status !== 0)
  throw new Error(`generic-playwright test:coverage failed with exit ${playwright.status ?? "signal"}`);
const printed = `${playwright.stdout}${playwright.stderr}`;
const frame = /^(.*> \d+ \|\s+expect\(testInfo\.retry\)\.toBe\(1\);)\n(.*\^)/m.exec(printed);
if (!frame)
  throw new Error("the retry failure's code frame does not show the spec's own line");
// The caret sits under the matcher, as Playwright puts it without Supercov.
if (frame[2].length - 1 !== frame[1].indexOf("toBe"))
  throw new Error(`the code frame's caret is not under the matcher:\n${frame[1]}\n${frame[2]}`);
if (printed.includes("__supercov"))
  throw new Error("a printed code frame shows Supercov's instrumented copy");
for (const script of [
  "opaque-runner-integration.mjs",
  "opaque-esm-integration.mjs",
  "workspaces-attribution-integration.mjs",
  "node-test-integration.mjs",
  "generic-build-integration.mjs",
  "next-integration.mjs",
  "distributed-merge-integration.mjs",
  "agent-query-eval.mjs",
]) {
  runNode([`scripts/${script}`]);
}
run(["run", "test:isolation"]);
run(["run", "test:watchdog"]);
for (const browser of ["firefox", "webkit"]) {
  run(
    ["--prefix", "tests/fixtures/generic-playwright", "run", "test:coverage"],
    { SUPERCOV_BROWSER: browser },
  );
}
runNode(["scripts/rust-syntax-matrix.mjs"]);

console.log(
  "[rust-fixture-matrix] Rust engine passed the adapter and independent syntax matrices in Node, Chromium, Firefox, and WebKit",
);
