import { spawnSync } from "node:child_process";
import { existsSync, readFileSync, readdirSync } from "node:fs";
import { resolve } from "node:path";
import { analyzeCoverageArchive } from "../dist/runAnalysis.js";

const fixture = resolve("tests/fixtures/generic-node");
const cli = resolve("bin/supercov.js");
const runsRoot = resolve(fixture, ".supercov/runs");

function storedRun(id) {
  const directory = resolve(runsRoot, id);
  const metadata = JSON.parse(readFileSync(resolve(directory, "run.json"), "utf8"));
  const report = analyzeCoverageArchive(resolve(directory, "evidence.raw.gz"), {
    runId: id,
    testExitCode: metadata.testExitCode,
    integrity: metadata.integrity,
  });
  return { id, directory, metadata, report };
}

function query(runId, ...args) {
  const result = spawnSync(
    process.execPath,
    [cli, "runs", runId, "coverage", ...args, "--json"],
    { cwd: fixture, encoding: "utf8" },
  );
  if (result.status !== 0) {
    throw new Error(
      `query failed: ${args.join(" ")}\n${result.stdout}\n${result.stderr}`,
    );
  }
  if (Buffer.byteLength(result.stdout) > 64 * 1024) {
    throw new Error(
      `${args.join(" ")} exceeded the 64 KiB agent-response budget`,
    );
  }
  return JSON.parse(result.stdout);
}

const runs = readdirSync(runsRoot).sort().map(storedRun);
const partial = runs.find(
  ({ report }) =>
    report.tests.some((test) => test.role === "test") &&
    report.summary.conditionCoveragePct > 0 &&
    report.summary.conditionCoveragePct < 100,
);
const complete = runs.find(
  ({ report }) => report.filters?.passed.summary.conditionCoveragePct === 100,
);
if (!partial || !complete) {
  throw new Error("agent evaluation requires one partial and one complete fixture run");
}

const summary = query(partial.id);
if (summary.filesWithGaps < 1 || summary.tests < 1) {
  throw new Error("summary did not expose the partial run's remaining work");
}

const scope = query(partial.id, "scope");
if (scope.entries.length > 20 || scope.counts.included < 1) {
  throw new Error("scope query was not bounded or omitted included source");
}

const files = query(partial.id, "files");
const gaps = query(partial.id, "gaps");
if (files.files.length > 20 || gaps.gaps.length < 1 || gaps.gaps.length > 20) {
  throw new Error("file queries did not provide a bounded gap inventory");
}

const file = gaps.gaps[0].file;
const detail = query(partial.id, "file", file);
if (detail.obligations.length < 1 || detail.obligations.length > 20) {
  throw new Error("file query did not expose bounded, actionable obligations");
}

const decision = partial.report.decisions[0];
if (!decision) throw new Error("partial run did not contain an MC/DC decision");
const decisionDetail = query(partial.id, "decision", decision.meta.id);
if (
  decisionDetail.decisions?.length !== 1 ||
  !Array.isArray(decisionDetail.decisions[0].conditions)
) {
  throw new Error("decision query did not expose conditions and witnesses");
}

const line = partial.report.lines.find((candidate) => candidate.file === file);
if (!line) throw new Error("partial run did not contain a queryable source line");
const covers = query(partial.id, "covers", `${line.file}:${line.line}`);
if (!Array.isArray(covers.tests)) {
  throw new Error("covers query did not expose per-test attribution");
}

const test = partial.report.tests.find((candidate) => candidate.role === "test");
if (!test) throw new Error("partial run did not contain a queryable test");
const testDetail = query(partial.id, "test", test.id);
if (testDetail.tests?.length !== 1 || testDetail.tests[0].id !== test.id) {
  throw new Error("test query did not resolve an exact test ID");
}

const minimized = query(
  complete.id,
  "minimize",
  "--filter",
  "passed",
  "--metric",
  "mcdc",
);
if (!minimized.optimal || minimized.summary.conditionCoveragePct !== 100) {
  throw new Error("minimizer did not prove an exact 100% MC/DC test subset");
}
if (minimized.selectedCount >= minimized.totalCandidateTests) {
  throw new Error("minimizer failed to eliminate the redundant MC/DC vector");
}

for (const run of runs) {
  for (const legacy of ["report.html", "report.json", "mcdc-report.html", "mcdc-report.json"]) {
    if (existsSync(resolve(run.directory, legacy))) {
      throw new Error(`run ${run.id} retained forbidden derived artifact ${legacy}`);
    }
  }
}

console.log(
  `[agent-eval] navigated ${partial.id} through bounded scope/gap/file/decision/line/test queries; ${complete.id} proved an exact ${minimized.selectedCount}/${minimized.totalCandidateTests} MC/DC subset`,
);
