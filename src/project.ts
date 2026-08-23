import { existsSync, readFileSync, readdirSync } from "node:fs";
import { resolve } from "node:path";

export interface CoverageProject {
  root: string;
  sourceRoots: string[];
  playwrightConfig?: string;
  vitestConfig?: string;
  playwrightModule: string;
  essentialOffline: boolean;
  buildCommand: string[];
}

const PLAYWRIGHT_CONFIG_CANDIDATES = [
  "playwright.config.ts",
  "playwright.config.mts",
  "playwright.config.js",
  "playwright.config.mjs",
  "playwright.config.cts",
  "playwright.config.cjs",
  "tests/offline/playwright.offline.config.ts",
];

const VITEST_CONFIG_CANDIDATES = [
  "vitest.config.ts",
  "vitest.config.mts",
  "vitest.config.js",
  "vitest.config.mjs",
  "vitest.config.cts",
  "vitest.config.cjs",
  "vite.config.ts",
  "vite.config.mts",
  "vite.config.js",
  "vite.config.mjs",
];

function packageJson(root: string): {
  scripts?: Record<string, string>;
  dependencies?: Record<string, string>;
  devDependencies?: Record<string, string>;
  optionalDependencies?: Record<string, string>;
} {
  try {
    return JSON.parse(readFileSync(resolve(root, "package.json"), "utf8"));
  } catch {
    return {};
  }
}

function testUsesModule(directory: string, moduleName: string): boolean {
  if (!existsSync(directory)) return false;
  for (const entry of readdirSync(directory, { withFileTypes: true })) {
    if (
      entry.name === "node_modules" ||
      entry.name === "results" ||
      entry.name.startsWith(".")
    )
      continue;
    const path = resolve(directory, entry.name);
    if (entry.isDirectory()) {
      if (testUsesModule(path, moduleName)) return true;
      continue;
    }
    if (!/\.[cm]?[jt]sx?$/.test(entry.name)) continue;
    try {
      if (readFileSync(path, "utf8").includes(moduleName)) return true;
    } catch {
      // Discovery is best-effort; unreadable files are not test adapters.
    }
  }
  return false;
}

export function discoverCoverageProject(
  root = process.cwd(),
  environment: NodeJS.ProcessEnv = process.env,
): CoverageProject {
  const manifest = packageJson(root);
  const configuredSourceRoots = environment["SUPERCOV_SOURCE_ROOTS"]
    ?.split(",")
    .map((value) => value.trim())
    .filter(Boolean);
  const sourceRoots = (
    configuredSourceRoots?.length ? configuredSourceRoots : ["app", "src"]
  ).filter((directory) => existsSync(resolve(root, directory)));
  if (sourceRoots.length === 0) {
    throw new Error(
      "No application source root found. Set SUPERCOV_SOURCE_ROOTS=src,app.",
    );
  }

  const configuredPlaywright =
    environment["SUPERCOV_PLAYWRIGHT_CONFIG"] ??
    environment["TEST_PLAYWRIGHT_CONFIG"];
  const playwrightConfig = configuredPlaywright
    ? resolve(root, configuredPlaywright)
    : PLAYWRIGHT_CONFIG_CANDIDATES.map((candidate) =>
        resolve(root, candidate),
      ).find((candidate) => existsSync(candidate));
  const configuredVitest = environment["SUPERCOV_VITEST_CONFIG"];
  const vitestConfig = configuredVitest
    ? resolve(root, configuredVitest)
    : VITEST_CONFIG_CANDIDATES.map((candidate) =>
        resolve(root, candidate),
      ).find((candidate) => existsSync(candidate));

  const explicitModule = environment["SUPERCOV_PLAYWRIGHT_MODULE"];
  const essentialModule = "@essential-apps/shopify-test-admin";
  const playwrightModule =
    explicitModule ??
    (testUsesModule(resolve(root, "tests"), essentialModule)
      ? essentialModule
      : "@playwright/test");
  const essentialOffline = playwrightModule === essentialModule;

  if (!manifest.scripts?.["build"]) {
    throw new Error(
      "No package.json build script found; a build adapter is required to instrument application source.",
    );
  }
  const dependencies = {
    ...manifest.dependencies,
    ...manifest.devDependencies,
    ...manifest.optionalDependencies,
  };
  const hasVite =
    Boolean(dependencies["vite"]) ||
    [
      "vite.config.ts",
      "vite.config.mts",
      "vite.config.js",
      "vite.config.mjs",
    ].some((candidate) => existsSync(resolve(root, candidate)));
  if (!hasVite) {
    throw new Error(
      "The project is not a detected Vite build. A framework build adapter is required.",
    );
  }

  return {
    root,
    sourceRoots,
    ...(playwrightConfig ? { playwrightConfig } : {}),
    ...(vitestConfig ? { vitestConfig } : {}),
    playwrightModule,
    essentialOffline,
    buildCommand: ["npm", "run", "build"],
  };
}
