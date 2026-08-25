var __rewriteRelativeImportExtension = (this && this.__rewriteRelativeImportExtension) || function (path, preserveJsx) {
    if (typeof path === "string" && /^\.\.?\//.test(path)) {
        return path.replace(/\.(tsx)$|((?:\.d)?)((?:\.[^./]+?)?)\.([cm]?)ts$/i, function (m, tsx, d, ext, cm) {
            return tsx ? preserveJsx ? ".jsx" : ".js" : d && (!ext || !cm) ? m : (d + ext + "." + cm.toLowerCase() + "js");
        });
    }
    return path;
};
import { resolve as resolvePath } from "node:path";
import { pathToFileURL } from "node:url";
const GENERATED_TARGET = "__SUPERCOV_PLAYWRIGHT_MODULE__";
const TARGET = process.env.SUPERCOV_PLAYWRIGHT_MODULE ??
    (GENERATED_TARGET.startsWith("__") ? "@playwright/test" : GENERATED_TARGET);
const REPLACEMENT = process.env.SUPERCOV_PLAYWRIGHT_WRAPPER ??
    "./.supercov/playwright.js";
const PROJECT_ROOT = process.env.SUPERCOV_PROJECT_ROOT;
const ORIGINAL_CONFIG = process.env.SUPERCOV_ORIGINAL_PLAYWRIGHT_CONFIG;
function belongsToProject(parentURL) {
    if (!parentURL || parentURL.includes("/node_modules/"))
        return false;
    // Never redirect the original config while Playwright is synchronously
    // loading it. Test modules still need the ESM redirect because Playwright's
    // transform path does not consistently pass through Module._load.
    if (ORIGINAL_CONFIG &&
        parentURL === pathToFileURL(resolvePath(ORIGINAL_CONFIG)).href)
        return false;
    if (!PROJECT_ROOT)
        return parentURL.includes("/tests/");
    const normalizedRoot = PROJECT_ROOT.replaceAll("\\", "/").replace(/\/$/, "");
    const projectURL = `file://${normalizedRoot}/`;
    const generatedURL = `${projectURL}.supercov/`;
    return (parentURL.startsWith(projectURL) && !parentURL.startsWith(generatedURL));
}
export async function resolve(specifier, context, nextResolve) {
    // Some transpilers preserve the source-relative runtime import while moving
    // only the transformed application file to an output directory. The
    // source-local copy keeps strict rootDir compilers happy; this fallback
    // resolves the emitted import to Supercov's generated runtime without
    // requiring the project's build to copy our helper directory.
    if (specifier.endsWith("/.supercov/runtime.js") &&
        belongsToProject(context.parentURL)) {
        return {
            url: new URL("./runtime.js", import.meta.url).href,
            shortCircuit: true,
        };
    }
    if (process.env.SUPERCOV_CJS_INTERCEPT === "1" &&
        (specifier === "node:test" || specifier === "test") &&
        belongsToProject(context.parentURL)) {
        return {
            url: new URL("./nodeTest.js", import.meta.url).href,
            shortCircuit: true,
        };
    }
    if (process.env.SUPERCOV_CJS_INTERCEPT === "1" &&
        ["assert", "node:assert", "assert/strict", "node:assert/strict"].includes(specifier) &&
        belongsToProject(context.parentURL)) {
        return {
            url: new URL(specifier.endsWith("/strict") ? "./nodeAssertStrict.js" : "./nodeAssert.js", import.meta.url).href,
            shortCircuit: true,
        };
    }
    if (process.env.SUPERCOV_INSIDE_PLAYWRIGHT === "1" &&
        TARGET &&
        REPLACEMENT &&
        specifier === TARGET &&
        belongsToProject(context.parentURL)) {
        if (process.env.SUPERCOV_DEBUG === "1") {
            console.error(`[supercov] redirected ${specifier} for ${context.parentURL}`);
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
export async function load(url, context, nextLoad) {
    const loaded = await nextLoad(url, context);
    const transformer = process.env.SUPERCOV_ESM_TRANSFORMER;
    const wrapper = process.env.SUPERCOV_ESM_CAPABILITY_WRAPPER;
    if (!transformer ||
        !wrapper ||
        loaded.format !== "module" ||
        !belongsToProject(url) ||
        loaded.source === undefined ||
        loaded.source === null)
        return loaded;
    const source = typeof loaded.source === "string"
        ? loaded.source
        : Buffer.from(loaded.source).toString("utf8");
    try {
        const transformerUrl = transformer.startsWith("file:")
            ? transformer
            : pathToFileURL(resolvePath(transformer)).href;
        const wrapperUrl = wrapper.startsWith("file:")
            ? wrapper
            : pathToFileURL(resolvePath(wrapper)).href;
        const { transformCapabilityImports } = await import(__rewriteRelativeImportExtension(transformerUrl));
        const transformed = transformCapabilityImports(source, url, wrapperUrl);
        return transformed.transformed
            ? { ...loaded, source: transformed.code }
            : loaded;
    }
    catch (error) {
        if (process.env.SUPERCOV_DEBUG === "1")
            console.error("[supercov] ESM capability transform skipped", { url, error });
        return loaded;
    }
}
//# sourceMappingURL=resolve-loader.mjs.map