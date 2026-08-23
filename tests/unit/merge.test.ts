import { mkdirSync, mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { resolve } from "node:path";
import { afterEach, describe, expect, it } from "vitest";
import { atomicWriteFileSync } from "../../src/atomic";
import { writeEvidenceArchiveEntries } from "../../src/evidenceArchive";
import { mergeCoverageRuns } from "../../src/merge";
import { analyzeCoverageArchive } from "../../src/runAnalysis";
import type { CoverageRunIntegrity, McdcRawTestResult } from "../../src/types";

const roots: string[] = [];
const integrity: CoverageRunIntegrity = {
  schemaVersion: 1,
  instrumenterVersion: "test",
  fingerprint: {
    algorithm: "sha256",
    source: "source",
    tests: "tests",
    dependencies: "deps",
    configuration: "config",
    instrumenter: "instrumenter",
    execution: "execution",
    combined: "combined",
    sourceFiles: 1,
    testFiles: 2,
  },
};

afterEach(() => {
  for (const root of roots.splice(0)) rmSync(root, { recursive: true, force: true });
});

function createRun(root: string, id: string, testId: string): void {
  const directory = resolve(root, ".supercov/runs", id);
  mkdirSync(directory, { recursive: true });
  const scope = {
    version: 1 as const,
    runId: id,
    workerId: "worker",
    testId,
    testKey: testId,
    retry: 0,
    attemptId: `${testId}-0`,
  };
  const raw: McdcRawTestResult = {
    testId,
    scope,
    test: testId,
    status: "passed",
    provenance: { runner: "node:test", kind: "unit", source: "runner-default" },
    runtime: [{ hits: ["statement"], decisions: [] }],
    browser: [],
    server: [],
  };
  const rawEvidence = writeEvidenceArchiveEntries(
    [
      {
        path: "manifest.json",
        contents: JSON.stringify({
          points: [{ id: "statement", kind: "statement", file: "src/a.js", line: 1, column: 1, source: "run" }],
          branches: [],
          decisions: [],
        }),
      },
      { path: `${testId}/mcdc.json`, contents: JSON.stringify(raw) },
    ],
    resolve(directory, "evidence.raw.gz"),
  );
  atomicWriteFileSync(
    resolve(directory, "run.json"),
    `${JSON.stringify({ id, startedAt: new Date(0).toISOString(), testExitCode: 0, integrity, rawEvidence })}\n`,
  );
}

describe("distributed evidence merging", () => {
  it("publishes an immutable compatible merged run and rewrites run scopes", () => {
    const root = mkdtempSync(resolve(tmpdir(), "supercov-merge-"));
    roots.push(root);
    createRun(root, "shard-a", "test-a");
    createRun(root, "shard-b", "test-b");

    const merged = mergeCoverageRuns(root, ["shard-a", "shard-b"]);
    const metadata = JSON.parse(readFileSync(resolve(root, ".supercov/runs", merged, "run.json"), "utf8"));
    expect(metadata.parents).toEqual(["shard-a", "shard-b"]);
    const report = analyzeCoverageArchive(
      resolve(root, ".supercov/runs", merged, "evidence.raw.gz"),
      { runId: merged, testExitCode: 0, integrity },
    );
    expect(report.tests.map((test) => test.id).sort()).toEqual(["test-a", "test-b"]);
    expect(report.summary.lines.percentage).toBe(100);
  });

  it("rejects incompatible fingerprints", () => {
    const root = mkdtempSync(resolve(tmpdir(), "supercov-merge-"));
    roots.push(root);
    createRun(root, "shard-a", "test-a");
    createRun(root, "shard-b", "test-b");
    const path = resolve(root, ".supercov/runs/shard-b/run.json");
    const value = JSON.parse(readFileSync(path, "utf8"));
    value.integrity.fingerprint.combined = "different";
    atomicWriteFileSync(path, JSON.stringify(value));
    expect(() => mergeCoverageRuns(root, ["shard-a", "shard-b"])).toThrow("incompatible");
  });
});
