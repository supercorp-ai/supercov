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
import { fork } from "node:child_process";

const children = new Map();
const tell = (record) => {
    if (process.connected)
        process.send(record);
};

process.on("message", (request) => {
    const { id } = request;
    if (request.fork) {
        let child;
        try {
            child = fork(request.modulePath, request.args, request.options);
        }
        catch (error) {
            tell({ id, error: { message: String(error?.message ?? error), code: error?.code } });
            tell({ id, exit: [null, null] });
            return;
        }
        children.set(id, child);
        child.on("spawn", () => tell({ id, spawned: child.pid }));
        child.on("message", (message) => tell({ id, message }));
        child.on("error", (error) => tell({ id, error: { message: String(error?.message ?? error), code: error?.code } }));
        child.stdout?.on("data", (data) => tell({ id, stdout: data }));
        child.stderr?.on("data", (data) => tell({ id, stderr: data }));
        // `close` comes after the process's output: all of it was relayed.
        child.on("close", (code, signal) => {
            children.delete(id);
            tell({ id, exit: [code, signal] });
        });
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
    for (const child of children.values())
        child.kill("SIGKILL");
    process.exit(0);
});
