// Strong mutation's process spawner (docs/design/strong-mutation.md).
//
// A strong pass starts a worker process for every copy of a test file, and
// Vitest forks each from its own process. Forking copies the parent's memory
// map, and Vitest's process holds about a gigabyte by then (its module graph,
// the bundler's native heap): on hono each fork held Vitest's event loop for
// 24 ms, and the workers then waited on it. This process starts small and
// stays small, forks the workers in Vitest's stead, and relays everything
// between them and Vitest: messages, standard output and error, signals and
// exits. vitestStrongReporter.mjs starts it and stands a ChildProcess-like
// object in for each worker.
//
// It also keeps workers started ahead of time. A copy's worker spends its
// first few hundred milliseconds starting Node, loading Vitest's worker and,
// where the project tests in a DOM environment, loading that environment's
// package (jsdom: 300 ms on tRPC). None of it depends on the copy: it is what
// every worker of the project does before Vitest tells it anything. So a
// worker is started with the same entry, arguments and options Vitest gives
// its workers, loads what a fresh worker would load in the order it would,
// and waits. Vitest's next request for a worker gets it, and its copy starts
// at once. The same file, run with SUPERCOV_STRONG_WORKER set, is that
// worker's entry.
import { fork } from "node:child_process";
import { fileURLToPath, pathToFileURL } from "node:url";

const workerSpec = process.env["SUPERCOV_STRONG_WORKER"];
if (workerSpec)
    await startAhead(JSON.parse(workerSpec));
else
    spawn();

// A worker started ahead: from here on the process is the one Vitest would
// have forked, waiting for its first message.
async function startAhead({ entry, args, preload }) {
    delete process.env["SUPERCOV_STRONG_WORKER"];
    process.argv.splice(1, process.argv.length - 1, entry, ...args);
    await import(pathToFileURL(entry).href);
    // A fresh worker loads its environment's package once Vitest has told it
    // which: after its entry, before any test file.
    // Vitest passes --experimental-import-meta-resolve, without which Node
    // resolves against this file instead of Vitest's: not the module Vitest
    // would load.
    if (process.execArgv.includes("--experimental-import-meta-resolve")) {
        for (const name of preload) {
            try {
                await import(import.meta.resolve(name, pathToFileURL(entry).href));
            }
            catch {
                // Vitest loads it itself, and reports what fails.
            }
        }
    }
    process.send({ supercovStrongReady: true });
}

// The DOM environments Vitest has built in, and the package each loads.
const ENVIRONMENT_PACKAGES = { jsdom: "jsdom", "happy-dom": "happy-dom" };

function spawn() {
    const children = new Map();
    const tell = (record) => {
        if (process.connected)
            process.send(record);
    };
    // Workers started ahead, by what Vitest forks them with: its worker entry,
    // arguments and options (every worker of a project gets the same).
    const pools = new Map();
    // How many workers each pool keeps started (the reporter says).
    let poolSize = 0;
    let startedAhead = 0;
    let usedAhead = 0;

    // A worker's output and exit go to Vitest under the id it asked for.
    const relay = (id, child) => {
        children.set(id, child);
        child.on("message", (message) => tell({ id, message }));
        child.on("error", (error) => tell({ id, error: { message: String(error?.message ?? error), code: error?.code } }));
        child.stdout?.on("data", (data) => tell({ id, stdout: data }));
        child.stderr?.on("data", (data) => tell({ id, stderr: data }));
        // `close` comes after the process's output: all of it was relayed.
        child.on("close", (code, signal) => {
            children.delete(id);
            tell({ id, exit: [code, signal] });
        });
    };

    const refill = (pool) => {
        while (pool.ready.length + pool.starting.size < poolSize) {
            const { modulePath, args, options } = pool.request;
            let child;
            try {
                child = fork(fileURLToPath(import.meta.url), [], {
                    ...options,
                    env: {
                        ...(options.env ?? process.env),
                        SUPERCOV_STRONG_WORKER: JSON.stringify({ entry: modulePath, args, preload: [...pool.preload] }),
                    },
                });
            }
            catch {
                return;
            }
            startedAhead += 1;
            const waiting = { child, output: [] };
            pool.starting.add(waiting);
            const stdout = (data) => waiting.output.push(["stdout", data]);
            const stderr = (data) => waiting.output.push(["stderr", data]);
            const ignore = () => { };
            const ready = (message) => {
                if (!message?.supercovStrongReady)
                    return;
                child.off("message", ready);
                pool.starting.delete(waiting);
                pool.ready.push(waiting);
            };
            // Ended before Vitest took it: not one of Vitest's workers.
            const ended = () => {
                pool.starting.delete(waiting);
                const at = pool.ready.indexOf(waiting);
                if (at >= 0)
                    pool.ready.splice(at, 1);
            };
            child.stdout?.on("data", stdout);
            child.stderr?.on("data", stderr);
            child.on("error", ignore);
            child.on("message", ready);
            child.on("exit", ended);
            waiting.release = () => {
                child.stdout?.off("data", stdout);
                child.stderr?.off("data", stderr);
                child.off("error", ignore);
                child.off("message", ready);
                child.off("exit", ended);
            };
        }
    };

    // Vitest's worker for request `id`: one started ahead where one is ready,
    // else a new one.
    const take = (request) => {
        const { id } = request;
        const key = JSON.stringify([request.modulePath, request.args, request.options]);
        let pool = pools.get(key);
        if (!pool) {
            pool = { request, ready: [], starting: new Set(), preload: new Set(), asked: 0 };
            pools.set(key, pool);
        }
        pool.asked += 1;
        const waiting = pool.ready.shift();
        if (waiting) {
            usedAhead += 1;
            const { child, output } = waiting;
            waiting.release();
            relay(id, child);
            tell({ id, spawned: child.pid });
            for (const [stream, data] of output)
                tell({ id, [stream]: data });
        }
        else {
            let child;
            try {
                child = fork(request.modulePath, request.args, request.options);
            }
            catch (error) {
                tell({ id, error: { message: String(error?.message ?? error), code: error?.code } });
                tell({ id, exit: [null, null] });
                return;
            }
            relay(id, child);
            child.on("spawn", () => tell({ id, spawned: child.pid }));
        }
        // Options Vitest gives one worker only are not worth workers waiting.
        if (pool.asked > 1)
            refill(pool);
    };

    const stopAhead = () => {
        for (const pool of pools.values()) {
            for (const { child } of [...pool.ready, ...pool.starting])
                child.kill("SIGKILL");
            pool.ready = [];
            pool.starting.clear();
        }
    };

    process.on("message", (request) => {
        const { id } = request;
        if (request.fork) {
            take(request);
            return;
        }
        if (request.pool !== undefined) {
            poolSize = request.pool;
            if (!poolSize)
                stopAhead();
            for (const pool of pools.values())
                if (pool.asked > 1)
                    refill(pool);
            return;
        }
        // The environment Vitest starts its workers in (its first message to
        // one says): a worker started from now on loads its package too.
        if (request.environment !== undefined) {
            const name = ENVIRONMENT_PACKAGES[request.environment];
            for (const pool of pools.values()) {
                if (name && pool.request.modulePath === request.modulePath && !pool.preload.has(name)) {
                    pool.preload.add(name);
                    // Those started without it are not what Vitest would start now.
                    for (const { child } of [...pool.ready, ...pool.starting])
                        child.kill("SIGKILL");
                    pool.ready = [];
                    pool.starting.clear();
                    if (pool.asked > 1)
                        refill(pool);
                }
            }
            return;
        }
        if (request.stats) {
            tell({ stats: { startedAhead, usedAhead } });
            return;
        }
        const child = children.get(id);
        if (!child)
            return;
        if (request.send) {
            if (child.connected)
                child.send(request.message, (error) => {
                    if (error)
                        tell({ id, error: { message: String(error.message), code: error.code } });
                });
            else
                tell({ id, error: { message: "Channel closed", code: "ERR_IPC_CHANNEL_CLOSED" } });
        }
        else if (request.kill)
            child.kill(request.kill);
        else if (request.disconnect && child.connected)
            child.disconnect();
    });

    // Vitest's process is gone: so are its workers.
    process.on("disconnect", () => {
        stopAhead();
        for (const child of children.values())
            child.kill("SIGKILL");
        process.exit(0);
    });
}
