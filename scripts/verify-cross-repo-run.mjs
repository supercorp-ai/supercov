import { readFileSync, readdirSync } from "node:fs";
import { resolve } from "node:path";
import { analyzeCoverageArchive } from "../dist/runAnalysis.js";

const root = process.cwd();
const runId = readdirSync(resolve(root, ".supercov/runs")).sort().at(-1);
if (!runId) throw new Error("external repository did not publish a coverage run");
const directory = resolve(root, ".supercov/runs", runId);
const metadata = JSON.parse(readFileSync(resolve(directory, "run.json"), "utf8"));
if (metadata.testExitCode !== 0) {
  throw new Error(`external test command exited ${metadata.testExitCode}`);
}
const report = analyzeCoverageArchive(resolve(directory, "evidence.raw.gz"), {
  runId,
  testExitCode: metadata.testExitCode,
  integrity: metadata.integrity,
});
if (report.summary.lines.total === 0 || report.points.length === 0) {
  throw new Error("external run produced no first-party coverage denominator");
}
if (!report.scope || report.scope.entries.length === 0) {
  throw new Error("external run did not retain an auditable source inventory");
}
console.log(
  `[cross-repo] ${runId}: ${report.summary.lines.covered}/${report.summary.lines.total} lines, ${report.summary.branches.covered}/${report.summary.branches.total} branches, ${report.summary.coveredConditions}/${report.summary.conditions} MC/DC conditions`,
);
