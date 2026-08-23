import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { resolve } from "node:path";
import { afterEach, describe, expect, it } from "vitest";
import { discoverCoverageProject } from "../../src/project";

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
      essentialOffline: false,
      buildCommand: ["npm", "run", "build"],
    });
  });

  it("detects the Essential fixture as an adapter rather than a core assumption", () => {
    const root = project({
      "package.json": JSON.stringify({
        scripts: { build: "vite build" },
        devDependencies: { vite: "1" },
      }),
      "app/root.tsx": "export default null",
      "tests/example.spec.ts":
        "import { offlineTest } from '@essential-apps/shopify-test-admin'",
    });
    expect(discoverCoverageProject(root, {})).toMatchObject({
      sourceRoots: ["app"],
      playwrightModule: "@essential-apps/shopify-test-admin",
      essentialOffline: true,
    });
  });
});
