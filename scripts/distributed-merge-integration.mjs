import { execFileSync } from "node:child_process";
import { readFileSync, readdirSync, rmSync } from "node:fs";
import { resolve } from "node:path";
import { analyzeCoverageArchive } from "../dist/runAnalysis.js";

const root = resolve("tests/fixtures/generic-node");
const cli = resolve("bin/supercov.js");
rmSync(resolve(root, ".supercov"), { recursive: true, force: true });
for (const pattern of ["admin|both", "owner|neither"]) {
  execFileSync(
    cli,
    ["--", "node", "--test", `--test-name-pattern=${pattern}`],
    { cwd: root, stdio: "inherit" },
  );
}
const shards = readdirSync(resolve(root, ".supercov/runs")).sort();
if (shards.length !== 2) throw new Error(`expected two local shards, received ${shards}`);
execFileSync(cli, ["merge", ...shards], { cwd: root, stdio: "inherit" });
const runs = readdirSync(resolve(root, ".supercov/runs")).sort();
const merged = runs.find((id) => !shards.includes(id));
if (!merged) throw new Error("merge did not publish a new run");
const metadata = JSON.parse(readFileSync(resolve(root, ".supercov/runs", merged, "run.json"), "utf8"));
const report = analyzeCoverageArchive(resolve(root, ".supercov/runs", merged, "evidence.raw.gz"), {
  runId: merged,
  testExitCode: metadata.testExitCode,
  integrity: metadata.integrity,
});
if (report.filters?.passed.summary.conditionCoveragePct !== 100)
  throw new Error(`merged passed-only MC/DC was ${report.filters?.passed.summary.conditionCoveragePct}%`);
console.log(`[merge] ${merged}: two compatible shards merged to 100% passed-only MC/DC`);
