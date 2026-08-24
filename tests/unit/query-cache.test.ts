import {
  mkdirSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { resolve } from "node:path";
import { afterEach, it } from "node:test";
import { expect } from "../support/expect.ts";
import { writeEvidenceArchive } from "../../src/evidenceArchive.ts";
import {
  analyzeCoverageArchiveCached,
  coverageQueryIndexPath,
  readCoverageQueryIndex,
} from "../../src/queryCache.ts";
import type { CoverageManifest, McdcRawTestResult } from "../../src/types.ts";

const temporaryDirectories: string[] = [];

afterEach(() => {
  for (const directory of temporaryDirectories.splice(0))
    rmSync(directory, { recursive: true, force: true });
});

function writeRun(root: string, covered: boolean): string {
  const manifest: CoverageManifest = {
    decisions: [],
    branches: [],
    points: [{
      id: "point",
      kind: "statement",
      file: "src/example.ts",
      line: 1,
      column: 1,
      source: "execute();",
    }],
  };
  const result: McdcRawTestResult = {
    testId: "test",
    test: "test",
    status: "passed",
    runtime: [{ decisions: [], hits: covered ? ["point"] : [] }],
    browser: [],
    server: [],
  };
  const loose = resolve(root, "loose");
  mkdirSync(resolve(loose, "test"), { recursive: true });
  writeFileSync(resolve(loose, "manifest.json"), JSON.stringify(manifest));
  writeFileSync(resolve(loose, "test/mcdc.json"), JSON.stringify(result));
  const archive = resolve(root, "evidence.raw.gz");
  writeEvidenceArchive(
    [
      { file: resolve(loose, "manifest.json"), path: "manifest.json" },
      { directory: resolve(loose, "test") },
    ],
    archive,
  );
  return archive;
}

it("lazily reuses a validated disposable query index", () => {
  const root = mkdtempSync(resolve(tmpdir(), "supercov-query-cache-"));
  temporaryDirectories.push(root);
  const archive = writeRun(root, true);
  const options = { runId: "run-1", testExitCode: 0 };

  expect(readCoverageQueryIndex(archive, options)).toBeUndefined();
  expect(analyzeCoverageArchiveCached(archive, options).summary.lines.covered).toBe(1);
  expect(readCoverageQueryIndex(archive, options)?.summary.lines.covered).toBe(1);
  const indexPath = coverageQueryIndexPath(archive);
  const firstIndex = readFileSync(indexPath);
  expect(analyzeCoverageArchiveCached(archive, options).summary.lines.covered).toBe(1);
  expect(readFileSync(indexPath)).toEqual(firstIndex);

  writeFileSync(indexPath, "corrupt");
  expect(analyzeCoverageArchiveCached(archive, options).summary.lines.covered).toBe(1);
  expect(readFileSync(indexPath).length).toBeGreaterThan("corrupt".length);
});

it("invalidates the query index when raw evidence changes", () => {
  const root = mkdtempSync(resolve(tmpdir(), "supercov-query-cache-"));
  temporaryDirectories.push(root);
  const archive = writeRun(root, true);
  const options = { runId: "run-1", testExitCode: 0 };
  expect(analyzeCoverageArchiveCached(archive, options).summary.lines.covered).toBe(1);

  writeRun(root, false);
  expect(analyzeCoverageArchiveCached(archive, options).summary.lines.covered).toBe(0);
});

it("invalidates the query index when immutable run metadata changes", () => {
  const root = mkdtempSync(resolve(tmpdir(), "supercov-query-cache-"));
  temporaryDirectories.push(root);
  const archive = writeRun(root, true);
  const valid = analyzeCoverageArchiveCached(archive, {
    runId: "run-1",
    testExitCode: 0,
  });
  expect(valid.execution).toEqual({ testExitCode: 0, valid: true });

  const invalid = analyzeCoverageArchiveCached(archive, {
    runId: "run-1",
    testExitCode: 1,
  });
  expect(invalid.execution).toEqual({ testExitCode: 1, valid: false });
});
