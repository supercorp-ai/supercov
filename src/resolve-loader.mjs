import { resolve as resolvePath } from "node:path";
import { pathToFileURL } from "node:url";

const GENERATED_TARGET = "__SUPERCOV_PLAYWRIGHT_MODULE__";
const TARGET =
  process.env.SUPERCOV_PLAYWRIGHT_MODULE ??
  (GENERATED_TARGET.startsWith("__") ? "@playwright/test" : GENERATED_TARGET);
const REPLACEMENT =
  process.env.SUPERCOV_PLAYWRIGHT_WRAPPER ??
  "./.supercov/playwright.js";
const PROJECT_ROOT = process.env.SUPERCOV_PROJECT_ROOT;

function belongsToProject(parentURL) {
  if (!parentURL || parentURL.includes("/node_modules/")) return false;
  if (!PROJECT_ROOT) return parentURL.includes("/tests/");
  const normalizedRoot = PROJECT_ROOT.replaceAll("\\", "/").replace(/\/$/, "");
  const projectURL = `file://${normalizedRoot}/`;
  const generatedURL = `${projectURL}.supercov/`;
  return (
    parentURL.startsWith(projectURL) && !parentURL.startsWith(generatedURL)
  );
}

export async function resolve(specifier, context, nextResolve) {
  if (
    TARGET &&
    REPLACEMENT &&
    specifier === TARGET &&
    belongsToProject(context.parentURL)
  ) {
    if (process.env.SUPERCOV_DEBUG === "1") {
      console.error(
        `[supercov] redirected ${specifier} for ${context.parentURL}`,
      );
    }
    if (REPLACEMENT.startsWith("file:")) {
      return { url: REPLACEMENT, shortCircuit: true };
    }
    if (REPLACEMENT.startsWith(".")) {
      return {
        url: pathToFileURL(resolvePath(process.cwd(), REPLACEMENT)).href,
        shortCircuit: true,
      };
    }
    return nextResolve(REPLACEMENT, context);
  }
  return nextResolve(specifier, context);
}
