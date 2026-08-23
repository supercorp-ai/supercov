import { execFileSync } from "node:child_process";
import { readFileSync, readdirSync, rmSync } from "node:fs";
import { delimiter, resolve } from "node:path";
import { analyzeCoverageArchive } from "../dist/runAnalysis.js";

const root = resolve("tests/fixtures/generic-next");
rmSync(resolve(root, ".supercov"), { recursive: true, force: true });
execFileSync(resolve("bin/supercov.js"), ["--", "npm", "test"], {
  cwd: root,
  stdio: "inherit",
  env: {
    ...process.env,
    PATH: `${resolve("node_modules/.bin")}${delimiter}${process.env.PATH ?? ""}`,
  },
});
const runId = readdirSync(resolve(root, ".supercov/runs")).sort().at(-1);
if (!runId) throw new Error("Next fixture did not publish a run");
const metadata = JSON.parse(readFileSync(resolve(root, ".supercov/runs", runId, "run.json"), "utf8"));
const report = analyzeCoverageArchive(resolve(root, ".supercov/runs", runId, "evidence.raw.gz"), {
  runId,
  testExitCode: metadata.testExitCode,
  integrity: metadata.integrity,
});
if (report.summary.conditionCoveragePct !== 100)
  throw new Error(`Next MC/DC was ${report.summary.conditionCoveragePct}%`);
console.log(`[generic-build] Next: production build and request tests passed at 100% MC/DC`);
