// A runner started by its own entry point -- Vitest, Mocha, an AVA worker, a
// tap child -- is adapted as it starts: Vitest takes the run's generated
// config, the others' runners are patched before they load. Without it every
// probe a test reaches is counted as background. Node runs this from
// register.mjs before the CLI parses argv; Bun loads no Node preload, so its
// adapter does instead.
import { resolve } from "node:path";

// A project's own coverage tool measures what runs, and under Supercov that is
// the instrumented copy, whose probes are branches no test was written to
// cover: c8, Jest and Vitest each failed a 100% gate on a suite that covers
// every branch (80%, 83.33%, 87.5%). The tool still runs and reports; its
// thresholds are not checked, and the run says so once.
export function noteSkippedCoverageThresholds(tool) {
    if (process.__SUPERCOV_SKIPPED_COVERAGE_THRESHOLDS_NOTED__)
        return;
    process.__SUPERCOV_SKIPPED_COVERAGE_THRESHOLDS_NOTED__ = true;
    console.error(`[supercov] ${tool}'s coverage thresholds were not checked: under Supercov, ${tool} measures the instrumented copy, probes included. Supercov's own report has this run's coverage.`);
}

/** Point a Vitest process at the run's generated config. */
function useGeneratedVitestConfig(entrypoint) {
    const generatedVitestConfig = process.env.SUPERCOV_GENERATED_VITEST_CONFIG;
    if (!generatedVitestConfig || !/\/vitest(?:\.m?js)?$/.test(entrypoint))
        return;
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
        noteSkippedCoverageThresholds("Vitest");
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

/** Adapt the runner whose entry point `entrypoint` is, if any. */
export async function adaptRunnerEntry(entrypoint) {
    useGeneratedVitestConfig(entrypoint);
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
}
