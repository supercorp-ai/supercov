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
import {
  buildCacheReusePaths,
  instrumentedBuildCacheKey,
  readInstrumentedBuildCache,
  writeInstrumentedBuildCache,
} from "../../src/buildCache.ts";
import type { CoverageProject } from "../../src/project.ts";
import type { CoverageRunIntegrity } from "../../src/types.ts";

const temporaryDirectories: string[] = [];

afterEach(() => {
  for (const directory of temporaryDirectories.splice(0))
    rmSync(directory, { recursive: true, force: true });
});

function integrity(execution: string): CoverageRunIntegrity {
  return {
    schemaVersion: 2,
    instrumenterVersion: "2",
    fingerprint: {
      algorithm: "sha256",
      source: "source",
      tests: "tests",
      dependencies: "dependencies",
      configuration: "configuration",
      instrumenter: "instrumenter",
      execution,
      combined: "combined",
      sourceFiles: 1,
      testFiles: 1,
    },
  };
}

function integrityWithTests(execution: string, tests: string): CoverageRunIntegrity {
  const value = integrity(execution);
  value.fingerprint.tests = tests;
  return value;
}

const project: CoverageProject = {
  root: "/project",
  sourceRoots: ["src"],
  sourceFiles: ["src/index.ts"],
  sourceScope: {
    version: 1,
    mode: "automatic",
    roots: ["src"],
    entries: [],
  },
  sourceLimitations: [],
  playwrightModule: "@playwright/test",
  playwrightTestExport: "test",
  playwrightExports: [],
  buildAdapter: "vite",
  buildCommand: ["npm", "run", "build"],
  buildEnvironment: {},
};

it("reuses only a complete build with an exact execution fingerprint", () => {
  const workspace = mkdtempSync(resolve(tmpdir(), "supercov-build-cache-"));
  temporaryDirectories.push(workspace);
  mkdirSync(resolve(workspace, "build"));
  mkdirSync(resolve(workspace, ".supercov"));
  writeFileSync(resolve(workspace, "build/index.js"), "instrumented\n");
  writeFileSync(resolve(workspace, ".supercov/manifest.json"), "{}\n");
  mkdirSync(resolve(workspace, "custom-output"));
  writeFileSync(resolve(workspace, "custom-output/index.js"), "custom\n");
  writeFileSync(
    resolve(workspace, ".supercov/build-outputs.json"),
    '{"paths":["custom-output","../outside"]}\n',
  );
  const key = instrumentedBuildCacheKey(integrity("same"), project);
  expect(
    instrumentedBuildCacheKey(integrityWithTests("same", "changed-tests"), project),
  ).toBe(key);
  expect(instrumentedBuildCacheKey(integrity("changed-source"), project)).not.toBe(
    key,
  );
  const written = writeInstrumentedBuildCache(workspace, key);
  expect(written).toBeDefined();
  const reused = readInstrumentedBuildCache(workspace, key);
  expect(reused).toEqual(written);
  expect(buildCacheReusePaths(reused!)).toEqual([
    "build",
    "custom-output",
    ".supercov/manifest.json",
    ".supercov/build-cache.json",
  ]);
  expect(readInstrumentedBuildCache(workspace, "changed")).toBeUndefined();

  rmSync(resolve(workspace, "build"), { recursive: true });
  expect(readInstrumentedBuildCache(workspace, key)).toBeUndefined();
});
