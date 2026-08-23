import { mkdtempSync, mkdirSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { resolve } from "node:path";
import { describe, expect, it } from "vitest";
import {
  compareRunIntegrity,
  createRunIntegrity,
} from "../../src/integrity";
import type { CoverageProject } from "../../src/project";

describe("run integrity", () => {
  it("fingerprints source, tests, dependencies, configuration, and instrumenter", () => {
    const root = mkdtempSync(resolve(tmpdir(), "supercov-integrity-"));
    try {
      mkdirSync(resolve(root, "app"));
      mkdirSync(resolve(root, "tests"));
      mkdirSync(resolve(root, "tool"));
      writeFileSync(resolve(root, "app/index.ts"), "export const value = 1;\n");
      writeFileSync(resolve(root, "tests/value.spec.ts"), "test('value', () => {});\n");
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
      writeFileSync(resolve(root, "app/index.ts"), "export const value = 2;\n");
      const changed = createRunIntegrity(root, project, resolve(root, "tool"));
      expect(compareRunIntegrity(first, changed)).toMatchObject({
        stale: true,
        reasons: ["instrumented source changed"],
      });
    } finally {
      rmSync(root, { recursive: true, force: true });
    }
  });
});
