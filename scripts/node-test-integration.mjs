import { execFileSync } from "node:child_process";
import { readFileSync, readdirSync, rmSync } from "node:fs";
import { resolve } from "node:path";
import { analyzeCoverageArchive } from "../dist/runAnalysis.js";

const root = resolve("tests/fixtures/generic-node");
rmSync(resolve(root, ".supercov"), { recursive: true, force: true });
execFileSync(resolve("bin/supercov.js"), ["--", "npm", "test"], {
  cwd: root,
  stdio: "inherit",
});
const runId = readdirSync(resolve(root, ".supercov/runs")).sort().at(-1);
if (!runId) throw new Error("node:test fixture did not publish a run");
const metadata = JSON.parse(
  readFileSync(resolve(root, ".supercov/runs", runId, "run.json"), "utf8"),
);
const report = analyzeCoverageArchive(
  resolve(root, ".supercov/runs", runId, "evidence.raw.gz"),
  { runId, testExitCode: metadata.testExitCode, integrity: metadata.integrity },
);
const tests = report.tests.filter((test) => test.role === "test");
if (tests.length !== 4 || tests.some((test) => test.provenance.runner !== "node:test"))
  throw new Error(`expected four attributed node:test tests, received ${JSON.stringify(tests)}`);
if (report.summary.conditionCoveragePct !== 100)
  throw new Error(`expected 100% MC/DC, received ${report.summary.conditionCoveragePct}%`);
console.log(`[node:test] run ${runId}: four exact test scopes, 100% MC/DC`);
