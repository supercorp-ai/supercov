// The strong pass's driver for the runners whose processes are started with
// a mutant on (docs/design/strong-mutation.md): Jest, node:test, Mocha, AVA,
// node-tap, bun:test and Vitest Browser Mode. The engine starts it in place of
// the run's command, `node strongJobs.mjs -- <command>`, in the instrumented
// copy, with the plan (SUPERCOV_STRONG_PLAN) and where the verdicts go
// (SUPERCOV_STRONG_RESULTS).
//
// Each job is one start of the command for one test file: the mutant on in
// every process from its start (SUPERCOV_ACTIVE_MUTANT), only the job's tests
// run (SUPERCOV_STRONG_JOB, read by the runner's adapter through
// strongJob.mjs), and the first failure stops it. The tests' outcomes are the
// adapter's own evidence records, in the job's own evidence directory. A
// fresh process per job is how these runners isolate a test file anyway: no
// mutant's module state, timers or process-wide changes reach another's.
//
// For each mutant: its candidate tests by test file, the cheapest first; the
// next file only while it lives. Each file runs first without a mutant (its
// baseline), with every test a mutant will run there; a test that fails there
// is no kill, and a file whose baseline fails or leaves a test out (a subtest
// whose parent was not chosen) runs whole from then on. A kill by a job that
// ran other tests than its baseline did is confirmed by running the job's
// tests again without the mutant. A job past 3x its baseline and 10 s is
// stopped: a timeout, unless a test had already failed. A stopped job may
// only have waited: on the jobs beside it (the CPU; Cargo's lock on a build
// directory, where each job builds Rust compiled to wasm), or on a machine
// busier than when its baseline ran (other work on it, beyond the pass's
// own). So it runs again alone, after its tests ran alone without the
// mutant, and its limit there is 3x the slower of that run and its
// baseline, and 10 s: only a job past that is a timeout.
import { fork, spawn } from "node:child_process";
import { appendFileSync, existsSync, mkdirSync, readFileSync, readdirSync, rmSync, writeFileSync } from "node:fs";
import { availableParallelism, constants, cpus } from "node:os";
import { basename, delimiter, dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

// A file whose baseline job took at most this long, the start of the command
// included, runs all its baseline's tests beside a mutant's own candidates
// rather than confirm a kill apart (`tryFile`).
const UNION_MS = 1_500;
// What a job costs beyond its tests' own time, in the order a mutant's
// files run: a start of the runner, or a resident runner's file setup.
const JOB_START_MS = 300;
// Vitest's node tests run here where the run also had browser tests, which
// Vitest's own driver cannot run beside them.
const RUNNERS = new Set(["jest", "mocha", "node:test", "ava", "tap", "bun:test", "vitest-browser", "vitest"]);

// The wasm output wrapper's source (strongWasmOut.cjs), written beside a
// pass's jobs as `wasm-bindgen` and `wasm-pack` where the pass switches Rust
// compiled to wasm.
export const WASM_OUT = "#!/usr/bin/env node\n// A strong pass over Rust compiled to wasm (strongJobs.mjs): each job that\n// starts the command builds the wasm again, and wasm-bindgen (or wasm-pack\n// build) writes its glue where every other job's tests load it. This stands\n// in for the tool on the jobs' PATH: the tool writes into a fresh directory\n// beside its output directory, and each file is then renamed into place, and\n// only where its bytes changed. A rebuild of the same wasm changes nothing,\n// and a test never reads a file half written. It notes that it ran (`used`\n// beside it), so the driver knows the wasm is built this way.\n// CommonJS, as Node loads a file without an extension.\nconst { spawnSync } = require(\"node:child_process\");\nconst { accessSync, constants, existsSync, mkdirSync, readdirSync, readFileSync, renameSync, rmSync, statSync, writeFileSync } = require(\"node:fs\");\nconst { basename, delimiter, dirname, join, resolve } = require(\"node:path\");\n\nconst here = dirname(process.argv[1]);\nconst tool = basename(process.argv[1]);\nconst real = (process.env.PATH ?? \"\")\n    .split(delimiter)\n    .filter((directory) => directory && resolve(directory) !== here)\n    .map((directory) => join(directory, tool))\n    .find((candidate) => {\n        try {\n            accessSync(candidate, constants.X_OK);\n            return true;\n        }\n        catch {\n            return false;\n        }\n    });\nif (!real) {\n    console.error(`supercov: no ${tool} on PATH`);\n    process.exit(127);\n}\nconst args = process.argv.slice(2);\nconst passThrough = () => process.exit(spawnSync(real, args, { stdio: \"inherit\" }).status ?? 1);\n\n// The output directory the tool was asked for, and where it is in `args`.\nlet output;\nlet at = -1;\nfor (let index = 0; index < args.length; index += 1) {\n    const argument = args[index];\n    if (argument === \"--out-dir\" || (tool === \"wasm-pack\" && argument === \"-d\")) {\n        output = args[index + 1];\n        at = index + 1;\n        break;\n    }\n    if (argument.startsWith(\"--out-dir=\")) {\n        output = argument.slice(\"--out-dir=\".length);\n        at = index;\n        break;\n    }\n}\nlet base = process.cwd();\nif (tool === \"wasm-pack\") {\n    if (args[0] !== \"build\")\n        passThrough();\n    // wasm-pack reads its output directory against the crate's, `pkg`\n    // where none is given.\n    const crate = args.slice(1).find((argument, index, rest) => !argument.startsWith(\"-\") && ![\"-d\", \"--out-dir\", \"-t\", \"--target\", \"--scope\", \"--mode\", \"--out-name\", \"--profile\"].includes(rest[index - 1]));\n    base = resolve(crate ?? \".\");\n    output ??= \"pkg\";\n}\nif (output === undefined)\n    passThrough();\nconst target = resolve(base, output);\nconst fresh = `${target}.supercov-${process.pid}`;\nrmSync(fresh, { recursive: true, force: true });\nconst changed = [...args];\nif (at < 0)\n    changed.push(\"--out-dir\", fresh);\nelse if (changed[at].startsWith(\"--out-dir=\"))\n    changed[at] = `--out-dir=${fresh}`;\nelse\n    changed[at] = fresh;\nconst ran = spawnSync(real, changed, { stdio: \"inherit\" });\nif (ran.status === 0) {\n    const move = (from, to) => {\n        for (const name of readdirSync(from)) {\n            const source = join(from, name);\n            const destination = join(to, name);\n            if (statSync(source).isDirectory()) {\n                mkdirSync(destination, { recursive: true });\n                move(source, destination);\n                continue;\n            }\n            const same = existsSync(destination) && readFileSync(destination).equals(readFileSync(source));\n            if (!same)\n                renameSync(source, destination);\n        }\n    };\n    mkdirSync(target, { recursive: true });\n    move(fresh, target);\n    writeFileSync(join(here, \"used\"), \"\");\n}\nrmSync(fresh, { recursive: true, force: true });\nprocess.exit(ran.status ?? 1);\n";

// Every process the pass started that may still run: each job's command and
// each resident runner, in a process group of its own. They are stopped with
// the pass, which a terminal's Ctrl-C does not reach them through.
const started = new Set();

function track(child) {
    started.add(child);
    child.once("exit", () => started.delete(child));
    return child;
}

// Stops every process the pass started.
export function stopStarted() {
    for (const child of started)
        stop(child);
}

function parallelism() {
    const given = Number(process.env.SUPERCOV_STRONG_WORKERS);
    if (Number.isSafeInteger(given) && given > 0)
        return given;
    const available = typeof availableParallelism === "function" ? availableParallelism() : cpus().length;
    return Math.max(1, available - 1);
}

// At most `size` jobs at once: the lowest `priority` first, and first come
// first served among equals.
export function slots(size) {
    let free = size;
    const waiting = [];
    return async function run(work, priority = 0) {
        if (free > 0)
            free -= 1;
        else {
            await new Promise((resolve) => {
                // After every waiting job of its priority or a lower one.
                const at = waiting.findIndex((other) => other.priority > priority);
                waiting.splice(at < 0 ? waiting.length : at, 0, { priority, resolve });
            });
        }
        try {
            return await work();
        }
        finally {
            const next = waiting.shift();
            if (next)
                next.resolve();
            else
                free += 1;
        }
    };
}

// Turns at the machine: `shared` work runs beside other shared work, and
// `alone` work waits until none runs, while no shared work starts. A shared
// turn learns whether other work ran beside it (`crowded`).
export function turns() {
    const running = new Set();
    const idle = [];
    let alone;
    return {
        async shared(work) {
            while (alone)
                await alone;
            const turn = { crowded: running.size > 0 };
            for (const other of running)
                other.crowded = true;
            running.add(turn);
            try {
                return await work(turn);
            }
            finally {
                running.delete(turn);
                if (running.size === 0)
                    for (const wake of idle.splice(0))
                        wake();
            }
        },
        async alone(work) {
            while (alone)
                await alone;
            let end;
            alone = new Promise((done) => { end = done; });
            try {
                if (running.size > 0)
                    await new Promise((wake) => idle.push(wake));
                return await work({ crowded: false });
            }
            finally {
                alone = undefined;
                end();
            }
        },
    };
}

// Every file under `directory`, recursively.
function filesUnder(directory) {
    let entries;
    try {
        entries = readdirSync(directory, { withFileTypes: true });
    }
    catch {
        return [];
    }
    return entries.flatMap((entry) => entry.isDirectory()
        ? filesUnder(join(directory, entry.name))
        : [join(directory, entry.name)]);
}

// Each test's outcome in a job's evidence: the last attempt's, where any
// record says more than `unknown` (Jest writes coverage and outcome apart),
// in the order the tests ended.
export function outcomes(directory) {
    const records = [];
    for (const path of filesUnder(directory)) {
        let text;
        try {
            text = readFileSync(path, "utf8");
        }
        catch {
            continue;
        }
        const lines = path.endsWith(".jsonl") ? text.split("\n") : path.endsWith(".json") ? [text] : [];
        for (const line of lines) {
            if (!line.trim())
                continue;
            try {
                records.push(JSON.parse(line));
            }
            catch {
                // A line cut short by a stopped process says nothing.
            }
        }
    }
    const tests = new Map();
    for (const record of records) {
        if (typeof record?.testId !== "string" || (record.role && record.role !== "test"))
            continue;
        const seen = tests.get(record.testId);
        const retry = record.retry ?? 0;
        const status = record.status ?? "unknown";
        if (!seen) {
            tests.set(record.testId, { status, retry });
            continue;
        }
        if (status === "unknown" && seen.status !== "unknown")
            continue;
        if (retry >= seen.retry || seen.status === "unknown")
            tests.set(record.testId, { status, retry: Math.max(retry, seen.retry) });
    }
    return new Map([...tests].map(([id, { status }]) => [id, status]));
}

// The command for one job: Bun's runner takes the file as an argument, as it
// loads every test file into one process; the other runners' adapters keep
// their runner from loading another file.
export function commandFor(command, file, root = process.cwd(), runner = undefined) {
    const [program, first] = command;
    // A tap test file is a program of its own, as tap's runner runs it (and
    // StrykerJS's tap runner): run by itself, beside other jobs, it shares
    // none of the working files tap's command keeps in the project.
    if (runner === "tap" && /\.[cm]?js$/.test(file))
        return [process.execPath, file];
    if (program && /^bun(?:\.exe)?$/.test(basename(program)) && first === "test")
        return [...command, `./${file}`];
    return withoutNpx(command, root);
}

// `npx <tool> ...` where the project has the tool: the tool itself. npx
// costs a start of npm's CLI before each job's, a third of a small suite's
// job.
export function withoutNpx(command, root) {
    const [program, ...rest] = command;
    if (!program || !/^npx(?:\.cmd)?$/.test(basename(program)) || process.platform === "win32")
        return command;
    const at = rest.findIndex((word) => !/^(?:-y|--yes|--no-install|--offline)$/.test(word));
    const tool = rest[at];
    if (at < 0 || !tool || tool.startsWith("-") || tool.includes("@") || tool.includes("/"))
        return command;
    const binary = join(root, "node_modules", ".bin", tool);
    return existsSync(binary) ? [binary, ...rest.slice(at + 1)] : command;
}

// The arguments a command gives Jest, where it starts Jest itself (`jest`,
// `npx jest`, `node_modules/.bin/jest`): a resident Jest parses them as
// Jest's CLI would.
export function jestArguments(command) {
    const at = command.findIndex((word) => /(?:^|[\\/])jest(?:\.js)?$/.test(word) || /jest-cli[\\/]bin[\\/]jest\.js$/.test(word));
    return at < 0 ? undefined : command.slice(at + 1);
}

// The arguments a command gives Mocha, where it starts Mocha itself.
export function mochaArguments(command) {
    const at = command.findIndex((word) => /(?:^|[\\/])_?mocha(?:\.js)?$/.test(word));
    return at < 0 ? undefined : command.slice(at + 1);
}

// The arguments a command gives Vitest, where it starts Vitest itself
// (`vitest run`, `npx vitest`, `node_modules/.bin/vitest`).
export function vitestArguments(command) {
    const at = command.findIndex((word) => /(?:^|[\\/])vitest(?:\.m?js)?$/.test(word));
    return at < 0 ? undefined : command.slice(at + 1);
}

// Whether a project's .js files are CommonJS (its package.json has no
// `"type": "module"`).
export function packageIsCommonJs(root) {
    try {
        return JSON.parse(readFileSync(join(root, "package.json"), "utf8")).type !== "module";
    }
    catch {
        return true;
    }
}

// Resident runners (strongJestHost.mjs, strongMochaHost.mjs), each running one
// job at a time. A host whose job passes its limit is stopped, and the next
// job starts another.
function residentHosts({ script, args, root, environment, log }) {
    const idle = [];
    const all = new Set();
    const host = () => new Promise((resolveHost, rejectHost) => {
        const child = fork(fileURLToPath(new URL(`./${script}`, import.meta.url)), [], {
            cwd: root,
            env: environment,
            stdio: ["ignore", "pipe", "pipe", "ipc"],
            detached: process.platform !== "win32",
        });
        // Its output is drained, and kept for whoever reads why a job failed.
        const keep = (chunk) => {
            try {
                appendFileSync(log, chunk);
            }
            catch {
                // The log is for reading only.
            }
        };
        child.stdout.on("data", keep);
        child.stderr.on("data", keep);
        track(child);
        all.add(child);
        child.once("exit", () => all.delete(child));
        child.on("message", function started(message) {
            if (message?.type === "started")
                child.send({ type: "init", args: args ?? [] });
            else if (message?.type === "ready") {
                child.off("message", started);
                if (message.error) {
                    stop(child);
                    rejectHost(new Error(message.error));
                }
                else
                    resolveHost(child);
            }
        });
        child.once("error", rejectHost);
    });
    let number = 0;
    return {
        async run(job, limitMs) {
            let child;
            try {
                child = idle.pop() ?? await host();
            }
            catch (error) {
                try {
                    appendFileSync(log, `[supercov] the resident runner did not start: ${error?.stack ?? error}\n`);
                }
                catch {
                    // The log is for reading only.
                }
                return { timedOut: false, error, notStarted: true };
            }
            const id = ++number;
            return new Promise((resolveRun) => {
                let settled = false;
                const settle = (outcome) => {
                    if (settled)
                        return;
                    settled = true;
                    clearTimeout(timer);
                    child.off("message", answered);
                    child.off("exit", exited);
                    resolveRun(outcome);
                };
                const answered = (message) => {
                    if (message?.type === "done" && message.id === id) {
                        // A host that can run no further job is stopped.
                        if (message.tainted)
                            stop(child);
                        else
                            idle.push(child);
                        settle({ timedOut: false, error: message.error, fallback: message.fallback, tainted: message.tainted });
                    }
                };
                const exited = () => settle({ timedOut: false, error: "the resident runner ended" });
                const timer = setTimeout(() => {
                    stop(child);
                    settle({ timedOut: true });
                }, limitMs);
                child.on("message", answered);
                child.once("exit", exited);
                child.send({ type: "job", id, ...job });
            });
        },
        close() {
            for (const child of all)
                stop(child);
        },
    };
}

function readTimeouts(path) {
    try {
        return readFileSync(path, "utf8").split("\n").filter(Boolean).map((line) => {
            try {
                return JSON.parse(line).mutant;
            }
            catch {
                return undefined;
            }
        });
    }
    catch {
        return [];
    }
}

function stop(child) {
    if (child.exitCode !== null || child.signalCode !== null)
        return;
    try {
        if (process.platform === "win32")
            spawn("taskkill", ["/pid", String(child.pid), "/T", "/F"], { stdio: "ignore" });
        else
            process.kill(-child.pid, "SIGKILL");
    }
    catch {
        try {
            child.kill("SIGKILL");
        }
        catch {
            // Already gone.
        }
    }
}

export async function drive({ command, plan, resultsPath, root = process.cwd(), workers = parallelism(), environment = process.env, graceMs = 10_000, baselineGraceMs = 60_000, aloneGraceMs = graceMs }) {
    const started = Date.now();
    // Once the pass is told to stop (`stopped-at` beside the results), its
    // jobs are stopped under their tests: one that ends then says nothing of
    // its mutant, and no verdict is written (vitestStrongReporter.mjs).
    const stopping = join(dirname(resultsPath), "stopped-at");
    const write = (record) => {
        if (record.status !== undefined && existsSync(stopping))
            return;
        appendFileSync(resultsPath, `${JSON.stringify(record)}\n`);
    };
    const work = `${resultsPath}.jobs`;
    mkdirSync(work, { recursive: true });
    const tests = plan.tests;
    const byId = new Map(tests.map((test) => [test.id, test]));
    const handled = (test) => RUNNERS.has(test?.runner);
    // Every test file of the run under these runners: a job's adapter runs
    // none of their tests but the job's.
    const known = [...new Set(tests.filter(handled).map((test) => test.file).filter(Boolean))].sort();
    const testsOf = new Map();
    const runnerOf = new Map(tests.filter(handled).map((test) => [test.file, test.runner]));
    for (const test of tests) {
        if (!handled(test) || !test.file)
            continue;
        if (!testsOf.has(test.file))
            testsOf.set(test.file, []);
        testsOf.get(test.file).push(test);
    }

    // The copy's own list: the mutant at each index must be the run's, and
    // one the instrumenter could not place has no switch to turn on.
    let listed = {};
    try {
        listed = JSON.parse(readFileSync(join(root, ".supercov/frontend-evidence/mutants.json"), "utf8")).files ?? {};
    }
    catch {
        listed = {};
    }
    const idOf = (file, entry) => entry.id ||
        `${file}:${entry.start?.[0]}:${entry.start?.[1]}-${entry.end?.[0]}:${entry.end?.[1]}:${entry.mutator}:${entry.replacement}`;
    const indexes = new Map();
    for (const [file, entries] of Object.entries(listed))
        entries.forEach((entry, index) => indexes.set(idOf(file, entry), { index, entry }));

    // A command that starts Jest or Mocha itself: its jobs run in resident
    // runners, one per worker, rather than start the command each.
    const residentAllowed = environment.SUPERCOV_STRONG_RESIDENT !== "0";
    const jestArgs = residentAllowed ? jestArguments(withoutNpx(command, root)) : undefined;
    const mochaArgs = residentAllowed ? mochaArguments(withoutNpx(command, root)) : undefined;
    const jestHosts = residentHosts({ script: "strongJestHost.mjs", args: jestArgs, root, environment, log: join(work, "jest-hosts.log") });
    const mochaHosts = residentHosts({ script: "strongMochaHost.mjs", args: mochaArgs, root, environment, log: join(work, "mocha-hosts.log") });
    // A suite of ES modules, which cannot be loaded again in one thread,
    // runs each job in a fresh thread of a resident Mocha
    // (SUPERCOV_STRONG_MOCHA_ESM=process: a process per job instead).
    const esmHosts = residentHosts({ script: "strongMochaHost.mjs", args: mochaArgs, root, environment: { ...environment, SUPERCOV_MOCHA_FRESH_WORKERS: "1" }, log: join(work, "mocha-esm-hosts.log") });
    const esmResident = process.env.SUPERCOV_STRONG_MOCHA_ESM !== "process";
    // Vitest Browser Mode: a resident Vitest, its Vite server and its
    // browser kept for job after job, each job in fresh browser contexts
    // (strongVitestHost.mjs). A file it cannot run as a command would runs
    // as a command from then on (`commandOnly`).
    const vitestArgs = residentAllowed ? vitestArguments(withoutNpx(command, root)) : undefined;
    const vitestHosts = residentHosts({ script: "strongVitestHost.mjs", args: vitestArgs, root, environment: { ...environment, SUPERCOV_STRONG_PLAN: "" }, log: join(work, "vitest-hosts.log") });
    const commandOnly = new Set();
    const commonJs = packageIsCommonJs(root);
    const broken = new Set();
    const residentFor = (file) => {
        const runner = runnerOf.get(file);
        if (broken.has(runner))
            return undefined;
        if (runner === "jest" && jestArgs)
            return jestHosts;
        if (runner === "mocha" && mochaArgs && (/\.cjs$/.test(file) || (commonJs && /\.js$/.test(file))))
            return mochaHosts;
        if (runner === "mocha" && mochaArgs && esmResident)
            return esmHosts;
        if (runner === "vitest-browser" && vitestArgs && !commandOnly.has(file))
            return vitestHosts;
        return undefined;
    };
    // Rust compiled to wasm (SUPERCOV_WASM_STRONG): a job that starts the
    // command builds the wasm again, writing it where every other job's
    // tests load it, so where any file's jobs start the command, one job
    // runs at a time.
    const rebuilds = Boolean(process.env.SUPERCOV_WASM_STRONG) && known.some((file) => !residentFor(file));
    // wasm-bindgen and wasm-pack build, first on the jobs' PATH, write the
    // glue so that a rebuild of the same wasm changes nothing in place
    // (strongWasmOut.cjs). Jobs start one at a time until one of them ran
    // (it notes that it did): a build that writes its wasm another way
    // keeps them so.
    const wasmTools = join(work, "wasm-tools");
    if (rebuilds) {
        mkdirSync(wasmTools, { recursive: true });
        for (const name of ["wasm-bindgen", "wasm-pack"])
            writeFileSync(join(wasmTools, name), WASM_OUT, { mode: 0o755 });
    }
    const wasmOutUsed = () => existsSync(join(wasmTools, "used"));
    const parallel = slots(workers);
    const serial = slots(1);
    const run = !rebuilds ? parallel : async (work, priority) => {
        if (wasmOutUsed())
            return parallel(work, priority);
        let handed = false;
        const result = await serial(async () => {
            if (wasmOutUsed()) {
                handed = true;
                return undefined;
            }
            return work();
        });
        return handed ? parallel(work, priority) : result;
    };
    const profile = { jobs: 0, baselines: 0, confirmations: 0, overturned: 0, stopped: 0, alone: 0, jobMs: 0, residentJobs: 0, residentFallbacks: 0 };
    const machine = turns();
    let jobNumber = 0;

    // One start of the command: `tests` of `file` (null: all of them), with
    // `mutant` on (null: none), stopped at `limitMs`. A job of a mutant's
    // later batch (`priority` 1, 2, ...) waits behind every first file's,
    // so that the kills most mutants get at once come first, and a
    // survivor's many files fill what is left. One run `alone` waits for no
    // slot: it has the machine to itself.
    function job({ mutant, file, tests: chosen, bail, limitMs, priority = 0, alone = false, command: asCommand = false }) {
        const start = (work) => (alone ? machine.alone(work) : run(() => machine.shared(work), priority));
        return start((turn) => new Promise((resolveJob) => {
            const number = ++jobNumber;
            profile.jobs += 1;
            const directory = join(work, String(number));
            const evidence = join(directory, "evidence");
            mkdirSync(evidence, { recursive: true });
            const spec = join(directory, "job.json");
            writeFileSync(spec, JSON.stringify({ files: { [file]: chosen === null ? null : [...chosen] }, known, bail, mutant: mutant ? `${mutant.file}#${mutant.index}` : null }));
            const results = join(directory, "results.jsonl");
            const begun = Date.now();
            const resident = asCommand ? undefined : residentFor(file);
            // A job's outcome, read from its evidence; then its directory is
            // removed, as nothing after reads it (a job's evidence ran to
            // 24 MB on nanoid, a gigabyte a pass).
            const read = (outcome) => {
                const statuses = outcomes(evidence);
                if (environment.SUPERCOV_STRONG_KEEP !== "1")
                    rmSync(directory, { recursive: true, force: true });
                return { ...outcome, statuses, crowded: turn.crowded };
            };
            // A runner's own command, started for this job alone.
            const spawnJob = () => {
                const [program, ...args] = commandFor(command, file, root, runnerOf.get(file));
                try {
                    const overrides = {
                        // What tap's runner gives each test file it starts.
                        ...(runnerOf.get(file) === "tap" ? { TAP_CHILD_ID: String(number) } : {}),
                        SUPERCOV_EVIDENCE_DIR: evidence,
                        SUPERCOV_STRONG_JOB: spec,
                        SUPERCOV_ACTIVE_MUTANT: mutant ? `${mutant.file}#${mutant.index}` : "-",
                        SUPERCOV_STRONG_RESULTS: results,
                        // Only this driver reads the plan: Vitest's and
                        // Playwright's configurations would drive their own.
                        SUPERCOV_STRONG_PLAN: "",
                        ...(rebuilds ? { PATH: `${wasmTools}${delimiter}${environment.PATH ?? ""}` } : {}),
                    };
                    // Supercov's preload, in this process too, gives every child
                    // this process's own Supercov settings over the ones it is
                    // given: the job's are this process's while it starts.
                    const saved = Object.fromEntries(Object.keys(overrides).map((name) => [name, process.env[name]]));
                    Object.assign(process.env, overrides);
                    let child;
                    try {
                        child = spawn(program, args, {
                            cwd: root,
                            env: { ...environment, ...overrides },
                            stdio: ["ignore", "pipe", "pipe"],
                            detached: process.platform !== "win32",
                            shell: process.platform === "win32",
                            windowsHide: true,
                        });
                    }
                    finally {
                        for (const [name, value] of Object.entries(saved)) {
                            if (value === undefined)
                                delete process.env[name];
                            else
                                process.env[name] = value;
                        }
                    }
                    track(child);
                    // Its output is read by no one: drained, so the job never
                    // waits on a full pipe.
                    child.stdout.resume();
                    child.stderr.resume();
                    let timedOut = false;
                    const timer = setTimeout(() => {
                        timedOut = true;
                        profile.stopped += 1;
                        stop(child);
                    }, limitMs);
                    const finish = (code, error) => {
                        clearTimeout(timer);
                        profile.jobMs += Date.now() - begun;
                        const mutantKey = mutant ? `${mutant.file}#${mutant.index}` : undefined;
                        resolveJob(read({
                            timedOut,
                            mutantTimedOut: mutantKey !== undefined && readTimeouts(`${results}.timeouts.jsonl`).includes(mutantKey),
                            exitCode: code,
                            error,
                            durationMs: Date.now() - begun,
                        }));
                    };
                    child.on("error", (error) => finish(null, error));
                    child.on("close", (code) => finish(code));
                }
                catch (error) {
                    resolveJob(read({ timedOut: false, mutantTimedOut: false, exitCode: null, error, durationMs: 0 }));
                }
            };
            if (!resident) {
                spawnJob();
                return;
            }
            // A resident runner runs the job (strongJestHost.mjs,
            // strongMochaHost.mjs); one that cannot start leaves its runner's
            // jobs to the command.
            const environmentOfJob = {
                SUPERCOV_EVIDENCE_DIR: evidence,
                SUPERCOV_STRONG_JOB: spec,
                SUPERCOV_STRONG_RESULTS: results,
                SUPERCOV_STRONG_PLAN: "",
            };
            resident.run({ file: join(root, file), mutant: mutant ? { file: mutant.file, index: mutant.index } : null, environment: environmentOfJob }, limitMs).then(({ timedOut, error, notStarted, fallback, tainted }) => {
                if (notStarted) {
                    broken.add(runnerOf.get(file));
                    spawnJob();
                    return;
                }
                // The resident runner did not run the job as the command
                // would have: the command runs it, from a clean slate.
                if (fallback !== undefined) {
                    profile.residentFallbacks += 1;
                    try {
                        appendFileSync(join(work, "resident-fallbacks.log"), `${file}: ${fallback}\n`);
                    }
                    catch {
                        // The log is for reading only.
                    }
                    if (tainted)
                        broken.add(runnerOf.get(file));
                    else
                        commandOnly.add(file);
                    rmSync(evidence, { recursive: true, force: true });
                    mkdirSync(evidence, { recursive: true });
                    rmSync(`${results}.timeouts.jsonl`, { force: true });
                    spawnJob();
                    return;
                }
                if (tainted)
                    broken.add(runnerOf.get(file));
                profile.residentJobs += 1;
                if (timedOut)
                    profile.stopped += 1;
                profile.jobMs += Date.now() - begun;
                const mutantKey = mutant ? `${mutant.file}#${mutant.index}` : undefined;
                resolveJob(read({
                    timedOut,
                    resident: true,
                    mutantTimedOut: mutantKey !== undefined && readTimeouts(`${results}.timeouts.jsonl`).includes(mutantKey),
                    exitCode: error ? 1 : 0,
                    error,
                    durationMs: Date.now() - begun,
                }));
            });
        }));
    }

    // What a file does without a mutant: once with every test a mutant will
    // run there, and, where that cannot stand for the file, once whole.
    const files = new Map();
    const fileState = (file) => {
        if (!files.has(file))
            files.set(file, { union: new Set(), wholeNeeded: false, subset: undefined, whole: undefined });
        return files.get(file);
    };
    const confirmations = new Map();
    const cost = (file, chosen) => (chosen ? [...chosen] : (testsOf.get(file) ?? []).map((test) => test.id))
        .reduce((sum, id) => sum + (byId.get(id)?.durationMs ?? 1_000), 0);
    const baselineLimit = (file, chosen) => 3 * cost(file, chosen) + baselineGraceMs;
    const passedAll = (result, chosen) => !result.timedOut && [...chosen].every((id) => result.statuses.get(id) === "passed");
    async function wholeBaseline(file) {
        const state = fileState(file);
        state.whole ??= (async () => {
            const once = async () => {
                profile.baselines += 1;
                const result = await job({ mutant: null, file, tests: null, bail: false, limitMs: baselineLimit(file, null) });
                const passing = new Set([...result.statuses].filter(([, status]) => status === "passed").map(([id]) => id));
                return { ...result, passing, finished: !result.timedOut && passing.size > 0 };
            };
            return commandWhereResidentFails(file, once, (result) => result.finished && [...result.statuses.values()].every((status) => status === "passed" || status === "skipped"));
        })();
        return state.whole;
    }
    // A baseline the resident runner ran that did not pass as it should is
    // run again as a command, and the file's jobs are the command's from
    // then on: what a test does without a mutant is the command's to say.
    async function commandWhereResidentFails(file, once, clean) {
        const result = await once();
        if (!result.resident || clean(result))
            return result;
        commandOnly.add(file);
        return once();
    }
    // The baseline a mutant's job in `file` is judged by: the subset's where
    // it stood for the file, else the whole file's.
    async function baselineFor(file) {
        const state = fileState(file);
        if (!state.wholeNeeded && state.union.size > 0) {
            state.subset ??= (async () => {
                const once = async () => {
                    profile.baselines += 1;
                    const chosen = new Set(state.union);
                    const result = await job({ mutant: null, file, tests: chosen, bail: false, limitMs: baselineLimit(file, chosen) });
                    return { ...result, chosen, finished: passedAll(result, chosen) };
                };
                return commandWhereResidentFails(file, once, (result) => result.finished);
            })();
            const subset = await state.subset;
            if (subset.finished)
                return subset;
            state.wholeNeeded = true;
        }
        return { ...(await wholeBaseline(file)), chosen: null };
    }
    // The tests a kill ran, again without the mutant: whether the killing
    // test fails without it too.
    async function failsWithout(file, chosen, killer, limitMs) {
        const key = `${file}\0${[...chosen].sort().join("\0")}`;
        if (!confirmations.has(key)) {
            profile.confirmations += 1;
            confirmations.set(key, job({ mutant: null, file, tests: chosen, bail: false, limitMs }));
        }
        const result = await confirmations.get(key);
        return result.timedOut || result.statuses.get(killer) !== "passed";
    }

    // A mutant against one file's tests: killed, timeout, or alive.
    async function tryFile(mutant, group, priority) {
        let baseline = group.whole ? { ...(await wholeBaseline(group.file)), chosen: null } : await baselineFor(group.file);
        if (!baseline.finished)
            return { status: "error", reason: `${group.file} does not finish without a mutant, so it could not test this one`, tests: 0 };
        let whole = group.whole || baseline.chosen === null;
        for (;;) {
            // Where the baseline's tests take little time beside a start of
            // the command, the job runs them all: a kill is then judged by a
            // baseline that ran the same tests, and needs no confirmation.
            const cheap = !whole && baseline.chosen && baseline.durationMs <= UNION_MS;
            const chosen = whole ? null : new Set(cheap ? [...baseline.chosen, ...group.tests] : group.tests);
            const excluded = whole ? (testId) => !baseline.passing.has(testId) : () => false;
            const limitMs = 3 * baseline.durationMs + graceMs;
            // A test that fails without the mutant must not stop the job.
            const bail = !whole || [...baseline.statuses.values()].every((status) => status === "passed");
            let result = await job({ mutant, file: group.file, tests: chosen, bail, limitMs, priority });
            const killerIn = (outcome) => [...outcome.statuses].find(([id, status]) => status === "failed" && (chosen === null || chosen.has(id)) && !excluded(id))?.[0];
            // Past its limit with nothing failed: it may only have waited, on
            // the jobs beside it or on a machine busier than when its
            // baseline ran. It runs again alone, its limit from the same
            // tests without the mutant run alone just before it, at the
            // machine's speed now.
            if (result.timedOut && !result.mutantTimedOut && killerIn(result) === undefined) {
                profile.alone += 1;
                // A resident runner's limit is tighter than a command's, which
                // starts the runner and the browser within it: the job runs
                // again as a command, with a command's limit.
                const asCommand = Boolean(result.resident);
                const now = await job({ mutant: null, file: group.file, tests: chosen, bail: false, limitMs: baselineLimit(group.file, chosen), alone: true, command: asCommand });
                const slowest = Math.max(baseline.durationMs, now.durationMs);
                result = await job({ mutant, file: group.file, tests: chosen, bail, limitMs: 3 * slowest + aloneGraceMs, alone: true, command: asCommand });
            }
            const ran = [...result.statuses].filter(([id, status]) => status !== "skipped" && (chosen === null || chosen.has(id)));
            const killer = killerIn(result);
            if (result.mutantTimedOut)
                return { status: "timeout", reason: "the mutant's evaluations passed its limit", tests: ran.length };
            if (killer === undefined) {
                if (result.timedOut)
                    return { status: "timeout", reason: `${group.file} did not finish within its limit`, tests: ran.length };
                // A test the baseline ran that this job never reported: the
                // test file stopped short.
                const expected = whole ? [...baseline.passing] : [...chosen];
                if (expected.some((id) => !result.statuses.has(id)))
                    return { status: "killed", killedBy: group.file, reason: "the test file did not finish", tests: ran.length };
                return { status: "alive", tests: ran.length };
            }
            const name = byId.get(killer)?.name ?? killer;
            // Judged by a baseline that ran the same tests: certain.
            const same = whole || (baseline.chosen !== null && baseline.chosen.size === chosen.size && [...chosen].every((id) => baseline.chosen.has(id)));
            if (same || !(await failsWithout(group.file, chosen, killer, baselineLimit(group.file, chosen))))
                return { status: "killed", killedBy: name, tests: ran.length };
            // The killing test fails without the mutant too, with only these
            // tests before it: the mutant runs again with its file whole.
            profile.overturned += 1;
            baseline = { ...(await wholeBaseline(group.file)), chosen: null };
            if (!baseline.finished)
                return { status: "error", reason: `${group.file} does not finish without a mutant, so it could not test this one`, tests: 0 };
            whole = true;
        }
    }

    // Each test file's kills over the mutants it ran so far in the pass: a
    // mutant's next file is the one likeliest to kill it for its cost, as the
    // Vitest driver orders its copies. Most killed mutants die in their first
    // file, and the cheapest file is often not the one that kills.
    const killsOf = new Map();
    const promise = (group) => {
        const seen = killsOf.get(group.file) ?? { ran: 0, kills: 0 };
        return (seen.kills + 1) / (seen.ran + 2) / (group.cost + JOB_START_MS);
    };
    async function runMutant(mutant, groups, elsewhere) {
        const begun = Date.now();
        let ran = 0;
        const left = [...groups];
        // Its first file alone, as most killed mutants die in it; then the
        // rest in batches of 4, 16, ... side by side, the likeliest first: a
        // survivor needs them all, and one after another its files were the
        // pass's longest wait (luxon: a module-level mutant's 58 test files).
        for (let size = 1, stage = 0; left.length > 0; size *= 4, stage += 1) {
            const batch = [];
            while (batch.length < size && left.length > 0) {
                // Ties go to the cheapest, as the groups are sorted by cost.
                const at = left.reduce((best, group, index) => (promise(group) > promise(left[best]) ? index : best), 0);
                batch.push(...left.splice(at, 1));
            }
            const verdicts = await Promise.all(batch.map(async (group) => {
                const verdict = await tryFile(mutant, group, stage);
                const seen = killsOf.get(group.file) ?? { ran: 0, kills: 0 };
                seen.ran += 1;
                if (verdict.status === "killed" || verdict.status === "timeout")
                    seen.kills += 1;
                killsOf.set(group.file, seen);
                return verdict;
            }));
            ran += verdicts.reduce((sum, verdict) => sum + verdict.tests, 0);
            // A kill decides it, then a timeout, then a file that does not
            // finish without it.
            const verdict = ["killed", "timeout", "error"]
                .map((status) => verdicts.find((one) => one.status === status))
                .find(Boolean);
            if (verdict) {
                write({ id: mutant.id, status: verdict.status, ...(verdict.killedBy ? { killedBy: verdict.killedBy } : {}), tests: ran, durationMs: Date.now() - begun, ...(verdict.reason ? { reason: verdict.reason } : {}) });
                return;
            }
        }
        // A mutant that candidates under another runner may still kill is
        // not a survivor this pass can tell.
        if (elsewhere.size > 0) {
            write({ id: mutant.id, status: "error", tests: ran, durationMs: Date.now() - begun, reason: `its other candidates ran under ${[...elsewhere].sort().join(", ")}, which this pass does not run` });
            return;
        }
        write({ id: mutant.id, status: "survived", tests: ran, durationMs: Date.now() - begun });
    }

    const pending = [];
    for (const planned of plan.mutants) {
        if (planned.decided)
            continue;
        // A Rust mutant (Rust compiled to wasm) is switched by its index in
        // its file's list, as the pass's build placed it.
        const found = planned.language === "rust" ? { index: planned.index, entry: {} } : indexes.get(planned.id);
        if (!found) {
            write({ id: planned.id, status: "error", reason: "the copy lists no such mutant: the source changed since the run", tests: 0, durationMs: 0 });
            continue;
        }
        if (found.entry.unplaceable) {
            write({ id: planned.id, status: "error", reason: `no switch could be placed (${found.entry.unplaceable})`, tests: 0, durationMs: 0 });
            continue;
        }
        const mutant = { ...planned, index: found.index };
        const byFile = new Map();
        const elsewhere = new Set();
        for (const candidate of planned.candidates) {
            const test = tests[candidate.test];
            if (!handled(test) || !test.file) {
                elsewhere.add(test?.runner ?? "an unknown runner");
                continue;
            }
            if (!byFile.has(test.file))
                byFile.set(test.file, { file: test.file, tests: [], cost: 0, whole: false });
            const group = byFile.get(test.file);
            group.tests.push(test.id);
            group.cost += test.durationMs ?? 1_000;
        }
        for (const file of planned.wholeFiles ?? []) {
            if (!testsOf.has(file))
                continue;
            if (!byFile.has(file))
                byFile.set(file, { file, tests: [], cost: cost(file, null), whole: true });
            byFile.get(file).whole = true;
        }
        const groups = [...byFile.values()].sort((a, b) => a.cost - b.cost);
        if (!groups.length) {
            write(elsewhere.size
                ? { id: planned.id, status: "error", tests: 0, durationMs: 0, reason: `its candidates ran under ${[...elsewhere].sort().join(", ")}, which this pass does not run` }
                : { id: planned.id, status: "survived", reason: "no-runnable-candidate", tests: 0, durationMs: 0 });
            continue;
        }
        for (const group of groups) {
            const state = fileState(group.file);
            if (!group.whole)
                group.tests.forEach((id) => state.union.add(id));
        }
        pending.push([mutant, groups, elsewhere]);
    }
    // Every file's baseline runs the tests of all its mutants: none starts
    // before all are known. Eight times as many mutants as workers are under
    // way at once, enough to keep every worker busy while some wait on a
    // file's baseline; a mutant that starts later picks its first file from
    // what the pass learned.
    const queue = pending.values();
    await Promise.all(Array.from({ length: Math.min(pending.length, 8 * workers) }, async () => {
        for (const task of queue)
            await runMutant(...task);
    }));
    jestHosts.close();
    mochaHosts.close();
    esmHosts.close();
    vitestHosts.close();
    write({ summary: true, driver: "jobs", workers, ...profile, durationMs: Date.now() - started });
    return 0;
}

// `node strongJobs.mjs -- <command>`
if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
    // Bun leaves the `--` out of process.argv.
    const at = process.argv.indexOf("--", 2);
    const command = process.argv.slice(at < 0 ? 2 : at + 1);
    const planPath = process.env.SUPERCOV_STRONG_PLAN;
    const resultsPath = process.env.SUPERCOV_STRONG_RESULTS;
    if (!command.length || !planPath || !resultsPath) {
        console.error("[supercov] strongJobs.mjs: a strong pass starts it with its plan, its results and -- <command>");
        process.exit(2);
    }
    // Ctrl-C reaches this process alone: the jobs' process groups are their
    // own. They stop with it, and so does it.
    for (const signal of ["SIGINT", "SIGTERM", "SIGHUP"]) {
        process.on(signal, () => {
            stopStarted();
            process.exit(128 + constants.signals[signal]);
        });
    }
    process.on("exit", stopStarted);
    const plan = JSON.parse(readFileSync(planPath, "utf8"));
    process.exitCode = await drive({ command, plan, resultsPath });
}
