import { existsSync, mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { resolve } from "node:path";
import { afterEach, it } from "node:test";
import { expect } from "../support/expect.ts";
import {
  runnerExecutionScope,
  writeRunnerEvidence,
} from "../../src/runnerEvidence.ts";

const roots: string[] = [];

afterEach(() => {
  for (const root of roots.splice(0))
    rmSync(root, { recursive: true, force: true });
});

it("keeps an attempt's evidence destination stable across user environment changes", () => {
  const captured = mkdtempSync(resolve(tmpdir(), "supercov-runner-captured-"));
  const changed = mkdtempSync(resolve(tmpdir(), "supercov-runner-changed-"));
  roots.push(captured, changed);
  const identity = {
    runner: "node:test",
    name: "mutates process.env",
    file: resolve("tests/unit/example.test.ts"),
    line: 1,
    column: 1,
  };
  const scope = runnerExecutionScope(identity);
  const previous = process.env["SUPERCOV_EVIDENCE_DIR"];

  try {
    process.env["SUPERCOV_EVIDENCE_DIR"] = changed;
    writeRunnerEvidence(identity, "passed", scope, captured);
  } finally {
    if (previous === undefined) delete process.env["SUPERCOV_EVIDENCE_DIR"];
    else process.env["SUPERCOV_EVIDENCE_DIR"] = previous;
  }

  const directory = `node_test-${scope.attemptId}`;
  expect(existsSync(resolve(captured, directory, "mcdc.json"))).toBe(true);
  expect(existsSync(resolve(changed, directory, "mcdc.json"))).toBe(false);
});
