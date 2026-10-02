"use strict";
// Instrumented sources load this before anything else. Under the preload it
// finds the runtime already installed and does nothing. A process that runs
// the workspace from somewhere else -- a container or VM the workspace is
// mounted into, started by an SDK that passed on none of Supercov's settings,
// or a worker the preload never reached -- has no runtime, and without one the
// first probe would throw. Such a process installs the runtime itself, with
// the run's settings read from beside this file and their paths moved to
// wherever the workspace is seen from here, so its coverage lands in the
// run's own evidence directory.
const installed = globalThis.__SUPERCOV_DIRECT_RUNTIME__ ?? process.__SUPERCOV_DIRECT_RUNTIME__;
if (installed) {
    globalThis.__SUPERCOV_DIRECT_RUNTIME__ ??= installed;
}
else {
    try {
        const fs = require("node:fs");
        const path = require("node:path");
        // <workspace>/.supercov/node_modules/bootstrap.cjs
        const root = path.resolve(__dirname, "..", "..");
        const settings = JSON.parse(fs.readFileSync(path.join(__dirname, "run.json"), "utf8"));
        const moved = (value) => typeof value === "string" && settings.root !== root
            ? value.split(settings.root).join(root)
            : value;
        // Settings carried over from the host name the host's paths, which do
        // not exist here; replace them all rather than keep a mixture.
        const foreign = process.env.SUPERCOV_PROJECT_ROOT !== root;
        for (const [name, value] of Object.entries(settings.environment)) {
            if (foreign || process.env[name] === undefined)
                process.env[name] = moved(value);
        }
        process.env.SUPERCOV_PROJECT_ROOT = root;
        process.env.SUPERCOV_EXECUTION_LOG_SHARD ??= `bootstrap-${process.pid}-${Date.now()}`;
        require("./runtime.mjs");
        // Counted in the run's summary: coverage from here belongs to no test.
        require("./launchSupervisor.mjs").recordExecution({ event: "guest-process", guestRoot: root });
    }
    catch {
        // Without its settings or its runtime nothing can be recorded here,
        // and the first probe fails as it did before this file existed.
    }
}
module.exports = globalThis.__SUPERCOV_DIRECT_RUNTIME__;
