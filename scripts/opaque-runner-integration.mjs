#!/usr/bin/env node

import { spawnSync } from "node:child_process";
import { existsSync, readFileSync, readdirSync } from "node:fs";
import { resolve } from "node:path";
import { gunzipSync } from "node:zlib";

const fixture = resolve("tests/fixtures/generic-playwright");
const runsRoot = resolve(fixture, ".supercov/runs");
const evidenceRoot = resolve(fixture, ".supercov/evidence");
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
const report = JSON.parse(
  gunzipSync(readFileSync(resolve(runsRoot, runId, "report.json.gz"))),
);
for (const metric of ["lines", "statements", "functions", "branches"]) {
  if (report.summary[metric].percentage !== 100)
    throw new Error(`${metric} coverage was not complete`);
}
if (report.summary.conditionCoveragePct !== 100)
  throw new Error("MC/DC coverage was not complete");

const traceDirectory = resolve(evidenceRoot, runId);
const traceFiles = readdirSync(traceDirectory).filter((name) =>
  /^execution\..+\.jsonl$/.test(name),
);
const events = traceFiles.flatMap((name) =>
  readFileSync(resolve(traceDirectory, name), "utf8")
    .split("\n")
    .filter(Boolean)
    .map((line, index) => {
      try {
        return JSON.parse(line);
      } catch (error) {
        throw new Error(`${name}:${index + 1}: ${error}`);
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
  `[opaque-runner] run ${runId}: ${traceFiles.length} parseable trace shards, generic remote launch, 100% coverage`,
);
