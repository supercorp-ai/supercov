import { mkdtempSync, mkdirSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { resolve } from "node:path";
import { describe, it } from "node:test";
import { expect } from "../support/expect.ts";
import {
  compareRunIntegrity,
  createRunIntegrity,
} from "../../src/integrity.ts";
import type { CoverageProject } from "../../src/project.ts";

describe("run integrity", () => {
  it("fingerprints source, tests, dependencies, configuration, and instrumenter", () => {
    const root = mkdtempSync(resolve(tmpdir(), "supercov-integrity-"));
    const previousEngine = process.env["SUPERCOV_ENGINE"];
    const previousBinary = process.env["SUPERCOV_RUST_BINARY"];
    delete process.env["SUPERCOV_ENGINE"];
    delete process.env["SUPERCOV_RUST_BINARY"];
    try {
      mkdirSync(resolve(root, "app"));
      mkdirSync(resolve(root, "tests"));
      mkdirSync(resolve(root, "tool"));
      mkdirSync(resolve(root, ".cache/test262/test"), { recursive: true });
      writeFileSync(resolve(root, "app/index.ts"), "export const value = 1;\n");
      writeFileSync(resolve(root, "tests/value.spec.ts"), "test('value', () => {});\n");
      writeFileSync(
        resolve(root, ".cache/test262/test/generated.test.js"),
        "test('external corpus', () => {});\n",
      );
      writeFileSync(resolve(root, "package.json"), '{"scripts":{"build":"vite build"}}\n');
      writeFileSync(resolve(root, "vite.config.ts"), "export default {};\n");
      writeFileSync(resolve(root, "tool/instrumenter.ts"), "export {};\n");
      const project: CoverageProject = {
        root,
        sourceRoots: ["app"],
        sourceFiles: ["app/index.ts"],
        sourceScope: {
          version: 1,
          mode: "automatic",
          roots: ["app"],
          entries: [],
        },
        sourceLimitations: [],
        playwrightModule: "@playwright/test",
        playwrightTestExport: "test",
        playwrightExports: ["expect", "test"],
        buildAdapter: "vite",
        buildCommand: ["npm", "run", "build"],
        buildEnvironment: {},
      };
      const first = createRunIntegrity(root, project, resolve(root, "tool"));
      expect(first.fingerprint).toMatchObject({
        sourceFiles: 1,
        testFiles: 1,
        execution: expect.stringMatching(/^[a-f0-9]{64}$/),
      });
      expect(compareRunIntegrity(first, first)).toEqual({
        stale: false,
        reasons: [],
      });
      const rustBinary = resolve(root, "rust-engine");
      writeFileSync(rustBinary, "candidate-a");
      process.env["SUPERCOV_ENGINE"] = "rust";
      process.env["SUPERCOV_RUST_BINARY"] = rustBinary;
      const rust = createRunIntegrity(root, project, resolve(root, "tool"));
      expect(rust.fingerprint.instrumenter).not.toBe(first.fingerprint.instrumenter);
      expect(rust.fingerprint.execution).not.toBe(first.fingerprint.execution);
      writeFileSync(rustBinary, "candidate-b");
      const rebuiltRust = createRunIntegrity(root, project, resolve(root, "tool"));
      expect(rebuiltRust.fingerprint.instrumenter).not.toBe(
        rust.fingerprint.instrumenter,
      );
      delete process.env["SUPERCOV_ENGINE"];
      delete process.env["SUPERCOV_RUST_BINARY"];
      writeFileSync(resolve(root, "app/index.ts"), "export const value = 2;\n");
      const changed = createRunIntegrity(root, project, resolve(root, "tool"));
      expect(compareRunIntegrity(first, changed)).toMatchObject({
        stale: true,
        reasons: ["instrumented source changed"],
      });
    } finally {
      if (previousEngine === undefined) delete process.env["SUPERCOV_ENGINE"];
      else process.env["SUPERCOV_ENGINE"] = previousEngine;
      if (previousBinary === undefined) delete process.env["SUPERCOV_RUST_BINARY"];
      else process.env["SUPERCOV_RUST_BINARY"] = previousBinary;
      rmSync(root, { recursive: true, force: true });
    }
  });
});
