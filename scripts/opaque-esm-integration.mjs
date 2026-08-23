import { spawnSync } from "node:child_process";
import { existsSync, readFileSync, readdirSync } from "node:fs";
import { resolve } from "node:path";
import { analyzeCoverageArchive } from "../dist/runAnalysis.js";
import { readEvidenceArchive } from "../dist/evidenceArchive.js";

const fixture = resolve("tests/fixtures/generic-playwright");
const runsRoot = resolve(fixture, ".supercov/runs");
const before = new Set(existsSync(runsRoot) ? readdirSync(runsRoot) : []);
const result = spawnSync(process.execPath, [resolve("bin/supercov.js"), "--", "npm", "run", "test:opaque:esm"], {
  cwd: fixture,
  encoding: "utf8",
  stdio: "pipe",
});
if (result.status !== 0)
  throw new Error(`opaque ESM runner failed:\n${result.stderr}\n${result.stdout}`);
const runId = readdirSync(runsRoot).filter((id) => !before.has(id)).sort().at(-1);
if (!runId) throw new Error("opaque ESM runner did not publish a run");
const metadata = JSON.parse(readFileSync(resolve(runsRoot, runId, "run.json"), "utf8"));
const evidencePath = resolve(runsRoot, runId, "evidence.raw.gz");
const report = analyzeCoverageArchive(evidencePath, {
  runId,
  testExitCode: metadata.testExitCode,
  integrity: metadata.integrity,
});
if (report.summary.conditionCoveragePct !== 100)
  throw new Error(`opaque ESM MC/DC was ${report.summary.conditionCoveragePct}%`);
const events = readEvidenceArchive(evidencePath).files
  .filter((entry) => /execution\..+\.jsonl$/.test(entry.path))
  .flatMap((entry) => entry.contents.split("\n").filter(Boolean).map((line) => JSON.parse(line)));
if (events.filter((event) => event.event === "workspace-capability").length !== 1)
  throw new Error("pure ESM workspace capability was not intercepted exactly once");
if (events.filter((event) => event.event === "remote-launch").length !== 1)
  throw new Error("pure ESM positional remote launch was not intercepted exactly once");
console.log(`[opaque-esm] run ${runId}: pure ESM SDK and positional launch intercepted, 100% coverage`);
