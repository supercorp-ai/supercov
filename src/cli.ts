import { spawn, type ChildProcess } from "node:child_process";
import { createRequire } from "node:module";
import {
  copyFileSync,
  existsSync,
  mkdirSync,
  readFileSync,
} from "node:fs";
import { relative, resolve, sep } from "node:path";
import { performance } from "node:perf_hooks";
import { fileURLToPath, pathToFileURL } from "node:url";
import { atomicRenameSync, atomicWriteFileSync } from "./atomic.ts";
import { coverageQueryCommands, runQueryCommand } from "./query.ts";
import { discoverCoverageProject, expandedCommand } from "./project.ts";
import { createRunIntegrity } from "./integrity.ts";
import { writeEngineEvidenceArchive } from "./engineEvidence.ts";
import { mergeCoverageRuns } from "./merge.ts";
import {
  buildCacheReusePaths,
  instrumentedBuildCacheKey,
  readInstrumentedBuildCache,
  writeInstrumentedBuildCache,
} from "./buildCache.ts";
import { instrumentDirectWorkspace } from "./directInstrumenter.ts";
import { instrumentNodeAssertionsInWorkspace } from "./nodeAssertionInstrumenter.ts";
import {
  acquireProjectLock,
  cachedWorkspacePath,
  cleanCoverageStorage,
  finalizePublishedRunStorage,
  prepareCachedWorkspace,
  pruneCachedWorkspaceSources,
  pruneCoverageStorage,
  recoverAbandonedRuns,
  removeStoredTreeDeferred,
  spawnTrashDeleter,
  updateRunState,
  writeRunState,
} from "./workspace.ts";
import { agentFailureJson, SupercovError } from "./agentJson.ts";
import { isolateCollectorRuntime } from "./runtimeIsolation.ts";
import {
  COMMAND_TERMINATION_GRACE_MS,
  COMMAND_TIMEOUT_EXIT_CODE,
  DEFAULT_DIAGNOSTIC_INTERVAL_MS,
  positiveMilliseconds,
  startProcessWatchdog,
} from "./processDiagnostics.ts";

interface ChildResult {
  status: number | null;
  signal: NodeJS.Signals | null;
  error?: Error;
  timedOut?: boolean;
}

interface RunPhaseTimings {
  initializationMs: number;
  workspacePreparationMs: number;
  adapterSetupMs: number;
  instrumentedBuildMs: number;
  testCommandMs: number;
  evidencePublicationMs: number;
}

function roundedTimings(timings: RunPhaseTimings): RunPhaseTimings {
  return Object.fromEntries(
    Object.entries(timings).map(([phase, duration]) => [
      phase,
      Math.round(duration * 10) / 10,
    ]),
  ) as unknown as RunPhaseTimings;
}

function formatTimings(timings: RunPhaseTimings, totalMs: number): string {
  const rounded = roundedTimings(timings);
  return [
    `initialization=${rounded.initializationMs}ms`,
    `workspace=${rounded.workspacePreparationMs}ms`,
    `setup=${rounded.adapterSetupMs}ms`,
    `build=${rounded.instrumentedBuildMs}ms`,
    `tests=${rounded.testCommandMs}ms`,
    `evidence=${rounded.evidencePublicationMs}ms`,
    `total=${Math.round(totalMs * 10) / 10}ms`,
  ].join(" ");
}

let activeChild: ChildProcess | undefined;
let signalEscalation: NodeJS.Timeout | undefined;

function terminateChild(signal: NodeJS.Signals): void {
  const child = activeChild;
  if (!child?.pid) return;
  try {
    if (process.platform !== "win32") process.kill(-child.pid, signal);
    else child.kill(signal);
  } catch {
    try {
      child.kill(signal);
    } catch {
      // The child may have exited between the signal and cleanup.
    }
  }
}

function runChild(
  command: string,
  args: string[],
  options: { cwd: string; env: NodeJS.ProcessEnv },
): Promise<ChildResult> {
  const diagnosticIntervalMs = positiveMilliseconds(
    options.env["SUPERCOV_DIAGNOSTIC_INTERVAL_MS"],
    "SUPERCOV_DIAGNOSTIC_INTERVAL_MS",
  ) ?? DEFAULT_DIAGNOSTIC_INTERVAL_MS;
  const commandTimeoutMs = positiveMilliseconds(
    options.env["SUPERCOV_COMMAND_TIMEOUT_MS"],
    "SUPERCOV_COMMAND_TIMEOUT_MS",
  );
  return new Promise((resolveChild) => {
    let error: Error | undefined;
    let timedOut = false;
    let timeoutEscalation: NodeJS.Timeout | undefined;
    const child = spawn(command, args, {
      cwd: options.cwd,
      env: options.env,
      stdio: "inherit",
      detached: process.platform !== "win32",
    });
    activeChild = child;
    const watchdog = startProcessWatchdog(child, {
      diagnosticIntervalMs,
      ...(commandTimeoutMs === undefined ? {} : { timeoutMs: commandTimeoutMs }),
      write(message) {
        console.error(message);
      },
      onTimeout() {
        if (timedOut) return;
        timedOut = true;
        console.error(
          `[supercov] command exceeded SUPERCOV_COMMAND_TIMEOUT_MS=${commandTimeoutMs}; terminating process group`,
        );
        terminateChild("SIGTERM");
        timeoutEscalation = setTimeout(
          () => terminateChild("SIGKILL"),
          COMMAND_TERMINATION_GRACE_MS,
        );
        timeoutEscalation.unref();
      },
    });
    child.once("error", (failure) => {
      error = failure;
    });
    child.once("close", (status, signal) => {
      watchdog.stop();
      if (timeoutEscalation) clearTimeout(timeoutEscalation);
      if (activeChild === child) activeChild = undefined;
      resolveChild({
        status,
        signal,
        ...(error ? { error } : {}),
        ...(timedOut ? { timedOut: true } : {}),
      });
    });
  });
}

function exitCode(result?: ChildResult): number {
  if (!result) return 0;
  if (result.timedOut) return COMMAND_TIMEOUT_EXIT_CODE;
  if (result.error) return 1;
  if (result.status !== null) return result.status;
  return result.signal ? 128 : 1;
}

function signalExitCode(signal: NodeJS.Signals): number {
  if (signal === "SIGHUP") return 129;
  if (signal === "SIGINT") return 130;
  if (signal === "SIGTERM") return 143;
  return 128;
}

function parseRetentionOptions(
  command: "clean" | "prune",
  args: string[],
): { keep: number; dryRun: boolean } {
  let keep = 20;
  let dryRun = false;
  for (let index = 0; index < args.length; index += 1) {
    const argument = args[index]!;
    if (argument === "--dry-run") dryRun = true;
    else if (argument === "--keep") {
      const value = Number(args[++index]);
      if (!Number.isSafeInteger(value) || value < 0)
        throw new Error("--keep must be a non-negative integer");
      keep = value;
    } else throw new Error(`Unknown ${command} option: ${argument}`);
  }
  return { keep, dryRun };
}

function cleanCommand(args: string[]): void {
  const options = parseRetentionOptions("clean", args);
  const result = cleanCoverageStorage(process.cwd(), options);
  console.log(
    `[supercov] ${options.dryRun ? "would remove" : "removed"} ${result.removedRuns.length} stored run(s), ${result.removedWorkspaces.length} per-run workspace(s), and ${result.removedBuildCache ? "the" : "no"} isolated build cache; keeping ${options.keep} newest run(s)`,
  );
  for (const id of result.removedRuns) console.log(id);
}

function pruneCommand(args: string[]): void {
  const options = parseRetentionOptions("prune", args);
  const result = pruneCoverageStorage(process.cwd(), options);
  console.log(
    `[supercov] ${options.dryRun ? "would remove" : "removed"} ${result.removedRuns.length} stored run(s), ${result.removedWorkspaces.length} terminal/orphan work director${result.removedWorkspaces.length === 1 ? "y" : "ies"}, and ${result.removedEvidence.length} loose evidence director${result.removedEvidence.length === 1 ? "y" : "ies"}; keeping ${options.keep} newest run(s) and preserving the shared cache`,
  );
  for (const id of result.removedRuns) console.log(id);
}

async function createCoverageRun(command: string[]): Promise<number> {
  const root = process.cwd();
  const runId = new Date().toISOString().replace(/[:.]/g, "-");
  const runStartedAt = Date.now();
  const runStartedMonotonic = performance.now();
  const timings: RunPhaseTimings = {
    initializationMs: 0,
    workspacePreparationMs: 0,
    adapterSetupMs: 0,
    instrumentedBuildMs: 0,
    testCommandMs: 0,
    evidencePublicationMs: 0,
  };
  const recovered = recoverAbandonedRuns(root);
  if (recovered.length > 0)
    console.error(`[supercov] recovered abandoned run(s): ${recovered.join(", ")}`);
  const lock = acquireProjectLock(root, runId);
  // The run store is derived local state; keep it out of the user's diff
  // without asking them to edit their own .gitignore.
  const storeIgnorePath = resolve(root, ".supercov/.gitignore");
  if (!existsSync(storeIgnorePath)) atomicWriteFileSync(storeIgnorePath, "*\n");
  const project = discoverCoverageProject(root, process.env, command);
  const packageSource = fileURLToPath(new URL(".", import.meta.url));
  const runIntegrity = createRunIntegrity(root, project, packageSource);
  const workspace = cachedWorkspacePath(root);
  const buildCacheKey = instrumentedBuildCacheKey(runIntegrity, project);
  const reusableBuild = project.buildAdapter !== "direct"
    ? readInstrumentedBuildCache(workspace, buildCacheKey)
    : undefined;
  const serverEvidenceRoot = resolve(workspace, ".supercov/server-evidence");
  const runStagingDirectory = resolve(
    root,
    ".supercov/work",
    runId,
    "run-publication",
  );
  const storedRunDirectory = resolve(root, ".supercov/runs", runId);
  const startedAt = new Date(runStartedAt).toISOString();
  writeRunState(root, runId, {
    id: runId,
    pid: process.pid,
    root,
    workspace,
    startedAt,
    status: "preparing",
  });

  let receivedSignal: NodeJS.Signals | undefined;
  const signalHandlers = new Map<NodeJS.Signals, () => void>();
  for (const signal of ["SIGINT", "SIGTERM", "SIGHUP"] as const) {
    const handler = (): void => {
      if (receivedSignal) return;
      receivedSignal = signal;
      try {
        updateRunState(root, runId, { status: "interrupted", signal });
      } catch {
        // State recovery on the next invocation remains the fallback.
      }
      terminateChild(signal);
      signalEscalation = setTimeout(
        () => terminateChild("SIGKILL"),
        COMMAND_TERMINATION_GRACE_MS,
      );
      signalEscalation.unref();
    };
    signalHandlers.set(signal, handler);
    process.once(signal, handler);
  }

  let buildResult: ChildResult | undefined;
  let testResult: ChildResult | undefined;
  let publicationFailed = false;
  let runPublished = false;
  let timingsPrinted = false;

  timings.initializationMs = performance.now() - runStartedMonotonic;

  try {
    let phaseStarted = performance.now();
    const isolatedRoot = prepareCachedWorkspace(root, {
      ...(reusableBuild
        ? { reusePaths: buildCacheReusePaths(reusableBuild) }
        : {}),
    });
    timings.workspacePreparationMs = performance.now() - phaseStarted;
    if (receivedSignal) throw new Error(`Interrupted by ${receivedSignal}`);
    phaseStarted = performance.now();
    const generatedDirectory = resolve(isolatedRoot, ".supercov");
    const evidenceDirectoryRelative = `.supercov/evidence/${runId}`;
    const isolatedEvidenceDirectory = resolve(isolatedRoot, evidenceDirectoryRelative);
    const persistedEvidenceDirectory = resolve(root, ".supercov/evidence", runId);
    const generatedPlaywrightConfig = resolve(generatedDirectory, "playwright.config.mjs");
    const generatedViteConfig = resolve(generatedDirectory, "vite.config.mjs");
    const generatedVitestConfig = resolve(generatedDirectory, "vitest.config.mjs");
    let installedJestMajor: number | undefined;
    try {
      const installed = JSON.parse(
        readFileSync(resolve(root, "node_modules/jest/package.json"), "utf8"),
      ) as { version?: string };
      installedJestMajor = Number.parseInt(installed.version?.split(".")[0] ?? "", 10);
    } catch {
      // Yarn PnP and custom launchers may not expose a physical Jest package.
    }
    const usesLegacyJestConfig =
      installedJestMajor !== undefined && installedJestMajor < 28;
    const generatedJestConfig = usesLegacyJestConfig
      ? resolve(generatedDirectory, "jest-cjs/config.js")
      : resolve(generatedDirectory, "jest.config.mjs");
    const manifestPath = resolve(generatedDirectory, "manifest.json");
    const buildOutputMetadataPath = resolve(
      generatedDirectory,
      "build-outputs.json",
    );
    const isolatedPlaywrightConfig = project.playwrightConfig
      ? resolve(isolatedRoot, relative(root, project.playwrightConfig))
      : undefined;
    const isolatedVitestConfig = project.vitestConfig
      ? resolve(isolatedRoot, relative(root, project.vitestConfig))
      : undefined;
    const isolatedJestConfig = project.jestConfig
      ? resolve(isolatedRoot, relative(root, project.jestConfig))
      : undefined;

    mkdirSync(generatedDirectory, { recursive: true });
    mkdirSync(isolatedEvidenceDirectory, { recursive: true });
    atomicWriteFileSync(
      resolve(generatedDirectory, "package.json"),
      `${JSON.stringify({ private: true, type: "module" })}\n`,
    );
    if (usesLegacyJestConfig) {
      mkdirSync(resolve(generatedDirectory, "jest-cjs"), { recursive: true });
      atomicWriteFileSync(
        resolve(generatedDirectory, "jest-cjs/package.json"),
        `${JSON.stringify({ private: true, type: "commonjs" })}\n`,
      );
    }
    for (const file of [
      "atomic.js",
      "launchSupervisor.js",
      "nodeAssert.js",
      "nodeAssertAdapter.js",
      "nodeAssertStrict.js",
      "nodeTest.js",
      "playwright.js",
      "playwrightReporter.js",
      "provenance.js",
      "register.mjs",
      "resolve-loader.mjs",
      "runtime.js",
      "runnerEvidence.js",
      "transport.js",
      "types.js",
    ]) {
      copyFileSync(resolve(packageSource, file), resolve(generatedDirectory, file));
    }
    const generatedRuntime = resolve(generatedDirectory, "runtime.js");
    atomicWriteFileSync(
      generatedRuntime,
      isolateCollectorRuntime(
        readFileSync(generatedRuntime, "utf8"),
        `collector-${buildCacheKey}`,
      ),
    );
    atomicWriteFileSync(
      resolve(generatedDirectory, "runtime.d.ts"),
      `${[
        "coverageHit",
        "selectionBegin",
        "selectionRight",
        "selectionEnd",
        "optionalSelect",
        "optionalCallBegin",
        "optionalCallReached",
        "optionalCallContinued",
        "optionalCallEnd",
        "defaultSelected",
        "defaultEntered",
        "tryBegin",
        "tryCatch",
        "tryEnd",
        "loopBegin",
        "loopEntered",
        "loopEnd",
        "mcdcBegin",
        "mcdcCondition",
        "mcdcEnd",
        "registerProbeV2",
        "coverageHitV2",
        "mcdcEndV2",
      ].map((name) => `export declare function ${name}(...args: any[]): any;`).join("\n")}\n`,
    );
    const generatedPlaywrightAdapter = resolve(generatedDirectory, "playwright.js");
    const generatedPlaywrightReporter = resolve(generatedDirectory, "playwrightReporter.js");
    atomicWriteFileSync(
      generatedPlaywrightAdapter,
      readFileSync(generatedPlaywrightAdapter, "utf8")
        .replace("__SUPERCOV_EVIDENCE_DIRECTORY__", evidenceDirectoryRelative)
        .replace("__SUPERCOV_PLAYWRIGHT_MODULE__", project.playwrightModule)
        .replace(
          "__SUPERCOV_PLAYWRIGHT_TEST_EXPORT__",
          project.playwrightTestExport,
        )
        .replace(
          "/*__SUPERCOV_ADAPTER_EXPORTS__*/",
          [
            ...(project.playwrightTestExport === "test"
              ? []
              : [
                  `export { instrumentedTest as ${project.playwrightTestExport} };`,
                ]),
            ...project.playwrightExports
              .filter(
                (name) =>
                  name !== "test" &&
                  name !== "expect" &&
                  name !== project.playwrightTestExport,
              )
              .map(
                (name) =>
                  `export const ${name} = adapter[${JSON.stringify(name)}];`,
              ),
          ].join("\n"),
        )
        .replace("__SUPERCOV_RUN_ID__", runId),
    );
    atomicWriteFileSync(
      generatedPlaywrightReporter,
      readFileSync(generatedPlaywrightReporter, "utf8").replace(
        "__SUPERCOV_EVIDENCE_DIRECTORY__",
        evidenceDirectoryRelative,
      ),
    );
    const generatedResolveLoader = resolve(generatedDirectory, "resolve-loader.mjs");
    atomicWriteFileSync(
      generatedResolveLoader,
      readFileSync(generatedResolveLoader, "utf8").replace(
        "__SUPERCOV_PLAYWRIGHT_MODULE__",
        project.playwrightModule,
      ),
    );

    // Strict (pnpm-style) installs never hoist vite to the project root, so
    // the generated configs must import it exactly where the project's own
    // tooling would find it: directly, or through vitest's dependencies.
    const viteModuleSpecifier = ((): string => {
      const projectRequire = createRequire(resolve(root, "package.json"));
      try {
        return pathToFileURL(projectRequire.resolve("vite")).href;
      } catch {
        try {
          return pathToFileURL(
            createRequire(
              projectRequire.resolve("vitest/package.json"),
            ).resolve("vite"),
          ).href;
        } catch {
          return "vite";
        }
      }
    })();

    if (isolatedPlaywrightConfig) {
      // Keep this import inside Playwright's own transform graph. In older
      // supported releases, a native ESM file-URL import of a TypeScript
      // config enters the synchronous transform bridge recursively and can
      // deadlock in Atomics.wait before test discovery starts.
      const configImport = `../${relative(isolatedRoot, isolatedPlaywrightConfig).split(sep).join("/")}`;
      atomicWriteFileSync(
        generatedPlaywrightConfig,
        [
          `import './register.mjs';`,
          `import { dirname, isAbsolute, relative, resolve } from 'node:path';`,
          `import { fileURLToPath } from 'node:url';`,
          `import original from '${configImport}';`,
          `const resolved = typeof original === 'function' ? await original({ command: 'test', mode: 'test' }) : original;`,
          `const runtimeProjectRoot = resolve(dirname(fileURLToPath(import.meta.url)), '..');`,
          `const originalDirectory = dirname(fileURLToPath(new URL(${JSON.stringify(configImport)}, import.meta.url)));`,
          `const sourceProjectRoot = process.env.SUPERCOV_SOURCE_PROJECT_ROOT;`,
          `const runtimePath = value => {`,
          `  if (!value) return value;`,
          `  const absolute = isAbsolute(value) ? value : resolve(originalDirectory, value);`,
          `  const local = relative(runtimeProjectRoot, absolute);`,
          `  if (local === '' || (!local.startsWith('..') && !isAbsolute(local))) return absolute;`,
          `  if (sourceProjectRoot) {`,
          `    const sourceLocal = relative(sourceProjectRoot, absolute);`,
          `    if (sourceLocal === '' || (!sourceLocal.startsWith('..') && !isAbsolute(sourceLocal))) return resolve(runtimeProjectRoot, sourceLocal);`,
          `  }`,
          `  throw new Error('Supercov refuses a Playwright output/cwd outside the isolated project: ' + absolute);`,
          `};`,
          `const normalizeWebServer = server => server ? ({ ...server, cwd: runtimePath(server.cwd ?? originalDirectory) }) : server;`,
          `const normalized = { ...resolved,`,
          `  testDir: runtimePath(resolved?.testDir),`,
          `  outputDir: runtimePath(resolved?.outputDir),`,
          `  snapshotDir: runtimePath(resolved?.snapshotDir),`,
          `  projects: resolved?.projects?.map(project => ({ ...project, testDir: runtimePath(project.testDir), outputDir: runtimePath(project.outputDir), snapshotDir: runtimePath(project.snapshotDir) })),`,
          `  webServer: Array.isArray(resolved?.webServer) ? resolved.webServer.map(normalizeWebServer) : normalizeWebServer(resolved?.webServer),`,
          `};`,
          `const configuredReporters = normalized.reporter;`,
          `const reporters = configuredReporters`,
          `  ? (typeof configuredReporters === 'string' ? [[configuredReporters]] : (Array.isArray(configuredReporters[0]) ? configuredReporters : [configuredReporters]))`,
          `  : [['list']];`,
          `const coverageReporter = resolve(runtimeProjectRoot, '.supercov/playwrightReporter.js');`,
          `export default { ...normalized, reporter: [...reporters, [coverageReporter]] };`,
          "",
        ].join("\n"),
      );
    }
    atomicWriteFileSync(
      generatedViteConfig,
      [
        `import { loadConfigFromFile, mergeConfig } from '${viteModuleSpecifier}';`,
        `import { isAbsolute, relative, resolve } from 'node:path';`,
        `import { mcdcVitePlugin } from '${pathToFileURL(resolve(packageSource, "vitePlugin.js")).href}';`,
        `export default async function supercovViteConfig(env) {`,
        `  const loaded = await loadConfigFromFile(env, undefined, process.cwd());`,
        `  const originalRoot = ${JSON.stringify(root)};`,
        `  const isolatedRoot = ${JSON.stringify(isolatedRoot)};`,
        `  const relocate = (value, label) => {`,
        `    const absolute = isAbsolute(value) ? value : resolve(isolatedRoot, value);`,
        `    const alreadyIsolated = relative(isolatedRoot, absolute);`,
        `    if (alreadyIsolated === '' || (!alreadyIsolated.startsWith('..') && !isAbsolute(alreadyIsolated))) return absolute;`,
        `    const local = relative(originalRoot, absolute);`,
        `    if (local.startsWith('..') || isAbsolute(local)) throw new Error('Supercov refuses ' + label + ' outside the isolated project: ' + absolute);`,
        `    return resolve(isolatedRoot, local);`,
        `  };`,
        `  const config = loaded?.config ?? {};`,
        `  const relocateOutput = output => output ? ({ ...output, dir: output.dir ? relocate(output.dir, 'Rollup output') : output.dir, file: output.file ? relocate(output.file, 'Rollup output') : output.file }) : output;`,
        `  const rollupOutput = config.build?.rollupOptions?.output;`,
        `  const safe = { ...config, cacheDir: resolve(isolatedRoot, '.supercov/vite-cache'), build: { ...config.build, outDir: relocate(config.build?.outDir ?? 'dist', 'Vite build output'), rollupOptions: { ...config.build?.rollupOptions, output: Array.isArray(rollupOutput) ? rollupOutput.map(relocateOutput) : relocateOutput(rollupOutput) } } };`,
        `  return mergeConfig(safe, { plugins: [mcdcVitePlugin(${JSON.stringify({ root: isolatedRoot, sourceRoots: project.sourceRoots, sourceFiles: project.sourceFiles, sourceScope: project.sourceScope, sourceLimitations: project.sourceLimitations, manifestPath, buildOutputMetadataPath })})] });`,
        `}`,
        "",
      ].join("\n"),
    );
    atomicWriteFileSync(
      generatedVitestConfig,
      [
        `import { loadConfigFromFile, mergeConfig } from '${viteModuleSpecifier}';`,
        `import { resolve } from 'node:path';`,
        `import { mcdcVitePlugin } from '${pathToFileURL(resolve(packageSource, "vitePlugin.js")).href}';`,
        `import SupercovVitestReporter from '${pathToFileURL(resolve(packageSource, "vitestReporter.js")).href}';`,
        `const discoveredConfig = ${JSON.stringify(isolatedVitestConfig)};`,
        `export default async function supercovVitestConfig(env) {`,
        `  const originalPath = process.env.SUPERCOV_ORIGINAL_VITEST_CONFIG || discoveredConfig;`,
        `  const loaded = originalPath ? await loadConfigFromFile(env, originalPath, process.cwd()) : undefined;`,
        `  const config = mergeConfig(loaded?.config ?? {}, {`,
        `    cacheDir: resolve(process.cwd(), '.supercov/vitest-cache'),`,
        `    plugins: ${project.buildAdapter === "vite" ? `[mcdcVitePlugin(${JSON.stringify({ root: isolatedRoot, sourceRoots: project.sourceRoots, sourceFiles: project.sourceFiles, sourceScope: project.sourceScope, sourceLimitations: project.sourceLimitations, manifestPath })})]` : "[]"},`,
        `    test: { setupFiles: [${JSON.stringify(resolve(packageSource, "vitest.js"))}], maxConcurrency: 1 },`,
        `  });`,
        `  const configuredReporters = loaded?.config?.test?.reporters;`,
        `  config.test ??= {};`,
        `  config.test.reporters = configuredReporters`,
        `    ? [...(Array.isArray(configuredReporters) ? configuredReporters : [configuredReporters]), new SupercovVitestReporter()]`,
        `    : ['default', new SupercovVitestReporter()];`,
        `  return config;`,
        `}`,
        "",
      ].join("\n"),
    );
    const generatedJestSetup = resolve(generatedDirectory, "jest.setup.cjs");
    const generatedJestReporter = resolve(generatedDirectory, "jest-reporter.cjs");
    const jestEvidenceHelpers = [
      `const { createHash } = require('crypto');`,
      `const { mkdirSync, renameSync, writeFileSync } = require('fs');`,
      `const { relative, resolve, sep } = require('path');`,
      `const local = file => file ? relative(process.cwd(), file).split(sep).join('/') : 'unknown';`,
      `const id = (file, name) => 'jest:' + createHash('sha256').update(['jest', local(file), 0, 0, name].join('\\0')).digest('hex').slice(0, 24);`,
      `const scope = (file, name, retry) => { const testId = id(file, name); const testKey = createHash('sha256').update(testId).digest('hex').slice(0, 24); return { version: 1, runId: process.env.SUPERCOV_RUN_ID || 'unscoped', workerId: 'jest-' + (process.env.JEST_WORKER_ID || process.pid), testId, testKey, retry, attemptId: testKey + '-' + retry }; };`,
      `const provenance = file => ({ runner: 'jest', kind: process.env.SUPERCOV_TEST_KIND || (/(^|[/_.-])(e2e|end-to-end|offline|online)([/_.-]|$)/i.test(file) ? 'e2e' : /(^|[/_.-])(integration|int)([/_.-]|$)/i.test(file) ? 'integration' : /(^|[/_.-])(component|components|ct)([/_.-]|$)/i.test(file) ? 'component' : 'unit'), source: process.env.SUPERCOV_TEST_KIND ? 'explicit' : 'runner-default' });`,
      `const write = (payload, suffix) => { const base = process.env.SUPERCOV_EVIDENCE_DIR; if (!base) return; const directory = resolve(process.cwd(), base, suffix); mkdirSync(directory, { recursive: true }); const target = resolve(directory, 'mcdc.json'); const temporary = target + '.' + process.pid + '.' + Math.random().toString(16).slice(2) + '.tmp'; writeFileSync(temporary, JSON.stringify(payload) + '\\n'); renameSync(temporary, target); };`,
    ];
    atomicWriteFileSync(
      generatedJestSetup,
      [
        ...jestEvidenceHelpers,
        `const { bind: bindJestEach } = require('jest-each');`,
        `const attempts = new Map();`,
        `const suites = [];`,
        `const copyModifiers = (target, source, wrap, ancestors = new Set()) => { if (ancestors.has(source)) return target; const seen = new Set(ancestors); seen.add(source); for (const key of Object.getOwnPropertyNames(source)) { if (['length', 'name', 'prototype'].includes(key)) continue; const value = source[key]; if (typeof value !== 'function') continue; try { const decorated = key === 'each' ? (...eachArgs) => wrap(value.apply(source, eachArgs), eachArgs) : copyModifiers(wrap(value.bind(source)), value, wrap, seen); Object.defineProperty(target, key, { configurable: true, enumerable: true, value: decorated }); } catch {} } return target; };`,
        `const wrapTest = (original, eachArgs) => { const wrapped = function(...args) { const callbackIndex = args.findLastIndex(value => typeof value === 'function'); if (callbackIndex < 0) return original.apply(this, args); const callback = args[callbackIndex]; const title = typeof args[0] === 'string' ? args[0] : callback.name || 'anonymous test'; const expandedNames = []; if (eachArgs) { try { bindJestEach(name => expandedNames.push([...suites, name].join(' ')))(...eachArgs)(title, callback, args[callbackIndex + 1]); } catch {} } const registeredName = [...suites, title].join(' '); let invocation = 0; const next = [...args]; const execute = (owner, originalCallbackArgs) => { const state = expect.getState(); const file = state.testPath; const name = expandedNames.length ? expandedNames[invocation++ % expandedNames.length] : registeredName; const testId = id(file, name); const retry = attempts.get(testId) || 0; attempts.set(testId, retry + 1); const execution = scope(file, name, retry); const runtime = process.__SUPERCOV_DIRECT_RUNTIME__; runtime.beginBufferedServerEvidence(execution); let flushed = false; const flush = () => { if (!flushed) { flushed = true; runtime.flushBufferedServerEvidence(execution); } }; const callbackArgs = [...originalCallbackArgs]; const doneIndex = callbackArgs.findLastIndex(value => typeof value === 'function'); if (doneIndex >= 0) { const done = callbackArgs[doneIndex]; callbackArgs[doneIndex] = (...doneArgs) => { flush(); return done(...doneArgs); }; } try { const result = runtime.withCoverageCarrier({ version: 1, scope: execution }, () => callback.apply(owner, callbackArgs)); if (result && typeof result.then === 'function') return Promise.resolve(result).finally(flush); if (doneIndex < 0) flush(); return result; } catch (error) { flush(); throw error; } }; let instrumented; if (eachArgs) { instrumented = function(...callbackArgs) { return execute(this, callbackArgs); }; try { Object.defineProperty(instrumented, 'length', { value: callback.length }); } catch {} } else { instrumented = callback.length ? function(done) { return execute(this, [done]); } : function() { return execute(this, []); }; } next[callbackIndex] = instrumented; return original.apply(this, next); }; return copyModifiers(wrapped, original, wrapTest); };`,
        `const wrapSuite = original => { const wrapped = function(...args) { const callbackIndex = args.findLastIndex(value => typeof value === 'function'); if (callbackIndex < 0) return original.apply(this, args); const callback = args[callbackIndex]; const name = typeof args[0] === 'string' ? args[0] : callback.name || 'suite'; const next = [...args]; next[callbackIndex] = function(...callbackArgs) { suites.push(name); try { return callback.apply(this, callbackArgs); } finally { suites.pop(); } }; return original.apply(this, next); }; return copyModifiers(wrapped, original, wrapSuite); };`,
        `globalThis.test = wrapTest(globalThis.test);`,
        `globalThis.it = globalThis.test;`,
        `globalThis.describe = wrapSuite(globalThis.describe);`,
        "",
      ].join("\n"),
    );
    atomicWriteFileSync(
      generatedJestReporter,
      [
        ...jestEvidenceHelpers,
        `module.exports = class SupercovJestReporter {`,
        `  onTestResult(_suite, result) { for (const assertion of result.testResults || []) {`,
        `    const file = result.testFilePath; const name = assertion.fullName || [...(assertion.ancestorTitles || []), assertion.title || 'unknown test'].join(' '); const retry = Math.max(0, (assertion.invocations || 1) - 1); const execution = scope(file, name, retry); const status = assertion.status === 'passed' ? 'passed' : assertion.status === 'failed' ? 'failed' : ['pending', 'todo', 'disabled'].includes(assertion.status) ? 'skipped' : 'unknown';`,
        `    write({ testId: execution.testId, scope: execution, test: name, testFile: local(file), title: assertion.title || name, retry, status, flaky: (assertion.retryReasons || []).length > 0, provenance: provenance(local(file)), runtime: [], browser: [], server: [] }, 'jest-' + execution.attemptId + '-status');`,
        `  } }`,
        `};`,
        "",
      ].join("\n"),
    );
    // Jest configuration commonly lives in the package.json "jest" field;
    // replacing it with {} silently changes test discovery (roots, testRegex).
    const packageJestConfig = ((): unknown => {
      try {
        return (
          JSON.parse(readFileSync(resolve(root, "package.json"), "utf8")) as {
            jest?: unknown;
          }
        ).jest;
      } catch {
        return undefined;
      }
    })();
    const jestOriginal = isolatedJestConfig
      ? usesLegacyJestConfig
        ? `const originalModule = require(${JSON.stringify(isolatedJestConfig)});\nconst original = typeof originalModule === 'function' ? originalModule() : originalModule;`
        : `import originalModule from ${JSON.stringify(pathToFileURL(isolatedJestConfig).href)};\nconst original = typeof originalModule === 'function' ? await originalModule() : originalModule;`
      : `const original = ${JSON.stringify(packageJestConfig ?? {})};`;
    atomicWriteFileSync(
      generatedJestConfig,
      [
        usesLegacyJestConfig
          ? `const { isAbsolute, resolve } = require('node:path');`
          : `import { isAbsolute, resolve } from 'node:path';`,
        jestOriginal,
        `const decorate = config => ({ ...config,`,
        `  rootDir: config?.rootDir ? (isAbsolute(config.rootDir) ? config.rootDir : resolve(${JSON.stringify(isolatedJestConfig ? resolve(isolatedJestConfig, "..") : isolatedRoot)}, config.rootDir)) : ${JSON.stringify(isolatedRoot)},`,
        // Jest matches ignore patterns against absolute test paths, and the
        // isolated workspace path itself contains a node_modules segment.
        // Anchor the default pattern below the workspace so it keeps ignoring
        // the project's dependencies without ignoring the whole workspace.
        `  testPathIgnorePatterns: (config?.testPathIgnorePatterns ?? ['/node_modules/']).map(pattern => pattern === '/node_modules/' ? '<rootDir>/.*node_modules/' : pattern),`,
        // Supercov replaces coverage measurement inside the wrapped run; the
        // project's own istanbul pass would measure instrumented code and
        // fail any thresholds against meaningless numbers.
        `  collectCoverage: false,`,
        `  coverageThreshold: undefined,`,
        `  setupFilesAfterEnv: [...(config?.setupFilesAfterEnv ?? []), ${JSON.stringify(generatedJestSetup)}],`,
        `  reporters: [...(config?.reporters ?? ['default']), ${JSON.stringify(generatedJestReporter)}],`,
        `  testLocationInResults: true,`,
        `});`,
        usesLegacyJestConfig
          ? `module.exports = { ...decorate(original), projects: original && original.projects && original.projects.map(project => typeof project === 'object' ? decorate(project) : project) };`
          : `export default { ...decorate(original), projects: original?.projects?.map(project => typeof project === 'object' ? decorate(project) : project) };`,
        "",
      ].join("\n"),
    );

    const coverageEnv: NodeJS.ProcessEnv = {
      ...process.env,
      SUPERCOV_EVIDENCE_DIR: evidenceDirectoryRelative,
      SUPERCOV_DIAGNOSTIC_OWNER_FILE: resolve(
        generatedDirectory,
        `diagnostic-owner-${runId}`,
      ),
      SUPERCOV_EXECUTION_FINGERPRINT: runIntegrity.fingerprint.execution,
      SUPERCOV_EXECUTION_LOG: resolve(isolatedEvidenceDirectory, "execution.jsonl"),
      SUPERCOV_ESM_TRANSFORMER: pathToFileURL(resolve(packageSource, "esmInterceptor.js")).href,
      SUPERCOV_ESM_CAPABILITY_WRAPPER: pathToFileURL(resolve(generatedDirectory, "launchSupervisor.js")).href,
      SUPERCOV_RUN_ID: runId,
      SUPERCOV_SERVER_EVIDENCE_ROOT: serverEvidenceRoot,
      SUPERCOV_MANIFEST: manifestPath,
      SUPERCOV_PLAYWRIGHT_MODULE: project.playwrightModule,
      SUPERCOV_PLAYWRIGHT_TEST_EXPORT: project.playwrightTestExport,
      SUPERCOV_PROJECT_ROOT: isolatedRoot,
      SUPERCOV_SOURCE_PROJECT_ROOT: root,
      ...(project.buildAdapter === "direct" || project.usesJest
        ? { SUPERCOV_DIRECT_INSTRUMENTATION: "1" }
        : {}),
      SUPERCOV_GENERATED_VITEST_CONFIG: generatedVitestConfig,
      SUPERCOV_GENERATED_PLAYWRIGHT_CONFIG: generatedPlaywrightConfig,
      SUPERCOV_GENERATED_JEST_CONFIG: generatedJestConfig,
      ...(isolatedPlaywrightConfig
        ? { SUPERCOV_ORIGINAL_PLAYWRIGHT_CONFIG: isolatedPlaywrightConfig }
        : {}),
    };
    const testNodeOptions = [
      process.env["NODE_OPTIONS"],
      `--import=${pathToFileURL(resolve(generatedDirectory, "register.mjs")).href}`,
    ]
      .filter(Boolean)
      .join(" ");
    timings.adapterSetupMs = performance.now() - phaseStarted;

    updateRunState(root, runId, { status: "building" });
    console.error(`[supercov] instrumenting isolated workspace ${isolatedRoot}`);
    if (Object.keys(project.buildEnvironment).length > 0)
      console.error(
        `[supercov] inferred build mode from command/config: ${Object.entries(project.buildEnvironment)
          .map(([key, value]) => `${key}=${value}`)
          .join(" ")}`,
      );
    phaseStarted = performance.now();
    if (reusableBuild) {
      console.error(
        `[supercov] reusing exact-fingerprint instrumented build ${buildCacheKey.slice(0, 12)}`,
      );
      buildResult = { status: 0, signal: null };
    } else if (project.buildAdapter === "direct") {
      instrumentDirectWorkspace(
        isolatedRoot,
        project.sourceFiles,
        manifestPath,
        project.sourceScope,
        project.sourceLimitations,
      );
      buildResult = { status: 0, signal: null };
    } else if (project.buildAdapter === "generic") {
      instrumentDirectWorkspace(
        isolatedRoot,
        project.sourceFiles,
        manifestPath,
        project.sourceScope,
        project.sourceLimitations,
        project.usesJest ? "global" : "module",
        project.sourceRoots,
      );
      buildResult = await runChild(
        project.buildCommand[0]!,
        project.buildCommand.slice(1),
        {
          cwd: isolatedRoot,
          env: {
            ...coverageEnv,
            ...project.buildEnvironment,
            NODE_ENV: "production",
            ...(project.usesJest ? { NODE_OPTIONS: testNodeOptions } : {}),
          },
        },
      );
    } else {
      buildResult = await runChild(
        project.buildCommand[0]!,
        [
          ...project.buildCommand.slice(1),
          "--",
          "--config",
          ".supercov/vite.config.mjs",
        ],
        {
          cwd: isolatedRoot,
          env: {
            ...coverageEnv,
            ...project.buildEnvironment,
            NODE_ENV: "production",
          },
        },
      );
    }
    if (
      buildResult.status === 0 &&
      project.buildAdapter !== "direct" &&
      !reusableBuild
    ) {
      writeInstrumentedBuildCache(isolatedRoot, buildCacheKey);
    }
    timings.instrumentedBuildMs = performance.now() - phaseStarted;
    if (receivedSignal) throw new Error(`Interrupted by ${receivedSignal}`);
    if (buildResult.error) throw buildResult.error;

    if (buildResult.status === 0) {
      const assertionCount = instrumentNodeAssertionsInWorkspace(
        isolatedRoot,
        project.sourceScope.entries.map((entry) => entry.file),
        [project.playwrightModule],
      );
      if (assertionCount > 0)
        console.error(
          `[supercov] attributed ${assertionCount} native node:assert call(s)`,
        );
      updateRunState(root, runId, { status: "testing" });
      const innerCoverageTool = expandedCommand(root, command)
        .split(/\s+/)
        .find((part) => /(?:^|\/)(?:c8|nyc)$/.test(part));
      if (innerCoverageTool)
        console.error(
          `[supercov] the wrapped command runs ${innerCoverageTool} over Supercov-instrumented code; its percentages and thresholds are not meaningful there. Wrap the underlying test command instead (for example: supercov -- node --test).`,
        );
      console.error(`[supercov] running in isolated workspace: ${command.join(" ")}`);
      phaseStarted = performance.now();
      testResult = await runChild(command[0]!, command.slice(1), {
        cwd: isolatedRoot,
        env: {
          ...coverageEnv,
          NODE_OPTIONS: testNodeOptions,
          SUPERCOV_CJS_INTERCEPT: "1",
        },
      });
      timings.testCommandMs = performance.now() - phaseStarted;
      if (receivedSignal) throw new Error(`Interrupted by ${receivedSignal}`);
      if (testResult.error) throw testResult.error;

      updateRunState(root, runId, { status: "publishing" });
      phaseStarted = performance.now();
      removeStoredTreeDeferred(root, persistedEvidenceDirectory);
      spawnTrashDeleter(root);
      if (existsSync(isolatedEvidenceDirectory))
        atomicRenameSync(isolatedEvidenceDirectory, persistedEvidenceDirectory);
      try {
        removeStoredTreeDeferred(root, runStagingDirectory);
        spawnTrashDeleter(root);
        const evidenceArchivePath = resolve(
          runStagingDirectory,
          "evidence.raw.gz",
        );
        const rawEvidence = writeEngineEvidenceArchive(
          [
            { file: manifestPath, path: "manifest.json" },
            { directory: persistedEvidenceDirectory },
            {
              directory: resolve(serverEvidenceRoot, runId),
              prefix: "server",
            },
          ],
          evidenceArchivePath,
        );
        timings.evidencePublicationMs = performance.now() - phaseStarted;
        atomicWriteFileSync(
          resolve(runStagingDirectory, "run.json"),
          `${JSON.stringify(
            {
              id: runId,
              startedAt,
              durationMs: Date.now() - runStartedAt,
              command,
              testExitCode: testResult.timedOut
                ? COMMAND_TIMEOUT_EXIT_CODE
                : testResult.status,
              integrity: runIntegrity,
              rawEvidence,
              isolatedBuild: true,
              instrumentedBuildCache: {
                key: buildCacheKey,
                reused: Boolean(reusableBuild),
              },
              timings: roundedTimings(timings),
            },
            null,
            2,
          )}\n`,
        );
        atomicRenameSync(runStagingDirectory, storedRunDirectory);
        runPublished = true;
        console.log(
          `[coverage] evidence: ${resolve(storedRunDirectory, "evidence.raw.gz")}`,
        );
      } catch (error) {
        timings.evidencePublicationMs = performance.now() - phaseStarted;
        publicationFailed = true;
        console.error("[supercov] failed to publish coverage evidence", error);
      }
    }

    const resultCode = exitCode(buildResult) || exitCode(testResult) || (publicationFailed ? 1 : 0);
    updateRunState(root, runId, { status: resultCode === 0 ? "complete" : "failed" });
    if (runPublished && !finalizePublishedRunStorage(root, runId))
      throw new Error(`Published run ${runId} is missing a durable artifact`);
    console.error(
      `[supercov] timings ${formatTimings(timings, performance.now() - runStartedMonotonic)}`,
    );
    timingsPrinted = true;
    return resultCode;
  } catch (error) {
    const status = receivedSignal ? "interrupted" : "failed";
    const message = error instanceof Error ? error.message : String(error);
    try {
      updateRunState(root, runId, {
        status,
        ...(receivedSignal ? { signal: receivedSignal } : {}),
        error: message,
      });
    } catch {
      // The original error remains authoritative.
    }
    if (!receivedSignal)
      console.error(
        `[supercov] ${process.env["SUPERCOV_DEBUG"] && error instanceof Error ? error.stack ?? message : message}`,
      );
    return receivedSignal ? signalExitCode(receivedSignal) : 1;
  } finally {
    if (!timingsPrinted)
      console.error(
        `[supercov] timings ${formatTimings(timings, performance.now() - runStartedMonotonic)}`,
      );
    if (signalEscalation) {
      clearTimeout(signalEscalation);
      signalEscalation = undefined;
    }
    for (const [signal, handler] of signalHandlers)
      process.removeListener(signal, handler);
    try {
      removeStoredTreeDeferred(root, runStagingDirectory);
      removeStoredTreeDeferred(root, resolve(serverEvidenceRoot, runId));
      // The stable isolated namespace persists as a build/snapshot cache at a
      // stable path, but its copied source and test files must not: ordinary
      // runner discovery at the project root would double-count the suite.
      if (!process.env["SUPERCOV_KEEP_WORKSPACE"])
        pruneCachedWorkspaceSources(root);
      spawnTrashDeleter(root);
    } catch (error) {
      console.error(`[supercov] isolated workspace cleanup failed: ${String(error)}`);
    }
    lock.release();
  }
}

const commandArgs = process.argv.slice(2);
const rawArgs =
  commandArgs[0] === "--help" || commandArgs[0] === "-h"
    ? ["help", ...commandArgs.slice(1)]
    : commandArgs;
const queryCommand = rawArgs[0];
const wantsJson = rawArgs.includes("--json");
const agentCommand = (() => {
  if (queryCommand !== "runs" || rawArgs[2] !== "coverage")
    return queryCommand;
  const child = rawArgs[3];
  return `coverage.${child && !child.startsWith("-") ? child : "summary"}`;
})();
const reportCliError = (error: unknown): void => {
  if (wantsJson) process.stdout.write(agentFailureJson(error, agentCommand));
  else console.error(`[supercov] ${error instanceof Error ? error.message : String(error)}`);
  process.exitCode = 2;
};

if (queryCommand === "clean" || queryCommand === "prune") {
  try {
    if (queryCommand === "clean") cleanCommand(rawArgs.slice(1));
    else pruneCommand(rawArgs.slice(1));
  } catch (error) {
    reportCliError(error);
  }
} else if (queryCommand === "merge") {
  try {
    const mergedRunId = mergeCoverageRuns(process.cwd(), rawArgs.slice(1));
    console.log(`[supercov] merged run ${mergedRunId}`);
    console.log(`npx supercov runs ${mergedRunId} coverage`);
  } catch (error) {
    reportCliError(error);
  }
} else if (queryCommand && coverageQueryCommands.has(queryCommand)) {
  try {
    await runQueryCommand(queryCommand, rawArgs.slice(1));
  } catch (error) {
    reportCliError(error);
  }
} else if (rawArgs[0] && rawArgs[0] !== "--") {
  reportCliError(
    new SupercovError(
      "UNKNOWN_COMMAND",
      `Unknown command: ${rawArgs[0]}. Try supercov help.`,
      { details: { command: rawArgs[0] } },
    ),
  );
} else {
  const separator = process.argv.indexOf("--");
  const command = separator >= 0 ? process.argv.slice(separator + 1) : [];
  if (command.length === 0) {
    console.error("Usage: supercov -- <test command>");
    process.exitCode = 2;
  } else {
    process.exitCode = await createCoverageRun(command);
  }
}
