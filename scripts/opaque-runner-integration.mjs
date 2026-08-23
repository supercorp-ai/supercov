#!/usr/bin/env node

import { spawnSync } from "node:child_process";
import { existsSync, readFileSync, readdirSync } from "node:fs";
import { resolve } from "node:path";
import { readEvidenceArchive } from "../dist/evidenceArchive.js";
import { analyzeCoverageArchive } from "../dist/runAnalysis.js";

const fixture = resolve("tests/fixtures/generic-playwright");
const runsRoot = resolve(fixture, ".supercov/runs");
const before = new Set(existsSync(runsRoot) ? readdirSync(runsRoot) : []);
const result = spawnSync(
  process.execPath,
  [resolve("bin/supercov.js"), "--", "npm", "run", "test:opaque"],
  { cwd: fixture, encoding: "utf8", stdio: "pipe" },
);
if (result.status !== 0) {
  throw new Error(
    `opaque runner coverage failed:\n${result.stderr}\n${result.stdout}`,
  );
}
const runIds = readdirSync(runsRoot)
  .filter((id) => !before.has(id))
  .sort();
if (runIds.length !== 1)
  throw new Error(`expected one opaque runner coverage run, received ${runIds}`);
const runId = runIds[0];
const metadata = JSON.parse(
  readFileSync(resolve(runsRoot, runId, "run.json"), "utf8"),
);
if (metadata.instrumentedBuildCache?.reused !== true)
  throw new Error("test-command-only change rebuilt unchanged instrumented source");
const evidencePath = resolve(runsRoot, runId, "evidence.raw.gz");
if (existsSync(resolve(runsRoot, runId, "report.json.gz")))
  throw new Error("opaque runner persisted a derived report");
const report = analyzeCoverageArchive(evidencePath, {
  runId,
  testExitCode: metadata.testExitCode,
  integrity: metadata.integrity,
  generatedAt: metadata.startedAt,
});
for (const metric of ["lines", "statements", "functions", "branches"]) {
  if (report.summary[metric].percentage !== 100)
    throw new Error(`${metric} coverage was not complete`);
}
if (report.summary.conditionCoveragePct !== 100)
  throw new Error("MC/DC coverage was not complete");

const archive = readEvidenceArchive(
  evidencePath,
);
const traceFiles = archive.files.filter((entry) =>
  /(?:^|\/)execution\..+\.jsonl$/.test(entry.path),
);
const events = traceFiles.flatMap(({ path, contents }) =>
  contents
    .split("\n")
    .filter(Boolean)
    .map((line, index) => {
      try {
        return JSON.parse(line);
      } catch (error) {
        throw new Error(`${path}:${index + 1}: ${error}`);
      }
    }),
);
if (events.filter((event) => event.event === "workspace-capability").length !== 1)
  throw new Error("opaque workspace capability was not discovered exactly once");
if (events.filter((event) => event.event === "remote-launch").length !== 1)
  throw new Error("opaque remote launch was not intercepted exactly once");
if (events.some((event) => JSON.stringify(event).includes("supermachine")))
  throw new Error("provider-specific behavior leaked into the public fixture");

console.log(
  `[opaque-runner] run ${runId}: reused build, ${traceFiles.length} archived parseable trace shards, generic remote launch, 100% coverage`,
);
