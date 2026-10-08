import { spawnSync } from "node:child_process";
import { existsSync, readdirSync } from "node:fs";
import { resolve } from "node:path";
import { coverageQuery, localRustEnvironment } from "./coverage-test-helpers.mjs";

const fixture = resolve("tests/fixtures/generic-playwright");
const runsRoot = resolve(fixture, ".supercov/runs");
// The runner serves what a build made, and this command has none in it.
const namedBuild = { SUPERCOV_BUILD_COMMAND: "npm run build" };
const before = new Set(existsSync(runsRoot) ? readdirSync(runsRoot) : []);
const result = spawnSync(process.execPath, [resolve("bin/supercov.js"), "--", "npm", "run", "test:opaque:esm"], {
  cwd: fixture,
  env: { ...process.env, ...localRustEnvironment, ...namedBuild },
  encoding: "utf8",
  stdio: "pipe",
});
if (result.status !== 0)
  throw new Error(`opaque ESM runner failed:\n${result.stderr}\n${result.stdout}`);
const runId = readdirSync(runsRoot).filter((id) => !before.has(id)).sort().at(-1);
if (!runId) throw new Error("opaque ESM runner did not publish a run");
const summary = coverageQuery(fixture, runId).data;
if (summary.coverage.conditionCoveragePct !== 100)
  throw new Error(`opaque ESM MC/DC was ${summary.coverage.conditionCoveragePct}%`);
if (summary.transport.workspaceCapabilities !== 1)
  throw new Error("pure ESM workspace capability was not intercepted exactly once");
if (summary.transport.remoteLaunches !== 1)
  throw new Error("pure ESM positional remote launch was not intercepted exactly once");
console.log(`[opaque-esm] run ${runId}: pure ESM SDK and positional launch intercepted, 100% coverage`);
