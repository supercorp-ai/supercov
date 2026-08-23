import { spawn, type ChildProcess } from "node:child_process";
import {
  copyFileSync,
  existsSync,
  mkdirSync,
  readFileSync,
  rmSync,
} from "node:fs";
import { relative, resolve, sep } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { atomicRenameSync, atomicWriteFileSync } from "./atomic.ts";
import { writeMcdcReport } from "./reporter.ts";
import { coverageQueryCommands, runQueryCommand } from "./query.ts";
import { discoverCoverageProject } from "./project.ts";
import { createRunIntegrity } from "./integrity.ts";
import { instrumentDirectWorkspace } from "./directInstrumenter.ts";
import {
  acquireProjectLock,
  cachedWorkspacePath,
  cleanCoverageStorage,
  prepareCachedWorkspace,
  recoverAbandonedRuns,
  updateRunState,
  writeRunState,
} from "./workspace.ts";

interface ChildResult {
  status: number | null;
  signal: NodeJS.Signals | null;
  error?: Error;
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
  return new Promise((resolveChild) => {
    let error: Error | undefined;
    const child = spawn(command, args, {
      cwd: options.cwd,
      env: options.env,
      stdio: "inherit",
      detached: process.platform !== "win32",
    });
    activeChild = child;
    child.once("error", (failure) => {
      error = failure;
    });
    child.once("close", (status, signal) => {
      if (activeChild === child) activeChild = undefined;
      resolveChild({ status, signal, ...(error ? { error } : {}) });
    });
  });
}

function exitCode(result?: ChildResult): number {
  if (!result) return 0;
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

function parseCleanOptions(args: string[]): { keep: number; dryRun: boolean } {
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
    } else throw new Error(`Unknown clean option: ${argument}`);
  }
  return { keep, dryRun };
}

function cleanCommand(args: string[]): void {
  const options = parseCleanOptions(args);
  const result = cleanCoverageStorage(process.cwd(), options);
  console.log(
    `[supercov] ${options.dryRun ? "would remove" : "removed"} ${result.removedRuns.length} stored run(s), ${result.removedWorkspaces.length} per-run workspace(s), and ${result.removedBuildCache ? "the" : "no"} isolated build cache; keeping ${options.keep} newest run(s)`,
  );
  for (const id of result.removedRuns) console.log(id);
}

async function createCoverageRun(command: string[]): Promise<number> {
  const root = process.cwd();
  const runId = new Date().toISOString().replace(/[:.]/g, "-");
  const runStartedAt = Date.now();
  const recovered = recoverAbandonedRuns(root);
  if (recovered.length > 0)
    console.error(`[supercov] recovered abandoned run(s): ${recovered.join(", ")}`);
  const lock = acquireProjectLock(root, runId);
  const project = discoverCoverageProject(root, process.env, command);
  const packageSource = fileURLToPath(new URL(".", import.meta.url));
  const runIntegrity = createRunIntegrity(root, project, packageSource);
  const workspace = cachedWorkspacePath(root);
  const serverEvidenceRoot = resolve(workspace, ".supercov/server-evidence");
  const reportStagingDirectory = resolve(
    root,
    ".supercov/work",
    runId,
    "report-publication",
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
      signalEscalation = setTimeout(() => terminateChild("SIGKILL"), 5_000);
      signalEscalation.unref();
    };
    signalHandlers.set(signal, handler);
    process.once(signal, handler);
  }

  let buildResult: ChildResult | undefined;
  let testResult: ChildResult | undefined;
  let reportFailed = false;

  try {
    const isolatedRoot = prepareCachedWorkspace(root);
    if (receivedSignal) throw new Error(`Interrupted by ${receivedSignal}`);
    const generatedDirectory = resolve(isolatedRoot, ".supercov");
    const evidenceDirectoryRelative = `.supercov/evidence/${runId}`;
    const isolatedEvidenceDirectory = resolve(isolatedRoot, evidenceDirectoryRelative);
    const persistedEvidenceDirectory = resolve(root, ".supercov/evidence", runId);
    const generatedPlaywrightConfig = resolve(generatedDirectory, "playwright.config.mjs");
    const generatedViteConfig = resolve(generatedDirectory, "vite.config.mjs");
    const generatedVitestConfig = resolve(generatedDirectory, "vitest.config.mjs");
    const manifestPath = resolve(generatedDirectory, "manifest.json");
    const isolatedPlaywrightConfig = project.playwrightConfig
      ? resolve(isolatedRoot, relative(root, project.playwrightConfig))
      : undefined;
    const isolatedVitestConfig = project.vitestConfig
      ? resolve(isolatedRoot, relative(root, project.vitestConfig))
      : undefined;

    mkdirSync(generatedDirectory, { recursive: true });
    mkdirSync(isolatedEvidenceDirectory, { recursive: true });
    for (const file of [
      "atomic.js",
      "launchSupervisor.js",
      "playwright.js",
      "playwrightReporter.js",
      "provenance.js",
      "register.mjs",
      "resolve-loader.mjs",
      "runtime.js",
      "transport.js",
      "types.js",
    ]) {
      copyFileSync(resolve(packageSource, file), resolve(generatedDirectory, file));
    }
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
        `import { loadConfigFromFile, mergeConfig } from 'vite';`,
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
        `  return mergeConfig(safe, { plugins: [mcdcVitePlugin(${JSON.stringify({ root: isolatedRoot, sourceRoots: project.sourceRoots, manifestPath })})] });`,
        `}`,
        "",
      ].join("\n"),
    );
    atomicWriteFileSync(
      generatedVitestConfig,
      [
        `import { loadConfigFromFile, mergeConfig } from 'vite';`,
        `import { resolve } from 'node:path';`,
        `import { mcdcVitePlugin } from '${pathToFileURL(resolve(packageSource, "vitePlugin.js")).href}';`,
        `import SupercovVitestReporter from '${pathToFileURL(resolve(packageSource, "vitestReporter.js")).href}';`,
        `const discoveredConfig = ${JSON.stringify(isolatedVitestConfig)};`,
        `export default async function supercovVitestConfig(env) {`,
        `  const originalPath = process.env.SUPERCOV_ORIGINAL_VITEST_CONFIG || discoveredConfig;`,
        `  const loaded = originalPath ? await loadConfigFromFile(env, originalPath, process.cwd()) : undefined;`,
        `  const config = mergeConfig(loaded?.config ?? {}, {`,
        `    cacheDir: resolve(process.cwd(), '.supercov/vitest-cache'),`,
        `    plugins: ${project.buildAdapter === "vite" ? `[mcdcVitePlugin(${JSON.stringify({ root: isolatedRoot, sourceRoots: project.sourceRoots, manifestPath })})]` : "[]"},`,
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

    const coverageEnv: NodeJS.ProcessEnv = {
      ...process.env,
      SUPERCOV_EVIDENCE_DIR: evidenceDirectoryRelative,
      SUPERCOV_EXECUTION_FINGERPRINT: runIntegrity.fingerprint.execution,
      SUPERCOV_EXECUTION_LOG: resolve(isolatedEvidenceDirectory, "execution.jsonl"),
      SUPERCOV_RUN_ID: runId,
      SUPERCOV_SERVER_EVIDENCE_ROOT: serverEvidenceRoot,
      SUPERCOV_MANIFEST: manifestPath,
      SUPERCOV_PLAYWRIGHT_MODULE: project.playwrightModule,
      SUPERCOV_PLAYWRIGHT_TEST_EXPORT: project.playwrightTestExport,
      SUPERCOV_PROJECT_ROOT: isolatedRoot,
      SUPERCOV_SOURCE_PROJECT_ROOT: root,
      ...(project.buildAdapter === "direct"
        ? { SUPERCOV_DIRECT_INSTRUMENTATION: "1" }
        : {}),
      SUPERCOV_GENERATED_VITEST_CONFIG: generatedVitestConfig,
      SUPERCOV_GENERATED_PLAYWRIGHT_CONFIG: generatedPlaywrightConfig,
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

    updateRunState(root, runId, { status: "building" });
    console.error(`[supercov] instrumenting isolated workspace ${isolatedRoot}`);
    if (Object.keys(project.buildEnvironment).length > 0)
      console.error(
        `[supercov] inferred build mode from command/config: ${Object.entries(project.buildEnvironment)
          .map(([key, value]) => `${key}=${value}`)
          .join(" ")}`,
      );
    if (project.buildAdapter === "direct") {
      instrumentDirectWorkspace(
        isolatedRoot,
        project.sourceRoots,
        manifestPath,
      );
      buildResult = { status: 0, signal: null };
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
    if (receivedSignal) throw new Error(`Interrupted by ${receivedSignal}`);
    if (buildResult.error) throw buildResult.error;

    if (buildResult.status === 0) {
      updateRunState(root, runId, { status: "testing" });
      console.error(`[supercov] running in isolated workspace: ${command.join(" ")}`);
      testResult = await runChild(command[0]!, command.slice(1), {
        cwd: isolatedRoot,
        env: {
          ...coverageEnv,
          NODE_OPTIONS: testNodeOptions,
          SUPERCOV_CJS_INTERCEPT: "1",
        },
      });
      if (receivedSignal) throw new Error(`Interrupted by ${receivedSignal}`);
      if (testResult.error) throw testResult.error;

      updateRunState(root, runId, { status: "reporting" });
      rmSync(persistedEvidenceDirectory, { recursive: true, force: true });
      if (existsSync(isolatedEvidenceDirectory))
        atomicRenameSync(isolatedEvidenceDirectory, persistedEvidenceDirectory);
      try {
        rmSync(reportStagingDirectory, { recursive: true, force: true });
        writeMcdcReport(
          persistedEvidenceDirectory,
          runId,
          runStartedAt,
          manifestPath,
          testResult.status,
          runIntegrity,
          {
            directory: reportStagingDirectory,
            displayDirectory: storedRunDirectory,
            serverEvidenceRoot,
          },
        );
        atomicWriteFileSync(
          resolve(reportStagingDirectory, "run.json"),
          `${JSON.stringify(
            {
              id: runId,
              startedAt,
              durationMs: Date.now() - runStartedAt,
              command,
              testExitCode: testResult.status,
              integrity: runIntegrity,
              isolatedBuild: true,
            },
            null,
            2,
          )}\n`,
        );
        atomicRenameSync(reportStagingDirectory, storedRunDirectory);
      } catch (error) {
        reportFailed = true;
        console.error("[supercov] failed to generate report", error);
      }
    }

    const resultCode = exitCode(buildResult) || exitCode(testResult) || (reportFailed ? 1 : 0);
    updateRunState(root, runId, { status: resultCode === 0 ? "complete" : "failed" });
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
    if (!receivedSignal) console.error(`[supercov] ${message}`);
    return receivedSignal ? signalExitCode(receivedSignal) : 1;
  } finally {
    if (signalEscalation) {
      clearTimeout(signalEscalation);
      signalEscalation = undefined;
    }
    for (const [signal, handler] of signalHandlers)
      process.removeListener(signal, handler);
    try {
      rmSync(reportStagingDirectory, { recursive: true, force: true });
      rmSync(resolve(serverEvidenceRoot, runId), {
        recursive: true,
        force: true,
      });
      // The stable isolated namespace is a deliberate build/snapshot cache.
      // It never overlaps the user's ordinary build and `supercov clean`
      // removes it deterministically.
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

if (queryCommand === "clean") {
  try {
    cleanCommand(rawArgs.slice(1));
  } catch (error) {
    console.error(`[supercov] ${error instanceof Error ? error.message : String(error)}`);
    process.exitCode = 2;
  }
} else if (queryCommand && coverageQueryCommands.has(queryCommand)) {
  try {
    await runQueryCommand(queryCommand, rawArgs.slice(1));
  } catch (error) {
    console.error(`[supercov] ${error instanceof Error ? error.message : String(error)}`);
    process.exitCode = 2;
  }
} else if (rawArgs[0] && rawArgs[0] !== "--") {
  console.error(`[supercov] Unknown command: ${rawArgs[0]}. Try supercov help.`);
  process.exitCode = 2;
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
