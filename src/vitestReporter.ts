import { mkdirSync } from "node:fs";
import { relative, resolve, sep } from "node:path";
import { fileURLToPath } from "node:url";
import type { Reporter } from "vitest/reporters";
import { inferTestProvenance } from "./provenance.ts";
import { atomicWriteFileSync } from "./atomic.ts";
import type { McdcRawTestResult } from "./types.ts";

interface ReportedTestCase {
  id: string;
  name: string;
  fullName: string;
  module: { moduleId: string };
  project: { name?: string };
  options: { fails?: boolean };
  result(): {
    state: "passed" | "failed" | "skipped" | "pending";
  };
  diagnostic():
    | { retryCount: number; flaky: boolean }
    | undefined;
}

function sourcePath(moduleId: string): string {
  const absolute = moduleId.startsWith("file:")
    ? fileURLToPath(moduleId)
    : moduleId;
  return relative(process.cwd(), absolute).split(sep).join("/");
}

/** Records final runner outcomes, including tests that never execute hooks. */
export default class SupercovVitestReporter implements Reporter {
  onTestCaseResult(testCase: ReportedTestCase): void {
    const evidenceDirectory =
      process.env["SUPERCOV_EVIDENCE_DIR"];
    if (!evidenceDirectory) return;
    const result = testCase.result();
    if (result.state === "pending") return;
    const diagnostic = testCase.diagnostic();
    const testFile = sourcePath(testCase.module.moduleId);
    const retry = diagnostic?.retryCount ?? 0;
    const payload: McdcRawTestResult = {
      testId: `vitest:${testCase.id}`,
      test: testCase.fullName,
      testFile,
      title: testCase.name,
      retry,
      status: result.state,
      expectedStatus: testCase.options.fails ? "failed" : "passed",
      flaky: diagnostic?.flaky ?? false,
      provenance: inferTestProvenance({
        runner: "vitest",
        file: testFile,
        project: testCase.project.name,
        explicitKind: process.env["SUPERCOV_TEST_KIND"],
      }),
      runtime: [],
      browser: [],
      server: [],
    };
    const safeId = testCase.id.replace(/[^a-zA-Z0-9_-]/g, "_");
    const directory = resolve(
      process.cwd(),
      evidenceDirectory,
      `vitest-${safeId}-${retry}-status`,
    );
    mkdirSync(directory, { recursive: true });
    atomicWriteFileSync(
      resolve(directory, "mcdc.json"),
      `${JSON.stringify(payload)}\n`,
    );
  }
}
