// A resident Mocha for a strong pass's jobs (strongJobs.mjs), as StrykerJS's
// Mocha runner keeps one: one process that keeps Mocha and the project's
// required modules loaded, and runs one job after another. Before each job
// every CommonJS module of the project is taken out of `require.cache`, so the
// job's test file and the code it tests load afresh, with the job's mutant
// switched on, as in a process of their own. (A suite of ES modules, which
// cannot be loaded again, runs a process per job instead: the driver does
// not start this for it.) Its tests and outcomes go through the Mocha adapter.
//
// The driver forks it in the project's copy and sends
//   { type: "init", args: [<the command's arguments to mocha>] }
//   { type: "job", id, file: <absolute test file>, mutant: { file, index } | null, environment: {...} }
// and it answers { type: "ready" } and { type: "done", id, error? }.
import { existsSync } from "node:fs";
import { createRequire } from "node:module";
import { join, resolve, sep } from "node:path";
import { pathToFileURL } from "node:url";

globalThis.__SUPERCOV_DIRECT_RUNTIME__ ??= await import("./runtime.mjs");
process.__SUPERCOV_DIRECT_RUNTIME__ ??= globalThis.__SUPERCOV_DIRECT_RUNTIME__;
const runtime = globalThis.__SUPERCOV_DIRECT_RUNTIME__;
const root = process.cwd();
const require = createRequire(join(root, "package.json"));

let Mocha;
let options;
let rootHooks;

async function init(args) {
    const mochaRoot = resolve(require.resolve("mocha/package.json"), "..");
    const { adaptMocha } = await import(new URL("./mocha.mjs", import.meta.url).href);
    await adaptMocha(join(mochaRoot, "package.json"));
    // Loaded as the adapter loads Mocha's runner, so both see one Mocha.
    // Mocha 10 and 11 name these .js; Mocha 12, .cjs where they are CommonJS.
    const load = async (name) => {
        for (const extension of [".js", ".cjs"]) {
            const path = join(mochaRoot, "lib", `${name}${extension}`);
            if (!existsSync(path))
                continue;
            const loaded = await import(pathToFileURL(path).href);
            return loaded.default ?? loaded;
        }
        throw new Error(`Mocha has no lib/${name}`);
    };
    const mocha = await load("mocha");
    Mocha = mocha.Mocha ?? mocha;
    const { loadOptions } = await load("cli/options");
    const { handleRequires } = await load("cli/run-helpers");
    options = loadOptions(args);
    // What --require loads (a root hooks plugin, a register hook) is loaded
    // once, as the command loads it once.
    const plugins = await handleRequires(options.require ?? [], { ignoredPlugins: ["mochaGlobalSetup", "mochaGlobalTeardown"] });
    rootHooks = plugins?.rootHooks;
}

// The project's own modules, outside node_modules and Supercov's directory.
function forgetProjectModules() {
    const ours = `${sep}.supercov${sep}node_modules${sep}`;
    for (const file of Object.keys(require.cache)) {
        if (file.startsWith(root + sep) && !file.includes(`${sep}node_modules${sep}`) && !file.includes(ours))
            delete require.cache[file];
    }
}

async function runJob({ file, mutant, environment }) {
    Object.assign(process.env, environment);
    forgetProjectModules();
    if (mutant)
        runtime.setActiveMutant(mutant.file, mutant.index);
    else
        runtime.clearActiveMutant();
    try {
        const mocha = new Mocha({ ...options, rootHooks, parallel: false, watch: false });
        mocha.files = [...[].concat(options.file ?? []).map((name) => resolve(root, name)), file];
        // Loaded by require, which loads a file again once it is out of
        // require.cache (Mocha's loadFilesAsync imports it, and an import is
        // never loaded again).
        mocha.loadFiles();
        mocha.lazyLoadFiles?.(true);
        await new Promise((done) => mocha.run(() => done()));
        try {
            mocha.dispose();
        }
        catch {
            // An older Mocha disposes nothing.
        }
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
