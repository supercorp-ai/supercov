import { existsSync, readFileSync, readdirSync } from "node:fs";
import { basename, resolve } from "node:path";

export interface CoverageProject {
  root: string;
  sourceRoots: string[];
  playwrightConfig?: string;
  vitestConfig?: string;
  playwrightModule: string;
  playwrightTestExport: string;
  playwrightExports: string[];
  buildAdapter: "vite" | "direct";
  buildCommand: string[];
  buildEnvironment: Record<string, string>;
}

const PLAYWRIGHT_CONFIG_CANDIDATES = [
  "playwright.config.ts",
  "playwright.config.mts",
  "playwright.config.js",
  "playwright.config.mjs",
  "playwright.config.cts",
  "playwright.config.cjs",
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

interface TestApiCandidate {
  module: string;
  score: number;
  testExport?: string;
  exports: string[];
}

function importedTestApis(contents: string): TestApiCandidate[] {
  const candidates: TestApiCandidate[] = [];
  const imports = contents.matchAll(
    /import\s+(?!type\b)\{([\s\S]*?)\}\s*from\s*["']([^"']+)["']/g,
  );
  for (const match of imports) {
    const bindings = match[1] ?? "";
    const module = match[2];
    if (!module) continue;
    let score = 0;
    let testExport: string | undefined;
    const exports: string[] = [];
    for (const rawBinding of bindings.split(",")) {
      const trimmed = rawBinding.trim();
      if (trimmed.startsWith("type ")) continue;
      const binding = trimmed;
      if (!binding) continue;
      const [imported = "", local = imported] = binding
        .split(/\s+as\s+/)
        .map((value) => value.trim());
      if (/^[A-Za-z_$][\w$]*$/.test(imported)) exports.push(imported);
      if (local === "test") {
        score += 20;
        testExport = imported;
      }
      else if (/test$/i.test(imported)) score += 8;
      if (local === "expect" || imported === "expect") score += 10;
    }
    if (score > 0) {
      if (module === "@playwright/test") score += 100;
      else if (/playwright/i.test(module)) score += 5;
      candidates.push({
        module,
        score,
        ...(testExport ? { testExport } : {}),
        exports,
      });
    }
  }
  return candidates;
}

function testApiCandidates(directory: string): TestApiCandidate[] {
  if (!existsSync(directory)) return [];
  const candidates: TestApiCandidate[] = [];
  for (const entry of readdirSync(directory, { withFileTypes: true })) {
    if (
      entry.name === "node_modules" ||
      entry.name === "results" ||
      entry.name.startsWith(".")
    )
      continue;
    const path = resolve(directory, entry.name);
    if (entry.isDirectory()) {
      candidates.push(...testApiCandidates(path));
      continue;
    }
    if (!/\.[cm]?[jt]sx?$/.test(entry.name)) continue;
    try {
      candidates.push(...importedTestApis(readFileSync(path, "utf8")));
    } catch {
      // Discovery is best-effort; unreadable files cannot contribute a test API.
    }
  }
  return candidates;
}

interface PlaywrightAdapter {
  module: string;
  testExport: string;
  exports: string[];
}

function discoverPlaywrightAdapter(root: string): PlaywrightAdapter {
  const candidates: TestApiCandidate[] = [];
  for (const directory of ["test", "tests", "e2e", "spec", "specs"]) {
    candidates.push(...testApiCandidates(resolve(root, directory)));
  }
  const scores = new Map<string, number>();
  for (const candidate of candidates)
    scores.set(candidate.module, (scores.get(candidate.module) ?? 0) + candidate.score);
  const module =
    [...scores.entries()].sort(
      ([leftModule, leftScore], [rightModule, rightScore]) =>
        rightScore - leftScore || leftModule.localeCompare(rightModule),
    )[0]?.[0] ?? "@playwright/test";
  const matching = candidates.filter((candidate) => candidate.module === module);
  const testExport =
    matching
      .filter((candidate) => candidate.testExport)
      .sort((left, right) => right.score - left.score)[0]?.testExport ?? "test";
  return {
    module,
    testExport,
    exports: [...new Set(matching.flatMap((candidate) => candidate.exports))].sort(),
  };
}

function nestedPlaywrightConfigs(root: string): string[] {
  const found: string[] = [];
  const visit = (directory: string, depth: number): void => {
    if (!existsSync(directory) || depth > 4) return;
    for (const entry of readdirSync(directory, { withFileTypes: true })) {
      if (entry.name.startsWith(".") || entry.name === "node_modules") continue;
      const path = resolve(directory, entry.name);
      if (entry.isDirectory()) visit(path, depth + 1);
      else if (/^playwright(?:\.[\w-]+)?\.config\.[cm]?[jt]s$/.test(entry.name))
        found.push(path);
    }
  };
  for (const directory of ["test", "tests", "e2e"])
    visit(resolve(root, directory), 0);
  return found.sort();
}

const GENERIC_COMMAND_TERMS = new Set([
  "bin",
  "bun",
  "exec",
  "node",
  "npm",
  "pnpm",
  "run",
  "script",
  "test",
  "tests",
  "yarn",
]);

function words(value: string): Set<string> {
  return new Set(
    value
      .toLowerCase()
      .split(/[^a-z0-9]+/)
      .filter((word) => word.length > 1 && !GENERIC_COMMAND_TERMS.has(word)),
  );
}

function expandedCommand(root: string, command: string[]): string {
  const executable = basename(command[0] ?? "").replace(/\.(?:cmd|exe)$/i, "");
  const runIndex = command.findIndex((argument) => argument === "run");
  const shorthandScript =
    ["npm", "pnpm", "yarn", "bun"].includes(executable) && runIndex < 0
      ? command[1]
      : undefined;
  const scriptName =
    runIndex >= 0 ? command[runIndex + 1] : shorthandScript;
  if (
    ["npm", "pnpm", "yarn", "bun"].includes(executable) &&
    scriptName
  ) {
    const script = packageJson(root).scripts?.[scriptName];
    return `${command.join(" ")} ${script ?? ""}`;
  }
  return command.join(" ");
}

function referencedBuildEnvironment(root: string): Map<string, string> {
  const values = new Map<string, string>();
  const configs = readdirSync(root, { withFileTypes: true })
    .filter(
      (entry) =>
        entry.isFile() &&
        /^(?:vite|webpack|rollup|remix|next|nuxt)\.config\.[cm]?[jt]s$/.test(
          entry.name,
        ),
    )
    .map((entry) => resolve(root, entry.name));
  for (const config of configs) {
    let contents: string;
    try {
      contents = readFileSync(config, "utf8");
    } catch {
      continue;
    }
    const references = contents.matchAll(
      /process\.env(?:\.([A-Z][A-Z0-9_]*)|\[['"]([A-Z][A-Z0-9_]*)['"]\])\s*={2,3}\s*(['"])([^'"]+)\3/g,
    );
    for (const match of references) {
      const name = match[1] ?? match[2];
      if (name && match[4]) values.set(name, match[4]);
    }
  }
  return values;
}

/**
 * Infer build-only mode flags when a test command's semantic mode matches a
 * flag the project's own build config reads. For example, a `test:preview`
 * command and `process.env.TEST_PREVIEW === "true"` are connected without
 * knowing the framework or runner that owns either convention.
 */
function inferBuildEnvironment(
  root: string,
  command: string[],
  environment: NodeJS.ProcessEnv,
): Record<string, string> {
  const commandWords = words(expandedCommand(root, command));
  if (commandWords.size === 0) return {};
  const inferred: Record<string, string> = {};
  for (const [name, activeValue] of referencedBuildEnvironment(root)) {
    if (environment[name] !== undefined) continue;
    const flagWords = words(name);
    if ([...flagWords].some((word) => commandWords.has(word)))
      inferred[name] = activeValue;
  }
  return inferred;
}

export function discoverCoverageProject(
  root = process.cwd(),
  environment: NodeJS.ProcessEnv = process.env,
  command: string[] = [],
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
    environment["SUPERCOV_PLAYWRIGHT_CONFIG"];
  const playwrightConfig = configuredPlaywright
    ? resolve(root, configuredPlaywright)
    : [
        ...PLAYWRIGHT_CONFIG_CANDIDATES.map((candidate) => resolve(root, candidate)),
        ...nestedPlaywrightConfigs(root),
      ].find((candidate) => existsSync(candidate));
  const configuredVitest = environment["SUPERCOV_VITEST_CONFIG"];
  const vitestConfig = configuredVitest
    ? resolve(root, configuredVitest)
    : VITEST_CONFIG_CANDIDATES.map((candidate) =>
        resolve(root, candidate),
      ).find((candidate) => existsSync(candidate));

  const discoveredPlaywright = discoverPlaywrightAdapter(root);
  const playwrightModule =
    environment["SUPERCOV_PLAYWRIGHT_MODULE"] ?? discoveredPlaywright.module;
  const playwrightTestExport =
    environment["SUPERCOV_PLAYWRIGHT_TEST_EXPORT"] ??
    (playwrightModule === discoveredPlaywright.module
      ? discoveredPlaywright.testExport
      : "test");

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
  const buildCommand = manifest.scripts?.["build"]
    ? ["npm", "run", "build"]
    : [];
  if (buildCommand.length > 0 && !hasVite) {
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
    playwrightTestExport,
    playwrightExports:
      playwrightModule === discoveredPlaywright.module
        ? discoveredPlaywright.exports
        : [playwrightTestExport, "expect"],
    buildAdapter: buildCommand.length > 0 ? "vite" : "direct",
    buildCommand,
    buildEnvironment: inferBuildEnvironment(root, command, environment),
  };
}
