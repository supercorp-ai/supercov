import { mkdirSync } from "node:fs";
import { relative, resolve, sep } from "node:path";
import { afterEach, beforeEach } from "vitest";
import { coverageSnapshot, resetCoverage } from "./runtime.ts";
import { inferTestProvenance } from "./provenance.ts";
import { atomicWriteFileSync } from "./atomic.ts";
import type { McdcRawTestResult } from "./types.ts";

const evidenceDirectory = process.env["SUPERCOV_EVIDENCE_DIR"];
const emittedSetupFiles = new Set<string>();

function attemptStatus(
  state: string | undefined,
): McdcRawTestResult["status"] {
  if (state === "pass") return "passed";
  if (state === "fail") return "failed";
  if (state === "skip" || state === "todo") return "skipped";
  return "unknown";
}

function titlePath(
  task: Readonly<{ name: string; suite?: unknown }>,
): string[] {
  const names: string[] = [task.name];
  let suite = task.suite as { name?: string; suite?: unknown } | undefined;
  while (suite?.name) {
    names.unshift(suite.name);
    suite = suite.suite as typeof suite;
  }
  return names;
}

function writeEvidence(payload: McdcRawTestResult, suffix: string): void {
  if (!evidenceDirectory) return;
  const directory = resolve(process.cwd(), evidenceDirectory, suffix);
  mkdirSync(directory, { recursive: true });
  atomicWriteFileSync(
    resolve(directory, "mcdc.json"),
    `${JSON.stringify(payload)}\n`,
  );
}

beforeEach((context) => {
  const task = context.task;
  const testFile = relative(process.cwd(), task.file.filepath)
    .split(sep)
    .join("/");
  if (!emittedSetupFiles.has(testFile)) {
    emittedSetupFiles.add(testFile);
    const setupSnapshot = coverageSnapshot();
    if (setupSnapshot.hits.length || setupSnapshot.decisions.length) {
      writeEvidence(
        {
          testId: `vitest:${task.file.id}:setup`,
          test: `${testFile} > module setup`,
          testFile,
          title: "module setup",
          retry: 0,
          status: "passed",
          provenance: inferTestProvenance({
            runner: "vitest",
            file: testFile,
            project: task.file.projectName,
            explicitKind: process.env["SUPERCOV_TEST_KIND"],
          }),
          role: "setup",
          runtime: [setupSnapshot],
          browser: [],
          server: [],
        },
        `vitest-${task.file.id}-setup`,
      );
    }
  }
  resetCoverage(`vitest:${task.id}`);
});

afterEach((context) => {
  const task = context.task;
  const testFile = relative(process.cwd(), task.file.filepath)
    .split(sep)
    .join("/");
  const retry = task.result?.retryCount ?? 0;
  const payload: McdcRawTestResult = {
    testId: `vitest:${task.id}`,
    test: [...titlePath(task)].join(" > "),
    testFile,
    title: task.name,
    retry,
    status: attemptStatus(task.result?.state),
    provenance: inferTestProvenance({
      runner: "vitest",
      file: testFile,
      project: task.file.projectName,
      explicitKind: process.env["SUPERCOV_TEST_KIND"],
    }),
    runtime: [coverageSnapshot()],
    browser: [],
    server: [],
  };
  writeEvidence(payload, `vitest-${task.id}-${retry}`);
});
