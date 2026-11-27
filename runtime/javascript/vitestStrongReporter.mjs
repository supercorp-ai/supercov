// Strong mutation's driver for Vitest (docs/design/strong-mutation.md).
//
// Vitest hands its reporters the running instance before it runs a file
// (`onInit`). In strong mode this reporter takes the instance's `runFiles`
// over: instead of one run of every file, it runs the plan in stages through
// the same resident instance -- the project's own pool and workers, no new
// process -- and records each mutant's verdict.
//
// A stage is one Vitest run of every copy the pass can run yet: each copy is
// one test file with at most one mutant. A file is evaluated in an isolated
// module graph, so copies of one stage can each carry a different mutant
// without one reaching another. A mutant with candidates in several files
// runs its cheapest file first and, if that leaves it alive, its other files
// in the next stage, and stops at its first kill. Where the project does not
// isolate files, a stage holds one mutant.
import childProcess from "node:child_process";
import { appendFileSync, closeSync, existsSync, fstatSync, mkdirSync, openSync, readFileSync, readSync, rmSync, statSync, writeFileSync } from "node:fs";
import { createHash } from "node:crypto";
import { EventEmitter } from "node:events";
import { syncBuiltinESMExports } from "node:module";
import { PassThrough } from "node:stream";
import { fileURLToPath } from "node:url";
import { availableParallelism } from "node:os";
import { relative, sep } from "node:path";

// A mutant's test, and its copy, get twice StrykerJS's limit: 1.5× the
// measured time and 5 s, doubled.
const TIMEOUT_FACTOR = 1.5;
const TIMEOUT_CONSTANT_MS = 5_000;
const HIT_LIMIT_FLOOR = 1_000;
// A baseline's tests run with no mutant, and only a test that cannot finish
// at all needs a limit there.
const BASELINE_TEST_TIMEOUT_MS = 60_000;
// How often a stage's copies are looked at while it runs, and how long a
// cancelled run gets to end: its workers are asked to stop, and after
// CANCEL_FORCE_AFTER_MS stopped by force.
const WATCH_INTERVAL_MS = 500;
const CANCEL_FORCE_AFTER_MS = 3_000;
const CANCEL_GRACE_MS = 15_000;
// The pass summary keeps this many stages' figures.
const PROFILED_STAGES = 50;
// How often the continuous pass looks at its copies (a free slot waits on
// their ends).
const CONTINUOUS_LOOK_MS = 50;
// How much each of a mutant's batches of later files grows over the one before.
const BATCH_GROWTH = Math.max(1, Number(process.env["SUPERCOV_STRONG_BATCH_GROWTH"]) || 4);
// A continuous run's slots beyond each file's own, and the bytes of test
// files they may name: Vitest reads every slot's file at once as the run
// starts (with a buffer of up to 512 KB each), and 50,000 slots (a pass over
// all of hono's mutants at once) ran its main process out of memory within
// seconds. A pass that needs more slots runs again; between runs its workers
// wait for the run's last copies, which cost tRPC's shards 9% at 1,000 slots
// (SUPERCOV_STRONG_RUN_SLOTS sets the count).
const RUN_SLOTS = Math.max(1, Number(process.env["SUPERCOV_STRONG_RUN_SLOTS"]) || 2_000);
const RUN_BYTES = 1024 * 1024 * 1024;

function toRelative(root, file) {
    return relative(root, file).split(sep).join("/");
}

function isolated(vitest) {
    for (const project of vitest.projects ?? [vitest.getCoreWorkspaceProject?.()]) {
        const config = project?.config;
        if (!config)
            continue;
        const pool = config.pool ?? "forks";
        const options = config.poolOptions?.[pool] ?? {};
        if (options.isolate === false || config.isolate === false)
            return false;
        if (options.singleThread || options.singleFork)
            return false;
    }
    return true;
}

// The longest a hook may take, as the projects set it (Vitest's default 10 s).
function hookTimeoutOf(vitest) {
    const configs = [vitest.config, ...(vitest.projects ?? []).map((project) => project.config)];
    return Math.max(10_000, ...configs.map((config) => Number(config?.hookTimeout) || 0));
}

const pause = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

// This process's memory, in MB: a worker is forked from it, and a fork takes
// longer the more memory it maps.
function memoryOf() {
    const { rss, heapUsed, heapTotal, external } = process.memoryUsage();
    const memory = { rssMb: Math.round(rss / 1e6), heapMb: Math.round(heapUsed / 1e6), heapTotalMb: Math.round(heapTotal / 1e6), externalMb: Math.round(external / 1e6) };
    // Linux: the page tables a fork copies, and what the resident memory is.
    try {
        const status = readFileSync("/proc/self/status", "utf8");
        for (const [name, key] of [["VmPTE", "pageTablesMb"], ["RssAnon", "anonMb"], ["RssFile", "fileMb"], ["Threads", "threads"]]) {
            const kb = Number(status.match(new RegExp(`^${name}:\\s+(\\d+)`, "m"))?.[1]);
            if (kb)
                memory[key] = key === "threads" ? kb : Math.round(kb / 1e3);
        }
    }
    catch {
        // Not Linux.
    }
    return memory;
}

// Linux: the machine's CPU time so far, busy and in all, to tell how much of
// the machine a stage kept busy.
function machineCpu() {
    try {
        const fields = readFileSync("/proc/stat", "utf8").split("\n", 1)[0].trim().split(/\s+/).slice(1).map(Number);
        const idle = fields[3] + (fields[4] ?? 0);
        const total = fields.reduce((sum, value) => sum + value, 0);
        return { busy: total - idle, total };
    }
    catch {
        return undefined;
    }
}

// What a copy reported (vitestStrong.mjs): its claim, its tests starting,
// each test's outcome, errors outside any test, its end.
function readOutcome(out) {
    const outcome = { claimed: false, done: false, tests: [], outside: [], records: 0 };
    if (!existsSync(out))
        return outcome;
    for (const line of readFileSync(out, "utf8").split("\n")) {
        if (!line.trim())
            continue;
        let record;
        try {
            record = JSON.parse(line);
        }
        catch {
            // A line the copy is still writing.
            continue;
        }
        outcome.records += 1;
        if (record.claimed) {
            outcome.claimed = true;
            outcome.claimedAt = record.at;
            outcome.pid = record.pid;
            outcome.up = record.up;
            outcome.skipped = Boolean(record.skipped);
            outcome.notRun = Boolean(record.notRun);
        }
        else if (record.started)
            outcome.startedAt = record.at;
        else if (record.done) {
            outcome.done = true;
            outcome.doneAt = record.at;
            outcome.channelLost = Boolean(record.channelLost);
        }
        else if (record.test)
            outcome.tests.push(record);
        else if (record.outside)
            outcome.outside.push(record);
    }
    return outcome;
}

// What a write to a worker process that has ended fails with.
const CLOSED_CHANNEL = new Set(["ERR_IPC_CHANNEL_CLOSED", "EPIPE", "ERR_IPC_DISCONNECTED"]);

// Whether a process is still running (signal 0 sends nothing).
function alive(pid) {
    try {
        process.kill(pid, 0);
        return true;
    }
    catch (error) {
        return error?.code === "EPERM";
    }
}

// Vitest 4 and later fork each worker from `dist/workers/forks.js` (or
// `vmForks.js`); Vitest 1-3 fork through tinypool, and are left as they are.
const VITEST_WORKER = /[\\/]vitest[\\/]dist[\\/]workers[\\/](?:forks|vmForks)\.js$/;
// The spawner's process id while it runs (vitestStrongSpawner.mjs), and the
// workers it forked.
const spawnerPids = new Set();
let spawnedWorkers = 0;
// When the spawner handed each worker process to Vitest: a worker started
// ahead was waiting before that, and its copy's start counts from here.
const handedAt = new Map();
// What the spawner reports of its workers started ahead.
let spawnerStats;
let askStats = async () => undefined;
// While a stage runs: which of its assignments a copy of a test file takes
// (vitestStrongSpawner.mjs's stand-ins ask as Vitest tells them the file),
// and whether any copy got its assignment this way.
let claimRun;
let mainClaims = false;
// The continuous pass learns each test file's project and environment from
// Vitest's "start" to its worker.
let observeRun;
// Whether Supercov's spawner forks Vitest's workers.
let spawnerInstalled = false;

// Vitest 3's workers are tinypool's processes, forked by tinypool. One the
// pass stops (a copy a failed test decided, or one past its limit), or one a
// mutant ends, has lost its channel while tinypool still relays to it: the
// write fails with "Channel closed" or EPIPE, which tinypool 1.1 rejects with
// nothing to catch it, and Vitest 3.2 ended the whole run at that, the
// mutants left without verdicts (destr: 81 of 141, in every pass). The
// process's exit tells tinypool it is gone, as it does for any worker that
// dies; the failed write says nothing more, and is dropped.
const TINYPOOL_WORKER = /[\\/]tinypool[\\/]dist[\\/]entry[\\/]process\.js$/;
function quietWhenGone(modulePath, child) {
    const path = modulePath instanceof URL ? fileURLToPath(modulePath) : String(modulePath);
    if (!TINYPOOL_WORKER.test(path) || !child)
        return child;
    const emit = child.emit;
    child.emit = function (event, ...rest) {
        if (event === "error" && CLOSED_CHANNEL.has(rest[0]?.code))
            return false;
        return emit.call(this, event, ...rest);
    };
    return child;
}

// Vitest's workers forked by a small process of their own instead of
// Vitest's: forking copies the parent's memory map, and Vitest's process
// held about a gigabyte (on hono each fork held its event loop for 24 ms,
// 72% of its time, while its workers waited on it). Each worker Vitest asks
// for is a stand-in here, relaying to and from the real one through the
// spawner. SUPERCOV_STRONG_DIRECT_FORK=1 forks them directly.
//
// The spawner also keeps `ahead` workers started ahead of Vitest's requests
// for them (vitestStrongSpawner.mjs; SUPERCOV_STRONG_WORKERS_AHEAD=0 starts
// each when Vitest asks).
function forkThroughSpawner(ahead) {
    const original = childProcess.fork;
    if (process.env["SUPERCOV_STRONG_DIRECT_FORK"] === "1") {
        childProcess.fork = function fork(modulePath) {
            return quietWhenGone(modulePath, original.apply(this, arguments));
        };
        syncBuiltinESMExports();
        return;
    }
    spawnerInstalled = true;
    const standIns = new Map();
    // One spawner: every message to and from a worker goes through it, and
    // four of them sharing the workers were no faster on hono.
    const spawner = { process: undefined, live: 0 };
    let nextId = 0;
    // A spawner's channel keeps this process alive only while one of its
    // workers does, as the worker's own handle did.
    const hold = (spawner) => {
        if (!spawner.process)
            return;
        if (spawner.live > 0) {
            spawner.process.ref();
            spawner.process.channel?.ref?.();
        }
        else {
            spawner.process.unref();
            spawner.process.channel?.unref?.();
        }
    };
    const start = (spawner) => {
        const child = original(fileURLToPath(new URL("./vitestStrongSpawner.mjs", import.meta.url)), [], {
            stdio: ["ignore", "inherit", "inherit", "ipc"],
            serialization: "advanced",
            execArgv: [],
        });
        spawner.process = child;
        spawnerPids.add(child.pid);
        child.send({ pool: ahead });
        child.on("message", (record) => {
            if (record.stats) {
                spawnerStats = record.stats;
                return;
            }
            standIns.get(record.id)?.receive(record);
        });
        child.on("error", () => { });
        child.on("exit", () => {
            spawnerPids.delete(child.pid);
            spawner.process = undefined;
            for (const standIn of [...standIns.values()])
                if (standIn.spawner === spawner)
                    standIn.receive({ exit: [null, "SIGKILL"] });
        });
    };
    // The environments the spawner was told of, by worker entry.
    const environments = new Set();
    askStats = async () => {
        if (!spawner.process?.connected)
            return spawnerStats;
        spawner.process.send({ stats: true });
        for (let waited = 0; waited < 1_000 && !spawnerStats; waited += 20)
            await pause(20);
        return spawnerStats;
    };
    // A stand-in takes its process only once Vitest says which test file it
    // runs. Vitest's first message ("start") is answered here, and kept; its
    // "run" names the file, and the stage running (`claimRun`) says which of
    // that file's assignments the copy takes. A copy with nothing to run
    // (its mutant decided meanwhile, or beyond its file's assignments) gets
    // no process at all: it ends here, as its setup file would have ended
    // it. Any other copy's process gets the kept "start", then the "run".
    const response = (type, extra) => ({ __vitest_worker_response__: true, type, ...extra });
    class Worker extends EventEmitter {
        constructor(id, spawner, modulePath, forkRequest) {
            super();
            this.id = id;
            this.spawner = spawner;
            this.modulePath = modulePath;
            this.forkRequest = forkRequest;
            // Before its process: what Vitest sent it, and how it ends.
            this.attached = false;
            this.ended = false;
            this.held = [];
            this.pid = undefined;
            this.exitCode = null;
            this.signalCode = null;
            this.killed = false;
            this.connected = true;
            this.stdin = null;
            this.stdout = new PassThrough();
            this.stderr = new PassThrough();
            this.stdio = [null, this.stdout, this.stderr];
            this.channel = { ref() { }, unref() { } };
        }
        attach() {
            this.attached = true;
            spawnedWorkers += 1;
            this.spawner.process.send(this.forkRequest);
        }
        receive(record) {
            if (record.spawned !== undefined) {
                this.pid = record.spawned;
                handedAt.set(this.pid, Date.now());
                this.emit("spawn");
            }
            else if (record.error) {
                // A write to a worker that is ending: its exit says the rest.
                if (!CLOSED_CHANNEL.has(record.error.code))
                    this.emit("error", Object.assign(new Error(record.error.message), { code: record.error.code }));
            }
            else if (record.exit) {
                this.exited(...record.exit);
            }
            else if (record.stdout)
                this.stdout.write(record.stdout);
            else if (record.stderr)
                this.stderr.write(record.stderr);
            else if ("message" in record) {
                const message = record.message;
                // The kept "start" was answered already: the process's own
                // answer lets the "run" go, or fails it.
                if (this.pendingRun && message?.__vitest_worker_response__ && message.type === "started") {
                    const run = this.pendingRun;
                    this.pendingRun = undefined;
                    if (message.error)
                        this.emit("message", response("testfileFinished", { error: message.error }));
                    else
                        this.relay(run);
                    return;
                }
                if (message?.__vitest_worker_response__ && message.type === "testfileFinished")
                    this.between = true;
                this.emit("message", message);
            }
        }
        exited(code, signal) {
            if (this.ended)
                return;
            // Between its file's end and Vitest's "stop" (a worker the pass
            // stopped once its mutant was decided), Vitest has nothing that
            // takes a worker's exit and fails: the exit waits for the "stop".
            if (this.between && !this.stopping) {
                this.exitAfterStop = [code, signal];
                return;
            }
            this.ended = true;
            [this.exitCode, this.signalCode] = [code, signal];
            this.connected = false;
            standIns.delete(this.id);
            this.spawner.live -= 1;
            hold(this.spawner);
            this.stdout.end();
            this.stderr.end();
            this.emit("disconnect");
            this.emit("exit", this.exitCode, this.signalCode);
            this.emit("close", this.exitCode, this.signalCode);
        }
        relay(message) {
            this.spawner.process?.send({ id: this.id, send: true, message });
        }
        // Vitest's "run", once the stage said what this copy takes: nothing
        // (it ends here, with no process), or its assignment, and in the
        // continuous pass possibly another test file than the slot's.
        run(message, claimed) {
            if (this.ended || this.nothingToRun)
                return;
            if (claimed?.nothingToRun) {
                this.nothingToRun = true;
                process.nextTick(() => this.emit("message", response("testfileFinished")));
                return;
            }
            if (claimed) {
                const provided = message.context.providedContext ?? {};
                const [file] = message.context.files;
                const files = claimed.filepath
                    ? [{ filepath: claimed.filepath, fileTags: file.fileTags }]
                    : message.context.files;
                message = {
                    ...message,
                    context: {
                        ...message.context,
                        files,
                        providedContext: { ...provided, supercovStrong: { ...provided.supercovStrong, claim: claimed.index } },
                    },
                };
            }
            this.attach();
            this.relay(this.start);
            this.pendingRun = message;
        }
        send(message, ...rest) {
            const callback = rest.find((value) => typeof value === "function");
            // A worker that ended, or is ending: its exit says the rest, and
            // an error here would only tell Vitest the channel closed.
            if (!this.connected || !this.spawner.process) {
                // Vitest waits for a stopped worker's answer to its "stop".
                if (message?.__vitest_worker_request__ && message.type === "stop")
                    process.nextTick(() => this.emit("message", response("stopped")));
                if (callback)
                    process.nextTick(callback, Object.assign(new Error("Channel closed"), { code: "ERR_IPC_CHANNEL_CLOSED" }));
                return false;
            }
            if (callback)
                process.nextTick(callback, null);
            const request = message?.__vitest_worker_request__ ? message.type : undefined;
            // Vitest's first message to a worker names the environment it
            // runs in: workers started ahead load its package too.
            const environment = request === "start" ? message.context?.environment?.name : undefined;
            if (environment && !environments.has(`${this.modulePath}\0${environment}`)) {
                environments.add(`${this.modulePath}\0${environment}`);
                this.spawner.process.send({ environment, modulePath: this.modulePath });
            }
            if (this.attached) {
                if (request === "stop") {
                    this.stopping = true;
                    if (this.exitAfterStop) {
                        const [code, signal] = this.exitAfterStop;
                        process.nextTick(() => {
                            this.emit("message", response("stopped"));
                            this.exited(code, signal);
                        });
                        return true;
                    }
                }
                else if (request === "run" || request === "collect")
                    this.between = false;
                this.relay(message);
                return true;
            }
            // Stopped while it waited for work (a cancelled run): it ends
            // with no process.
            if (this.waitingRun && request === "stop")
                this.nothingToRun = true;
            if (this.nothingToRun) {
                if (request === "stop")
                    process.nextTick(() => this.emit("message", response("stopped")));
                return true;
            }
            if (request === "start" && !this.start) {
                this.start = message;
                process.nextTick(() => this.emit("message", response("started")));
                return true;
            }
            const files = request === "run" ? message.context?.files : undefined;
            if (this.start && files?.length === 1) {
                const filepath = files[0].filepath ?? files[0];
                observeRun?.(filepath, this.start);
                const claimed = claimRun?.(filepath, this.start, () => !this.ended && !this.nothingToRun);
                if (typeof claimed?.then === "function") {
                    this.waitingRun = true;
                    claimed.then((value) => {
                        this.waitingRun = false;
                        this.run(message, value);
                    });
                }
                else
                    this.run(message, claimed);
                return true;
            }
            // Vitest's calls to a worker (its cancel) before the worker has a
            // process: nothing to tell.
            if (!request)
                return true;
            // Anything else: the process now, and what Vitest sent it so far.
            this.attach();
            if (this.start)
                this.relay(this.start);
            this.relay(message);
            return true;
        }
        kill(signal = "SIGTERM") {
            // Its exit was held for Vitest's "stop", which a forced stop (a
            // cancelled run) skips: the exit comes now.
            if (this.exitAfterStop && !this.ended) {
                const [code, held] = this.exitAfterStop;
                this.exitAfterStop = undefined;
                this.stopping = true;
                process.nextTick(() => this.exited(code, held));
                return true;
            }
            if (this.exitCode !== null || this.signalCode !== null || this.ended)
                return false;
            this.killed = true;
            if (!this.attached) {
                process.nextTick(() => this.exited(null, signal));
                return true;
            }
            this.spawner.process?.send({ id: this.id, kill: signal });
            return true;
        }
        disconnect() {
            if (this.attached)
                this.spawner.process?.send({ id: this.id, disconnect: true });
        }
        // Vitest waits for "exit" when stopping a worker whose `exitCode` is
        // null, as it is for a process a signal ended: one the pass stopped
        // already (a copy a failed test decided) has emitted it, and would
        // hold Vitest's close for its teardown limit.
        once(event, listener) {
            if (event === "exit" && this.ended) {
                process.nextTick(listener, this.exitCode, this.signalCode);
                return this;
            }
            return super.once(event, listener);
        }
        ref() { }
        unref() { }
    }
    childProcess.fork = function fork(modulePath, args, options) {
        const path = modulePath instanceof URL ? fileURLToPath(modulePath) : String(modulePath);
        if (!VITEST_WORKER.test(path))
            return quietWhenGone(path, original.apply(this, arguments));
        if (!Array.isArray(args)) {
            options = args;
            args = [];
        }
        if (!spawner.process)
            start(spawner);
        const id = nextId++;
        const worker = new Worker(id, spawner, path, { id, fork: true, modulePath: path, args, options: { ...options } });
        spawner.live += 1;
        hold(spawner);
        standIns.set(worker.id, worker);
        return worker;
    };
    syncBuiltinESMExports();
}

// How many workers the spawner keeps started ahead: one per worker Vitest
// runs at once.
function workersAhead(vitest) {
    const set = process.env["SUPERCOV_STRONG_WORKERS_AHEAD"];
    if (set !== undefined && set !== "")
        return Math.max(0, Number(set) || 0);
    return Math.max(1, Number(vitest.config?.maxWorkers) || availableParallelism());
}

// How long a copy's worker took to start before it claimed: from when the
// spawner handed it to Vitest, or from its process's start.
function startOf(outcome) {
    const handed = outcome.pid ? handedAt.get(outcome.pid) : undefined;
    if (handed !== undefined && outcome.claimedAt !== undefined)
        return Math.max(0, outcome.claimedAt - handed);
    return outcome.up ?? 0;
}

// Whether a process is one of this process's own workers: running, and its
// child (or the spawner's) where /proc tells (a worker that ended may leave
// its id to another).
function ownWorker(pid) {
    if (!pid || pid === process.pid || !alive(pid))
        return false;
    let stat;
    try {
        stat = readFileSync(`/proc/${pid}/stat`, "utf8");
    }
    catch {
        return true;
    }
    const parent = Number(stat.slice(stat.lastIndexOf(")") + 2).split(" ")[1]);
    return parent === process.pid || spawnerPids.has(parent);
}

// The whole lines appended to `path` since the cursor, which moves past them.
function readAppended(path, cursor) {
    let descriptor;
    try {
        descriptor = openSync(path, "r");
    }
    catch {
        return [];
    }
    try {
        const size = fstatSync(descriptor).size;
        if (size <= cursor.offset)
            return [];
        const buffer = Buffer.alloc(size - cursor.offset);
        const read = readSync(descriptor, buffer, 0, buffer.length, cursor.offset);
        const end = buffer.subarray(0, read).lastIndexOf(10);
        if (end < 0)
            return [];
        cursor.offset += end + 1;
        return buffer.subarray(0, end).toString("utf8").split("\n").filter(Boolean);
    }
    finally {
        closeSync(descriptor);
    }
}

// Vitest sorts a run's files (slowest file first, by its own cache); the pass
// lists its copies longest first itself, and each copy claims the next of its
// file's assignments as it starts.
function keepOrder(vitest) {
    const Sequencer = vitest.config?.sequence?.sequencer;
    if (typeof Sequencer !== "function")
        return;
    class InOrder extends Sequencer {
        async sort(files) {
            return files;
        }
    }
    vitest.config.sequence.sequencer = InOrder;
    // Vitest 3 and later keep the sequencer they made first.
    if (vitest.sequencer)
        vitest.sequencer = new InOrder(vitest);
}

function provide(vitest, value) {
    for (const project of vitest.projects ?? []) {
        project.provide?.("supercovStrong", value);
    }
    vitest.provide?.("supercovStrong", value);
}

function setTestTimeout(vitest, ms) {
    for (const project of vitest.projects ?? []) {
        if (project.config)
            project.config.testTimeout = ms;
    }
    if (vitest.config)
        vitest.config.testTimeout = ms;
}

// SUPERCOV_STRONG_CPU_PROFILE=<file>: a CPU profile of this process -- the
// one that schedules the copies and serves its workers' modules -- for the
// whole pass, to see what holds the workers up.
async function profileMainProcess() {
    const path = process.env["SUPERCOV_STRONG_CPU_PROFILE"];
    if (!path)
        return async () => { };
    const { Session } = await import("node:inspector");
    const session = new Session();
    session.connect();
    const post = (method, params) => new Promise((resolve, reject) => session.post(method, params, (error, result) => error ? reject(error) : resolve(result)));
    await post("Profiler.enable");
    await post("Profiler.setSamplingInterval", { interval: 1000 });
    await post("Profiler.start");
    return async () => {
        const { profile } = await post("Profiler.stop");
        writeFileSync(path, JSON.stringify(profile));
        session.disconnect();
        // Linux: this process's largest mappings by resident memory, for what
        // a fork of it copies.
        try {
            const mappings = readFileSync("/proc/self/smaps", "utf8").split(/\n(?=[0-9a-f]+-[0-9a-f]+ )/).map((entry) => {
                const rss = Number(entry.match(/^Rss:\s+(\d+)/m)?.[1] ?? 0);
                return { rss, line: entry.split("\n")[0] };
            }).sort((a, b) => b.rss - a.rss);
            const total = mappings.reduce((sum, mapping) => sum + mapping.rss, 0);
            writeFileSync(`${path}.mappings.txt`, `${mappings.length} mappings, ${Math.round(total / 1e3)} MB resident\n${mappings.slice(0, 40).map((mapping) => `${Math.round(mapping.rss / 1e3)} MB ${mapping.line}`).join("\n")}\n`);
        }
        catch {
            // Not Linux.
        }
    };
}

export default class SupercovStrongReporter {
    constructor(configureProjects) {
        this.configureProjects = configureProjects;
    }
    onInit(vitest) {
        this.configureProjects?.(vitest.projects);
        const planPath = process.env["SUPERCOV_STRONG_PLAN"];
        const resultsPath = process.env["SUPERCOV_STRONG_RESULTS"];
        if (!planPath || !resultsPath)
            return;
        forkThroughSpawner(workersAhead(vitest));
        const original = vitest.runFiles.bind(vitest);
        vitest.runFiles = async (specs) => {
            const stopProfile = await profileMainProcess();
            const last = await drive(vitest, original, specs, JSON.parse(readFileSync(planPath, "utf8")), resultsPath);
            await stopProfile();
            // A killed mutant fails its test, and that is the point: the
            // command's exit code is the pass's, not the rounds'.
            process.exitCode = 0;
            // Vitest 3 and later read what the run gave back.
            return last ?? { testModules: [], unhandledErrors: [] };
        };
    }
}

async function drive(vitest, runFiles, specs, plan, resultsPath) {
    const root = vitest.config.root;
    // Vitest 2 and later pass specifications; Vitest 1, [project, file].
    const fileOf = (spec) => spec.moduleId ?? (Array.isArray(spec) ? spec[1] : spec);
    const specOf = new Map(specs.map((spec) => [toRelative(root, fileOf(spec)), spec]));
    const write = (record) => appendFileSync(resultsPath, `${JSON.stringify(record)}\n`);
    const tests = plan.tests;
    const started = Date.now();
    const passCpu = machineCpu();
    let last;

    // The copy's own list: the mutant at each index must be the run's, and
    // one the instrumenter could not place has no switch to turn on.
    let listed = {};
    try {
        listed = JSON.parse(readFileSync(`${root}/.supercov/frontend-evidence/mutants.json`, "utf8")).files ?? {};
    }
    catch {
        listed = {};
    }
    // A mutant's switch, by its id in the copy's list: the index the copy
    // gave it, or why it has none.
    const idOf = (file, entry) => entry.id ||
        `${file}:${entry.start?.[0]}:${entry.start?.[1]}-${entry.end?.[0]}:${entry.end?.[1]}:${entry.mutator}:${entry.replacement}`;
    const indexes = new Map();
    for (const [file, entries] of Object.entries(listed)) {
        entries.forEach((entry, index) => indexes.set(idOf(file, entry), { index, entry }));
    }
    const placed = (mutant) => {
        // A Rust mutant (Rust compiled to wasm) by its index, as the pass's build placed it.
        if (mutant.language === "rust")
            return { index: mutant.index };
        const found = indexes.get(mutant.id);
        if (!found)
            return { error: "the copy lists no such mutant: the source changed since the run" };
        if (found.entry.unplaceable)
            return { error: `no switch could be placed (${found.entry.unplaceable})` };
        return { index: found.index };
    };

    // Each mutant to run: its candidates grouped by test file, the cheapest
    // file first.
    const pending = [];
    for (let mutant of plan.mutants) {
        if (mutant.decided)
            continue;
        const switchOf = placed(mutant);
        if (switchOf.error) {
            write({ id: mutant.id, status: "error", reason: switchOf.error, tests: 0, durationMs: 0 });
            continue;
        }
        mutant = { ...mutant, index: switchOf.index };
        const byFile = new Map();
        for (const candidate of mutant.candidates) {
            const test = tests[candidate.test];
            if (!test?.file || !specOf.has(test.file))
                continue;
            if (!byFile.has(test.file))
                byFile.set(test.file, { file: test.file, tests: [], cost: 0, whole: false });
            const group = byFile.get(test.file);
            // A Rust wasm run names a test by its file too (`<file>::vitest:<id>`).
            group.tests.push(test.id.startsWith(`${test.file}::`) ? test.id.slice(test.file.length + 2) : test.id);
            group.cost += test.durationMs ?? 1_000;
        }
        for (const file of mutant.wholeFiles ?? []) {
            if (!specOf.has(file))
                continue;
            if (!byFile.has(file))
                byFile.set(file, { file, tests: [], cost: 0, whole: true });
            byFile.get(file).whole = true;
        }
        const files = [...byFile.values()].sort((a, b) => a.cost - b.cost);
        if (!files.length) {
            write({ id: mutant.id, status: "survived", reason: "no-runnable-candidate", tests: 0, durationMs: 0 });
            continue;
        }
        pending.push({ mutant, files, ran: 0, started: 0 });
    }

    // Stages run through claims: each assignment is a file in the stage's
    // directory, and each copy of a test file Vitest runs takes the next one
    // for its file (vitestStrong.mjs) and reports its tests' outcomes beside it.
    const stagesDirectory = `${root}/.supercov/strong-stages`;
    rmSync(stagesDirectory, { recursive: true, force: true });
    const keyOf = (file) => createHash("sha256").update(file).digest("hex").slice(0, 24);
    keepOrder(vitest);
    let stageNumber = 0;
    let stalled = false;
    // The stage running: where each of its assignments was placed.
    let running;
    // Where the pass's time goes: each stage's wall time against the copies'
    // own (claim to end) and when its last copy started.
    const profile = { stages: [], spawnedWorkers: 0, wallMs: 0, copies: 0, mutantCopies: 0, checkCopies: 0, skippedCopies: 0, killedCopies: 0, settledCopies: 0, baselineCopies: 0, besideCopies: 0, unevaluatedReruns: 0, filesByStatus: {}, hungCopies: 0, deferredCopies: 0, cancelledStages: 0, testsRun: 0, copyMs: 0, startMs: 0, baselineMs: 0 };
    // Runs one stage. `early(at, outcome)` is offered each copy's outcome as
    // soon as the copy ends, and says whether it took it; a stage's copies
    // are otherwise judged when its run has ended. `limitOf(assignment)` is
    // how long a mutant's copy may run, if known yet; `expectedOf` how long it
    // should take.
    async function runStage(assignments, early, limitOf, expectedOf) {
        const directory = `${stagesDirectory}/${stageNumber++}`;
        const counts = new Map();
        const outs = [];
        const placed = [];
        const positionOf = new Map();
        mkdirSync(directory, { recursive: true });
        assignments.forEach((assignment, at) => {
            const key = keyOf(assignment.file);
            const index = counts.get(key) ?? 0;
            counts.set(key, index + 1);
            if (!index)
                mkdirSync(`${directory}/${key}`, { recursive: true });
            writeFileSync(`${directory}/${key}/${index}.json`, JSON.stringify(assignment.claim));
            outs.push(`${directory}/${key}/${index}.out`);
            placed.push(`${directory}/${key}/${index}`);
            positionOf.set(`${key}/${index}`, at);
        });
        running = { placed };
        provide(vitest, { root, stage: directory });
        // A copy whose worker Vitest has just told the file it runs takes
        // the file's next assignment, here, in the order Vitest starts them.
        // One with nothing to run ends without a process: its mutant was
        // decided meanwhile (recorded as its setup file would), or it is
        // beyond its file's assignments.
        const nextOf = new Map();
        // A copy held back (`gated`: a mutant's next files, listed after the
        // stage's other work) runs only once its mutant's other copies in the
        // stage have ended with every test passing, else it goes back to its
        // mutant's files to run later.
        const ended = (position) => stopped.has(position) || deferred.has(position) || decided.has(position) || readOutcome(outs[position]).done;
        claimRun = (filepath) => {
            mainClaims = true;
            const key = keyOf(toRelative(root, String(filepath)));
            const index = nextOf.get(key) ?? 0;
            nextOf.set(key, index + 1);
            const at = `${directory}/${key}/${index}`;
            if (!existsSync(`${at}.json`))
                return { nothingToRun: true };
            const position = positionOf.get(`${key}/${index}`);
            const assignment = assignments[position];
            const skipped = existsSync(`${at}.skip`);
            // Held back too where a copy before it failed a test: most such
            // failures kill the mutant, which the stage judges a moment later.
            const notRun = !skipped && assignment?.gated &&
                assignment.job.positions.some((other) => !assignments[other].gated &&
                    (!ended(other) || decided.has(other) || readOutcome(outs[other]).tests.some((test) => test.failed)));
            if (skipped || notRun) {
                const now = Date.now();
                appendFileSync(`${at}.out`, `${JSON.stringify({ claimed: true, at: now, up: 0, skipped: true, notRun })}\n${JSON.stringify({ done: true, at: now })}\n`);
                appendFileSync(`${directory}/claimed`, `${key}/${index}\n`);
                return { nothingToRun: true };
            }
            return { index };
        };
        // A copy sets its own test limit (vi.setConfig); the longest is the
        // run's, for a setup that cannot.
        const testTimeoutMs = Math.max(...assignments.map((assignment) => assignment.claim.testTimeout));
        setTestTimeout(vitest, testTimeoutMs);
        const hookMs = hookTimeoutOf(vitest);
        const stageStarted = Date.now();
        const cpuBefore = machineCpu();
        let settled;
        const ran = runFiles(assignments.map((assignment) => specOf.get(assignment.file)), false)
            .then((result) => {
                settled = { result };
            }, (error) => {
                settled = { error };
            });
        // A copy reports as it goes: its claim, each test, its end. One that
        // stops (a mutant that left a module's import waiting forever, a
        // loop that never yields) must not hold its worker. A mutant's copy
        // may take three times its file's baseline copy and 10 s more (twice
        // StrykerJS's limit), plus the longest test or hook limit: a test
        // that hangs is Vitest's to end, as a timeout. Past that its worker
        // process is stopped, and the stage goes on. A worker that is a
        // thread of Vitest's own process cannot be stopped alone: once every
        // copy still going is such a one past its limit, or nothing has
        // reported for twice the longest test or hook limit and 30 s more,
        // the stage's run is cancelled.
        const quietLimit = Math.max(testTimeoutMs, hookMs) * 2 + 30_000;
        const claims = { offset: 0 };
        const going = new Map();
        const handled = new Set();
        const stopped = new Map();
        // Copies stopped once a failed test decided them.
        const decided = new Map();
        const settles = (at, outcome) => {
            const assignment = assignments[at];
            return Boolean(assignment.job && assignment.claim.mutant && known(assignment.file) &&
                failedTest(outcome) && ownWorker(outcome.pid));
        };
        // Copies stopped at the stage's end, to run again in the next.
        const deferred = new Map();
        let claimedCount = 0;
        let lastReport = stageStarted;
        let lastClaim = stageStarted;
        // What every copy had reported when the run was cancelled. A
        // cancelled copy ends gracefully, its tests left skipped: only what
        // it reported before counts. A copy still going then is a timeout
        // unless a test had already failed; one no worker had taken runs in
        // a later stage.
        let atCancel;
        // Past its limit, and past Vitest's own test and hook limits after
        // that.
        const overdue = (at, outcome, now) => {
            const assignment = assignments[at];
            const limit = assignment.job ? limitOf(assignment) : undefined;
            if (limit === undefined || outcome.claimedAt === undefined)
                return false;
            return now - outcome.claimedAt > limit + Math.max(assignment.claim.testTimeout, hookMs);
        };
        const stop = (at, outcome) => {
            if (ownWorker(outcome.pid)) {
                try {
                    process.kill(outcome.pid, "SIGKILL");
                }
                catch {
                    // It ended meanwhile.
                }
            }
            going.delete(at);
            stopped.set(at, outcome);
            profile.killedCopies += 1;
        };
        const look = () => {
            const now = Date.now();
            let reported = false;
            for (const line of readAppended(`${directory}/claimed`, claims)) {
                const at = positionOf.get(line);
                if (at === undefined || going.has(at) || handled.has(at) || stopped.has(at))
                    continue;
                claimedCount += 1;
                going.set(at, { records: -1 });
                reported = true;
                lastClaim = now;
            }
            for (const [at, seen] of going) {
                let outcome = readOutcome(outs[at]);
                // A copy whose worker process is gone without its end (its
                // test file failed to load, its worker crashed) is over: what
                // it reported, read once its process is gone, is all there is.
                const gone = !outcome.done && outcome.pid && outcome.pid !== process.pid && !alive(outcome.pid);
                if (gone)
                    outcome = readOutcome(outs[at]);
                if (outcome.records !== seen.records) {
                    seen.records = outcome.records;
                    reported = true;
                }
                seen.outcome = outcome;
                // A mutant's copy that has failed a test its file's known
                // baseline passed is decided by that test: the rest of its
                // tests are skipped, and what it does from here on cannot
                // change its verdict. Its worker stops now, rather than at
                // its file's end, which a timed-out test can leave a runaway
                // loop holding (on tRPC such copies held 2 GB each).
                if (!outcome.done && settles(at, outcome)) {
                    try {
                        process.kill(outcome.pid, "SIGKILL");
                    }
                    catch {
                        // It ended meanwhile.
                    }
                    outcome = { ...outcome, done: true, doneAt: now };
                    decided.set(at, outcome);
                    seen.outcome = outcome;
                    profile.settledCopies += 1;
                }
                if (outcome.done) {
                    going.delete(at);
                    if (early(at, outcome))
                        handled.add(at);
                }
                else if (gone)
                    going.delete(at);
            }
            if (reported)
                lastReport = now;
            const quiet = going.size > 0 && now - lastReport > quietLimit;
            let cancel = false;
            for (const [at, { outcome }] of going) {
                if (!outcome?.claimed || !(quiet || overdue(at, outcome, now)))
                    continue;
                // A worker process of its own (forks), not Vitest's.
                if (outcome.pid && outcome.pid !== process.pid)
                    stop(at, outcome);
                else
                    cancel = true;
            }
            // Every copy started: the stage ends with its last ones. A
            // mutant's copy still going well past what it should take (1.5×
            // and 1 s) holds the stage alone; it stops, and runs again with
            // its whole limit in the next stage, beside that stage's work.
            if (claimedCount === assignments.length) {
                for (const [at, { outcome }] of going) {
                    const assignment = assignments[at];
                    if (!assignment.job || assignment.group.deferredOnce || !outcome?.claimed || !ownWorker(outcome.pid))
                        continue;
                    if (now - outcome.claimedAt > expectedOf(assignment) * TIMEOUT_FACTOR + 1_000) {
                        try {
                            process.kill(outcome.pid, "SIGKILL");
                        }
                        catch {
                            // It ended meanwhile.
                        }
                        going.delete(at);
                        deferred.set(at, outcome);
                        profile.deferredCopies += 1;
                    }
                }
            }
            const stuck = [...going].every(([at, { outcome }]) => outcome?.claimed && (quiet || overdue(at, outcome, now)));
            return cancel && (quiet || stuck);
        };
        let cancelled = false;
        while (!settled) {
            await Promise.race([ran, pause(WATCH_INTERVAL_MS)]);
            if (settled)
                break;
            if (look()) {
                cancelled = true;
                atCancel = outs.map(readOutcome);
                break;
            }
        }
        if (cancelled) {
            profile.cancelledStages += 1;
            const cancel = () => {
                try {
                    vitest.cancelCurrentRun?.("supercov: a test file stopped reporting")?.catch?.(() => { });
                }
                catch {
                    // Nothing to cancel with: the grace below runs out.
                }
            };
            cancel();
            await Promise.race([ran, pause(CANCEL_FORCE_AFTER_MS)]);
            // A second cancel stops the workers by force.
            if (!settled)
                cancel();
            await Promise.race([ran, pause(CANCEL_GRACE_MS)]);
            // Workers that will not stop even then: the pass ends here, and
            // the mutants left run in the next pass `mutants test` starts.
            if (!settled)
                stalled = true;
        }
        if (settled?.result)
            last = settled.result;
        running = undefined;
        claimRun = undefined;
        const stageEnded = Date.now();
        const outcomes = assignments.map((assignment, at) => {
            let outcome;
            if (decided.has(at))
                outcome = decided.get(at);
            else if (stopped.has(at))
                outcome = { ...stopped.get(at), hung: true };
            else if (deferred.has(at))
                outcome = { ...deferred.get(at), deferred: true };
            else if (cancelled) {
                outcome = atCancel[at];
                outcome.hung = outcome.claimed && !outcome.done;
            }
            else
                outcome = readOutcome(outs[at]);
            outcome.cancelled = cancelled;
            if (outcome.hung)
                profile.hungCopies += 1;
            return outcome;
        });
        let copyMs = 0;
        let claimed = 0;
        outcomes.forEach((outcome, at) => {
            if (!outcome.claimedAt)
                return;
            claimed += 1;
            profile.copies += 1;
            if (outcome.skipped)
                profile.skippedCopies += 1;
            else if (assignments[at].claim.mutant)
                profile.mutantCopies += 1;
            profile.testsRun += outcome.tests.length;
            profile.startMs += startOf(outcome);
            if (outcome.doneAt)
                copyMs += outcome.doneAt - outcome.claimedAt;
        });
        profile.copyMs += copyMs;
        profile.wallMs += stageEnded - stageStarted;
        const cpuAfter = machineCpu();
        const cpuBusy = cpuBefore && cpuAfter && cpuAfter.total > cpuBefore.total
            ? Math.round((cpuAfter.busy - cpuBefore.busy) / (cpuAfter.total - cpuBefore.total) * 100) / 100
            : undefined;
        if (profile.stages.length < PROFILED_STAGES)
            profile.stages.push({ copies: assignments.length, claimed, wallMs: stageEnded - stageStarted, lastClaimMs: lastClaim - stageStarted, copyMs, cpuBusy, ...memoryOf() });
        return { outcomes, handled };
    }
    // A baseline: each file with the union of its candidates, no mutant.
    // A test that fails there cannot count as a kill. A file's baseline runs
    // in the same stage as its mutants' first copies, listed before them: a
    // copy is judged once its file's baseline is known.
    const baselines = new Map();
    for (const job of pending) {
        for (const group of job.files) {
            const entry = baselines.get(group.file) ?? { file: group.file, tests: new Set(), whole: false, state: "waiting", tries: 0 };
            group.tests.forEach((id) => entry.tests.add(id));
            entry.whole ||= group.whole;
            baselines.set(group.file, entry);
        }
    }
    const baselineFailed = new Set();
    // What each file's baseline ran, in order, and the files it ran whole:
    // a kill by a test whose file's baseline ran the same tests before it
    // is certain; any other is confirmed without the mutant first.
    const baselineOrder = new Map();
    const wholeBaseline = new Set();
    // How long each file's baseline copy took, from its claim to its end:
    // a mutant's copy of the file that runs past a multiple of that has
    // stopped.
    const baselineCopyMs = new Map();
    // What the baselines tell of the time a copy takes: each test's own, and
    // each file's beyond its tests (loading, hooks, teardown).
    const baselineTestMs = new Map();
    const fileOverheadMs = new Map();
    const known = (file) => baselines.get(file)?.state === "known";
    // A baseline's outcome. A candidate that fails with only the candidates
    // of its file around it leans on a test before it: its file runs whole
    // (`widened`), and only what fails then, with nothing mutated, is no
    // kill. A copy no worker took (a cancelled stage) runs again.
    const recordBaseline = (entry, outcome) => {
        entry.state = "waiting";
        if (outcome.cancelled && !outcome.claimed && !stalled && ++entry.tries < 3)
            return;
        const failed = outcome.tests.some((test) => test.failed);
        if ((failed || !outcome.done) && !entry.whole) {
            entry.whole = true;
            entry.tests = new Set();
            baselineCopyMs.delete(entry.file);
            for (const job of pending)
                for (const group of job.files)
                    if (group.file === entry.file)
                        group.whole = true;
            return;
        }
        entry.state = "known";
        baselineOrder.set(entry.file, outcome.tests.map((test) => test.test));
        if (outcome.done) {
            baselineCopyMs.set(entry.file, outcome.doneAt - outcome.claimedAt);
            let testsMs = 0;
            for (const test of outcome.tests) {
                baselineTestMs.set(test.test, test.ms ?? 0);
                testsMs += test.ms ?? 0;
            }
            fileOverheadMs.set(entry.file, Math.max(0, outcome.doneAt - outcome.claimedAt - testsMs));
        }
        else
            baselineCopyMs.delete(entry.file);
        if (entry.whole)
            wholeBaseline.add(entry.file);
        if (outcome.outside.length)
            baselineFailed.add(`${entry.file}#outside`);
        if (!outcome.done)
            baselineFailed.add(`${entry.file}#file`);
        for (const test of outcome.tests)
            if (test.failed)
                baselineFailed.add(test.test);
    };

    const oneAtATime = !isolated(vitest);
    const workers = Math.max(1, Number(vitest.config.maxWorkers) || availableParallelism());
    const durationOf = new Map(tests.map((test) => [test.id, test.durationMs ?? 1_000]));
    const testsOf = new Map();
    for (const test of tests) {
        if (!test.file)
            continue;
        if (!testsOf.has(test.file))
            testsOf.set(test.file, []);
        testsOf.get(test.file).push(test.id);
    }
    // A copy's estimated time: its file's own beyond its tests and its tests'
    // durations (as its baseline ran them, or as measured), or its file's
    // baseline copy where it runs the file whole.
    const testMsOf = (id) => baselineTestMs.get(id) ?? durationOf.get(id) ?? 1_000;
    const overheadOf = (file) => fileOverheadMs.get(file) ??
        (fileOverheadMs.size ? [...fileOverheadMs.values()].reduce((sum, ms) => sum + ms, 0) / fileOverheadMs.size : 0);
    const estimateOf = (group) => group.whole
        ? baselineCopyMs.get(group.file) ?? group.cost
        : overheadOf(group.file) + group.tests.reduce((sum, id) => sum + (baselineFailed.has(id) ? 0 : testMsOf(id)), 0);
    const baselineEstimateOf = (entry) => entry.whole
        ? tests.reduce((sum, test) => sum + (test.file === entry.file ? test.durationMs ?? 1_000 : 0), 0)
        : [...entry.tests].reduce((sum, id) => sum + (durationOf.get(id) ?? 1_000), 0);
    // A mutant runs its cheapest file first; if that does not kill it, its
    // other files in batches of 1, 4, 16, ... (`job.batch`), the files most
    // likely to kill it first: a copy listed in a stage costs a worker's
    // start even where it ends at once (its mutant decided meanwhile), and a
    // mutant's later files mostly belong to survivors, which need them all.
    // On the public benchmark's kill matrix all-at-once listed 30% more
    // copies than this.
    for (const job of pending) {
        job.waiting = [job.files[0]];
        job.later = job.files.slice(1);
        job.batch = 1;
        job.open = 0;
    }
    // How often each test file killed the mutants it ran, learned as the
    // pass goes: a file's chance to kill over its cost (its tests' measured
    // time and a copy's start) orders a mutant's later files.
    const fileRuns = new Map();
    const fileKills = new Map();
    const startCost = () => profile.copies ? profile.startMs / profile.copies : 300;
    const killScore = (group) => ((fileKills.get(group.file) ?? 0) + 1) / ((fileRuns.get(group.file) ?? 0) + 2) / (estimateOf(group) + startCost());
    const nextBatch = (job) => {
        job.later.sort((a, b) => killScore(b) - killScore(a));
        const batch = job.later.splice(0, job.batch);
        job.batch *= BATCH_GROWTH;
        return batch;
    };
    // A mutant's copies of the stage running that no worker has taken yet
    // are not needed once a copy killed it: they end before their test file
    // loads (one taken already runs on).
    const skipRest = (job) => {
        if (!running)
            return;
        for (const at of job.positions ?? [])
            writeFileSync(`${running.placed[at]}.skip`, "");
        job.positions = [];
    };
    const finish = (job, verdict) => {
        write({ id: job.mutant.id, ...verdict, tests: job.ran, durationMs: Date.now() - (job.started || Date.now()) });
        job.done = true;
        job.deferred = [];
        skipRest(job);
    };
    // A copy runs only a mutant's candidates, and a test can lean on one
    // the copy left out (state an earlier test built): it fails then with
    // or without the mutant. A kill counts where the file's baseline ran
    // the same tests before the killing one (or the copy ran what the
    // baseline ran whole); any other kill is confirmed by running the same
    // tests without the mutant. Where they fail that way too, it is no
    // kill: the mutant runs again with its test file whole, and only what
    // fails there without the mutant as well is none.
    const confirmations = new Map();
    let overturned = 0;
    const confirmationKey = (file, chosen, whole) => `${file}\0${whole ? "*" : chosen.join("\0")}`;
    const certain = (group, chosen, evidence) => {
        if (group.whole)
            return wholeBaseline.has(group.file);
        const order = baselineOrder.get(group.file);
        if (!order)
            return false;
        if (evidence.test) {
            const at = order.indexOf(evidence.test);
            return at >= 0 && order.slice(0, at).every((id) => chosen.includes(id));
        }
        return order.length === chosen.length && order.every((id) => chosen.includes(id));
    };
    const confirm = (job, group, chosen, evidence, verdict) => {
        const key = confirmationKey(group.file, chosen, group.whole);
        if (!confirmations.has(key))
            confirmations.set(key, {
                file: group.file,
                claim: { mutant: null, tests: chosen, wholeFile: group.whole },
                tries: 0,
                result: undefined,
            });
        (job.confirming ??= []).push({ key, group, evidence, verdict });
        // Most such kills stand: its other copies wait for the confirmation
        // (job.deferred), and run only if it overturns the kill.
        skipRest(job);
    };
    // Each of a job's kills waiting on a confirmation, once it has run.
    const settle = (job) => {
        for (const waiting of [...job.confirming]) {
            const { key, group, evidence, verdict } = waiting;
            const result = confirmations.get(key).result;
            if (!result)
                continue;
            job.confirming.splice(job.confirming.indexOf(waiting), 1);
            const alsoWithout = evidence.test ? result.failed.has(evidence.test)
                : evidence.outside ? result.outside
                    : !result.done;
            if (!alsoWithout) {
                finish(job, verdict);
                return;
            }
            overturned += 1;
            if (!group.whole)
                job.waiting.push({ file: group.file, tests: [], cost: group.cost, whole: true });
            else if (evidence.test)
                baselineFailed.add(evidence.test);
        }
        // Every kill overturned: the files its kills held back run now.
        if (!job.confirming.length && job.deferred?.length) {
            job.waiting.push(...job.deferred);
            job.deferred = [];
        }
    };
    // The test a mutant's copy failed first, where it failed with nothing
    // mutated too only in its file's baseline.
    const failedTest = (outcome) => outcome.tests.find((test) => test.failed && !baselineFailed.has(test.test));
    // A copy that ran beside its file's baseline cannot always be judged:
    // its file then failed to finish (its mutants are not run), or had to
    // run whole (the copy runs again whole), or the copy stopped at a test
    // that fails without the mutant too (it runs again without it).
    const usable = ({ job, group, claim }, outcome) => {
        if (baselineFailed.has(`${group.file}#file`)) {
            job.unrun = group.file;
            return false;
        }
        const firstFailed = outcome.tests.find((test) => test.failed);
        if (claim.wholeFile !== group.whole || (claim.unknownBaseline && firstFailed && baselineFailed.has(firstFailed.test))) {
            job.waiting.push(group);
            return false;
        }
        return true;
    };
    // What a mutant's copy says of it.
    const judge = (assignment, outcome) => {
        const { job, group, claim } = assignment;
        job.open -= 1;
        if (job.done)
            return;
        // Held back: its mutant's other copies had not ended.
        if (outcome.notRun) {
            job.later.unshift(group);
            return;
        }
        // Skipped while a kill waits on its confirmation.
        if (outcome.skipped) {
            (job.deferred ??= []).push(group);
            return;
        }
        if (!outcome.claimed) {
            if (stalled)
                return;
            // A cancelled stage's copies no worker took are not this
            // mutant's doing.
            if (outcome.cancelled) {
                job.waiting.push(group);
                return;
            }
            group.unclaimed = (group.unclaimed ?? 0) + 1;
            if (group.unclaimed > 2)
                finish(job, { status: "error", reason: "no copy of its test file ran it" });
            else
                job.waiting.push(group);
            return;
        }
        if (!usable(assignment, outcome))
            return;
        // Stopped at its stage's end before a test failed: it runs again.
        if (outcome.deferred && !failedTest(outcome)) {
            group.deferredOnce = true;
            job.waiting.unshift(group);
            return;
        }
        job.ran += outcome.tests.length;
        const chosen = claim.tests;
        fileRuns.set(group.file, (fileRuns.get(group.file) ?? 0) + 1);
        const kill = (evidence, verdict) => {
            fileKills.set(group.file, (fileKills.get(group.file) ?? 0) + 1);
            if (certain(group, chosen, evidence))
                finish(job, verdict);
            else if (evidence.test && !group.whole) {
                // Tests after the failing one cannot change it: the
                // confirmation runs what ran up to it, which many
                // mutants' copies share.
                const ran = outcome.tests.map((test) => test.test);
                confirm(job, group, ran.slice(0, ran.indexOf(evidence.test) + 1), evidence, verdict);
            }
            else
                confirm(job, group, chosen, evidence, verdict);
        };
        const failed = failedTest(outcome);
        const outside = baselineFailed.has(`${group.file}#outside`) ? undefined : outcome.outside[0];
        if (!failed && outside) {
            kill({ outside: true }, outside.timeout
                ? { status: "timeout", reason: "outside any test: the mutant's evaluations passed its limit" }
                : { status: "killed", killedBy: group.file, reason: `an error outside any test: ${outside.message}` });
        }
        else if (failed) {
            kill({ test: failed.test }, { status: failed.timeout ? "timeout" : "killed", killedBy: failed.test });
        }
        else if (outcome.hung && !stalled) {
            // The copy stopped reporting past its limit, as its file's
            // baseline did not: a timeout, unless its tests left out hang
            // the same way without the mutant.
            kill({ unfinished: true }, { status: "timeout", killedBy: group.file, reason: "its test file stopped before its end, past its limit" });
        }
        else if (!outcome.done) {
            // The copy started and its file did not finish: the mutant
            // broke it while it loaded or ran.
            if (stalled)
                finish(job, { status: "timeout", reason: "its test file did not finish by the stage's deadline" });
            else
                kill({ unfinished: true }, { status: "killed", killedBy: group.file, reason: "the test file did not finish" });
        }
        else {
            const short = ranNone(claim, outcome, { order: baselineOrder.get(group.file) }, group.file);
            if (short)
                finish(job, { status: "error", reason: short });
        }
    };
    // What a confirmation's copy says: the tests that fail without the
    // mutant, and whether the file finished.
    const record = ({ check }, outcome) => {
        check.running = false;
        if (!outcome.claimed && outcome.cancelled && !stalled)
            return;
        check.tries += 1;
        if (!outcome.claimed && !stalled && check.tries < 3)
            return;
        check.result = {
            failed: new Set(outcome.tests.filter((test) => test.failed).map((test) => test.test)),
            outside: outcome.outside.length > 0,
            // A copy no worker took cannot say the tests fail without the
            // mutant: the kill stands.
            done: outcome.done || !outcome.claimed,
        };
    };
    // What a copy of a mutant should take, by its file's baseline copy.
    const limitBaseOf = ({ group }) => group.whole && !baselines.get(group.file)?.whole
        ? undefined
        : baselineCopyMs.get(group.file);
    // How long a copy of a mutant may run: its file's baseline copy, as a
    // test's limit, unless it runs its file whole and the baseline did not.
    // Read while the copy runs: its baseline may end meanwhile.
    const limitOf = ({ group }) => {
        const baselineMs = baselineCopyMs.get(group.file);
        if (baselineMs === undefined || (group.whole && !baselines.get(group.file)?.whole))
            return undefined;
        return (baselineMs * TIMEOUT_FACTOR + TIMEOUT_CONSTANT_MS) * 2;
    };
    // The continuous pass (where Supercov's spawner forks Vitest's workers,
    // Vitest 4 and later, files isolated): no stages. Each file's baselines
    // run first, once per shape a mutant needs (its candidates' union, or
    // the file whole), in a run of their own. Then one Vitest run lists more
    // worker slots than the pass can use; a slot takes its work only when
    // Vitest tells its worker the test file (the stand-in waits for it), and
    // the work it takes is whatever is ready: a baseline a mutant now needs,
    // a confirmation, or the next file of a mutant whose copy before has
    // ended with every test passing, the mutant with the most work left
    // first. The worker is then told to run that work's test file, in place
    // of the slot's (a file of the same project and environment). A
    // mutant's files run one after another, likeliest killer first, and its
    // chain stops at its first kill: parallelism comes from many mutants at
    // once. When nothing is left, the run is cancelled.
    // A copy that ended with no test failing proves the mutant survives its
    // file only where it ran every test it was to run: one that ran fewer
    // (its tests' ids matched none of the file's, say) says nothing, and its
    // mutant is an error, never a survivor. `baselineOrder` is what the file
    // ran whole, for a copy of the whole file.
    function ranNone(claim, outcome, baselineEntry, file) {
        const ran = new Set(outcome.tests.map((test) => test.test));
        const expected = claim.wholeFile ? baselineEntry?.order ?? [] : claim.tests;
        const missing = expected.filter((id) => !ran.has(id));
        if (!missing.length && (ran.size || !claim.wholeFile))
            return undefined;
        if (!ran.size)
            return `its copy of ${file} ran none of the tests it was to run`;
        return `its copy of ${file} ran ${expected.length - missing.length} of the ${expected.length} tests it was to run`;
    }
    async function runContinuous() {
        // Each file's baselines: `union` (its mutants' candidates together)
        // and `whole`, as mutants need them.
        const base = new Map();
        const baseOf = (file) => {
            if (!base.has(file))
                base.set(file, { union: { tests: new Set(), state: "unneeded" }, whole: { state: "unneeded" } });
            return base.get(file);
        };
        const need = (file, shape) => {
            const entry = baseOf(file)[shape];
            if (entry.state === "unneeded")
                entry.state = "waiting";
        };
        // A candidate that fails beside only its file's other candidates leans
        // on a test before it: the file's copies run it whole from then on.
        const forceWhole = new Set();
        // A file's whole baseline runs before the copies of the whole file.
        // (Run only once such a copy failed, it made tRPC's pass a third
        // slower: a whole file's tests that fail without a mutant then cost a
        // copy, a baseline and the copy again, one after another.)
        for (const job of pending) {
            for (const group of job.files) {
                if (group.whole)
                    need(group.file, "whole");
                else {
                    need(group.file, "union");
                    group.tests.forEach((id) => baseOf(group.file).union.tests.add(id));
                }
            }
        }
        const shapeOf = (group) => group.whole || forceWhole.has(group.file) ? "whole" : "union";
        const baselineReady = (group) => baseOf(group.file)[shapeOf(group)].state === "known";
        const unusable = (group) => {
            const entry = baseOf(group.file)[shapeOf(group)];
            return entry.state === "known" && !entry.done;
        };
        const recordShape = (file, shape, outcome) => {
            const entry = baseOf(file)[shape];
            if (!outcome.claimed && ++entry.tries < 3) {
                entry.state = "waiting";
                return;
            }
            if (shape === "union" && (!outcome.done || outcome.tests.some((test) => test.failed))) {
                // Recorded, and the file's copies run whole from now on.
                entry.state = "known";
                entry.done = false;
                forceWhole.add(file);
                need(file, "whole");
                return;
            }
            entry.state = "known";
            entry.done = outcome.done;
            entry.order = outcome.tests.map((test) => test.test);
            entry.failed = new Set(outcome.tests.filter((test) => test.failed).map((test) => test.test));
            entry.outside = outcome.outside.length > 0;
            if (outcome.done) {
                entry.copyMs = outcome.doneAt - outcome.claimedAt;
                let testsMs = 0;
                for (const test of outcome.tests) {
                    baselineTestMs.set(test.test, test.ms ?? 0);
                    testsMs += test.ms ?? 0;
                }
                if (shape === "union" || !fileOverheadMs.has(file))
                    fileOverheadMs.set(file, Math.max(0, entry.copyMs - testsMs));
                if (shape === "union" || !baselineCopyMs.has(file))
                    baselineCopyMs.set(file, entry.copyMs);
            }
        };
        const baselineItem = (file, shape) => {
            const entry = baseOf(file)[shape];
            entry.state = "running";
            entry.tries ??= 0;
            const ids = shape === "union" ? [...entry.tests] : testsOf.get(file) ?? [];
            const estimate = ids.reduce((sum, id) => sum + (durationOf.get(id) ?? 1_000), 0);
            return { baseline: { file, shape }, file, estimate, rank: -1, claim: { mutant: null, tests: shape === "union" ? ids : [], wholeFile: shape === "whole", testTimeout: BASELINE_TEST_TIMEOUT_MS } };
        };
        // A file's project and environment, learned from the "start" Vitest
        // sends its worker: work for a file runs only in a slot of the same.
        const classOf = new Map();
        const slotOf = new Map();
        const classKeyOf = (start) => JSON.stringify([start?.context?.config?.name ?? "", start?.context?.environment ?? null, start?.context?.pool ?? ""]);
        observeRun = (filepath, start) => {
            const file = toRelative(root, String(filepath));
            const key = classKeyOf(start);
            if (!classOf.has(file))
                classOf.set(file, key);
            if (!slotOf.has(key))
                slotOf.set(key, file);
        };

        // The baselines run in the same run as the mutants, first: each in a
        // slot of its own test file at the head of the run (Vitest's "start"
        // for it says the file's project and environment), and a mutant's
        // file runs once the file's baseline is known.
        // The run.
        const directory = `${stagesDirectory}/${stageNumber++}`;
        mkdirSync(directory, { recursive: true });
        provide(vitest, { root, stage: directory });
        const fileKeyOf = new Map();
        const nextIndex = new Map();
        // Every mutant's files, in the order it would take them; the most
        // work left first (long chains do not end the pass).
        // A copy of the whole file costs what its tests took in the measured
        // run (its whole baseline may never run).
        const estimateC = (group) => shapeOf(group) === "whole"
            ? overheadOf(group.file) + (testsOf.get(group.file) ?? []).reduce((sum, id) => sum + testMsOf(id), 0)
            : estimateOf(group);
        const scoreC = (group) => ((fileKills.get(group.file) ?? 0) + 1) / ((fileRuns.get(group.file) ?? 0) + 2) / (estimateC(group) + startCost());
        const remainingOf = (job) => job.chain.reduce((sum, group) => sum + estimateC(group) + startCost(), 0);
        // A mutant's copies running, and its kills waiting on a confirmation.
        for (const job of pending) {
            job.chain = [...job.files];
            job.open = 0;
            job.confirmingKills = 0;
        }
        const idle = (job) => !job.done && !job.open && !job.confirmingKills;
        const order = [...pending].sort((a, b) => remainingOf(b) - remainingOf(a));
        const rankOf = new Map(order.map((job, rank) => [job, rank]));
        const checks = [];
        const waiters = [];
        const runningCopies = new Map();
        let over = false;
        let testTimeoutMs = BASELINE_TEST_TIMEOUT_MS;
        const hookMs = hookTimeoutOf(vitest);
        // A file whose own slot has not started yet has no known class: only
        // its own slots run its work.
        const classOfFile = (file) => classOf.get(file) ?? `file:${file}`;
        // A mutant's next file: the likeliest killer of those whose baseline
        // is known (where a baseline is still running, the mutant waits).
        const nextOf = (job) => {
            let best;
            for (const group of job.chain) {
                if (!baselineReady(group) || unusable(group))
                    continue;
                if (!best || scoreC(group) > scoreC(best))
                    best = group;
            }
            return best;
        };
        const finishC = (job, verdict) => {
            if (job.done)
                return;
            // How many files each verdict took, for the pass summary.
            const counts = (profile.filesByStatus[verdict.status] ??= { mutants: 0, files: 0 });
            counts.mutants += 1;
            counts.files += job.copies ?? 0;
            finish(job, verdict);
        };
        // A mutant with nothing left to run is decided.
        const conclude = (job) => {
            if (!idle(job))
                return;
            job.chain = job.chain.filter((group) => {
                if (!unusable(group))
                    return true;
                job.unrun = group.file;
                return false;
            });
            if (job.chain.length)
                return;
            if (job.unrun)
                finishC(job, { status: "error", reason: `${job.unrun} does not finish without a mutant, so it could not test this one` });
            else
                finishC(job, { status: "survived" });
        };
        const claimOf = (job, group) => {
            const whole = shapeOf(group) === "whole";
            const failedBefore = baseOf(group.file)[shapeOf(group)].failed ?? new Set();
            const chosen = whole ? [] : group.tests.filter((id) => !failedBefore.has(id));
            const timed = whole ? testsOf.get(group.file) ?? [] : chosen;
            const budget = Math.max(...timed.map((id) => Math.max(durationOf.get(id) ?? 1_000, baselineTestMs.get(id) ?? 0)), 1_000);
            const testTimeout = Math.ceil((budget * TIMEOUT_FACTOR + TIMEOUT_CONSTANT_MS) * 2);
            // A whole copy goes on past a test that fails without the mutant
            // too, as its file's whole baseline did.
            const continueAfter = whole ? [...failedBefore] : undefined;
            return { mutant: { file: job.mutant.file, index: job.mutant.index }, tests: chosen, wholeFile: whole, continueAfter, unknownBaseline: false, hitLimit: Math.max(HIT_LIMIT_FLOOR, 1_000_000), testTimeout };
        };
        // The work a slot of class `cls` takes now, if any.
        const take = (cls, slotFile) => {
            const own = base.get(slotFile);
            for (const shape of ["union", "whole"]) {
                if (own?.[shape].state === "waiting")
                    return baselineItem(slotFile, shape);
            }
            for (const [file, entry] of base) {
                for (const shape of ["union", "whole"]) {
                    if (entry[shape].state === "waiting" && classOf.get(file) === cls)
                        return baselineItem(file, shape);
                }
            }
            for (const check of checks) {
                if (!check.result && !check.running && classOfFile(check.file) === cls) {
                    check.running = true;
                    const budget = Math.max(...check.claim.tests.map((id) => durationOf.get(id) ?? 1_000), 1_000);
                    return { check, file: check.file, claim: { ...check.claim, testTimeout: Math.ceil(budget * TIMEOUT_FACTOR + TIMEOUT_CONSTANT_MS) } };
                }
            }
            const start = (job, group) => {
                job.chain.splice(job.chain.indexOf(group), 1);
                job.open += 1;
                job.copies = (job.copies ?? 0) + 1;
                if (!job.started)
                    job.started = Date.now();
                return { job, group, file: group.file, claim: claimOf(job, group) };
            };
            for (const job of order) {
                if (!idle(job))
                    continue;
                const group = nextOf(job);
                if (group && classOfFile(group.file) === cls)
                    return start(job, group);
            }
            // No mutant is waiting for its next file: rather than leave the
            // worker idle, a mutant whose copy is still running takes its
            // next file beside it (the most work left first). Its verdict
            // is the same either way; a copy its mutant's kill makes moot is
            // stopped.
            for (const job of order) {
                if (job.done || job.confirmingKills || !job.open)
                    continue;
                const group = nextOf(job);
                if (group && classOfFile(group.file) === cls) {
                    profile.besideCopies += 1;
                    return start(job, group);
                }
            }
            return undefined;
        };
        const readyElsewhere = (cls) => {
            for (const [file, entry] of base)
                if ((entry.union.state === "waiting" || entry.whole.state === "waiting") && classOfFile(file) !== cls)
                    return true;
            if (checks.some((check) => !check.result && !check.running && classOfFile(check.file) !== cls))
                return true;
            return order.some((job) => idle(job) && nextOf(job) && classOfFile(nextOf(job).file) !== cls);
        };
        const assign = (item) => {
            const key = fileKeyOf.get(item.file) ?? keyOf(item.file);
            fileKeyOf.set(item.file, key);
            const index = nextIndex.get(key) ?? 0;
            nextIndex.set(key, index + 1);
            if (!index)
                mkdirSync(`${directory}/${key}`, { recursive: true });
            writeFileSync(`${directory}/${key}/${index}.json`, JSON.stringify(item.claim));
            testTimeoutMs = Math.max(testTimeoutMs, item.claim.testTimeout);
            if (item.job)
                profile.mutantCopies += 1;
            else if (item.check)
                profile.checkCopies += 1;
            else
                profile.baselineCopies += 1;
            runningCopies.set(`${key}/${index}`, { item, out: `${directory}/${key}/${index}.out`, records: -1, lastReport: Date.now() });
            return { index, filepath: `${root}/${item.file}` };
        };
        const dispatch = () => {
            for (let at = 0; at < waiters.length;) {
                const waiter = waiters[at];
                // A slot Vitest stopped meanwhile takes nothing.
                if (waiter.waiting && !waiter.waiting()) {
                    waiters.splice(at, 1);
                    waiter.resolve({ nothingToRun: true });
                    continue;
                }
                const item = over ? undefined : take(waiter.cls, waiter.slotFile);
                if (item) {
                    waiters.splice(at, 1);
                    waiter.resolve(assign(item));
                    continue;
                }
                // Nothing of its class to run: it lets the slot go where
                // other work is ready, or nothing runs that could make some.
                if (over || runningCopies.size === 0 || readyElsewhere(waiter.cls)) {
                    waiters.splice(at, 1);
                    waiter.resolve({ nothingToRun: true });
                    continue;
                }
                at += 1;
            }
        };
        claimRun = (filepath, start, waiting) => {
            if (over)
                return { nothingToRun: true };
            return new Promise((resolve) => {
                waiters.push({ cls: classKeyOf(start), slotFile: toRelative(root, String(filepath)), resolve, waiting });
                dispatch();
            });
        };
        // What a copy's end says.
        const kill = (job, group, claim, outcome, evidence, verdict) => {
            fileKills.set(group.file, (fileKills.get(group.file) ?? 0) + 1);
            const shape = claim.wholeFile ? "whole" : "union";
            const entry = baseOf(group.file)[shape];
            let sure;
            if (claim.wholeFile)
                sure = entry.state === "known" && entry.done;
            else if (evidence.test) {
                const at = entry.order?.indexOf(evidence.test) ?? -1;
                sure = at >= 0 && entry.order.slice(0, at).every((id) => claim.tests.includes(id));
            }
            else
                sure = entry.order?.length === claim.tests.length && entry.order.every((id) => claim.tests.includes(id));
            if (sure) {
                finishC(job, verdict);
                return;
            }
            // The copy left out tests its file's baseline ran before the
            // failure: they run again with no mutant, up to the failing one.
            // A copy of the whole file left out none: its file's whole
            // baseline, run once for every such kill, says whether the test
            // fails without the mutant too.
            const ran = outcome.tests.map((test) => test.test);
            const chosen = claim.wholeFile ? [] : evidence.test ? ran.slice(0, ran.indexOf(evidence.test) + 1) : claim.tests;
            const key = confirmationKey(group.file, chosen, claim.wholeFile);
            let check = checks.find((candidate) => candidate.key === key);
            if (!check) {
                check = { key, file: group.file, claim: { mutant: null, tests: chosen, wholeFile: claim.wholeFile }, tries: 0, result: undefined, baseline: claim.wholeFile };
                checks.push(check);
                confirmations.set(key, check);
            }
            job.confirmingKills += 1;
            const waiting = { job, group, evidence, verdict, whole: claim.wholeFile };
            // Confirmed already, for another mutant's kill.
            if (check.result)
                settleKill(check.result, waiting);
            else
                (check.jobs ??= []).push(waiting);
        };
        const judgeC = ({ job, group, claim }, outcome) => {
            job.open -= 1;
            if (job.done)
                return;
            if (!outcome.claimed) {
                job.chain.unshift(group);
                return;
            }
            // The file's copies run whole since: this copy says nothing.
            if (!claim.wholeFile && forceWhole.has(group.file)) {
                job.chain.unshift(group);
                return;
            }
            job.ran += outcome.tests.length;
            fileRuns.set(group.file, (fileRuns.get(group.file) ?? 0) + 1);
            const entry = baseOf(group.file)[claim.wholeFile ? "whole" : "union"];
            const failed = outcome.tests.find((test) => test.failed && !entry.failed?.has(test.test));
            const outside = entry.outside ? undefined : outcome.outside[0];
            // Up to the mutant's first evaluation a copy runs exactly as its
            // file does without it: a test that failed before then (its
            // switch never taken in this process) failed for another reason,
            // such as a test that depends on timing. The copy runs again,
            // once; a failure that repeats counts (the mutant may have run in
            // a process the copy started).
            const unevaluated = (evidence) => evidence?.hits === 0;
            if (!group.rerunUnevaluated && (unevaluated(failed) || (!failed && unevaluated(outside)))) {
                group.rerunUnevaluated = true;
                profile.unevaluatedReruns += 1;
                job.ran -= outcome.tests.length;
                job.chain.unshift(group);
                conclude(job);
                return;
            }
            if (failed)
                kill(job, group, claim, outcome, { test: failed.test }, { status: failed.timeout ? "timeout" : "killed", killedBy: failed.test });
            else if (outside)
                kill(job, group, claim, outcome, { outside: true }, outside.timeout
                    ? { status: "timeout", reason: "outside any test: the mutant's evaluations passed its limit" }
                    : { status: "killed", killedBy: group.file, reason: `an error outside any test: ${outside.message}` });
            else if (outcome.hung)
                kill(job, group, claim, outcome, { unfinished: true }, { status: "timeout", killedBy: group.file, reason: "its test file stopped before its end, past its limit" });
            else if (!outcome.done)
                kill(job, group, claim, outcome, { unfinished: true }, { status: "killed", killedBy: group.file, reason: "the test file did not finish" });
            else if (ranNone(claim, outcome, entry, group.file))
                finishC(job, { status: "error", reason: ranNone(claim, outcome, entry, group.file) });
            else
                conclude(job);
        };
        // A kill once the tests up to it have run with no mutant: it stands
        // unless they fail that way too, which a test the copy left out
        // caused; the mutant then runs the file whole.
        const settleKill = (result, { job, group, evidence, verdict, whole }) => {
            job.confirmingKills -= 1;
            if (job.done)
                return;
            // The whole file does not finish without the mutant: it cannot
            // test it.
            if (whole && !result.done) {
                job.unrun = group.file;
                return;
            }
            const alsoWithout = evidence.test ? result.failed.has(evidence.test)
                : evidence.outside ? result.outside
                    : !result.done;
            if (!alsoWithout) {
                finishC(job, verdict);
                return;
            }
            overturned += 1;
            // A copy of the whole file runs again, past what fails without
            // the mutant; a copy of some tests runs the file whole, once its
            // whole baseline is known.
            if (!whole)
                need(group.file, "whole");
            job.chain.unshift(whole ? group : { file: group.file, tests: [], cost: group.cost, whole: true });
        };
        const recordCheck = (check, outcome) => {
            check.running = false;
            check.tries += 1;
            if (!outcome.claimed && check.tries < 3)
                return;
            check.result = {
                failed: new Set(outcome.tests.filter((test) => test.failed).map((test) => test.test)),
                outside: outcome.outside.length > 0,
                done: outcome.done || !outcome.claimed,
            };
            // A run of the whole file with no mutant is its whole baseline.
            if (check.baseline && outcome.claimed)
                recordShape(check.file, "whole", outcome);
            for (const waiting of check.jobs ?? [])
                settleKill(check.result, waiting);
            check.jobs = [];
        };
        const ended = (entry, outcome) => {
            const { item } = entry;
            if (outcome.claimedAt !== undefined) {
                profile.copies += 1;
                profile.testsRun += outcome.tests.length;
                profile.startMs += startOf(outcome);
                if (outcome.doneAt)
                    profile.copyMs += outcome.doneAt - outcome.claimedAt;
                // A baseline's share of the workers' time, start included.
                if (item.baseline)
                    profile.baselineMs += startOf(outcome) + (outcome.doneAt ? outcome.doneAt - outcome.claimedAt : 0);
                if (outcome.hung)
                    profile.hungCopies += 1;
            }
            if (item.baseline)
                recordShape(item.baseline.file, item.baseline.shape, outcome);
            else if (item.check)
                recordCheck(item.check, outcome);
            else
                judgeC(item, outcome);
            if (item.job)
                conclude(item.job);
        };
        // The copies going, looked at often: a free slot waits on them.
        const limitFor = (item) => {
            const entry = item.job ? baseOf(item.file)[item.claim.wholeFile ? "whole" : "union"] : undefined;
            // A whole copy before its file's whole baseline: the file as the
            // measured run ran it.
            const copyMs = entry?.copyMs ?? (item.job && item.claim.wholeFile
                ? overheadOf(item.file) + (testsOf.get(item.file) ?? []).reduce((sum, id) => sum + testMsOf(id), 0)
                : undefined);
            if (!copyMs)
                return undefined;
            return (copyMs * TIMEOUT_FACTOR + TIMEOUT_CONSTANT_MS) * 2 + Math.max(item.claim.testTimeout, hookMs);
        };
        const look = () => {
            const now = Date.now();
            for (const [name, entry] of [...runningCopies]) {
                let outcome = readOutcome(entry.out);
                const gone = !outcome.done && outcome.pid && outcome.pid !== process.pid && !alive(outcome.pid);
                if (gone)
                    outcome = readOutcome(entry.out);
                if (outcome.records !== entry.records) {
                    entry.records = outcome.records;
                    entry.lastReport = now;
                }
                // A copy whose worker lost its channel to Vitest (a mutant that
                // disconnects it) did not finish its file, whatever it wrote:
                // Vitest never hears the file end.
                let end = outcome.done || gone ? outcome : undefined;
                if (end && outcome.channelLost)
                    end = { ...outcome, done: false };
                // Its mutant was decided by another copy meanwhile.
                if (!end && entry.item.job?.done && outcome.pid && ownWorker(outcome.pid)) {
                    try {
                        process.kill(outcome.pid, "SIGKILL");
                    }
                    catch {
                        // It ended meanwhile.
                    }
                    end = { ...outcome, done: true, doneAt: now };
                }
                // Decided by a failed test (its baseline known): stopped now.
                if (!end && entry.item.job && outcome.tests.some((test) => test.failed && !baseOf(entry.item.file)[entry.item.claim.wholeFile ? "whole" : "union"].failed?.has(test.test)) && ownWorker(outcome.pid)) {
                    try {
                        process.kill(outcome.pid, "SIGKILL");
                    }
                    catch {
                        // It ended meanwhile.
                    }
                    end = { ...outcome, done: true, doneAt: now };
                    profile.settledCopies += 1;
                }
                const limit = limitFor(entry.item);
                if (!end && outcome.claimedAt !== undefined && limit !== undefined && now - outcome.claimedAt > limit && ownWorker(outcome.pid)) {
                    try {
                        process.kill(outcome.pid, "SIGKILL");
                    }
                    catch {
                        // It ended meanwhile.
                    }
                    end = { ...outcome, hung: true };
                    profile.killedCopies += 1;
                }
                if (end) {
                    runningCopies.delete(name);
                    ended(entry, end);
                }
            }
            for (const job of pending)
                conclude(job);
            if (!over && pending.every((job) => job.done) && checks.every((check) => check.result) && runningCopies.size === 0)
                over = true;
            dispatch();
        };
        // Slots: each file's own first (its baseline, the longest first),
        // then enough for every copy the pass could run, the files in turn.
        const files = [...base.keys()].filter((file) => specOf.has(file));
        const firstEstimate = (file) => (testsOf.get(file) ?? []).reduce((sum, id) => sum + (durationOf.get(id) ?? 1_000), 0);
        files.sort((x, y) => firstEstimate(y) - firstEstimate(x));
        const bound = pending.reduce((sum, job) => sum + job.files.length * 3 + 4, 0) + files.length * 2 + workers;
        const sizeOf = new Map(files.map((file) => {
            try {
                return [file, statSync(`${root}/${file}`).size];
            }
            catch {
                return [file, 0];
            }
        }));
        const slots = files.map((file) => specOf.get(file));
        let bytes = 0;
        for (let at = 0; slots.length < bound && files.length; at++) {
            const file = files[at % files.length];
            bytes += sizeOf.get(file);
            if (slots.length > files.length && (bytes > RUN_BYTES || slots.length >= files.length + RUN_SLOTS))
                break;
            slots.push(specOf.get(file));
        }
        setTestTimeout(vitest, Math.max(testTimeoutMs, ...pending.flatMap((job) => job.files.map((group) => claimOf(job, group).testTimeout))));
        // A run's slots are bounded, and a slot a waiting copy gives
        // up uses one, so a run can run out of slots before the pass is over:
        // another run of the same slots goes on where it stopped, for as
        // long as runs make progress, within the slots the pass could need.
        let offered = 0;
        for (let runs = 0; !over && slots.length && offered < bound * 2; runs++) {
            const decidedBefore = pending.filter((job) => job.done).length;
            const copiesBefore = profile.copies;
            offered += slots.length;
            const runStarted = Date.now();
            let settledRun;
            const ran = runFiles(slots, false).then((result) => {
                settledRun = { result };
            }, (error) => {
                settledRun = { error };
            });
            let cancelled = false;
            while (!settledRun) {
                await Promise.race([ran, pause(CONTINUOUS_LOOK_MS)]);
                look();
                if (over && !cancelled) {
                    cancelled = true;
                    dispatch();
                    try {
                        vitest.cancelCurrentRun?.("supercov: the strong pass is over")?.catch?.(() => { });
                    }
                    catch {
                        // Nothing to cancel.
                    }
                }
            }
            look();
            if (settledRun.result)
                last = settledRun.result;
            profile.wallMs += Date.now() - runStarted;
            profile.continuousRuns = runs + 1;
            if (!over && runs > 0 && pending.filter((job) => job.done).length === decidedBefore && profile.copies === copiesBefore)
                break;
        }
        claimRun = undefined;
        observeRun = undefined;
        // Whatever the run left undecided (its slots ran out): an error,
        // which the next `mutants test` runs again.
        for (const job of pending) {
            if (job.done)
                continue;
            // What it was waiting for, for the pass summary.
            if ((profile.left ??= []).length < 20)
                profile.left.push({
                    id: job.mutant.id,
                    open: job.open,
                    confirming: job.confirmingKills,
                    chain: job.chain.map((group) => ({ file: group.file, shape: shapeOf(group), baseline: baseOf(group.file)[shapeOf(group)].state, class: classOf.has(group.file) })),
                    checks: checks.filter((check) => check.jobs?.some((waiting) => waiting.job === job)).map((check) => ({ file: check.file, running: check.running, tries: check.tries })),
                });
            finishC(job, { status: "error", reason: "the pass ended before its files ran" });
        }
    }
    const continuous = spawnerInstalled && !oneAtATime && Number(String(vitest.version ?? "0").split(".")[0]) >= 4 &&
        process.env["SUPERCOV_STRONG_STAGES"] !== "1";
    if (continuous) {
        await runContinuous();
        pending.length = 0;
    }
    while (pending.length && !stalled) {
        // A file whose baseline did not finish runs no mutant: a mutant it
        // was to test cannot be called a survivor.
        for (const job of pending) {
            for (const group of job.waiting.filter((group) => baselineFailed.has(`${group.file}#file`)))
                job.unrun = group.file;
            job.waiting = job.waiting.filter((group) => !baselineFailed.has(`${group.file}#file`));
        }
        // A stage whose work would not keep every worker busy for twice its
        // longest copy ends waiting on that copy: each mutant in it takes its
        // next batch of later files beside it, until it would.
        while (!oneAtATime) {
            const files = new Set(pending.flatMap((job) => job.waiting.map((group) => group.file)));
            const estimates = [
                ...pending.flatMap((job) => job.waiting.map((group) => estimateOf(group) + startCost())),
                ...[...baselines.values()].filter((entry) => entry.state === "waiting" && files.has(entry.file)).map((entry) => baselineEstimateOf(entry) + startCost()),
            ];
            const work = estimates.reduce((sum, estimate) => sum + estimate, 0);
            const growing = pending.filter((job) => job.waiting.length && job.later.length);
            if (!estimates.length || !growing.length || work >= workers * Math.max(...estimates) * 2)
                break;
            for (const job of growing)
                job.waiting.push(...nextBatch(job));
        }
        // Every copy the pass can run now: the baselines not known yet
        // (where a mutant waits on the file), the confirmations not run
        // yet, and each mutant's files waiting.
        const needed = new Set(pending.flatMap((job) => job.waiting.map((group) => group.file)));
        const work = [];
        for (const entry of baselines.values()) {
            if (entry.state === "waiting" && needed.has(entry.file))
                work.push({ baseline: entry, rank: -1 });
        }
        for (const [key, check] of confirmations) {
            if (!check.result && !check.running)
                work.push({ check, key, rank: 0 });
        }
        for (const job of pending) {
            job.waiting.forEach((group, rank) => work.push({ job, group, rank }));
        }
        // Where the reporter gives copies their assignments (Supercov's
        // spawner), each mutant in the stage also takes its next batch of
        // files, held back (`gated`) until its copies before them end: a
        // mutant they leave alive goes on in the same stage, in what would
        // be the stage's idle end, instead of waiting for the next stage.
        // One its other copies killed skips them, and costs no process.
        if (mainClaims && !oneAtATime) {
            for (const job of pending) {
                if (!job.waiting.length || !job.later.length || job.confirming?.length)
                    continue;
                nextBatch(job).forEach((group, rank) => work.push({ job, group, rank: rank + job.waiting.length, gated: true }));
            }
        }
        // Where files are not isolated, one copy at a time: its file's
        // baseline first.
        const stage = oneAtATime ? work.slice(0, 1) : work;
        const assignments = stage.map((item) => {
            if (item.baseline) {
                const entry = item.baseline;
                entry.state = "running";
                const chosen = [...entry.tests];
                const estimate = baselineEstimateOf(entry);
                return { ...item, file: entry.file, estimate, claim: { mutant: null, tests: chosen, wholeFile: entry.whole, testTimeout: BASELINE_TEST_TIMEOUT_MS } };
            }
            if (item.check) {
                const { check } = item;
                check.running = true;
                const chosen = check.claim.tests;
                const budget = Math.max(...chosen.map((id) => durationOf.get(id) ?? 1_000), 1_000);
                const work = chosen.reduce((sum, id) => sum + (durationOf.get(id) ?? 1_000), 0);
                return { ...item, file: check.file, estimate: work, claim: { ...check.claim, testTimeout: Math.ceil(budget * TIMEOUT_FACTOR + TIMEOUT_CONSTANT_MS) } };
            }
            const { job, group } = item;
            if (!item.gated)
                job.waiting.splice(job.waiting.indexOf(group), 1);
            job.open += 1;
            if (!job.started)
                job.started = Date.now();
            const chosen = group.tests.filter((id) => !baselineFailed.has(id));
            // Its slowest test, as measured or as its baseline ran it (a copy
            // that runs its file whole, the file's slowest).
            const timed = group.whole ? testsOf.get(group.file) ?? [] : chosen;
            const budget = Math.max(...timed.map((id) => Math.max(durationOf.get(id) ?? 1_000, baselineTestMs.get(id) ?? 0)), 1_000);
            const testTimeout = Math.ceil((budget * TIMEOUT_FACTOR + TIMEOUT_CONSTANT_MS) * 2);
            // One stopped at a stage's end likely hangs to its limit: it goes
            // first, so that the stage's other work runs beside it.
            const estimate = group.deferredOnce
                ? Math.max(estimateOf(group), limitOf({ group }) ?? testTimeout)
                : estimateOf(group);
            return {
                ...item,
                file: group.file,
                estimate,
                claim: {
                    mutant: { file: job.mutant.file, index: job.mutant.index },
                    tests: chosen,
                    wholeFile: group.whole,
                    // Its file's baseline runs in the same stage.
                    unknownBaseline: !known(group.file),
                    hitLimit: Math.max(HIT_LIMIT_FLOOR, 1_000_000),
                    testTimeout,
                },
            };
        });
        if (!assignments.length) {
            for (const job of pending)
                finish(job, { status: "error", reason: "could not be scheduled" });
            break;
        }
        // Baselines first. Then a mutant's next file before its files after
        // it (a mutant's files wait cheapest first), so that most mutants
        // are decided before their later copies start, and those skip; and
        // long copies early, so that the stage does not end waiting on a
        // long copy that started last: by the copy's estimate over its rank.
        const priority = (assignment) => assignment.estimate / (Math.max(assignment.rank, 0) + 1);
        assignments.sort((a, b) => Number(b.rank < 0) - Number(a.rank < 0) || Number(Boolean(a.gated)) - Number(Boolean(b.gated)) || priority(b) - priority(a));
        profile.checkCopies += assignments.filter((assignment) => assignment.check).length;
        for (const assignment of assignments) {
            if (assignment.job)
                assignment.job.positions = [];
        }
        assignments.forEach((assignment, at) => assignment.job?.positions.push(at));
        // A mutant's copy that ended with a failed test, its file's baseline
        // known, is judged at once (no later record changes which test
        // failed first), so a verdict shows while the stage runs, and its
        // other copies not yet started skip.
        // A baseline that ended is recorded at once: its file's copies beside
        // it are judged by it, and bounded by its time.
        const { outcomes, handled } = await runStage(assignments, (at, outcome) => {
            const assignment = assignments[at];
            if (assignment.baseline) {
                recordBaseline(assignment.baseline, outcome);
                return true;
            }
            if (!assignment.job || !known(assignment.file) || !failedTest(outcome))
                return false;
            judge(assignment, outcome);
            return true;
        }, limitOf, (assignment) => Math.max(assignment.estimate, limitBaseOf(assignment) ?? 0));
        assignments.forEach((assignment, at) => {
            if (!assignment.baseline)
                return;
            // An error outside any test can come after a copy's end.
            if (handled.has(at)) {
                if (outcomes[at].outside.length && known(assignment.file))
                    baselineFailed.add(`${assignment.file}#outside`);
            }
            else
                recordBaseline(assignment.baseline, outcomes[at]);
        });
        assignments.forEach((assignment, at) => {
            if (handled.has(at) || assignment.baseline)
                return;
            if (assignment.check)
                record(assignment, outcomes[at]);
            else
                judge(assignment, outcomes[at]);
        });
        for (const job of pending) {
            if (job.confirming?.length && !job.done)
                settle(job);
            if (job.done || job.open || job.waiting.length || job.confirming?.length)
                continue;
            if (job.later.length) {
                job.waiting = nextBatch(job);
            }
            else if (job.unrun)
                finish(job, { status: "error", reason: `${job.unrun} does not finish without a mutant, so it could not test this one` });
            else
                finish(job, { status: "survived" });
        }
        for (let index = pending.length - 1; index >= 0; index--) {
            if (pending[index].done)
                pending.splice(index, 1);
        }
    }
    // SUPERCOV_STRONG_KEEP_STAGES=1 keeps each stage's assignments and what
    // every copy reported, to read where a verdict came from.
    if (!process.env["SUPERCOV_STRONG_KEEP_STAGES"])
        rmSync(stagesDirectory, { recursive: true, force: true });
    profile.spawnedWorkers = spawnedWorkers;
    const endCpu = machineCpu();
    if (passCpu && endCpu && endCpu.total > passCpu.total)
        profile.cpuBusy = Math.round((endCpu.busy - passCpu.busy) / (endCpu.total - passCpu.total) * 100) / 100;
    const ahead = await askStats();
    if (ahead)
        Object.assign(profile, ahead);
    write({ summary: true, durationMs: Date.now() - started, isolated: !oneAtATime, stalled, left: pending.filter((job) => !job.done).length, confirmations: confirmations.size, overturned, workers, stages: stageNumber, profile });
    if (stalled) {
        // A worker that will not end keeps Vitest from closing: the mutants
        // left run in the next pass `mutants test` starts.
        process.exit(0);
    }
    return last;
}
