var __rewriteRelativeImportExtension = (this && this.__rewriteRelativeImportExtension) || function (path, preserveJsx) {
    if (typeof path === "string" && /^\.\.?\//.test(path)) {
        return path.replace(/\.(tsx)$|((?:\.d)?)((?:\.[^./]+?)?)\.([cm]?)ts$/i, function (m, tsx, d, ext, cm) {
            return tsx ? preserveJsx ? ".jsx" : ".js" : d && (!ext || !cm) ? m : (d + ext + "." + cm.toLowerCase() + "js");
        });
    }
    return path;
};
import Module, { register, syncBuiltinESMExports } from "node:module";
import fs, { closeSync, openSync, readFileSync, realpathSync, unlinkSync } from "node:fs";
import { isAbsolute, relative, resolve, sep } from "node:path";
import { fileURLToPath } from "node:url";
import { installLaunchSupervisor, wrapImportedCapability } from "./launchSupervisor.mjs";
import { __supercovBindCapabilityWrapper } from "./capability.mjs";
installLaunchSupervisor();
// Instrumented sources import the browser-safe capability seam; bind the
// real supervisor implementation before any user module evaluates.
__supercovBindCapabilityWrapper(wrapImportedCapability);
// Long-running commands are diagnosed without changing their runner's exit
// semantics. The first preloaded Node process atomically elects itself for the
// run and periodically reports public active-resource types. This deliberately
// uses no Unix signal: a launch tree may contain uninstrumented Node children,
// and signalling one would terminate a healthy command by default.
const verboseDiagnostics = [process.env.SUPERCOV_VERBOSE, process.env.SUPERCOV_DEBUG]
    .some(value => value === "1" || value === "true" || value === "yes");
if (verboseDiagnostics && !process.__SUPERCOV_DIAGNOSTIC_REPORTER__) {
    process.__SUPERCOV_DIAGNOSTIC_REPORTER__ = true;
    const ownerFile = process.env.SUPERCOV_DIAGNOSTIC_OWNER_FILE;
    let ownerDescriptor;
    try {
        if (ownerFile)
            ownerDescriptor = openSync(ownerFile, "wx");
    }
    catch {
        // Another preloaded process owns diagnostics for this run.
    }
    if (ownerDescriptor !== undefined) {
        const configuredInterval = Number(process.env.SUPERCOV_DIAGNOSTIC_INTERVAL_MS ?? 60_000);
        const intervalMs = Number.isSafeInteger(configuredInterval) && configuredInterval > 0
            ? configuredInterval
            : 60_000;
        const reportResources = () => {
            const resources = typeof process.getActiveResourcesInfo === "function"
                ? process.getActiveResourcesInfo()
                : [];
            const counts = Object.fromEntries([...new Set(resources)].sort().map(resource => [
                resource,
                resources.filter(candidate => candidate === resource).length,
            ]));
            console.error(`[supercov:active-resources] ${JSON.stringify({
                pid: process.pid,
                ppid: process.ppid,
                uptimeMs: Math.round(process.uptime() * 1000),
                resources: counts,
            })}`);
        };
        const diagnosticTimer = setInterval(reportResources, intervalMs);
        diagnosticTimer.unref();
        process.once("exit", () => {
            clearInterval(diagnosticTimer);
            try {
                closeSync(ownerDescriptor);
            }
            catch { }
            try {
                unlinkSync(ownerFile);
            }
            catch { }
        });
    }
}
// Direct instrumentation must initialize the collector before script files
// with no import boundary evaluate. Other modes let the runner adapter or an
// instrumented module load it, except when durable remote evidence requires it.
if (process.env.SUPERCOV_DURABLE_EVIDENCE_EACH_TEST === "1" ||
    process.env.SUPERCOV_DIRECT_INSTRUMENTATION === "1") {
    // A translated remote/VM command can execute an ahead-of-run transformed
    // test through an opaque runner that bypasses the ordinary Playwright or
    // node:test import boundary. The remote-launch adapter marks that process;
    // initialize the runtime before its transformed module can evaluate.
    globalThis.__SUPERCOV_DIRECT_RUNTIME__ ??= await import("./runtime.mjs");
    process.__SUPERCOV_DIRECT_RUNTIME__ ??= globalThis.__SUPERCOV_DIRECT_RUNTIME__;
    // Generic builds can contain script files with no import/export syntax.
    // They cannot import the collector without changing their module semantics,
    // and may evaluate before any ESM instrumented file imports it.
    globalThis.__supercovRuntime ??= globalThis.__SUPERCOV_DIRECT_RUNTIME__;
}
// Workers are independent Node processes and an explicit `execArgv: []`
// otherwise strips the preload that supplies the isolated runtime. Preserve
// every user option while adding exactly one Supercov import.
const workerThreads = Module._load("node:worker_threads", undefined, false);
const NativeWorker = workerThreads.Worker;
const registerArgument = `--import=${new URL("./register.mjs", import.meta.url).href}`;
const appendRegister = values => values.some(value => value === registerArgument || value.includes("/.supercov/node_modules/register.mjs"))
    ? values
    : [...values, registerArgument];
workerThreads.Worker = class SupercovWorker extends NativeWorker {
    constructor(filename, options = {}) {
        const workerOptions = { ...options };
        let workerEntrypoint = filename;
        // Node currently records --import in an eval Worker's process.execArgv but
        // does not execute the preload. Bootstrap the unchanged source after the
        // register promise instead so eval workers receive the same runtime.
        if (options.eval === true && typeof filename === "string") {
            workerEntrypoint = `import(${JSON.stringify(new URL("./register.mjs", import.meta.url).href)}).then(() => {\n${filename}\n}).catch(error => queueMicrotask(() => { throw error; }));`;
        }
        // Omitted execArgv already inherits Node's internally validated parent
        // flags. Only an explicit override needs the Supercov import restored.
        if (options.eval !== true && options.execArgv !== undefined)
            workerOptions.execArgv = appendRegister(options.execArgv);
        super(workerEntrypoint, {
            ...workerOptions,
        });
    }
};
syncBuiltinESMExports();
// NODE_OPTIONS reaches commands launched through npm scripts. When that child
// is Vitest, replace its config with our generated merging config before the
// CLI parses argv. This is what makes `supercov -- npm test` work
// without editing package scripts, Vitest configs, setup files, or test imports.
const generatedVitestConfig = process.env.SUPERCOV_GENERATED_VITEST_CONFIG;
const generatedPlaywrightConfig = process.env.SUPERCOV_GENERATED_PLAYWRIGHT_CONFIG;
const entrypoint = process.argv[1]?.replaceAll("\\", "/") ?? "";
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
if (NODE_COVERAGE_FILTERS) {
    const compile = Module.prototype._compile;
    Module.prototype._compile = function _compile(content, filename, ...rest) {
        return Reflect.apply(compile, this, [mapToCopy(content, filename), filename, ...rest]);
    };
}
const playwrightTarget = process.env.SUPERCOV_PLAYWRIGHT_MODULE;
const projectRoot = process.env.SUPERCOV_PROJECT_ROOT?.replaceAll("\\", "/").replace(/\/$/, "");
const nodeTestWrapper = new URL("./nodeTest.mjs", import.meta.url).href;
const nodeAssertWrapper = new URL("./nodeAssert.mjs", import.meta.url).href;
const nodeAssertStrictWrapper = new URL("./nodeAssertStrict.mjs", import.meta.url).href;
const isPlaywrightEntrypoint = /\/(?:node_modules\/\.bin\/playwright|node_modules\/(?:@playwright\/test|playwright)\/(?:cli\.js|.*\/program\.js))$/.test(entrypoint);
const isJestEntrypoint = /\/node_modules\/(?:\.bin\/jest|(?:jest|jest-cli)\/bin\/jest\.js)$/.test(entrypoint);
if (generatedPlaywrightConfig && isPlaywrightEntrypoint)
    process.env.SUPERCOV_INSIDE_PLAYWRIGHT = "1";
// Linters, formatters and type checks read the project to judge it, not to
// run it. The workspace holds rewritten copies, and ESLint failed semver,
// js-yaml and picomatch on code nobody wrote ("Strings must use singlequote",
// "'globalThis' is not defined"), Supercov's own .supercov/*.mjs included.
// Such a process reads every rewritten file as its author wrote it, and no
// directory listing in the workspace shows .supercov.
const analysisTools = "eslint|eslint_d|prettier|standard|semistandard|ts-standard|xo|jshint|tslint";
const isAnalysisEntrypoint = new RegExp(`/node_modules/(?:\\.bin/(?:${analysisTools})$|(?:${analysisTools})/)`).test(entrypoint) ||
    (/\/node_modules\/(?:\.bin\/(?:tsc|vue-tsc)$|(?:typescript|vue-tsc)\/bin\/)/.test(entrypoint) &&
        process.argv.includes("--noEmit"));
if (isAnalysisEntrypoint)
    installAuthoredSourceView();
// `next build` type-checks in a worker of its own instead of running tsc. A
// probe in a condition stops TypeScript narrowing through it, so an inferred
// return type widens, and a file outside the source roots that calls the
// function fails the build on code nobody wrote ("'client' is possibly
// 'null'"). Only that worker gets the authored view: the bundler, and `next
// dev`, which sets TypeScript up in its main process, must read the copies.
else if (/\/node_modules\/next\/dist\/compiled\/jest-worker\/processChild\.js$/.test(entrypoint) ||
    (!workerThreads.isMainThread && /\/node_modules\/(?:\.bin\/next$|next\/dist\/)/.test(entrypoint))) {
    const compile = Module.prototype._compile;
    let installed = false;
    Module.prototype._compile = function _compile(content, filename, ...rest) {
        if (!installed && /[\\/]next[\\/]dist[\\/]lib[\\/]verify-typescript-setup\.js$/.test(filename)) {
            installed = true;
            installAuthoredSourceView();
        }
        return Reflect.apply(compile, this, [content, filename, ...rest]);
    };
}
function installAuthoredSourceView() {
    let authored;
    try {
        authored = new Set(JSON.parse(readFileSync(new URL("./authored-sources.json", import.meta.url), "utf8")));
    }
    catch {
        authored = new Set();
    }
    const authoredRoot = fileURLToPath(new URL("./.authored/", import.meta.url));
    const workspace = fileURLToPath(new URL("../../", import.meta.url));
    const roots = [...new Set([workspace, (() => {
                try {
                    return realpathSync(workspace);
                }
                catch {
                    return workspace;
                }
            })()])];
    const inside = (path) => {
        if (typeof path !== "string" && !(path instanceof URL))
            return undefined;
        let absolute;
        try {
            absolute = resolve(path instanceof URL ? fileURLToPath(path) : path);
        }
        catch {
            return undefined;
        }
        for (const root of roots) {
            const local = relative(root, absolute);
            if (local !== ".." && !local.startsWith(`..${sep}`) && !isAbsolute(local))
                return local.split(sep).join("/");
        }
        return undefined;
    };
    const authoredPath = (path) => {
        const local = inside(path);
        return local !== undefined && authored.has(local) ? resolve(authoredRoot, local) : path;
    };
    // A recursive listing names nested entries by relative path, or as
    // entries whose parent is inside .supercov.
    const supercovPath = (path) => typeof path === "string" && path.split(/[\\/]/).includes(".supercov");
    const hidden = (entry) => {
        if (typeof entry === "string")
            return supercovPath(entry);
        if (entry instanceof Uint8Array)
            return supercovPath(new TextDecoder().decode(entry));
        const parent = inside(entry?.parentPath ?? entry?.path ?? "");
        return entry?.name === ".supercov" || supercovPath(parent);
    };
    const listed = (path, entries) => inside(path) === undefined || !Array.isArray(entries)
        ? entries
        : entries.filter((entry) => !hidden(entry));
    const { readFileSync: readSync, readFile: readCallback, readdirSync: listSync, readdir: listCallback } = fs;
    const { readFile: readPromise, readdir: listPromise } = fs.promises;
    fs.readFileSync = function readFileSync(path, ...rest) {
        return Reflect.apply(readSync, this, [authoredPath(path), ...rest]);
    };
    fs.readFile = function readFile(path, ...rest) {
        return Reflect.apply(readCallback, this, [authoredPath(path), ...rest]);
    };
    fs.promises.readFile = function readFile(path, ...rest) {
        return Reflect.apply(readPromise, this, [authoredPath(path), ...rest]);
    };
    fs.readdirSync = function readdirSync(path, ...rest) {
        return listed(path, Reflect.apply(listSync, this, [path, ...rest]));
    };
    fs.readdir = function readdir(path, ...rest) {
        const callback = rest.at(-1);
        if (typeof callback !== "function")
            return Reflect.apply(listCallback, this, [path, ...rest]);
        return Reflect.apply(listCallback, this, [path, ...rest.slice(0, -1), (error, entries) => callback(error, error ? entries : listed(path, entries))]);
    };
    fs.promises.readdir = async function readdir(path, ...rest) {
        return listed(path, await Reflect.apply(listPromise, this, [path, ...rest]));
    };
    syncBuiltinESMExports();
}
// A project's own coverage tool measures what runs, and under Supercov that is
// the instrumented copy, whose probes are branches no test was written to
// cover: c8, Jest and Vitest each failed a 100% gate on a suite that covers
// every branch (80%, 83.33%, 87.5%). The tool still runs and reports; its
// thresholds are not checked, and the run says so once. Jest's and Vitest's
// thresholds are dropped from the configurations Supercov hands them, which
// call this too.
process.__SUPERCOV_SKIPPED_COVERAGE_THRESHOLDS__ ??= (tool) => {
    if (process.__SUPERCOV_SKIPPED_COVERAGE_THRESHOLDS_NOTED__)
        return;
    process.__SUPERCOV_SKIPPED_COVERAGE_THRESHOLDS_NOTED__ = true;
    console.error(`[supercov] ${tool}'s coverage thresholds were not checked: under Supercov, ${tool} measures the instrumented copy, probes included. Supercov's own report has this run's coverage.`);
};
const coverageTool = /\/node_modules\/(?:\.bin\/(c8|nyc)|(c8|nyc)\/bin\/(?:c8|nyc)\.js)$/.exec(entrypoint);
if (coverageTool)
    skipCoverageThresholds(coverageTool[1] ?? coverageTool[2]);
if (/\/node_modules\/(?:\.bin\/karma|karma\/bin\/karma)$/.test(entrypoint))
    skipKarmaCoverageCheck();
// karma-coverage checks `coverageReporter.check` in the reporter karma builds
// from its plugin, which the karma process requires by name from the project.
// Loaded first, the plugin's constructor is replaced by one that leaves the
// check out of the configuration it is given.
function skipKarmaCoverageCheck() {
    try {
        const plugin = Module.createRequire(resolve(process.cwd(), "package.json"))("karma-coverage");
        const entry = plugin?.["reporter:coverage"];
        if (!Array.isArray(entry) || typeof entry[1] !== "function")
            return;
        const Reporter = entry[1];
        function CoverageReporter(rootConfig, ...rest) {
            const options = rootConfig?.coverageReporter;
            if (options && Object.prototype.hasOwnProperty.call(options, "check")) {
                delete options.check;
                process.__SUPERCOV_SKIPPED_COVERAGE_THRESHOLDS__("karma-coverage");
            }
            return Reflect.construct(Reporter, [rootConfig, ...rest], new.target);
        }
        CoverageReporter.$inject = Reporter.$inject;
        CoverageReporter.prototype = Reporter.prototype;
        plugin["reporter:coverage"] = [entry[0], CoverageReporter];
    }
    catch (error) {
        if (process.env.SUPERCOV_DEBUG === "1")
            console.error("[supercov] karma-coverage's check stays as configured", error);
    }
}
// nyc (through 18) remaps with istanbul-lib-source-maps 4, which drops an
// `if`'s implicit else from any file with a source map: it has no location to
// map. The instrumented copy has one, so nyc read one branch fewer for every
// `if` without an `else` than it reads without Supercov. Version 5 keeps the
// location in the file of the branch's other one; so does this, loaded before
// nyc's remapping captures the function.
function keepImplicitElse(toolRequire) {
    try {
        const path = toolRequire.resolve("istanbul-lib-source-maps/lib/get-mapping.js");
        const getMapping = toolRequire(path);
        const cached = Module._cache[path];
        if (typeof getMapping !== "function" || !cached)
            return;
        let lastSource;
        cached.exports = function getMappingKeepingImplicitElse(sourceMap, location, originalFile) {
            const mapping = getMapping(sourceMap, location, originalFile);
            if (mapping) {
                lastSource = mapping.source;
                return mapping;
            }
            const implicit = location?.start?.line === undefined && location?.end?.line === undefined;
            const source = lastSource;
            lastSource = undefined;
            return implicit && source !== undefined ? { source, loc: location } : mapping;
        };
    }
    catch (error) {
        if (process.env.SUPERCOV_DEBUG === "1")
            console.error("[supercov] nyc keeps its own remapping", error);
    }
}
function skipCoverageThresholds(tool) {
    const skipped = async function skippedCoverageThresholds() {
        process.__SUPERCOV_SKIPPED_COVERAGE_THRESHOLDS__(tool);
    };
    try {
        // Resolved from the tool's own bin, so the module patched is the one
        // its CLI loads next (npm links .bin/c8 to c8/bin/c8.js).
        const toolRequire = Module.createRequire(realpathSync(process.argv[1]));
        if (tool === "c8") {
            // Every c8 threshold -- --check-coverage, --100, .c8rc, .nycrc,
            // package.json, `c8 check-coverage` -- is checked here, and
            // report.js takes this export when it loads.
            toolRequire("../lib/commands/check-coverage.js").checkCoverages = skipped;
        }
        else {
            keepImplicitElse(toolRequire);
            // A gated nyc run, `nyc report --check-coverage` and
            // `nyc check-coverage` all check through this method.
            const NYC = toolRequire("../index.js");
            NYC.prototype.checkCoverage = skipped;
            // The instrumented copy's source map leads to the project's own
            // file, outside the isolated workspace nyc runs in, and
            // excluding after remapping (nyc's default) dropped it: nyc
            // reported "All files 0". The project's excludes still apply,
            // to the copies, before remapping.
            const collect = NYC.prototype.getCoverageMapFromAllCoverageFiles;
            if (typeof collect === "function") {
                NYC.prototype.getCoverageMapFromAllCoverageFiles = function getCoverageMapFromAllCoverageFiles(...args) {
                    this.config.excludeAfterRemap = false;
                    return Reflect.apply(collect, this, args);
                };
            }
        }
    }
    catch (error) {
        if (process.env.SUPERCOV_DEBUG === "1")
            console.error(`[supercov] ${tool}'s coverage thresholds stay as configured`, error);
    }
}
// A handler for `.ts` in the CommonJS loader that is not Node's own is a
// transpiler's require hook: ts-node/register, @swc/register and the like.
// It was there before this preload, and it is how the project's TypeScript
// loads. Any `--import` sends the entry point through the module loader
// instead, where such a file is read as an ES module and the hook never
// sees it; the loader is told so it can hand those files back.
const typescriptHandler = Module._extensions[".ts"];
register(new URL("./resolve-loader.mjs", import.meta.url), {
    data: { typescriptRequireHook: typeof typescriptHandler === "function" && typescriptHandler.name !== "loadTS" },
});
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
        }
        else if (argument?.startsWith("--config=")) {
            const resolvedConfig = resolve(process.cwd(), argument.slice("--config=".length));
            if (resolvedConfig !== resolve(generatedVitestConfig)) {
                originalConfig = resolvedConfig;
            }
            process.argv.splice(index, 1);
            index -= 1;
        }
    }
    // Thresholds on the command line override the configuration's, which the
    // generated config leaves out (see the coverage tools above). Vitest 0.x
    // took them directly under --coverage.
    for (let index = 2; index < process.argv.length; index += 1) {
        const argument = process.argv[index];
        if (!/^--coverage\.(?:thresholds(?:\.[^=]+)?|lines|functions|branches|statements|perFile|100|thresholdAutoUpdate)(?:=|$)/.test(argument))
            continue;
        const valued = !argument.includes("=") && /^(?:\d+(?:\.\d+)?|true|false)$/.test(process.argv[index + 1] ?? "");
        process.argv.splice(index, valued ? 2 : 1);
        index -= 1;
        process.__SUPERCOV_SKIPPED_COVERAGE_THRESHOLDS__("Vitest");
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
if ((isJestEntrypoint || process.env.JEST_WORKER_ID) && process.env.SUPERCOV_EVIDENCE_DIR) {
    // Jest evaluates instrumented modules in its own module system, past the
    // loader hook that imports the runtime on first use elsewhere, and
    // jest-environment-node exposes to each test's sandbox only the globals
    // the outer process held when that class loaded. The Jest process and
    // each of its workers (JEST_WORKER_ID) therefore load the runtime first;
    // without this, a suite with more than one test file failed on every
    // instrumented module's first line inside the workers.
    globalThis.__SUPERCOV_DIRECT_RUNTIME__ ??= await import("./runtime.mjs");
    process.__SUPERCOV_DIRECT_RUNTIME__ ??= globalThis.__SUPERCOV_DIRECT_RUNTIME__;
}
// The Mocha CLI, the child it forks for Node options, and each --parallel
// worker run tests through Mocha's Runner, which the adapter patches before
// Mocha loads it.
const isMochaEntrypoint = /\/node_modules\/(?:\.bin\/_?mocha|mocha\/(?:bin\/_?mocha(?:\.js)?|lib\/cli\/cli\.js|lib\/nodejs\/worker\.c?js))$/.test(entrypoint);
if (isMochaEntrypoint && process.env.SUPERCOV_EVIDENCE_DIR) {
    globalThis.__SUPERCOV_DIRECT_RUNTIME__ ??= await import("./runtime.mjs");
    process.__SUPERCOV_DIRECT_RUNTIME__ ??= globalThis.__SUPERCOV_DIRECT_RUNTIME__;
    const { adaptMocha } = await import(new URL("./mocha.mjs", import.meta.url).href);
    await adaptMocha(entrypoint);
}
// An AVA worker, a thread or a child process, runs one test file through
// AVA's Runner, which the adapter patches before the worker loads it.
if (/\/node_modules\/ava\/lib\/worker\/base\.js$/.test(entrypoint) && process.env.SUPERCOV_EVIDENCE_DIR) {
    globalThis.__SUPERCOV_DIRECT_RUNTIME__ ??= await import("./runtime.mjs");
    process.__SUPERCOV_DIRECT_RUNTIME__ ??= globalThis.__SUPERCOV_DIRECT_RUNTIME__;
    const { adaptAva } = await import(new URL("./ava.mjs", import.meta.url).href);
    await adaptAva(entrypoint);
}
// The tap CLI runs each test file as a process of its own, marked with
// TAP_CHILD_ID; the adapter patches tap's TestBase before the file loads it.
if (process.env.TAP_CHILD_ID && process.env.SUPERCOV_EVIDENCE_DIR && entrypoint) {
    globalThis.__SUPERCOV_DIRECT_RUNTIME__ ??= await import("./runtime.mjs");
    process.__SUPERCOV_DIRECT_RUNTIME__ ??= globalThis.__SUPERCOV_DIRECT_RUNTIME__;
    const { adaptTap } = await import(new URL("./tap.mjs", import.meta.url).href);
    await adaptTap(entrypoint);
}
if (isJestEntrypoint && process.env.SUPERCOV_EVIDENCE_DIR) {
    // Jest reads one configuration. Ours (jest.config.mjs) reads the user's
    // the way Jest would and adds the adapter and reporter; an explicit
    // --config on the command line reaches it through the environment.
    const generatedJestConfig = fileURLToPath(new URL("./jest.config.mjs", import.meta.url));
    for (let index = 2; index < process.argv.length; index += 1) {
        const argument = process.argv[index];
        if (argument === "--config" || argument === "-c") {
            const value = process.argv[index + 1];
            // Expo's Jest executable forwards argv to the real Jest process.
            // Preserve the original config across that second preload; reading
            // our own config as the user's would recurse until the heap fills.
            if (value && resolve(value) !== resolve(generatedJestConfig))
                process.env.SUPERCOV_ORIGINAL_JEST_CONFIG = resolve(value);
            process.argv.splice(index, value ? 2 : 1);
            index -= 1;
        }
        else if (argument?.startsWith("--config=")) {
            const value = resolve(argument.slice("--config=".length));
            if (value !== resolve(generatedJestConfig))
                process.env.SUPERCOV_ORIGINAL_JEST_CONFIG = value;
            process.argv.splice(index, 1);
            index -= 1;
        }
        else if (/^--coverage-?[Tt]hreshold(?:=|$)/.test(argument ?? "")) {
            // Overrides the configuration's coverageThreshold, which
            // jest.config.mjs leaves out (see the coverage tools above).
            const valued = !argument.includes("=") && process.argv[index + 1]?.startsWith("{");
            process.argv.splice(index, valued ? 2 : 1);
            index -= 1;
            process.__SUPERCOV_SKIPPED_COVERAGE_THRESHOLDS__("Jest");
        }
    }
    process.argv.push("--config", generatedJestConfig);
}
if (generatedPlaywrightConfig &&
    isPlaywrightEntrypoint) {
    for (let index = 2; index < process.argv.length; index += 1) {
        const argument = process.argv[index];
        if (argument === "--config" || argument === "-c") {
            process.argv.splice(index, process.argv[index + 1] ? 2 : 1);
            index -= 1;
        }
        else if (argument?.startsWith("--config=")) {
            process.argv.splice(index, 1);
            index -= 1;
        }
    }
    process.argv.push("--config", generatedPlaywrightConfig);
}
if (process.env.SUPERCOV_CJS_INTERCEPT === "1" &&
    process.env.SUPERCOV_INSIDE_PLAYWRIGHT === "1" &&
    !isPlaywrightEntrypoint &&
    process.env.SUPERCOV_INSIDE_VITEST !== "1" &&
    process.env.VITEST !== "true") {
    const target = playwrightTarget ?? "@playwright/test";
    const projectRoot = process.env.SUPERCOV_PROJECT_ROOT;
    const originalPlaywrightConfig = process.env.SUPERCOV_ORIGINAL_PLAYWRIGHT_CONFIG
        ?.replaceAll("\\", "/");
    const wrapper = await import(__rewriteRelativeImportExtension(new URL("./playwright.mjs", import.meta.url)));
    const originalLoad = Module._load;
    Module._load = function supercovLoad(request, parent, isMain) {
        const parentFile = parent?.filename?.replaceAll("\\", "/");
        const normalizedRoot = projectRoot
            ?.replaceAll("\\", "/")
            .replace(/\/$/, "");
        const generatedRoot = normalizedRoot
            ? `${normalizedRoot}/.supercov/`
            : undefined;
        const belongsToProject = Boolean(parentFile) &&
            !parentFile.includes("/node_modules/") &&
            parentFile !== originalPlaywrightConfig &&
            (!generatedRoot || !parentFile.startsWith(generatedRoot)) &&
            (normalizedRoot
                ? parentFile.startsWith(`${normalizedRoot}/`)
                : parentFile.includes("/tests/"));
        if (request === target && belongsToProject)
            return wrapper;
        return originalLoad.call(this, request, parent, isMain);
    };
}
// CJS test files receive the same node:test adapter as ESM files. The adapter
// itself imports the native built-in from Supercov's generated directory, so
// only first-party callers are redirected and recursion is impossible.
if (process.env.SUPERCOV_CJS_INTERCEPT === "1") {
    const entrypointBelongsToProject = Boolean(projectRoot && entrypoint.startsWith(`${projectRoot}/`));
    const runnerCanLoadProjectCode = entrypointBelongsToProject ||
        entrypoint === "" ||
        process.env.SUPERCOV_INSIDE_VITEST === "1" ||
        (process.env.SUPERCOV_INSIDE_PLAYWRIGHT === "1" && !isPlaywrightEntrypoint);
    if (!runnerCanLoadProjectCode) {
        // Package-manager and Playwright coordinator processes propagate the
        // preload to their children but never evaluate project test modules.
        // Their worker/child entrypoints install these synchronous CJS hooks.
        // Avoid paying to import three runner adapters in every launcher.
    }
    else {
    const nodeTestAdapter = await import(__rewriteRelativeImportExtension(nodeTestWrapper));
    const cjsNodeTestAdapter = Object.assign(nodeTestAdapter.test, nodeTestAdapter);
    const nodeAssertAdapter = await import(__rewriteRelativeImportExtension(nodeAssertWrapper));
    const cjsNodeAssertAdapter = Object.assign(nodeAssertAdapter.default, nodeAssertAdapter);
    const nodeAssertStrictAdapter = await import(__rewriteRelativeImportExtension(nodeAssertStrictWrapper));
    const cjsNodeAssertStrictAdapter = Object.assign(nodeAssertStrictAdapter.default, nodeAssertStrictAdapter);
    const originalLoad = Module._load;
    Module._load = function supercovNodeTestLoad(request, parent, isMain) {
        const parentFile = parent?.filename?.replaceAll("\\", "/");
        const belongsToProject = Boolean(projectRoot &&
            parentFile?.startsWith(`${projectRoot}/`) &&
            !parentFile.includes("/node_modules/") &&
            !parentFile.startsWith(`${projectRoot}/.supercov/`));
        if (process.env.SUPERCOV_DEBUG === "1" &&
            (request === "node:test" || request === "test"))
            console.error("[supercov] node:test CJS request", { parentFile, projectRoot, belongsToProject });
        if ((request === "node:test" || request === "test") && belongsToProject)
            return cjsNodeTestAdapter;
        if (["assert", "node:assert", "assert/strict", "node:assert/strict"].includes(request) &&
            belongsToProject)
            return request.endsWith("/strict")
                ? cjsNodeAssertStrictAdapter
                : cjsNodeAssertAdapter;
        return originalLoad.call(this, request, parent, isMain);
    };
    }
}
