import { spawnSync } from "node:child_process";
import {
  existsSync,
  mkdirSync,
  readFileSync,
  readdirSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { resolve } from "node:path";
import { analyzeCoverageArchive } from "../dist/runAnalysis.js";
import { writeEvidenceArchiveEntries } from "../dist/evidenceArchive.js";

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
  const envelope = JSON.parse(result.stdout);
  if (envelope.schemaVersion !== 1 || envelope.ok !== true) {
    throw new Error(`invalid agent success envelope: ${result.stdout}`);
  }
  if (!envelope.data || typeof envelope.data !== "object") {
    throw new Error(`agent response omitted object data: ${result.stdout}`);
  }
  if (envelope.pagination) {
    const page = envelope.pagination;
    if (
      !Number.isSafeInteger(page.offset) ||
      !Number.isSafeInteger(page.limit) ||
      !Number.isSafeInteger(page.returned) ||
      !Number.isSafeInteger(page.total) ||
      typeof page.hasMore !== "boolean" ||
      (page.nextOffset !== null && !Number.isSafeInteger(page.nextOffset))
    ) {
      throw new Error(`invalid agent pagination envelope: ${result.stdout}`);
    }
  }
  return { ...envelope.data, pagination: envelope.pagination };
}

function failingQuery(runId, ...args) {
  const result = spawnSync(
    process.execPath,
    [cli, "runs", runId, "coverage", ...args, "--json"],
    { cwd: fixture, encoding: "utf8" },
  );
  if (result.status !== 2 || result.stderr !== "") {
    throw new Error(`agent failure did not use a clean status-2 JSON channel`);
  }
  const envelope = JSON.parse(result.stdout);
  if (
    envelope.schemaVersion !== 1 ||
    envelope.ok !== false ||
    typeof envelope.error?.code !== "string" ||
    typeof envelope.error?.retryable !== "boolean"
  ) {
    throw new Error(`invalid agent failure envelope: ${result.stdout}`);
  }
  return envelope;
}

function requirePagination(response, label) {
  if (!response.pagination || response.pagination.limit !== 20) {
    throw new Error(`${label} omitted the default pagination contract`);
  }
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
if (summary.filesWithGaps < 1 || summary.tests < 1 || summary.pagination) {
  throw new Error("summary did not expose the partial run's remaining work");
}

const scope = query(partial.id, "scope");
requirePagination(scope, "scope");
if (scope.entries.length > 20 || scope.counts.included < 1) {
  throw new Error("scope query was not bounded or omitted included source");
}

const files = query(partial.id, "files");
const gaps = query(partial.id, "gaps");
requirePagination(files, "files");
requirePagination(gaps, "gaps");
if (files.files.length > 20 || gaps.gaps.length < 1 || gaps.gaps.length > 20) {
  throw new Error("file queries did not provide a bounded gap inventory");
}

const file = gaps.gaps[0].file;
const detail = query(partial.id, "file", file);
requirePagination(detail, "file detail");
if (detail.obligations.length < 1 || detail.obligations.length > 20) {
  throw new Error("file query did not expose bounded, actionable obligations");
}

const mcdcGaps = query(partial.id, "gaps", "--metric", "mcdc");
requirePagination(mcdcGaps, "MC/DC gap inventory");
if (
  mcdcGaps.metric !== "mcdc" ||
  mcdcGaps.gaps.length < 1 ||
  mcdcGaps.gaps.some(
    (gap) => gap.missingMcdcConditions === 0 && gap.measurementLimitations === 0,
  )
) {
  throw new Error("metric-filtered gaps included unrelated coverage work");
}
const mcdcDetail = query(
  partial.id,
  "file",
  mcdcGaps.gaps[0].file,
  "--metric",
  "mcdc",
);
if (
  mcdcDetail.metric !== "mcdc" ||
  mcdcDetail.obligations.some((obligation) => obligation.kind !== "mcdc")
) {
  throw new Error("metric-filtered file detail included unrelated obligations");
}

const grouped = query(
  partial.id,
  "file",
  mcdcGaps.gaps[0].file,
  "--metric",
  "mcdc",
  "--group",
  "decision",
);
requirePagination(grouped, "grouped file detail");
if (
  grouped.group !== "decision" ||
  typeof grouped.totals?.decisions !== "number" ||
  !Array.isArray(grouped.decisions) ||
  grouped.decisions.length < 1 ||
  grouped.decisions.some(
    (row) =>
      typeof row.id !== "string" ||
      !Number.isSafeInteger(row.line) ||
      !Number.isSafeInteger(row.conditions) ||
      !Number.isSafeInteger(row.missingConditions) ||
      row.missingConditions < 1 ||
      row.missingConditions > row.conditions ||
      typeof row.source !== "string",
  )
) {
  throw new Error("grouped file query did not expose per-decision missing counts");
}
const sortedGrouped = query(
  partial.id,
  "file",
  mcdcGaps.gaps[0].file,
  "--group",
  "decision",
  "--sort",
  "missing",
);
for (let index = 1; index < sortedGrouped.decisions.length; index += 1) {
  const previous = sortedGrouped.decisions[index - 1];
  const current = sortedGrouped.decisions[index];
  if (
    previous.missingConditions - previous.waivedConditions <
    current.missingConditions - current.waivedConditions
  ) {
    throw new Error("--sort missing did not order decisions by unwaived missing count");
  }
}

const missingFile = failingQuery(partial.id, "file", "src/does-not-exist.ts");
if (missingFile.command !== "coverage.file" || missingFile.error.code !== "SOURCE_NOT_FOUND") {
  throw new Error("file query did not expose a stable structured error code");
}

const malformedRuns = spawnSync(
  process.execPath,
  [cli, "runs", partial.id, "file", mcdcGaps.gaps[0].file, "--json"],
  { cwd: fixture, encoding: "utf8" },
);
if (malformedRuns.status === 0) {
  throw new Error("runs without the coverage segment must fail, not list runs");
}
const malformedEnvelope = JSON.parse(malformedRuns.stdout);
if (
  malformedEnvelope.ok !== false ||
  malformedEnvelope.error?.code !== "UNKNOWN_COMMAND" ||
  !malformedEnvelope.error.message.includes("Unknown runs query: file")
) {
  throw new Error("malformed runs query did not return a structured usage error");
}

const waiversPath = resolve(fixture, "supercov.waivers.json");
const waivedObligation = mcdcDetail.obligations[0];
try {
  writeFileSync(
    waiversPath,
    JSON.stringify({
      version: 1,
      waivers: [
        {
          file: mcdcGaps.gaps[0].file,
          decision: waivedObligation.id,
          condition: waivedObligation.missingCondition,
          reason: "agent-query-eval fixture waiver",
        },
        {
          file: "src/never-existed.ts",
          condition: "impossible",
          reason: "must surface as unmatched",
        },
      ],
    }),
  );
  const waivedSummary = query(partial.id);
  if (
    waivedSummary.waivers?.applied !== 1 ||
    waivedSummary.waivers.unmatched.length !== 1 ||
    waivedSummary.waivers.unmatched[0].file !== "src/never-existed.ts" ||
    waivedSummary.waivers.contradicted.length !== 0 ||
    waivedSummary.waivers.mcdcExcludingWaived.total !==
      waivedSummary.coverage.conditions - 1
  ) {
    throw new Error("summary did not report applied and unmatched waivers as facts");
  }
  const waivedDetail = query(
    partial.id,
    "file",
    mcdcGaps.gaps[0].file,
    "--metric",
    "mcdc",
  );
  const annotated = waivedDetail.obligations.find(
    (obligation) =>
      obligation.id === waivedObligation.id &&
      obligation.missingCondition === waivedObligation.missingCondition,
  );
  if (
    !annotated?.waived ||
    annotated.waiverReason !== "agent-query-eval fixture waiver" ||
    waivedDetail.counts.waivedMcdcConditions < 1
  ) {
    throw new Error("file query did not annotate the waived condition");
  }
  const waivedGrouped = query(
    partial.id,
    "file",
    mcdcGaps.gaps[0].file,
    "--group",
    "decision",
  );
  const waivedRow = waivedGrouped.decisions.find(
    (row) => row.id === waivedObligation.id,
  );
  if (!waivedRow || waivedRow.waivedConditions < 1) {
    throw new Error("grouped view did not count the waived condition");
  }
} finally {
  rmSync(waiversPath, { force: true });
}

const decision = partial.report.decisions[0];
if (!decision) throw new Error("partial run did not contain an MC/DC decision");
const decisionDetail = query(partial.id, "decision", decision.meta.id);
requirePagination(decisionDetail, "decision detail");
if (
  decisionDetail.decisions?.length !== 1 ||
  !Array.isArray(decisionDetail.decisions[0].conditions) ||
  decisionDetail.paginationAppliesTo !==
    "conditions, vectorObservations, and tests independently within each decision" ||
  typeof decisionDetail.decisions[0].totals?.conditions !== "number"
) {
  throw new Error("decision query did not expose conditions and witnesses");
}

const line = partial.report.lines.find((candidate) => candidate.file === file);
if (!line) throw new Error("partial run did not contain a queryable source line");
const covers = query(partial.id, "covers", `${line.file}:${line.line}`);
requirePagination(covers, "line attribution");
if (!Array.isArray(covers.tests)) {
  throw new Error("covers query did not expose per-test attribution");
}

const test = partial.report.tests.find((candidate) => candidate.role === "test");
if (!test) throw new Error("partial run did not contain a queryable test");
const testDetail = query(partial.id, "test", test.id);
requirePagination(testDetail, "test detail");
if (testDetail.tests?.length !== 1 || testDetail.tests[0].id !== test.id) {
  throw new Error("test query did not resolve an exact test ID");
}
if (
  testDetail.paginationAppliesTo !==
    "lines, hits/hitDetails, decisions, and phases independently within the test" ||
  testDetail.tests[0].hitDetails.some(
    (hit) => typeof hit.id !== "string" || typeof hit.obligation !== "string",
  ) ||
  testDetail.tests[0].decisions.some(
    (decision) => !decision.meta?.file || !Number.isSafeInteger(decision.meta.line),
  )
) {
  throw new Error("test query exposed opaque evidence without source metadata");
}

const minimized = query(
  complete.id,
  "minimize",
  "--filter",
  "passed",
  "--metric",
  "mcdc",
);
requirePagination(minimized, "minimum test set");
if (!minimized.optimal || minimized.summary.conditionCoveragePct !== 100) {
  throw new Error("minimizer did not prove an exact 100% MC/DC test subset");
}
if (minimized.selectedCount >= minimized.totalCandidateTests) {
  throw new Error("minimizer failed to eliminate the redundant MC/DC vector");
}

const limitationRunId = `agent-limitations-${process.pid}`;
const limitationDirectory = resolve(runsRoot, limitationRunId);
try {
  mkdirSync(limitationDirectory, { recursive: true });
  const rawEvidence = writeEvidenceArchiveEntries(
    [
      {
        path: "manifest.json",
        contents: JSON.stringify({
          decisions: [],
          branches: [],
          points: [],
          limitations: [{
            id: "dynamic-source",
            kind: "dynamic-code",
            file: "src/dynamic.mjs",
            line: 3,
            column: 1,
            source: "eval(source)",
            reason: "Runtime-generated source cannot be instrumented statically.",
          }],
        }),
      },
      {
        path: "test/mcdc.json",
        contents: JSON.stringify({
          testId: "limitation-test",
          test: "limitation-test",
          status: "passed",
          runtime: [{ decisions: [], hits: [] }],
          browser: [],
          server: [],
        }),
      },
      {
        path: "execution.host.1.jsonl",
        contents: `${JSON.stringify({ event: "remote-launch" })}\n`,
      },
    ],
    resolve(limitationDirectory, "evidence.raw.gz"),
  );
  writeFileSync(
    resolve(limitationDirectory, "run.json"),
    JSON.stringify({
      id: limitationRunId,
      startedAt: "2026-08-24T00:00:00.000Z",
      testExitCode: 0,
      rawEvidence,
    }),
  );

  const limitedSummary = query(limitationRunId);
  if (
    limitedSummary.measurement?.complete !== false ||
    limitedSummary.measurement?.blocking !== 1 ||
    limitedSummary.structurallyComplete !== false ||
    limitedSummary.diagnostics?.[0]?.code !== "REMOTE_SERVER_EVIDENCE_MISSING"
  ) {
    throw new Error("summary did not expose the blocking measurement limitation");
  }
  const limitedGaps = query(limitationRunId, "gaps");
  requirePagination(limitedGaps, "limitation gap inventory");
  if (
    limitedGaps.gaps?.[0]?.file !== "src/dynamic.mjs" ||
    limitedGaps.gaps[0].measurementLimitations !== 1
  ) {
    throw new Error("gaps did not expose the limitation-only file");
  }
  const limitedFile = query(
    limitationRunId,
    "file",
    "src/dynamic.mjs",
  );
  requirePagination(limitedFile, "limitation file detail");
  if (
    limitedFile.totalObligations !== 0 ||
    limitedFile.totalLimitations !== 1 ||
    limitedFile.limitations?.[0]?.kind !== "dynamic-code" ||
    limitedFile.limitations[0].blocking !== true
  ) {
    throw new Error("file detail did not separate limitations from obligations");
  }
} finally {
  rmSync(limitationDirectory, { recursive: true, force: true });
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
