import { readFileSync } from "node:fs";
import { dirname, resolve as resolvePath } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
const GENERATED_TARGET = "__SUPERCOV_PLAYWRIGHT_MODULE__";
const TARGET = process.env.SUPERCOV_PLAYWRIGHT_MODULE ??
    (GENERATED_TARGET.startsWith("__") ? "@playwright/test" : GENERATED_TARGET);
const REPLACEMENT = process.env.SUPERCOV_PLAYWRIGHT_WRAPPER ??
    "./.supercov/node_modules/playwright.mjs";
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
    // Derive the URL the way Node derives parentURL, or the two never match on
    // Windows: a hand-built `file://C:/...` is not the `file:///C:/...` that
    // pathToFileURL produces, and every project module would read as foreign.
    const projectURL = pathToFileURL(PROJECT_ROOT).href.replace(/\/?$/, "/");
    const generatedURL = `${projectURL}.supercov/`;
    return (parentURL.startsWith(projectURL) && !parentURL.startsWith(generatedURL));
}
let typescriptRequireHook = false;
export function initialize(data) {
    typescriptRequireHook = data?.typescriptRequireHook === true;
}
const packageTypes = new Map();
function packageType(directory) {
    if (packageTypes.has(directory))
        return packageTypes.get(directory);
    let type;
    try {
        type = JSON.parse(readFileSync(resolvePath(directory, "package.json"), "utf8")).type ?? "commonjs";
    }
    catch {
        const parent = dirname(directory);
        type = parent === directory ? "commonjs" : packageType(parent);
    }
    packageTypes.set(directory, type);
    return type;
}
// `node -r ts-node/register --test tests/a.test.ts` in a package without
// `"type"` passed alone and failed here with ERR_MODULE_NOT_FOUND on an
// extensionless import: the preload made Node load the entry point as an ES
// module, past the require hook that compiles it. A TypeScript file of a
// CommonJS package is given back to the CommonJS loader, where that hook is.
function throughRequireHook(resolved) {
    if (!typescriptRequireHook || !resolved?.url?.startsWith("file:") || resolved.url.includes("/node_modules/"))
        return resolved;
    if (!/\.(?:ts|tsx|cts)$/.test(resolved.url) || /\.d\.c?ts$/.test(resolved.url))
        return resolved;
    if (resolved.format === "module" || resolved.format === "module-typescript")
        return resolved;
    if (!resolved.url.endsWith(".cts") && packageType(dirname(fileURLToPath(resolved.url))) === "module")
        return resolved;
    return { ...resolved, format: "commonjs" };
}
export async function resolve(specifier, context, nextResolve) {
    // Some transpilers preserve the source-relative runtime import while moving
    // only the transformed application file to an output directory. The
    // source-local copy keeps strict rootDir compilers happy; this fallback
    // resolves the emitted import to Supercov's generated runtime without
    // requiring the project's build to copy our helper directory.
    if (specifier.endsWith("/.supercov/node_modules/runtime.mjs") &&
        belongsToProject(context.parentURL)) {
        return {
            url: new URL("./runtime.mjs", import.meta.url).href,
            shortCircuit: true,
        };
    }
    if (process.env.SUPERCOV_CJS_INTERCEPT === "1" &&
        (specifier === "node:test" || specifier === "test") &&
        belongsToProject(context.parentURL)) {
        return {
            url: new URL("./nodeTest.mjs", import.meta.url).href,
            shortCircuit: true,
        };
    }
    if (process.env.SUPERCOV_CJS_INTERCEPT === "1" &&
        ["assert", "node:assert", "assert/strict", "node:assert/strict"].includes(specifier) &&
        belongsToProject(context.parentURL)) {
        return {
            url: new URL(specifier.endsWith("/strict") ? "./nodeAssertStrict.mjs" : "./nodeAssert.mjs", import.meta.url).href,
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
    return throughRequireHook(await nextResolve(specifier, context));
}

// Node's own test coverage filters files by `--test-coverage-include` and
// `--test-coverage-exclude` globs relative to the working directory, and the
// instrumented copy's source map names the project's file, outside the
// isolated workspace the tests run in: `src/**` matched nothing, the report
// was empty and its thresholds passed with nothing checked. When those globs
// are given, the map this process caches names the copy's own path instead;
// its embedded original text is what Node reports. Nothing is written: other
// coverage tools read the file as it is on disk.
const NODE_COVERAGE_FILTERS = process.execArgv.some((argument) => /^--test-coverage-(?:include|exclude)(?:=|$)/.test(argument)) &&
    (Boolean(process.env.NODE_V8_COVERAGE) || process.execArgv.includes("--experimental-test-coverage"));
const INLINE_MAP = "//# sourceMappingURL=data:application/json;base64,";
function mapToCopy(source, filename) {
    if (typeof source !== "string" || !source.includes("__supercov"))
        return source;
    const at = source.lastIndexOf(INLINE_MAP);
    if (at < 0)
        return source;
    const end = source.indexOf("\n", at);
    const encoded = source.slice(at + INLINE_MAP.length, end < 0 ? undefined : end).trim();
    try {
        const map = JSON.parse(Buffer.from(encoded, "base64").toString("utf8"));
        if (!Array.isArray(map.sources) || map.sources.length !== 1)
            return source;
        map.sources = [filename.replace(/^.*[\\/]/, "")];
        return `${source.slice(0, at)}${INLINE_MAP}${Buffer.from(JSON.stringify(map)).toString("base64")}${end < 0 ? "" : source.slice(end)}`;
    }
    catch {
        return source;
    }
}
export async function load(url, context, nextLoad) {
    const result = await nextLoad(url, context);
    if (!NODE_COVERAGE_FILTERS || result.source == null || !url.startsWith("file:"))
        return result;
    const text = typeof result.source === "string" ? result.source : Buffer.from(result.source).toString("utf8");
    const mapped = mapToCopy(text, fileURLToPath(url));
    return mapped === text ? result : { ...result, source: mapped };
}
