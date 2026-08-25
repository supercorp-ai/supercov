import { createHash } from "node:crypto";
import {
  existsSync,
  readFileSync,
  readdirSync,
  statSync,
} from "node:fs";
import { dirname, relative, resolve, sep } from "node:path";
import type {
  CoverageLimitation,
  CoverageSourceScope,
  CoverageSourceScopeEntry,
} from "./types.ts";

const SOURCE_PATTERN = /\.[cm]?[jt]sx?$/i;
const DECLARATION_PATTERN = /\.d\.[cm]?ts$/i;
const TEST_FILE_PATTERN = /(?:^|[/_.-])(?:test|spec)(?:[/_.-]|$)/i;
const TEST_DIRECTORY_PATTERN = /(?:^|\/)(?:__tests__|test|tests|spec|specs|e2e|fixtures?|mocks?|__mocks__)(?:\/|$)/i;
const TOOL_DIRECTORY_PATTERN = /(?:^|\/)(?:scripts)(?:\/|$)/i;
// Known tool configs are excluded anywhere; at the project root, any
// "<tool>.config.*" follows the same convention (tsdown, lint-staged, ...)
// and is tool configuration, not application source.
const CONFIG_PATTERN = /(?:^|\/)(?:(?:babel|eslint|graphql|jest|next|nuxt|playwright|postcss|prettier|remix|rollup|stylelint|tailwind|tsup|vite|vitest|webpack)\.config\.[cm]?[jt]s|\.(?:babel|eslint|graphql|prettier|stylelint)rc\.[cm]?[jt]s)$|^[^/]+\.config\.[cm]?[jt]s$/i;
const ROOT_BUILD_SCRIPT_PATTERN = /^(?:build|gulpfile|gruntfile)\.[cm]?[jt]sx?$/i;
const GENERATED_DIRECTORIES = new Map<string, string>([
  [".cache", "generated tool cache"],
  [".git", "version-control metadata"],
  [".next", "generated Next.js output"],
  [".nuxt", "generated Nuxt output"],
  [".output", "generated framework output"],
  [".supercov", "Supercov-owned data"],
  ["build", "generated build output"],
  ["coverage", "generated coverage output"],
  ["dist", "generated distribution output"],
  ["node_modules", "third-party dependencies"],
  ["out", "generated compiler output"],
  ["playwright-report", "generated test output"],
  ["results", "generated test output"],
  ["test-results", "generated test output"],
  ["vendor", "vendored dependencies"],
]);
const CONVENTIONAL_SOURCE_DIRECTORIES = [
  "app",
  "src",
  "lib",
  "server",
  "client",
  "functions",
  "api",
];
const PACKAGE_PARENT_DIRECTORIES = new Set([
  "apps",
  "packages",
  "services",
  "workspaces",
]);

interface PackageManifest {
  main?: unknown;
  module?: unknown;
  browser?: unknown;
  bin?: unknown;
  exports?: unknown;
  workspaces?: unknown;
}

export interface DiscoveredSourceScope {
  sourceFiles: string[];
  sourceRoots: string[];
  scope: CoverageSourceScope;
  limitations: CoverageLimitation[];
}

function normalized(root: string, path: string): string {
  const local = relative(root, path).split(sep).join("/");
  return local || ".";
}

function readManifest(directory: string): PackageManifest | undefined {
  try {
    return JSON.parse(readFileSync(resolve(directory, "package.json"), "utf8")) as PackageManifest;
  } catch {
    return undefined;
  }
}

function filesUnder(root: string, directory = root): string[] {
  if (!existsSync(directory) || !statSync(directory).isDirectory()) return [];
  return readdirSync(directory, { withFileTypes: true }).flatMap((entry) => {
    if (entry.isDirectory() && GENERATED_DIRECTORIES.has(entry.name)) return [];
    const path = resolve(directory, entry.name);
    if (entry.isDirectory()) return filesUnder(root, path);
    return entry.isFile() && SOURCE_PATTERN.test(entry.name) ? [path] : [];
  });
}

function packageDirectories(root: string): string[] {
  const found = new Set<string>([root]);
  const visit = (directory: string, depth: number): void => {
    if (depth > 5 || !existsSync(directory)) return;
    for (const entry of readdirSync(directory, { withFileTypes: true })) {
      if (!entry.isDirectory() || GENERATED_DIRECTORIES.has(entry.name)) continue;
      const path = resolve(directory, entry.name);
      const local = normalized(root, path);
      const segments = local.split("/");
      if (
        existsSync(resolve(path, "package.json")) &&
        (depth === 0 || segments.some((segment) => PACKAGE_PARENT_DIRECTORIES.has(segment)))
      ) found.add(path);
      visit(path, depth + 1);
    }
  };
  visit(root, 0);
  return [...found].sort();
}

function stringTargets(value: unknown, depth = 0): string[] {
  if (depth > 8) return [];
  if (typeof value === "string") return [value];
  if (Array.isArray(value))
    return value.flatMap((entry) => stringTargets(entry, depth + 1));
  if (!value || typeof value !== "object") return [];
  return Object.values(value).flatMap((entry) => stringTargets(entry, depth + 1));
}

function entryTargets(directory: string, manifest: PackageManifest): string[] {
  const targets = [
    ...stringTargets(manifest.main),
    ...stringTargets(manifest.module),
    ...stringTargets(manifest.browser),
    ...stringTargets(manifest.bin),
    ...stringTargets(manifest.exports),
  ];
  return targets.flatMap((target) => {
    if (!target.startsWith(".") || target.includes("node_modules")) return [];
    const withoutPattern = target.split("*")[0]?.replace(/\/$/, "") ?? "";
    if (!withoutPattern) return [];
    return [resolve(directory, withoutPattern)];
  });
}

function tsconfigRoots(directory: string): string[] {
  const path = resolve(directory, "tsconfig.json");
  if (!existsSync(path)) return [];
  let contents: string;
  try {
    contents = readFileSync(path, "utf8")
      .replace(/\/\*[\s\S]*?\*\//g, "")
      .replace(/^\s*\/\/.*$/gm, "")
      .replace(/,\s*([}\]])/g, "$1");
    const config = JSON.parse(contents) as {
      compilerOptions?: { rootDir?: unknown };
      include?: unknown;
    };
    const values = [
      config.compilerOptions?.rootDir,
      ...(Array.isArray(config.include) ? config.include : []),
    ];
    // TypeScript's default include is the directory containing tsconfig.json.
    // Mirroring that default lets root-level libraries be discovered without
    // guessing from filenames or requiring a conventional src directory.
    if (!values.some((value) => typeof value === "string" && value.length > 0))
      return [directory];
    return values.flatMap((value) => {
      if (typeof value !== "string" || value.startsWith("!")) return [];
      const prefix = value.split(/[?*{[]/, 1)[0]?.replace(/\/$/, "") ?? "";
      return prefix ? [resolve(directory, prefix)] : [];
    });
  } catch {
    return [];
  }
}

function within(parent: string, child: string): boolean {
  const local = relative(parent, child);
  return local === "" || (!local.startsWith("..") && !local.startsWith(`..${sep}`));
}

function nearestPackageRoot(path: string, packages: string[]): string | undefined {
  return [...packages]
    .filter((directory) => within(directory, path))
    .sort((left, right) => right.length - left.length)[0];
}

function scopeLimitation(file: string): CoverageLimitation {
  return {
    id: `scope:${createHash("sha256").update(file).digest("hex").slice(0, 20)}`,
    kind: "source-scope",
    file,
    line: 1,
    column: 1,
    source: file,
    reason:
      "First-party JavaScript/TypeScript source could not be classified automatically. Configure SUPERCOV_SOURCE_ROOTS or move it under a discovered package source root.",
  };
}

export function discoverSourceScope(
  root: string,
  configuredRoots?: string[],
): DiscoveredSourceScope {
  const packages = packageDirectories(root);
  const explicit = Boolean(configuredRoots?.length);
  const includeRoots = explicit
    ? configuredRoots!.map((directory) => resolve(root, directory))
    : packages.flatMap((directory) => {
        const manifest = readManifest(directory) ?? {};
        return [
          ...CONVENTIONAL_SOURCE_DIRECTORIES.map((name) => resolve(directory, name)),
          ...entryTargets(directory, manifest),
          ...tsconfigRoots(directory),
        ];
      });
  const existingRoots = [...new Set(includeRoots)]
    .filter(existsSync)
    .sort();
  const allFiles = filesUnder(root);
  const entries: CoverageSourceScopeEntry[] = [];
  const included: string[] = [];
  const limitations: CoverageLimitation[] = [];

  for (const path of allFiles.sort()) {
    const file = normalized(root, path);
    const packageRoot = nearestPackageRoot(path, packages);
    const packageName = packageRoot ? normalized(root, packageRoot) : undefined;
    const withPackage = packageName && packageName !== "."
      ? { packageRoot: packageName }
      : {};
    if (DECLARATION_PATTERN.test(file)) {
      entries.push({ file, status: "excluded", reason: "TypeScript declaration", ...withPackage });
      continue;
    }
    if (TEST_DIRECTORY_PATTERN.test(file) || TEST_FILE_PATTERN.test(file)) {
      entries.push({ file, status: "excluded", reason: "test or fixture source", ...withPackage });
      continue;
    }
    if (TOOL_DIRECTORY_PATTERN.test(file)) {
      entries.push({ file, status: "excluded", reason: "conventional tool script", ...withPackage });
      continue;
    }
    if (CONFIG_PATTERN.test(file) || ROOT_BUILD_SCRIPT_PATTERN.test(file)) {
      entries.push({ file, status: "excluded", reason: "build/test/tool configuration", ...withPackage });
      continue;
    }
    const matchedRoot = existingRoots.find((directory) =>
      statSync(directory).isDirectory() ? within(directory, path) : directory === path,
    );
    if (matchedRoot) {
      included.push(path);
      entries.push({
        file,
        status: "included",
        reason: explicit ? "explicit source root" : "discovered package source root",
        ...withPackage,
      });
      continue;
    }
    if (explicit) {
      entries.push({ file, status: "excluded", reason: "outside explicit source roots", ...withPackage });
      continue;
    }
    entries.push({ file, status: "ambiguous", reason: "unclassified first-party source", ...withPackage });
    limitations.push(scopeLimitation(file));
  }

  return {
    sourceFiles: included.map((path) => normalized(root, path)),
    sourceRoots: existingRoots.map((path) => normalized(root, path)),
    scope: {
      version: 1,
      mode: explicit ? "explicit" : "automatic",
      roots: existingRoots.map((path) => normalized(root, path)),
      entries,
    },
    limitations,
  };
}
