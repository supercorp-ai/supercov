import { execFileSync } from "node:child_process";
import { readFileSync, readdirSync, rmSync } from "node:fs";
import { delimiter, resolve } from "node:path";
import { analyzeCoverageArchive } from "../dist/runAnalysis.js";

for (const adapter of ["esbuild", "webpack", "swc"]) {
  const root = resolve(`tests/fixtures/generic-${adapter}`);
  rmSync(resolve(root, ".supercov"), { recursive: true, force: true });
  execFileSync(resolve("bin/supercov.js"), ["--", "npm", "test"], {
    cwd: root,
    stdio: "inherit",
    env: {
      ...process.env,
      PATH: `${resolve("node_modules/.bin")}${delimiter}${process.env.PATH ?? ""}`,
      ...(adapter === "swc" ? { SUPERCOV_SOURCE_ROOTS: "src" } : {}),
    },
  });
  const runId = readdirSync(resolve(root, ".supercov/runs")).sort().at(-1);
  if (!runId) throw new Error(`${adapter} fixture did not publish a run`);
  const metadata = JSON.parse(readFileSync(resolve(root, ".supercov/runs", runId, "run.json"), "utf8"));
  const report = analyzeCoverageArchive(resolve(root, ".supercov/runs", runId, "evidence.raw.gz"), {
    runId,
    testExitCode: metadata.testExitCode,
    integrity: metadata.integrity,
  });
  if (report.filters?.passed.summary.conditionCoveragePct !== 100)
    throw new Error(`${adapter} passed-only MC/DC was ${report.filters?.passed.summary.conditionCoveragePct}%`);
  console.log(`[generic-build] ${adapter}: build and four attributed tests passed at 100% MC/DC`);
}
