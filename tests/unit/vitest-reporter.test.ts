import { mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { resolve } from "node:path";
import { afterEach, describe, expect, it } from "vitest";
import SupercovVitestReporter from "../../src/vitestReporter";

const temporaryDirectories: string[] = [];

afterEach(() => {
  delete process.env.SUPERCOV_EVIDENCE_DIR;
  for (const directory of temporaryDirectories.splice(0))
    rmSync(directory, { recursive: true, force: true });
});

describe("Vitest reporter compatibility", () => {
  it("records final pass, failure, and skipped outcomes from the Vitest 2 task tree", () => {
    const directory = mkdtempSync(resolve(tmpdir(), "supercov-vitest-"));
    temporaryDirectories.push(directory);
    process.env.SUPERCOV_EVIDENCE_DIR = directory;
    const reporter = new SupercovVitestReporter();
    const suite = { name: "legacy suite" };
    reporter.onFinished([
      {
        id: "file",
        type: "suite",
        filepath: resolve(directory, "tests/unit/legacy.test.ts"),
        projectName: "unit",
        tasks: [
          { id: "pass", type: "test", name: "passes", suite, result: { state: "pass" } },
          { id: "fail", type: "test", name: "fails", suite, result: { state: "fail", retryCount: 1 } },
          { id: "skip", type: "test", name: "skips", suite, result: { state: "skip" } },
        ],
      },
    ]);

    const read = (path: string) =>
      JSON.parse(readFileSync(resolve(directory, path), "utf8")) as {
        status: string;
        retry: number;
        provenance: { runner: string; kind: string };
      };
    expect(read("vitest-pass-0-status/mcdc.json")).toMatchObject({
      status: "passed",
      retry: 0,
      provenance: { runner: "vitest", kind: "unit" },
    });
    expect(read("vitest-fail-1-status/mcdc.json")).toMatchObject({
      status: "failed",
      retry: 1,
    });
    expect(read("vitest-skip-0-status/mcdc.json")).toMatchObject({
      status: "skipped",
      retry: 0,
    });
  });
});
