import {
  mkdirSync,
  mkdtempSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { resolve } from "node:path";
import { afterEach, describe, it } from "node:test";
import { expect } from "../support/expect.ts";
import { createMcdcReport } from "../../src/analyze.ts";
import { discoverSourceScope } from "../../src/sourceDiscovery.ts";

const roots: string[] = [];

function repository(files: Record<string, string>): string {
  const root = mkdtempSync(resolve(tmpdir(), "supercov-scope-"));
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

describe("first-party source discovery", () => {
  it("discovers conventional and workspace-package source while blocking ambiguity", () => {
    const root = repository({
      "package.json": JSON.stringify({ workspaces: ["packages/*"] }),
      "src/index.ts": "export const root = true",
      "lib/helper.js": "export const helper = true",
      "src/index.test.ts": "test('root', () => {})",
      "tests/e2e.spec.ts": "test('e2e', () => {})",
      "scripts/release.mjs": "export const release = true",
      "build.mjs": "export const build = true",
      "vite.config.ts": "export default {}",
      ".eslintrc.cjs": "module.exports = {}",
      ".graphqlrc.ts": "export default {}",
      "orphan.ts": "export const missed = true",
      "packages/ui/package.json": JSON.stringify({ module: "./src/index.ts" }),
      "packages/ui/src/index.ts": "export const ui = true",
      "packages/ui/tests/ui.spec.ts": "test('ui', () => {})",
      "dist/generated.js": "export const generated = true",
      ".cache/tool/generated.js": "export const cached = true",
    });

    const discovered = discoverSourceScope(root);

    expect(discovered.sourceFiles).toEqual([
      "lib/helper.js",
      "packages/ui/src/index.ts",
      "src/index.ts",
    ]);
    expect(discovered.scope.entries).toEqual(
      expect.arrayContaining([
        expect.objectContaining({ file: "orphan.ts", status: "ambiguous" }),
        expect.objectContaining({ file: "src/index.test.ts", status: "excluded" }),
        expect.objectContaining({
          file: "scripts/release.mjs",
          status: "excluded",
          reason: "conventional tool script",
        }),
        expect.objectContaining({ file: "vite.config.ts", status: "excluded" }),
        expect.objectContaining({
          file: "build.mjs",
          status: "excluded",
          reason: "build/test/tool configuration",
        }),
        expect.objectContaining({ file: ".eslintrc.cjs", status: "excluded" }),
        expect.objectContaining({ file: ".graphqlrc.ts", status: "excluded" }),
      ]),
    );
    expect(discovered.limitations).toMatchObject([
      { kind: "source-scope", file: "orphan.ts" },
    ]);
    expect(
      discovered.scope.entries.some((entry) => entry.file.includes(".cache")),
    ).toBe(false);
  });

  it("treats explicit roots as authoritative and explains files outside them", () => {
    const root = repository({
      "package.json": "{}",
      "product/main.ts": "export const product = true",
      "orphan.ts": "export const intentionallyExcluded = true",
    });

    const discovered = discoverSourceScope(root, ["product"]);

    expect(discovered.sourceFiles).toEqual(["product/main.ts"]);
    expect(discovered.scope.mode).toBe("explicit");
    expect(discovered.scope.entries).toContainEqual(
      expect.objectContaining({
        file: "orphan.ts",
        status: "excluded",
        reason: "outside explicit source roots",
      }),
    );
    expect(discovered.limitations).toEqual([]);
  });

  it("mirrors TypeScript's default root for root-level libraries", () => {
    const root = repository({
      "package.json": JSON.stringify({ main: "./dist/index.js" }),
      "tsconfig.json": JSON.stringify({ compilerOptions: { target: "es2022" } }),
      "events.ts": "export const event = true",
      "library.ts": "export const library = true",
      "library.test.ts": "test('library', () => {})",
    });

    const discovered = discoverSourceScope(root);

    expect(discovered.sourceRoots).toContain(".");
    expect(discovered.sourceFiles).toEqual(["events.ts", "library.ts"]);
    expect(discovered.limitations).toEqual([]);
  });

  it("blocks a complete score while first-party source remains ambiguous", () => {
    const root = repository({
      "package.json": "{}",
      "src/index.ts": "export const covered = true",
      "unclassified.ts": "export const possiblyProductCode = true",
    });
    const discovered = discoverSourceScope(root);
    const report = createMcdcReport(
      {
        points: [],
        branches: [],
        decisions: [],
        scope: discovered.scope,
        limitations: discovered.limitations,
      },
      [],
    );

    expect(report.scope).toEqual(discovered.scope);
    expect(report.summary.coverageComplete).toBe(false);
    expect(report.summary.completenessBlocked).toBe(true);
  });
});
