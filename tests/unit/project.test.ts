import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { resolve } from "node:path";
import { afterEach, describe, it } from "node:test";
import { expect } from "../support/expect.ts";
import { discoverCoverageProject } from "../../src/project.ts";

const roots: string[] = [];

function project(files: Record<string, string>): string {
  const root = mkdtempSync(resolve(tmpdir(), "supercov-project-"));
  roots.push(root);
  for (const [file, contents] of Object.entries(files)) {
    const path = resolve(root, file);
    mkdirSync(resolve(path, ".."), { recursive: true });
    writeFileSync(path, contents);
  }
  return root;
}

afterEach(() => {
  for (const root of roots.splice(0))
    rmSync(root, { recursive: true, force: true });
});

describe("coverage project discovery", () => {
  it("discovers a conventional Vite and Playwright project", () => {
    const root = project({
      "package.json": JSON.stringify({
        scripts: { build: "vite build" },
        devDependencies: { vite: "1" },
      }),
      "src/main.ts": "export const ready = true",
      "playwright.config.ts": "export default {}",
      "vitest.config.ts": "export default {}",
      "tests/example.spec.ts": "import { test } from '@playwright/test'",
    });
    expect(discoverCoverageProject(root, {})).toMatchObject({
      sourceRoots: ["src"],
      playwrightConfig: resolve(root, "playwright.config.ts"),
      vitestConfig: resolve(root, "vitest.config.ts"),
      playwrightModule: "@playwright/test",
      playwrightTestExport: "test",
      playwrightExports: ["test"],
      buildAdapter: "vite",
      buildCommand: ["npm", "run", "build"],
      buildEnvironment: {},
    });
  });

  it("uses direct isolated instrumentation when no build script exists", () => {
    const root = project({
      "package.json": JSON.stringify({
        type: "module",
        scripts: { test: "node test.mjs" },
      }),
      "src/decision.js":
        "export const allowed = (left, right) => left && right",
      "test.mjs": "import './src/decision.js'",
    });
    expect(discoverCoverageProject(root, {}, ["npm", "test"])).toMatchObject({
      sourceRoots: ["src"],
      buildAdapter: "direct",
      buildCommand: [],
      buildEnvironment: {},
    });
  });

  it("does not run an unrelated production build for a source-executing node:test suite", () => {
    const root = project({
      "package.json": JSON.stringify({
        scripts: {
          build: "tsc -p tsconfig.build.json",
          test: "node --test tests/*.test.ts",
        },
        devDependencies: { typescript: "1", vite: "1" },
      }),
      "src/index.ts": "export const ready = true",
      "tests/index.test.ts": "import { test } from 'node:test'",
    });
    expect(discoverCoverageProject(root, {}, ["npm", "test"])).toMatchObject({
      buildAdapter: "direct",
      buildCommand: [],
    });
  });

  it("uses the generic isolated adapter for a non-Vite build", () => {
    const root = project({
      "package.json": JSON.stringify({
        type: "module",
        scripts: { build: "webpack", test: "node --test" },
        devDependencies: { webpack: "1" },
      }),
      "src/index.js": "export const ready = true",
    });
    expect(discoverCoverageProject(root, {})).toMatchObject({
      sourceRoots: ["src"],
      buildAdapter: "generic",
      buildCommand: ["npm", "run", "build"],
    });
  });

  it("does not mistake an installed Vite compatibility dependency for the build tool", () => {
    const root = project({
      "package.json": JSON.stringify({
        scripts: { build: "tsc -p tsconfig.build.json", test: "node --test" },
        devDependencies: { typescript: "1", vite: "1" },
      }),
      "src/index.ts": "export const ready = true",
    });
    expect(discoverCoverageProject(root, {})).toMatchObject({
      buildAdapter: "generic",
      buildCommand: ["npm", "run", "build"],
    });
  });

  it("discovers a project-owned Playwright fixture module from test imports", () => {
    const root = project({
      "package.json": JSON.stringify({
        scripts: { build: "vite build" },
        devDependencies: { vite: "1" },
      }),
      "app/root.tsx": "export default null",
      "tests/nested/playwright.browser.config.ts": "export default {}",
      "tests/example.spec.ts":
        "import { browserTest as test, expect, fixtureValue } from '@acme/browser-fixtures'",
    });
    expect(discoverCoverageProject(root, {})).toMatchObject({
      sourceRoots: ["app"],
      playwrightConfig: resolve(
        root,
        "tests/nested/playwright.browser.config.ts",
      ),
      playwrightModule: "@acme/browser-fixtures",
      playwrightTestExport: "browserTest",
      playwrightExports: ["browserTest", "expect", "fixtureValue"],
    });
  });

  it("infers a build mode from the test command and build config", () => {
    const root = project({
      "package.json": JSON.stringify({
        scripts: {
          build: "vite build",
          "test:isolated": "node tools/run-suite.js",
        },
        devDependencies: { vite: "1" },
      }),
      "app/root.ts": "export const ready = true",
      "vite.config.ts":
        "const isolated = process.env.TEST_ISOLATED === 'true'; const url = process.env.PRODUCT_URL; export default { isolated, url }",
    });
    expect(
      discoverCoverageProject(root, {}, ["npm", "run", "test:isolated"])
        .buildEnvironment,
    ).toEqual({ TEST_ISOLATED: "true" });
  });
});
