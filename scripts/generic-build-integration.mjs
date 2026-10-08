import { rmSync } from "node:fs";
import { delimiter, resolve } from "node:path";
import {
  coverageQuery,
  latestRun,
  requireSupercov,
  runMetadata,
} from "./coverage-test-helpers.mjs";

for (const adapter of ["esbuild", "webpack", "swc"]) {
  const root = resolve(`tests/fixtures/generic-${adapter}`);
  rmSync(resolve(root, ".supercov"), { recursive: true, force: true });
  // The fixture builds before its tests, in a `pretest` script, as a project
  // does: Supercov runs the command and no build of its own, and the bundler
  // reads the instrumented files like any others. Twice, since the second run
  // starts from the workspace the first one left.
  for (const attempt of ["first", "second"]) {
    requireSupercov(root, ["--", "npm", "test"], {
      stdio: "inherit",
      env: {
        ...process.env,
        PATH: `${resolve("node_modules/.bin")}${delimiter}${process.env.PATH ?? ""}`,
        ...(adapter === "swc" ? { SUPERCOV_SOURCE_ROOTS: "src" } : {}),
      },
    });
    const runId = latestRun(root);
    const metadata = runMetadata(root, runId);
    const summary = coverageQuery(root, runId, "--filter", "passed").data;
    if (summary.coverage.conditionCoveragePct !== 100)
      throw new Error(
        `${adapter} ${attempt} passed-only MC/DC was ${summary.coverage.conditionCoveragePct}%`,
      );
    if (metadata.timings?.instrumentedBuildMs !== 0)
      throw new Error(`${adapter} ${attempt} run built something the command did not ask for`);
  }
  console.log(
    `[generic-build] ${adapter}: the project's own build kept four attributed tests at 100% MC/DC, twice`,
  );
}
