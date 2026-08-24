import { mkdtempSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { resolve } from "node:path";
import { afterEach, describe, it } from "node:test";
import { expect } from "../support/expect.ts";
import { instrumentDirectWorkspace } from "../../src/directInstrumenter.ts";

const roots: string[] = [];

afterEach(() => {
  for (const root of roots.splice(0))
    rmSync(root, { recursive: true, force: true });
});

describe("direct isolated instrumentation", () => {
  it("rewrites the virtual runtime import to a module-format-neutral global", () => {
    const root = mkdtempSync(resolve(tmpdir(), "supercov-direct-"));
    roots.push(root);
    mkdirSync(resolve(root, "src"), { recursive: true });
    writeFileSync(
      resolve(root, "src/decision.js"),
      "export function allowed(left, right) { if (left && right) return 'yes'; return 'no'; }\n",
    );
    const manifestPath = resolve(root, ".supercov/manifest.json");
    const manifest = instrumentDirectWorkspace(
      root,
      ["src/decision.js"],
      manifestPath,
    );
    const transformed = readFileSync(resolve(root, "src/decision.js"), "utf8");
    expect(transformed).toContain("globalThis.__SUPERCOV_DIRECT_RUNTIME__");
    expect(transformed).not.toContain("virtual:supercov-runtime");
    expect(manifest.decisions).toHaveLength(1);
    expect(manifest.points.length).toBeGreaterThan(0);
    expect(JSON.parse(readFileSync(manifestPath, "utf8"))).toEqual(manifest);
  });

  it("uses a physical in-workspace runtime module for generic bundlers", () => {
    const root = mkdtempSync(resolve(tmpdir(), "supercov-bundler-"));
    roots.push(root);
    mkdirSync(resolve(root, "src"), { recursive: true });
    writeFileSync(
      resolve(root, "src/decision.js"),
      "export const allowed = (left, right) => left && right;\n",
    );
    instrumentDirectWorkspace(
      root,
      ["src/decision.js"],
      resolve(root, ".supercov/manifest.json"),
      undefined,
      [],
      "module",
    );
    const transformed = readFileSync(resolve(root, "src/decision.js"), "utf8");
    expect(transformed).toContain('from "../.supercov/runtime.js"');
    expect(transformed).not.toContain("__SUPERCOV_DIRECT_RUNTIME__");
  });
});
