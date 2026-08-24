import {
  mkdirSync,
  mkdtempSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { resolve } from "node:path";
import { afterEach, it } from "node:test";
import { expect } from "../support/expect.ts";
import { writeEvidenceArchive } from "../../src/evidenceArchive.ts";
import { analyzeCoverageArchive } from "../../src/runAnalysis.ts";
import type { CoverageManifest, McdcRawTestResult } from "../../src/types.ts";

const temporaryDirectories: string[] = [];

afterEach(() => {
  for (const directory of temporaryDirectories.splice(0))
    rmSync(directory, { recursive: true, force: true });
});

it("reconstructs observed and outcome-filtered views solely from archived evidence", () => {
  const root = mkdtempSync(resolve(tmpdir(), "supercov-analysis-"));
  temporaryDirectories.push(root);
  const manifest: CoverageManifest = {
    decisions: [],
    branches: [],
    points: [
      {
        id: "executed",
        kind: "statement",
        file: "src/example.ts",
        line: 1,
        column: 1,
        source: "execute();",
      },
    ],
  };
  const result: McdcRawTestResult = {
    testId: "passing-test",
    test: "passing-test",
    status: "passed",
    runtime: [{ decisions: [], hits: ["executed"] }],
    browser: [],
    server: [],
  };
  const manifestPath = resolve(root, "manifest-source.json");
  const evidenceDirectory = resolve(root, "evidence/test");
  mkdirSync(evidenceDirectory, { recursive: true });
  writeFileSync(manifestPath, `${JSON.stringify(manifest)}\n`);
  writeFileSync(resolve(evidenceDirectory, "mcdc.json"), `${JSON.stringify(result)}\n`);
  writeFileSync(
    resolve(root, "evidence/execution.host.1.jsonl"),
    [
      JSON.stringify({ event: "process" }),
      JSON.stringify({ event: "workspace-capability" }),
      JSON.stringify({ event: "remote-launch" }),
    ].join("\n") + "\n",
  );
  const archivePath = resolve(root, "evidence.raw.gz");
  writeEvidenceArchive(
    [
      { file: manifestPath, path: "manifest.json" },
      { directory: resolve(root, "evidence") },
    ],
    archivePath,
  );

  const report = analyzeCoverageArchive(archivePath, {
    runId: "run-1",
    testExitCode: 0,
    generatedAt: "2026-08-24T00:00:00.000Z",
  });

  expect(report.generatedAt).toBe("2026-08-24T00:00:00.000Z");
  expect(report.summary.lines).toMatchObject({ covered: 1, total: 1 });
  expect(report.filters?.passed.summary.lines).toMatchObject({
    covered: 1,
    total: 1,
  });
  expect(report.filters?.failed.summary.lines).toMatchObject({
    covered: 0,
    total: 1,
  });
  expect(report.execution).toEqual({ testExitCode: 0, valid: true });
  expect(report.transport).toEqual({
    processes: 1,
    childLaunches: 0,
    remoteLaunches: 1,
    workspaceCapabilities: 1,
    scopedServerRecords: 0,
    backgroundServerRecords: 0,
  });
  expect(report.filters?.passed.transport).toEqual(report.transport);
});
