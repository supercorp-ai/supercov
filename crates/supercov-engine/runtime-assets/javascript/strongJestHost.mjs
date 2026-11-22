// A resident Jest for a strong pass's jobs (strongJobs.mjs): one process that
// keeps Jest loaded, as StrykerJS's Jest runner does, and runs one job after
// another in band, each test file in the fresh module registry and
// environment Jest gives every test file. The job's mutant is switched on
// here before Jest loads the file's modules; its tests and outcomes go
// through the Jest adapter as in a job started from the command.
//
// The driver forks it in the project's copy and sends
//   { type: "init", args: [<the command's arguments to jest>] }
//   { type: "job", id, file: <absolute test file>, mutant: { file, index } | null, environment: {...} }
// and it answers { type: "ready" } and { type: "done", id, error? }.
import { createRequire } from "node:module";
import { join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

globalThis.__SUPERCOV_DIRECT_RUNTIME__ ??= await import("./runtime.mjs");
process.__SUPERCOV_DIRECT_RUNTIME__ ??= globalThis.__SUPERCOV_DIRECT_RUNTIME__;
const runtime = globalThis.__SUPERCOV_DIRECT_RUNTIME__;
const generatedConfig = fileURLToPath(new URL("./jest.config.mjs", import.meta.url));

// The project's Jest: its CLI's argument parser and its core.
function projectJest(root) {
    const fromRoot = createRequire(join(root, "package.json"));
    let cli;
    try {
        cli = fromRoot.resolve("jest-cli");
    }
    catch {
        cli = createRequire(fromRoot.resolve("jest/package.json")).resolve("jest-cli");
    }
    const { buildArgv } = createRequire(cli)("jest-cli");
    const { runCLI } = createRequire(cli)("@jest/core");
    return { buildArgv, runCLI };
}

let jest;
let base;
const root = process.cwd();

async function init(args) {
    jest = projectJest(root);
    base = await jest.buildArgv(args);
    // The project's own configuration, read by Supercov's (jest.config.mjs).
    if (base.config && resolve(String(base.config)) !== generatedConfig)
        process.env.SUPERCOV_ORIGINAL_JEST_CONFIG = resolve(String(base.config));
}

async function runJob({ file, mutant, environment }) {
    Object.assign(process.env, environment);
    if (mutant)
        runtime.setActiveMutant(mutant.file, mutant.index);
    else
        runtime.clearActiveMutant();
    try {
        const argv = {
            ...base,
            _: [file],
            config: generatedConfig,
            runTestsByPath: true,
            runInBand: true,
            watch: false,
            watchAll: false,
        };
        await jest.runCLI(argv, Array.isArray(base.projects) && base.projects.length ? base.projects : [root]);
    }
    finally {
        runtime.clearActiveMutant();
    }
}

process.on("message", (message) => {
    const reply = (answer) => process.send?.(answer);
    if (message?.type === "init") {
        init(message.args).then(() => reply({ type: "ready" }), (error) => reply({ type: "ready", error: String(error?.stack ?? error) }));
        return;
    }
    if (message?.type === "job") {
        runJob(message).then(() => reply({ type: "done", id: message.id }), (error) => reply({ type: "done", id: message.id, error: String(error?.stack ?? error) }));
    }
});
process.send?.({ type: "started" });
