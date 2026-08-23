import { spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import {
  copyFileSync,
  existsSync,
  mkdirSync,
  readFileSync,
  writeFileSync,
} from "node:fs";
import { dirname, relative, resolve, sep } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { writeMcdcReport } from "./reporter.ts";
import { coverageQueryCommands, runQueryCommand } from "./query.ts";
import { discoverCoverageProject } from "./project.ts";
import { createRunIntegrity } from "./integrity.ts";

const commandArgs = process.argv.slice(2);
const rawArgs =
  commandArgs[0] === "--help" || commandArgs[0] === "-h"
    ? ["help", ...commandArgs.slice(1)]
    : commandArgs;
const queryCommand = rawArgs[0];

if (queryCommand && coverageQueryCommands.has(queryCommand)) {
  try {
    await runQueryCommand(queryCommand, rawArgs.slice(1));
  } catch (error) {
    console.error(
      `[supercov] ${error instanceof Error ? error.message : String(error)}`,
    );
    process.exitCode = 2;
  }
} else if (rawArgs[0] && rawArgs[0] !== "--") {
  console.error(
    `[supercov] Unknown command: ${rawArgs[0]}. Try supercov help.`,
  );
  process.exitCode = 2;
} else {
  const separator = process.argv.indexOf("--");
  const command = separator >= 0 ? process.argv.slice(separator + 1) : [];
  if (command.length === 0) {
    console.error("Usage: supercov -- <test command>");
    process.exit(2);
  }

  const root = process.cwd();
  const project = discoverCoverageProject(root);
  const runId = new Date().toISOString().replace(/[:.]/g, "-");
  const runStartedAt = Date.now();
  const generatedDirectory = resolve(root, ".supercov");
  const evidenceDirectoryRelative = `.supercov/evidence/${runId}`;
  const evidenceDirectory = resolve(root, evidenceDirectoryRelative);
  const generatedPlaywrightConfig = resolve(
    generatedDirectory,
    "playwright.config.mjs",
  );
  const generatedViteConfig = resolve(generatedDirectory, "vite.config.mjs");
  const generatedVitestConfig = resolve(generatedDirectory, "vitest.config.mjs");
  const manifestRelative = `.supercov/work/${runId}/mcdc-manifest.json`;
  const manifestPath = resolve(root, manifestRelative);
  const packageSource = fileURLToPath(new URL(".", import.meta.url));
  const runIntegrity = createRunIntegrity(root, project, packageSource);
  mkdirSync(generatedDirectory, { recursive: true });
  mkdirSync(evidenceDirectory, { recursive: true });
  for (const file of [
    "playwright.js",
    "playwrightReporter.js",
    "provenance.js",
    "register.mjs",
    "resolve-loader.mjs",
    "transport.js",
    "types.js",
  ]) {
    copyFileSync(
      resolve(packageSource, file),
      resolve(generatedDirectory, file),
    );
  }
  const generatedPlaywrightAdapter = resolve(
    generatedDirectory,
    "playwright.js",
  );
  const generatedPlaywrightReporter = resolve(
    generatedDirectory,
    "playwrightReporter.js",
  );
  writeFileSync(
    generatedPlaywrightAdapter,
    readFileSync(generatedPlaywrightAdapter, "utf8")
      .replace(
        "__SUPERCOV_EVIDENCE_DIRECTORY__",
        evidenceDirectoryRelative,
      )
      .replace(
        "__SUPERCOV_PLAYWRIGHT_MODULE__",
        project.playwrightModule,
      )
      .replace(
        "__SUPERCOV_RUN_ID__",
        runId,
      ),
  );
  writeFileSync(
    generatedPlaywrightReporter,
    readFileSync(generatedPlaywrightReporter, "utf8").replace(
      "__SUPERCOV_EVIDENCE_DIRECTORY__",
      evidenceDirectoryRelative,
    ),
  );
  const generatedResolveLoader = resolve(
    generatedDirectory,
    "resolve-loader.mjs",
  );
  writeFileSync(
    generatedResolveLoader,
    readFileSync(generatedResolveLoader, "utf8").replace(
      "__SUPERCOV_PLAYWRIGHT_MODULE__",
      project.playwrightModule,
    ),
  );
  if (project.playwrightConfig) {
    const configImport = project.essentialOffline
      ? `../${relative(root, project.playwrightConfig).split(sep).join("/")}`
      : pathToFileURL(project.playwrightConfig).href;
    writeFileSync(
      generatedPlaywrightConfig,
      [
        `import './register.mjs';`,
        `import { isAbsolute, resolve } from 'node:path';`,
        `import original from '${configImport}';`,
        `const resolved = typeof original === 'function' ? await original({ command: 'test', mode: 'test' }) : original;`,
        `const originalDirectory = ${JSON.stringify(dirname(project.playwrightConfig))};`,
        `const absoluteFromOriginal = value => value && !isAbsolute(value) ? resolve(originalDirectory, value) : value;`,
        `const normalizeWebServer = server => server ? ({ ...server, cwd: absoluteFromOriginal(server.cwd ?? originalDirectory) }) : server;`,
        `const normalized = { ...resolved,`,
        `  testDir: absoluteFromOriginal(resolved?.testDir),`,
        `  outputDir: absoluteFromOriginal(resolved?.outputDir),`,
        `  snapshotDir: absoluteFromOriginal(resolved?.snapshotDir),`,
        `  projects: resolved?.projects?.map(project => ({ ...project, testDir: absoluteFromOriginal(project.testDir), outputDir: absoluteFromOriginal(project.outputDir), snapshotDir: absoluteFromOriginal(project.snapshotDir) })),`,
        `  webServer: Array.isArray(resolved?.webServer) ? resolved.webServer.map(normalizeWebServer) : normalizeWebServer(resolved?.webServer),`,
        `};`,
        `const configuredReporters = normalized.reporter;`,
        `const reporters = configuredReporters`,
        `  ? (typeof configuredReporters === 'string' ? [[configuredReporters]] : (Array.isArray(configuredReporters[0]) ? configuredReporters : [configuredReporters]))`,
        `  : [['list']];`,
        `const coverageReporter = process.env.TEST_IN_CONTAINER === 'true'`,
        `  ? '/workspace/.supercov/playwrightReporter.js'`,
        `  : ${JSON.stringify(resolve(packageSource, "playwrightReporter.js"))};`,
        `export default { ...normalized, reporter: [...reporters, [coverageReporter]] };`,
        "",
      ].join("\n"),
    );
  }
  writeFileSync(
    generatedViteConfig,
    [
      `import { loadConfigFromFile, mergeConfig } from 'vite';`,
      `import { mcdcVitePlugin } from '${pathToFileURL(resolve(packageSource, "vitePlugin.js")).href}';`,
      `export default async function supercovViteConfig(env) {`,
      `  const loaded = await loadConfigFromFile(env, undefined, process.cwd());`,
      `  return mergeConfig(loaded?.config ?? {}, { plugins: [mcdcVitePlugin(${JSON.stringify({ root, sourceRoots: project.sourceRoots, manifestPath })})] });`,
      `}`,
      "",
    ].join("\n"),
  );
  writeFileSync(
    generatedVitestConfig,
    [
      `import { loadConfigFromFile, mergeConfig } from 'vite';`,
      `import { mcdcVitePlugin } from '${pathToFileURL(resolve(packageSource, "vitePlugin.js")).href}';`,
      `import SupercovVitestReporter from '${pathToFileURL(resolve(packageSource, "vitestReporter.js")).href}';`,
      `const discoveredConfig = ${JSON.stringify(project.vitestConfig)};`,
      `export default async function supercovVitestConfig(env) {`,
      `  const originalPath = process.env.SUPERCOV_ORIGINAL_VITEST_CONFIG || discoveredConfig;`,
      `  const loaded = originalPath ? await loadConfigFromFile(env, originalPath, process.cwd()) : undefined;`,
      `  const config = mergeConfig(loaded?.config ?? {}, {`,
      `    plugins: [mcdcVitePlugin(${JSON.stringify({ root, sourceRoots: project.sourceRoots, manifestPath })})],`,
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
  const coverageEnv = {
    ...process.env,
    TEST_MCDC: "true",
    TEST_OFFLINE_RESULTS_RUN_ID: runId,
    SUPERCOV_EVIDENCE_DIR: evidenceDirectoryRelative,
    SUPERCOV_RUN_ID: runId,
    SUPERCOV_MANIFEST: manifestRelative,
    SUPERCOV_PLAYWRIGHT_MODULE: project.playwrightModule,
    SUPERCOV_PROJECT_ROOT: root,
    SUPERCOV_GENERATED_VITEST_CONFIG: generatedVitestConfig,
    SUPERCOV_GENERATED_PLAYWRIGHT_CONFIG: generatedPlaywrightConfig,
  };
  const testNodeOptions = [
    process.env["NODE_OPTIONS"],
    `--import=${pathToFileURL(resolve(generatedDirectory, "register.mjs")).href}`,
  ]
    .filter(Boolean)
    .join(" ");

  console.error(`[supercov] instrumenting ${root}`);
  const instrumentedBuild = spawnSync(
    project.buildCommand[0]!,
    [
      ...project.buildCommand.slice(1),
      "--",
      "--config",
      ".supercov/vite.config.mjs",
    ],
    {
      cwd: root,
      env: {
        ...coverageEnv,
        ...(project.essentialOffline ? { TEST_OFFLINE: "true" } : {}),
        NODE_ENV: "production",
      },
      stdio: "inherit",
    },
  );

  let testRun: ReturnType<typeof spawnSync> | undefined;
  let reportFailed = false;

  if (!instrumentedBuild.error && instrumentedBuild.status === 0) {
    // The Essential offline VM pool keys its snapshot on dependency overlays,
    // not the mounted application build. A tiny build-hash overlay invalidates
    // the warm snapshot exactly when the instrumented bundle changes. Other
    // runners simply ignore these environment variables.
    const overlayPackage = "essential-seo/.mcdc-pool";
    if (project.essentialOffline) {
      const overlayDist = resolve(root, ".mcdc-pool/dist");
      const overlayMarker = resolve(overlayDist, "instrumented-build.sha256");
      const bundleHash = createHash("sha256")
        .update(readFileSync(resolve(root, "build/server/index.js")))
        .update(readFileSync(manifestPath))
        .digest("hex");
      mkdirSync(overlayDist, { recursive: true });
      if (
        !existsSync(overlayMarker) ||
        readFileSync(overlayMarker, "utf8").trim() !== bundleHash
      ) {
        writeFileSync(overlayMarker, `${bundleHash}\n`);
      }
    }

    console.error(`[supercov] running: ${command.join(" ")}`);
    testRun = spawnSync(command[0]!, command.slice(1), {
      cwd: root,
      stdio: "inherit",
      env: {
        ...coverageEnv,
        NODE_OPTIONS: testNodeOptions,
        ...(!project.essentialOffline
          ? {
              SUPERCOV_CJS_INTERCEPT: "1",
            }
          : {}),
        ...(project.essentialOffline
          ? {
              TEST_OFFLINE_LOCAL_OVERLAY: "true",
              TEST_OFFLINE_LOCAL_OVERLAY_PKGS: overlayPackage,
              TEST_PLAYWRIGHT_CONFIG:
                ".supercov/playwright.config.mjs",
            }
          : {}),
      },
    });

    try {
      writeMcdcReport(
        evidenceDirectory,
        runId,
        runStartedAt,
        manifestPath,
        testRun.status,
        runIntegrity,
      );
      const storedRunDirectory = resolve(
        root,
        ".supercov/runs",
        runId,
      );
      writeFileSync(
        resolve(storedRunDirectory, "run.json"),
        `${JSON.stringify(
          {
            id: runId,
            startedAt: new Date(runStartedAt).toISOString(),
            durationMs: Date.now() - runStartedAt,
            command,
            testExitCode: testRun.status,
            integrity: runIntegrity,
          },
          null,
          2,
        )}\n`,
      );
    } catch (error) {
      reportFailed = true;
      console.error("[supercov] failed to generate report", error);
    }
  }

  console.error("[supercov] restoring the ordinary build");
  const restore = spawnSync(
    project.buildCommand[0]!,
    project.buildCommand.slice(1),
    {
      cwd: root,
      env: {
        ...process.env,
        ...(project.essentialOffline ? { TEST_OFFLINE: "true" } : {}),
        NODE_ENV: "production",
      },
      stdio: "inherit",
    },
  );

  if (instrumentedBuild.error) throw instrumentedBuild.error;
  if (testRun?.error) throw testRun.error;
  if (restore.error) throw restore.error;
  process.exitCode =
    instrumentedBuild.status ||
    testRun?.status ||
    restore.status ||
    (reportFailed ? 1 : 0);
}
