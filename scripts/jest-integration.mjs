import { execFileSync } from "node:child_process";
import { readFileSync, readdirSync, rmSync } from "node:fs";
import { resolve } from "node:path";
import { analyzeCoverageArchive } from "../dist/runAnalysis.js";

const root = resolve("tests/fixtures/generic-jest");
rmSync(resolve(root, ".supercov"), { recursive: true, force: true });
execFileSync(
  resolve("bin/supercov.js"),
  ["--", resolve("node_modules/.bin/jest"), "--runInBand"],
  { cwd: root, stdio: "inherit" },
);
const runId = readdirSync(resolve(root, ".supercov/runs")).sort().at(-1);
if (!runId) throw new Error("Jest fixture did not publish a run");
const metadata = JSON.parse(readFileSync(resolve(root, ".supercov/runs", runId, "run.json"), "utf8"));
const report = analyzeCoverageArchive(
  resolve(root, ".supercov/runs", runId, "evidence.raw.gz"),
  { runId, testExitCode: metadata.testExitCode, integrity: metadata.integrity },
);
const tests = report.tests.filter((test) => test.role === "test");
if (tests.length !== 4 || tests.some((test) => test.provenance.runner !== "jest"))
  throw new Error(`expected four attributed Jest tests, received ${JSON.stringify(tests)}`);
if (report.filters?.passed.summary.conditionCoveragePct !== 100)
  throw new Error(`expected 100% passed-only MC/DC, received ${report.filters?.passed.summary.conditionCoveragePct}%`);
console.log(`[jest] run ${runId}: four exact test scopes, 100% passed-only MC/DC`);
