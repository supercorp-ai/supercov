// Supercov's Jest configuration, handed to Jest through `--config` by the
// preload (register.mjs) in place of the user's. It reads the user's own
// configuration the way Jest reads it -- a jest.config.* file in any format
// Jest accepts, or the "jest" key of package.json -- and adds the adapter
// (jest.cjs) and the reporter (jestReporter.mjs). Nothing of the user's is
// replaced: their setup files and reporters keep running.
import { readFileSync, realpathSync } from "node:fs";
import { createRequire } from "node:module";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const here = (name) => fileURLToPath(new URL(`./${name}`, import.meta.url));
const asList = (value) => (value === undefined ? [] : Array.isArray(value) ? value : [value]);

// The jest-config of the Jest that is running, so version and resolution
// match Jest's. npm may leave it only under jest-cli or @jest/core
// (node_modules/jest-cli/node_modules/jest-config), where the project root
// does not see it: from the root, then from the running Jest's own packages.
function jestConfig() {
    const fromRoot = createRequire(resolve(process.cwd(), "package.json"));
    const bases = [fromRoot];
    try {
        bases.push(createRequire(realpathSync(process.argv[1])));
    } catch {}
    for (const base of [...bases]) {
        for (const owner of ["jest-cli/package.json", "@jest/core/package.json", "jest/package.json"]) {
            try {
                bases.push(createRequire(base.resolve(owner)));
            } catch {}
        }
    }
    let first;
    for (const base of bases) {
        try {
            const path = base.resolve("jest-config");
            return { module: base(path), path };
        } catch (error) {
            first ??= error;
        }
    }
    throw first;
}

// The project's own options, unnormalized, as Jest 29.3 and later read them
// (`readInitialOptions`). Earlier Jest (27, 28) has the same two steps as
// modules of its build, which its package's exports hide from a require by
// name: they are required beside its entry.
async function readProjectOptions(original) {
    const { module, path } = jestConfig();
    if (typeof module.readInitialOptions === "function")
        return module.readInitialOptions(original, { skipMultipleConfigError: true });
    if (original && module.isJSONString?.(original))
        return { config: JSON.parse(original), configPath: null };
    const internal = createRequire(path);
    const step = (name) => {
        const loaded = internal(`./${name}`);
        return loaded.default ?? loaded;
    };
    const configPath = step("resolveConfigPath")(original ?? process.cwd(), process.cwd(), true);
    const config = await step("readConfigFileAndSetRootDir")(configPath);
    return { config, configPath };
}

// A strong pass's job (strongJob.mjs) runs one of the run's test files: the
// run's others are left out as Jest finds them.
function strongJobFiles(config) {
    const path = process.env["SUPERCOV_STRONG_JOB"];
    if (!path)
        return {};
    let spec;
    try {
        spec = JSON.parse(readFileSync(path, "utf8"));
    } catch {
        return {};
    }
    const running = new Set(Object.keys(spec.files ?? {}));
    const escape = (text) => text.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
    const left = (spec.known ?? []).filter((file) => !running.has(file))
        .map((file) => `^${escape(resolve(process.cwd(), file))}$`);
    return { testPathIgnorePatterns: [...asList(config.testPathIgnorePatterns ?? "/node_modules/"), ...left] };
}

export default async function supercovJestConfig() {
    let config = { rootDir: process.cwd() };
    try {
        const original = process.env["SUPERCOV_ORIGINAL_JEST_CONFIG"] || undefined;
        const read = await readProjectOptions(original);
        config = read.config ?? config;
        config.rootDir ??= read.configPath ? dirname(read.configPath) : process.cwd();
    } catch (error) {
        // A configuration Jest itself could not read either, or a Jest
        // whose jest-config is not where Jest 27-30 keep it.
        process.emitWarning(`[supercov] the project's Jest configuration was not read (${error?.message ?? error}); Jest's defaults apply`);
    }
    // Jest's coverage measures the instrumented copy, probes included, and a
    // suite at 100% read 83.33% of branches: its coverageThreshold failed
    // passing runs. Jest still collects and reports coverage.
    const { coverageThreshold, ...own } = config;
    const collecting = own.collectCoverage === true ||
        process.argv.some((argument) => /^--(?:coverage|collectCoverage|collect-coverage)(?:=true)?$/.test(argument));
    if (coverageThreshold && collecting)
        process.__SUPERCOV_SKIPPED_COVERAGE_THRESHOLDS__?.("Jest");
    return {
        ...own,
        ...strongJobFiles(own),
        // Concurrent tests in one file would share the worker's scope.
        maxConcurrency: 1,
        setupFiles: [here("jestRuntime.cjs"), ...asList(config.setupFiles)],
        // Babel-based presets (including React Native) lower the runtime
        // side-effect import in instrumented tests to require(). Jest cannot
        // require our ESM shim. Its environment already owns the runtime, so
        // resolve only our generated import to a CommonJS bridge instead.
        moduleNameMapper: {
            "(?:^|/)\\.supercov/node_modules/runtime\\.mjs$": here("jestRuntime.cjs"),
            ...config.moduleNameMapper,
        },
        setupFilesAfterEnv: [here("jest.cjs"), ...asList(config.setupFilesAfterEnv)],
        reporters: [
            ...(config.reporters === undefined ? ["default"] : asList(config.reporters)),
            here("jestReporter.mjs"),
        ],
    };
}
