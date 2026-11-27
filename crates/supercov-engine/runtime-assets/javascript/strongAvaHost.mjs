// A resident AVA for a strong pass's jobs (strongJobs.mjs): one process that
// keeps AVA's command loaded, its configuration read and its options worked
// out, and runs one job after another, where each job started the command.
//
// AVA runs every test file in a worker of its own (a worker thread, or a
// child process under --no-worker-threads), made for the file as the run
// starts and given the main process's environment as it is then, so each
// job's file and the code it tests load afresh, with the job's mutant on
// from the worker's start, as in a command of its own. This process loads
// none of the project's code: the command's main process does not either. Between runs the main
// process keeps only what AVA's own watch mode keeps between its reruns:
// its options and its cache directory.
//
// The options are AVA's command's own: the host runs AVA's command
// (`lib/cli.js`) with the command's arguments and takes the Api it makes as
// it would start its first run. It refuses (the driver starts the command
// for each job, as before) where the command watches or debugs, or where
// AVA's command ends instead (a configuration it rejects).
//
// The driver forks it in the project's copy and sends
//   { type: "init", args: [<the command's arguments to ava>] }
//   { type: "job", id, file: <absolute test file>, mutant: { file, index } | null, environment: {...} }
// and it answers { type: "ready", error? } and { type: "done", id, error?, tainted? }:
// `tainted` that this process runs no further job.
import { realpathSync } from "node:fs";
import { createRequire, syncBuiltinESMExports } from "node:module";
import { join } from "node:path";
import { pathToFileURL } from "node:url";
import threads from "node:worker_threads";

const root = process.cwd();
// A shared worker (AVA's plugin API) lives as long as the main process: in a
// command, one job's; here, every later job's too. A job that starts one
// is the host's last.
let sharedWorker = false;
{
    const { Worker } = threads;
    threads.Worker = class extends Worker {
        constructor(file, ...rest) {
            if (/shared-worker-loader/.test(String(file)))
                sharedWorker = true;
            super(file, ...rest);
        }
    };
    syncBuiltinESMExports();
}
let api;
let run;
let runOptions;

async function init(args) {
    if (args.some((argument) => /^(?:-w|--watch|debug|reset-cache)$/.test(argument)))
        throw new Error("the command watches, debugs or resets AVA's cache");
    process.env.SUPERCOV_STRONG_PLAN = "";
    process.env.SUPERCOV_ACTIVE_MUTANT = "-";
    // The installed package, by its real path: AVA's command loads its Api
    // from there, and only that module's Api is the one it makes.
    const main = realpathSync(createRequire(join(root, "package.json")).resolve("ava"));
    const at = main.replaceAll("\\", "/").lastIndexOf("/node_modules/ava/");
    if (at < 0)
        throw new Error(`AVA is not where a package is: ${main}`);
    const avaRoot = main.slice(0, at + "/node_modules/ava".length);
    const load = async (name) => import(pathToFileURL(join(avaRoot, "lib", name)).href);
    const { default: Api } = await load("api.js");
    run = Api.prototype.run;
    // AVA's command makes its Api from the command line and the project's
    // configuration, and starts its run: the run is taken here instead, with
    // what the command asked of it (its files, its line numbers).
    const made = new Promise((resolveMade) => {
        Api.prototype.run = function capture(options) {
            resolveMade({ api: this, options });
            return new Promise(() => { });
        };
    });
    process.argv = [process.execPath, join(avaRoot, "entrypoints", "cli.mjs"), ...args];
    const { default: loadCli } = await load("cli.js");
    const exited = loadCli().then(() => {
        throw new Error("AVA's command ended without starting a run");
    });
    ({ api, options: runOptions } = await Promise.race([made, exited]));
    Api.prototype.run = run;
}

// A job runs what the command runs, as the command's own run would: every
// test file in a worker of its own, where the adapter runs only the job's
// tests (strongJob.mjs). The workers take the environment as it is when the
// run makes them.
async function runJob({ environment, mutant }) {
    Object.assign(process.env, environment, { SUPERCOV_ACTIVE_MUTANT: mutant ? `${mutant.file}#${mutant.index}` : "-" });
    await run.call(api, runOptions);
    return sharedWorker ? { tainted: true } : {};
}

process.on("message", (message) => {
    const reply = (answer) => process.send?.(answer);
    if (message?.type === "init") {
        init(message.args ?? []).then(() => reply({ type: "ready" }), (error) => reply({ type: "ready", error: String(error?.stack ?? error) }));
        return;
    }
    if (message?.type === "job")
        runJob(message).then((outcome) => reply({ type: "done", id: message.id, ...outcome }), (error) => reply({ type: "done", id: message.id, error: String(error?.stack ?? error), tainted: true }));
});
process.send?.({ type: "started" });
