#!/usr/bin/env node
// A strong pass over Rust compiled to wasm (strongJobs.mjs): each job that
// starts the command builds the wasm again, and wasm-bindgen (or wasm-pack
// build) writes its glue where every other job's tests load it. This stands
// in for the tool on the jobs' PATH: the tool writes into a fresh directory
// beside its output directory, and each file is then renamed into place, and
// only where its bytes changed. A rebuild of the same wasm changes nothing,
// and a test never reads a file half written. It notes that it ran (`used`
// beside it), so the driver knows the wasm is built this way.
// CommonJS, as Node loads a file without an extension.
const { spawnSync } = require("node:child_process");
const { accessSync, constants, existsSync, mkdirSync, readdirSync, readFileSync, renameSync, rmSync, statSync, writeFileSync } = require("node:fs");
const { basename, delimiter, dirname, join, resolve } = require("node:path");

const here = dirname(process.argv[1]);
const tool = basename(process.argv[1]);
const real = (process.env.PATH ?? "")
    .split(delimiter)
    .filter((directory) => directory && resolve(directory) !== here)
    .map((directory) => join(directory, tool))
    .find((candidate) => {
        try {
            accessSync(candidate, constants.X_OK);
            return true;
        }
        catch {
            return false;
        }
    });
if (!real) {
    console.error(`supercov: no ${tool} on PATH`);
    process.exit(127);
}
const args = process.argv.slice(2);
const passThrough = () => process.exit(spawnSync(real, args, { stdio: "inherit" }).status ?? 1);

// The output directory the tool was asked for, and where it is in `args`.
let output;
let at = -1;
for (let index = 0; index < args.length; index += 1) {
    const argument = args[index];
    if (argument === "--out-dir" || (tool === "wasm-pack" && argument === "-d")) {
        output = args[index + 1];
        at = index + 1;
        break;
    }
    if (argument.startsWith("--out-dir=")) {
        output = argument.slice("--out-dir=".length);
        at = index;
        break;
    }
}
let base = process.cwd();
if (tool === "wasm-pack") {
    if (args[0] !== "build")
        passThrough();
    // wasm-pack reads its output directory against the crate's, `pkg`
    // where none is given.
    const crate = args.slice(1).find((argument, index, rest) => !argument.startsWith("-") && !["-d", "--out-dir", "-t", "--target", "--scope", "--mode", "--out-name", "--profile"].includes(rest[index - 1]));
    base = resolve(crate ?? ".");
    output ??= "pkg";
}
if (output === undefined)
    passThrough();
const target = resolve(base, output);
const fresh = `${target}.supercov-${process.pid}`;
rmSync(fresh, { recursive: true, force: true });
const changed = [...args];
if (at < 0)
    changed.push("--out-dir", fresh);
else if (changed[at].startsWith("--out-dir="))
    changed[at] = `--out-dir=${fresh}`;
else
    changed[at] = fresh;
const ran = spawnSync(real, changed, { stdio: "inherit" });
if (ran.status === 0) {
    const move = (from, to) => {
        for (const name of readdirSync(from)) {
            const source = join(from, name);
            const destination = join(to, name);
            if (statSync(source).isDirectory()) {
                mkdirSync(destination, { recursive: true });
                move(source, destination);
                continue;
            }
            const same = existsSync(destination) && readFileSync(destination).equals(readFileSync(source));
            if (!same)
                renameSync(source, destination);
        }
    };
    mkdirSync(target, { recursive: true });
    move(fresh, target);
    writeFileSync(join(here, "used"), "");
}
rmSync(fresh, { recursive: true, force: true });
process.exit(ran.status ?? 1);
