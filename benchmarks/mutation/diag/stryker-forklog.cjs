// Loaded into every Node process of a diagnosed run (NODE_OPTIONS=--require): appends one JSON line
// per event to $SUPERCOV_FORKLOG -- the process's start and end, each child it forks or spawns,
// and in a Vitest worker each "run" message's test files.
const { appendFileSync } = require("node:fs");
const childProcess = require("node:child_process");
const file = process.env.SUPERCOV_FORKLOG;
if (file) {
  const at = () => Date.now();
  const log = (record) => {
    try {
      appendFileSync(file, `${JSON.stringify({ pid: process.pid, at: at(), ...record })}\n`);
    } catch {}
  };
  const script = process.argv[1] ?? "";
  log({ start: true, ppid: process.ppid, script: script.split(/[\\/]/).slice(-3).join("/"), up: Math.round(process.uptime() * 1000) });
  process.on("exit", () => log({ exit: true, up: Math.round(process.uptime() * 1000) }));
  for (const name of ["fork", "spawn"]) {
    const original = childProcess[name];
    childProcess[name] = function (...args) {
      const child = original.apply(this, args);
      log({ [name]: String(args[0]).split(/[\\/]/).slice(-3).join("/"), child: child.pid });
      return child;
    };
  }
  // ES modules that imported the functions by name see the replacements too.
  require("node:module").syncBuiltinESMExports();
  if (process.send && /workers[\\/](forks|vmForks)\.js$/.test(script)) {
    process.on("message", (message) => {
      if (message?.__vitest_worker_request__)
        log({ request: message.type, files: message.context?.files?.map((f) => String(f.filepath ?? f).split(/[\\/]/).slice(-2).join("/")) });
    });
  }
}
