import { createHash } from "node:crypto";
import { spawnSync } from "node:child_process";
import {
  existsSync,
  readFileSync,
  readdirSync,
  statSync,
} from "node:fs";
import { relative, resolve, sep } from "node:path";
import type { CoverageProject } from "./project.ts";
import { instrumentationEngineIdentity } from "./engineInstrumenter.ts";
import type {
  CoverageRunFingerprint,
  CoverageRunIntegrity,
} from "./types.ts";

export const COVERAGE_REPORT_SCHEMA_VERSION = 2;
export const COVERAGE_INSTRUMENTER_VERSION = "2.0.0";

const SOURCE_PATTERN = /\.[cm]?[jt]sx?$/;
const TEST_PATTERN = /(?:^|[/_.-])(test|spec)(?:[/_.-]|$).*\.[cm]?[jt]sx?$/i;
const SKIPPED_DIRECTORIES = new Set([
  ".cache",
  ".git",
  ".mcdc-pool",
  ".next",
  ".nuxt",
  ".output",
  ".supercov",
  "build",
  "coverage",
  "dist",
  "node_modules",
  "out",
  "playwright-report",
  "results",
  "test-results",
  "vendor",
]);

function filesUnder(directory: string): string[] {
  if (!existsSync(directory) || !statSync(directory).isDirectory()) return [];
  return readdirSync(directory, { withFileTypes: true }).flatMap((entry) => {
    if (entry.isDirectory() && SKIPPED_DIRECTORIES.has(entry.name)) return [];
    const path = resolve(directory, entry.name);
    if (entry.isDirectory()) return filesUnder(path);
    return entry.isFile() ? [path] : [];
  });
}

function normalized(root: string, path: string): string {
  return relative(root, path).split(sep).join("/");
}

function digestFiles(root: string, paths: Iterable<string>): string {
  const hash = createHash("sha256");
  for (const path of [...new Set(paths)].sort()) {
    if (!existsSync(path) || !statSync(path).isFile()) continue;
    hash.update(normalized(root, path));
    hash.update("\0");
    hash.update(readFileSync(path));
    hash.update("\0");
  }
  return hash.digest("hex");
}

function git(root: string): CoverageRunIntegrity["git"] {
  const revision = spawnSync("git", ["rev-parse", "HEAD"], {
    cwd: root,
    encoding: "utf8",
  });
  const status = spawnSync("git", ["status", "--porcelain=v1"], {
    cwd: root,
    encoding: "utf8",
  });
  if (revision.status !== 0 && status.status !== 0) return undefined;
  return {
    ...(revision.status === 0
      ? { revision: revision.stdout.trim() }
      : {}),
    dirty: status.status !== 0 || status.stdout.trim().length > 0,
  };
}

export function createRunIntegrity(
  root: string,
  project: CoverageProject,
  toolSourceDirectory: string,
): CoverageRunIntegrity {
  const sourceFiles = project.sourceFiles.map((path) => resolve(root, path));
  const explicitTests = ["test", "tests", "__tests__"].flatMap((directory) =>
    filesUnder(resolve(root, directory)).filter((path) => SOURCE_PATTERN.test(path)),
  );
  const colocatedTests = filesUnder(root).filter((path) =>
    SOURCE_PATTERN.test(path) && TEST_PATTERN.test(normalized(root, path)),
  );
  const testFiles = [...new Set([...explicitTests, ...colocatedTests])];
  const dependencyFiles = [
    "package.json",
    "package-lock.json",
    "npm-shrinkwrap.json",
    "pnpm-lock.yaml",
    "yarn.lock",
    "bun.lock",
    "bun.lockb",
  ]
    .map((path) => resolve(root, path))
    .filter(existsSync);
  const configurationFiles = [
    "playwright.config.ts",
    "playwright.config.mts",
    "playwright.config.js",
    "playwright.config.mjs",
    "vitest.config.ts",
    "vitest.config.mts",
    "vitest.config.js",
    "vitest.config.mjs",
    "vite.config.ts",
    "vite.config.mts",
    "vite.config.js",
    "vite.config.mjs",
    "tsconfig.json",
    ".npmrc",
  ]
    .map((path) => resolve(root, path))
    .concat(
      [project.playwrightConfig, project.vitestConfig].filter(
        (value): value is string => Boolean(value),
      ),
    )
    .filter(existsSync);
  const instrumenterFiles = filesUnder(toolSourceDirectory).filter((path) =>
    /\.[cm]?[jt]s$/.test(path),
  );
  const executionFiles = [
    "atomic.js",
    "buildCache.js",
    "cli.js",
    "directInstrumenter.js",
    "engineInstrumenter.js",
    "esmInterceptor.js",
    "instrumenter.js",
    "launchSupervisor.js",
    "nodeTest.js",
    "nodeAssert.js",
    "nodeAssertAdapter.js",
    "nodeAssertStrict.js",
    "playwright.js",
    "playwrightReporter.js",
    "processDiagnostics.js",
    "project.js",
    "provenance.js",
    "queueAdapters.js",
    "register.mjs",
    "resolve-loader.mjs",
    "runnerEvidence.js",
    "runtime.js",
    "sourceDiscovery.js",
    "transport.js",
    "types.js",
    "vitePlugin.js",
    "vitest.js",
    "vitestReporter.js",
    "workspace.js",
  ]
    .map((path) => resolve(toolSourceDirectory, path))
    .filter(existsSync);
  const source = digestFiles(root, sourceFiles);
  const tests = digestFiles(root, testFiles);
  const dependencies = digestFiles(root, dependencyFiles);
  const configuration = digestFiles(root, configurationFiles);
  const instrumenterSource = digestFiles(toolSourceDirectory, instrumenterFiles);
  const engine = instrumentationEngineIdentity();
  const instrumenter = createHash("sha256")
    .update(JSON.stringify({ instrumenterSource, engine }))
    .digest("hex");
  const executionInstrumenter = digestFiles(
    toolSourceDirectory,
    executionFiles.length > 0 ? executionFiles : instrumenterFiles,
  );
  const execution = createHash("sha256")
    .update(
      JSON.stringify({
        schema: COVERAGE_REPORT_SCHEMA_VERSION,
        version: COVERAGE_INSTRUMENTER_VERSION,
        source,
        dependencies,
        configuration,
        buildEnvironment: project.buildEnvironment,
        executionInstrumenter,
        engine,
      }),
    )
    .digest("hex");
  const combined = createHash("sha256")
    .update(
      JSON.stringify({
        schema: COVERAGE_REPORT_SCHEMA_VERSION,
        version: COVERAGE_INSTRUMENTER_VERSION,
        source,
        tests,
        dependencies,
        configuration,
        instrumenter,
      }),
    )
    .digest("hex");
  const fingerprint: CoverageRunFingerprint = {
    algorithm: "sha256",
    source,
    tests,
    dependencies,
    configuration,
    instrumenter,
    execution,
    combined,
    sourceFiles: sourceFiles.length,
    testFiles: testFiles.length,
  };
  const gitState = git(root);
  return {
    schemaVersion: COVERAGE_REPORT_SCHEMA_VERSION,
    instrumenterVersion: COVERAGE_INSTRUMENTER_VERSION,
    ...(gitState ? { git: gitState } : {}),
    fingerprint,
  };
}

export function compareRunIntegrity(
  stored: CoverageRunIntegrity | undefined,
  current: CoverageRunIntegrity,
): { stale: boolean; reasons: string[] } {
  if (!stored) return { stale: true, reasons: ["run predates integrity fingerprints"] };
  const reasons: string[] = [];
  if (stored.schemaVersion !== current.schemaVersion)
    reasons.push("coverage schema changed");
  if (stored.fingerprint.instrumenter !== current.fingerprint.instrumenter)
    reasons.push("instrumenter changed");
  if (stored.fingerprint.source !== current.fingerprint.source)
    reasons.push("instrumented source changed");
  if (stored.fingerprint.tests !== current.fingerprint.tests)
    reasons.push("test files changed");
  if (stored.fingerprint.dependencies !== current.fingerprint.dependencies)
    reasons.push("dependencies or lockfile changed");
  if (stored.fingerprint.configuration !== current.fingerprint.configuration)
    reasons.push("test/build configuration changed");
  return { stale: reasons.length > 0, reasons };
}
