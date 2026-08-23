import Module, { register } from "node:module";
import { resolve } from "node:path";
import { installLaunchSupervisor } from "./launchSupervisor.js";

installLaunchSupervisor();

if (process.env.SUPERCOV_DIRECT_INSTRUMENTATION === "1") {
  globalThis.__SUPERCOV_DIRECT_RUNTIME__ ??= await import("./runtime.js");
  process.__SUPERCOV_DIRECT_RUNTIME__ ??= globalThis.__SUPERCOV_DIRECT_RUNTIME__;
}

// NODE_OPTIONS reaches commands launched through npm scripts. When that child
// is Vitest, replace its config with our generated merging config before the
// CLI parses argv. This is what makes `supercov -- npm test` work
// without editing package scripts, Vitest configs, setup files, or test imports.
const generatedVitestConfig =
  process.env.SUPERCOV_GENERATED_VITEST_CONFIG;
const generatedPlaywrightConfig =
  process.env.SUPERCOV_GENERATED_PLAYWRIGHT_CONFIG;
const generatedJestConfig = process.env.SUPERCOV_GENERATED_JEST_CONFIG;
const entrypoint = process.argv[1]?.replaceAll("\\", "/") ?? "";
const playwrightTarget = process.env.SUPERCOV_PLAYWRIGHT_MODULE;
const nodeTestWrapper = new URL("./nodeTest.js", import.meta.url).href;
const isPlaywrightEntrypoint =
  /\/(?:node_modules\/\.bin\/playwright|node_modules\/(?:@playwright\/test|playwright)\/(?:cli\.js|.*\/program\.js))$/.test(
    entrypoint,
  );
const isJestEntrypoint =
  /\/node_modules\/(?:\.bin\/jest|(?:jest|jest-cli)\/bin\/jest\.js)$/.test(entrypoint);
if (generatedPlaywrightConfig && isPlaywrightEntrypoint)
  process.env.SUPERCOV_INSIDE_PLAYWRIGHT = "1";
register(new URL("./resolve-loader.mjs", import.meta.url));

if (process.env.SUPERCOV_DEBUG === "1") {
  console.error("[supercov] preload", { entrypoint });
}

if (generatedVitestConfig && /\/vitest(?:\.m?js)?$/.test(entrypoint)) {
  // Worker processes inherit this marker. In particular, do not eagerly load
  // Playwright's expect implementation in a Vitest worker: both runners use
  // the Jest matcher registry symbol and intentionally cannot coexist there.
  process.env.SUPERCOV_INSIDE_VITEST = "1";
  let originalConfig;
  for (let index = 2; index < process.argv.length; index += 1) {
    const argument = process.argv[index];
    if (argument === "--config" || argument === "-c") {
      const configured = process.argv[index + 1];
      const resolvedConfig = configured
        ? resolve(process.cwd(), configured)
        : undefined;
      if (resolvedConfig && resolvedConfig !== resolve(generatedVitestConfig)) {
        originalConfig = resolvedConfig;
      }
      process.argv.splice(index, configured ? 2 : 1);
      index -= 1;
    } else if (argument?.startsWith("--config=")) {
      const resolvedConfig = resolve(
        process.cwd(),
        argument.slice("--config=".length),
      );
      if (resolvedConfig !== resolve(generatedVitestConfig)) {
        originalConfig = resolvedConfig;
      }
      process.argv.splice(index, 1);
      index -= 1;
    }
  }
  if (originalConfig) {
    process.env.SUPERCOV_ORIGINAL_VITEST_CONFIG = originalConfig;
  }
  process.argv.push("--config", generatedVitestConfig);
  if (process.env.SUPERCOV_DEBUG === "1") {
    console.error("[supercov] Vitest argv configured", {
      entrypoint,
      originalConfig,
      generatedVitestConfig,
      argv: process.argv.slice(2),
    });
  }
}

if (generatedJestConfig && isJestEntrypoint) {
  for (let index = 2; index < process.argv.length; index += 1) {
    const argument = process.argv[index];
    if (argument === "--config" || argument === "-c") {
      process.argv.splice(index, process.argv[index + 1] ? 2 : 1);
      index -= 1;
    } else if (argument?.startsWith("--config=")) {
      process.argv.splice(index, 1);
      index -= 1;
    }
  }
  process.argv.push("--config", generatedJestConfig);
}

if (
  generatedPlaywrightConfig &&
  isPlaywrightEntrypoint
) {
  for (let index = 2; index < process.argv.length; index += 1) {
    const argument = process.argv[index];
    if (argument === "--config" || argument === "-c") {
      process.argv.splice(index, process.argv[index + 1] ? 2 : 1);
      index -= 1;
    } else if (argument?.startsWith("--config=")) {
      process.argv.splice(index, 1);
      index -= 1;
    }
  }
  process.argv.push("--config", generatedPlaywrightConfig);
}

if (
  process.env.SUPERCOV_CJS_INTERCEPT === "1" &&
  process.env.SUPERCOV_INSIDE_PLAYWRIGHT === "1" &&
  process.env.SUPERCOV_INSIDE_VITEST !== "1" &&
  process.env.VITEST !== "true"
) {
  const target = playwrightTarget ?? "@playwright/test";
  const projectRoot = process.env.SUPERCOV_PROJECT_ROOT;
  const originalPlaywrightConfig = process.env.SUPERCOV_ORIGINAL_PLAYWRIGHT_CONFIG
    ?.replaceAll("\\", "/");
  const wrapper = await import(new URL("./playwright.js", import.meta.url));
  const originalLoad = Module._load;

  Module._load = function supercovLoad(request, parent, isMain) {
    const parentFile = parent?.filename?.replaceAll("\\", "/");
    const normalizedRoot = projectRoot
      ?.replaceAll("\\", "/")
      .replace(/\/$/, "");
    const generatedRoot = normalizedRoot
      ? `${normalizedRoot}/.supercov/`
      : undefined;
    const belongsToProject =
      Boolean(parentFile) &&
      !parentFile.includes("/node_modules/") &&
      parentFile !== originalPlaywrightConfig &&
      (!generatedRoot || !parentFile.startsWith(generatedRoot)) &&
      (normalizedRoot
        ? parentFile.startsWith(`${normalizedRoot}/`)
        : parentFile.includes("/tests/"));
    if (request === target && belongsToProject) return wrapper;
    return originalLoad.call(this, request, parent, isMain);
  };
}

// CJS test files receive the same node:test adapter as ESM files. The adapter
// itself imports the native built-in from Supercov's generated directory, so
// only first-party callers are redirected and recursion is impossible.
if (process.env.SUPERCOV_CJS_INTERCEPT === "1") {
  const projectRoot = process.env.SUPERCOV_PROJECT_ROOT?.replaceAll("\\", "/").replace(/\/$/, "");
  const nodeTestAdapter = await import(nodeTestWrapper);
  const cjsNodeTestAdapter = Object.assign(nodeTestAdapter.test, nodeTestAdapter);
  const originalLoad = Module._load;
  Module._load = function supercovNodeTestLoad(request, parent, isMain) {
    const parentFile = parent?.filename?.replaceAll("\\", "/");
    const belongsToProject = Boolean(
      projectRoot &&
      parentFile?.startsWith(`${projectRoot}/`) &&
      !parentFile.includes("/node_modules/") &&
      !parentFile.startsWith(`${projectRoot}/.supercov/`),
    );
    if (
      process.env.SUPERCOV_DEBUG === "1" &&
      (request === "node:test" || request === "test")
    )
      console.error("[supercov] node:test CJS request", { parentFile, projectRoot, belongsToProject });
    if ((request === "node:test" || request === "test") && belongsToProject)
      return cjsNodeTestAdapter;
    return originalLoad.call(this, request, parent, isMain);
  };
}
