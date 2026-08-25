#!/usr/bin/env node

import { mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { basename, resolve } from "node:path";
import { readEvidenceArchive } from "../dist/evidenceArchive.js";
import { analyzeCoverageArchive } from "../dist/runAnalysis.js";
import {
  canonicalDigest,
  canonicalEvidenceSignatures,
} from "./rust-parity-normalize.mjs";

const [mode, projectArgument, run, outputArgument, parityRoot, comparisonClass] =
  process.argv.slice(2);
if (!mode || !projectArgument || !run || !outputArgument)
  throw new Error(
    "usage: rust-parity-state <evidence|report> <project> <run> <output> [parity-root]",
  );
const project = resolve(projectArgument);
const output = resolve(outputArgument);
const runDirectory = resolve(project, ".supercov/runs", run);
mkdirSync(output, { recursive: true });
const context = {
  run,
  project,
  projectName: basename(project),
  parityRoot,
  attempts: new Map(),
  unorderedEvidence: true,
  selfHosting: comparisonClass === "self-hosting",
};

if (mode === "evidence") {
  const archive = readEvidenceArchive(resolve(runDirectory, "evidence.raw.gz"));
  const manifest = archive.files.find((entry) => entry.path === "manifest.json")
    ?.contents;
  if (!manifest) throw new Error(`run ${run} has no manifest`);
  const signatures = canonicalEvidenceSignatures(archive, context);
  writeFileSync(resolve(output, "manifest.json"), manifest);
  writeFileSync(
    resolve(output, "evidence-signatures.json"),
    JSON.stringify(signatures),
  );
  writeFileSync(
    resolve(output, "context.json"),
    JSON.stringify({
      attempts: [...context.attempts],
    }),
  );
} else if (mode === "report") {
  const metadata = JSON.parse(
    readFileSync(resolve(runDirectory, "run.json"), "utf8"),
  );
  const savedContext = JSON.parse(
    readFileSync(resolve(output, "context.json"), "utf8"),
  );
  context.attempts = new Map(savedContext.attempts);
  context.omitTimestampCorrelation = true;
  context.omitEngineTransportTopology = true;
  const report = analyzeCoverageArchive(resolve(runDirectory, "evidence.raw.gz"), {
    runId: run,
    testExitCode: metadata.testExitCode,
    integrity: metadata.integrity,
    generatedAt: metadata.startedAt,
  });
  const outcome = (title) => report.tests.find((test) => test.title === title);
  const flaky = outcome("retains a failed retry before the terminal pass");
  const skipped = outcome("records a skipped outcome without inventing coverage");
  const expectedFailure = outcome("keeps expected failure out of passed-only coverage");
  const firstCoveredLine = report.lines.find((line) => line.covered);
  writeFileSync(
    resolve(output, "report-state.json"),
    JSON.stringify({
      digest: canonicalDigest(report, context),
      digests: Object.fromEntries(
        Object.entries(report).map(([key, value]) => [
          key,
          canonicalDigest({ [key]: value }, context),
        ]),
      ),
      transport: report.transport,
      testOutcomes: report.tests
        .filter((test) => test.role === "test")
        .map((test) => ({
          id: test.id,
          title: test.title,
          outcome: test.outcome,
          attempts: test.attempts,
        }))
        .sort((left, right) => left.id.localeCompare(right.id)),
      selectors: {
        firstFile: report.lines[0]?.file,
        firstDecision: report.decisions[0]?.meta.id,
        firstTest: report.tests.find((test) => test.role === "test")?.id,
        firstCoveredLine: firstCoveredLine
          ? `${firstCoveredLine.file}:${firstCoveredLine.line}`
          : undefined,
      },
      playwrightContract: {
        flaky: flaky
          ? { outcome: flaky.outcome, attempts: flaky.attempts }
          : undefined,
        skipped: skipped
          ? { outcome: skipped.outcome, hits: skipped.hits }
          : undefined,
        expectedFailure: expectedFailure
          ? { outcome: expectedFailure.outcome, attempts: expectedFailure.attempts }
          : undefined,
        passedContainsFlakyTerminalAttempt: report.filters?.passed.tests.some(
          (test) => test.id === flaky?.id && test.retries.join(",") === "1",
        ),
        passedContainsExpectedFailure: report.filters?.passed.tests.some(
          (test) => test.id === expectedFailure?.id,
        ),
      },
    }),
  );
} else {
  throw new Error(`unknown materialization mode: ${mode}`);
}
