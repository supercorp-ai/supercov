import fs, { appendFileSync, chmodSync, existsSync, mkdirSync, openSync, closeSync, readFileSync, fsyncSync, renameSync, rmSync, statSync, writeFileSync, } from "node:fs";
import { createHash, randomUUID } from "node:crypto";
import Module, { syncBuiltinESMExports } from "node:module";
import { threadId } from "node:worker_threads";
import { basename, dirname, resolve, sep } from "node:path";
import { Buffer } from "node:buffer";
import vm from "node:vm";

// In a container Supercov's `docker` launcher mounts the run's directory
// into, its user (root) owns what it creates there; a directory made with
// the usual mask would keep the run, outside, from removing what goes in it.
function makeDirectory(directory) {
    if (process.env.SUPERCOV_CONTAINER !== "1")
        return mkdirSync(directory, { recursive: true });
    const previous = process.umask(0);
    try {
        return mkdirSync(directory, { recursive: true, mode: 0o777 });
    }
    finally {
        process.umask(previous);
    }
}

// And a file made there is readable by every user, whatever the process's
// mask: a container's root wrote evidence only it could read, and the run,
// as another user, recorded nothing ("Permission denied", 4.24.1).
const inContainer = () => process.env.SUPERCOV_CONTAINER === "1";
const fileMode = () => (inContainer() ? 0o666 : 0o600);
function openToAll(path) {
    if (!inContainer())
        return;
    try {
        chmodSync(path, 0o666);
    }
    catch {
        // Left as the mask made it: the run says what it could not read.
    }
}

function fsyncDirectory(path) {
    try {
        const directory = openSync(path, "r");
        try {
            fsyncSync(directory);
        }
        finally {
            closeSync(directory);
        }
    }
    catch {
        // Some platforms (notably Windows) do not permit opening directories.
    }
}
/** Write a complete sibling file and atomically replace the destination. */
export function atomicWriteFileSync(path, data, options) {
    makeDirectory(dirname(path));
    const temporary = `${path}.${process.pid}.${randomUUID()}.tmp`;
    let descriptor;
    try {
        descriptor = openSync(temporary, "wx", fileMode());
        openToAll(temporary);
        writeFileSync(descriptor, data, options);
        fsyncSync(descriptor);
        closeSync(descriptor);
        descriptor = undefined;
        renameSync(temporary, path);
        // Persist the directory entry as well as the file contents on POSIX. Some
        // platforms (notably Windows) do not permit opening directories, so file
        // fsync + atomic rename remains the portable fallback there.
        fsyncDirectory(dirname(path));
    }
    finally {
        if (descriptor !== undefined)
            closeSync(descriptor);
        rmSync(temporary, { force: true });
    }
}
/** Append one recoverable JSONL record and make the completed line durable. */
export function appendJsonLineDurableSync(path, data) {
    makeDirectory(dirname(path));
    const existed = existsSync(path);
    const descriptor = openSync(path, "a", fileMode());
    try {
        writeFileSync(descriptor, data.endsWith("\n") ? data : `${data}\n`);
        fsyncSync(descriptor);
    }
    finally {
        closeSync(descriptor);
    }
    if (!existed) {
        openToAll(path);
        fsyncDirectory(dirname(path));
    }
}
/** Append a complete local record; process exit closes it before publication. */
export function appendJsonLineSync(path, data) {
    makeDirectory(dirname(path));
    const opening = inContainer() && !existsSync(path);
    appendFileSync(path, data.endsWith("\n") ? data : `${data}\n`, { mode: fileMode() });
    if (opening)
        openToAll(path);
}
/** Atomically publish a fully prepared file or directory and persist its entry. */
export function atomicRenameSync(source, destination) {
    makeDirectory(dirname(destination));
    renameSync(source, destination);
    fsyncDirectory(dirname(destination));
}
/**
 * Append one test's evidence to this writer's own journal.
 *
 * A file per test cost a directory, a temporary file, two fsyncs and a rename:
 * jshttp/cookie's 63,740 tests had written 26,600 of them after seven
 * minutes, where the suite alone takes two seconds. An append is one write the
 * kernel keeps when the process is killed, which is the durability the rest of
 * the workspace has; set SUPERCOV_DURABLE_EVIDENCE_EACH_TEST=1 to fsync each
 * record as well. The reader already takes `*.mcdc.jsonl` journals, one record
 * per line, as Playwright writes them.
 *
 * A journal is named by a token, never by the pid alone: worker threads share
 * a pid, and VMs restored from one snapshot share the token too, so a journal
 * that grew by anything but this writer's own appends is left to its other
 * writer and a new one is started.
 */
const journals = new Map();
export function appendEvidenceRecord(directory, prefix, payload) {
    const line = `${JSON.stringify(payload)}\n`;
    let journal = journals.get(prefix);
    if (journal) {
        let size = -1;
        try {
            size = statSync(journal.path).size;
        }
        catch {
            // A missing journal is started again below.
        }
        if (size !== journal.size)
            journal = undefined;
    }
    if (!journal) {
        const writer = (process.env.SUPERCOV_EXECUTION_LOG_SHARD ?? `pid-${process.pid}`)
            .replace(/[^A-Za-z0-9_-]/g, "_");
        journal = { path: resolve(directory, `${prefix}-${writer}-${randomUUID()}.mcdc.jsonl`), size: 0 };
        journals.set(prefix, journal);
    }
    const append = process.env.SUPERCOV_DURABLE_EVIDENCE_EACH_TEST === "1"
        ? appendJsonLineDurableSync
        : appendJsonLineSync;
    append(journal.path, line);
    journal.size += Buffer.byteLength(line);
}

// Playwright 1.59 writes each file it compiles into its cache in place and
// reads it back unchecked: a process that read one while another wrote it
// ran an empty module -- a config of no `default` failed at "Cannot add
// property metadata, object is not extensible", a test file at "does not
// provide an export named". The cache a run gives it starts empty and is
// shared by every Playwright of the run, the containers a sealed suite execs
// into among them, so every first compile raced; there each file is written
// whole, or is not there yet and is compiled again. A file written beside the
// cache's folders, not in one, is never taken for an old entry and removed.
//
// Nor is an entry removed. Before it writes one, Playwright removes every
// cached file of that source, the entry another process has just written
// for the same content among them, and a process that had read that entry's
// code reads its source map only when a test is declared: in between, the
// map was gone, the test was placed by the compiled file's lines, and a
// `file:line` filter found no tests -- one test of a file, now and then, in
// a sealed suite whose containers start Playwright side by side. An entry is
// named by its source's content, so an old one is never read for new
// content; the cache is the run's own and goes with it.
//
// Playwright compiles in two threads: the config and CommonJS files in the
// process's own, ES modules in the thread its loader hooks run in, which has
// its own `fs` and does not run `--import`. Each calls this. The partial
// file's name is unique to the write: a process ID repeats across containers
// and both threads of one process count from zero.
let cacheWritesWhole = false;
export function writePlaywrightCacheWhole() {
    const transformCache = process.env.PWTEST_CACHE_DIR;
    if (!transformCache || cacheWritesWhole)
        return;
    cacheWritesWhole = true;
    const cacheFolder = resolve(transformCache) + sep;
    const inCache = (file) => typeof file === "string" && resolve(file).startsWith(cacheFolder);
    const writeFileSync = fs.writeFileSync;
    const rmSync = fs.rmSync;
    fs.rmSync = function (file, ...rest) {
        if (inCache(file) && !basename(file).startsWith(".writing-"))
            return;
        return rmSync.call(this, file, ...rest);
    };
    fs.writeFileSync = function (file, ...rest) {
        if (!inCache(file))
            return writeFileSync.call(this, file, ...rest);
        const partial = resolve(transformCache, `.writing-${process.pid}-${threadId}-${randomUUID()}`);
        try {
            writeFileSync.call(this, partial, ...rest);
            fs.renameSync(partial, file);
        }
        finally {
            rmSync(partial, { force: true });
        }
    };
    syncBuiltinESMExports();
}

// Compiled code for instrumented modules, kept for the rest of the run.
//
// Vitest evaluates every module a test file imports in a fresh worker for
// each test file, and compiles each again: an instrumented module is several
// times its source, so most of what Supercov added to a large unit suite was
// V8 compiling the same instrumented text over and over. V8 can start from
// code it compiled before (a script's cached data), checks it against the
// source and its own version and flags, and compiles afresh when it does not
// match, so a module behaves as it would have either way.
//
// `vm.runInThisContext` is what Vitest's module runner calls; it is wrapped
// for instrumented text alone. Each compiled module's code is written once
// its test file is done, when it holds the functions the file ran as well as
// the module's top level, for the workers after it to start from.

// The installed wrapper carries its state: where the cache is, and the
// scripts compiled with no cached data that matched, each with where to keep
// its code. Any copy of this module saves what the one wrapper compiled.
const STATE = Symbol.for("supercov.compileCache");
const MARK = "__supercov";

/**
 * Wraps `vm.runInThisContext` in this process to compile instrumented text
 * from what `directory` keeps. Once per process: every copy of the runtime
 * shares the one wrapper.
 */
/**
 * Node's own compile cache for every module Node loads in this process --
 * its CommonJS and ES modules, the runtime's among them -- kept in `directory`
 * for the processes after it: a runner that starts a fresh process or worker
 * per test file compiled the same packages again in each. A command that set
 * NODE_COMPILE_CACHE keeps its own. Only how long compiling takes changes.
 * Not on Windows: there `node --test`'s child for a test file never finished
 * once the cache was on.
 */
export function enableNodeCompileCache(directory) {
    if (!directory || process.platform === "win32" || process.env.NODE_COMPILE_CACHE !== undefined ||
        typeof Module.enableCompileCache !== "function")
        return;
    Module.enableCompileCache(resolve(directory, "node"));
}
export function installCompileCache(directory) {
    if (!directory || vm.runInThisContext[STATE])
        return;
    const original = vm.runInThisContext;
    const state = { directory, unsaved: [] };
    const cached = function runInThisContext(code, options) {
        // The filename-only form and anything not instrumented go as they
        // came.
        if (typeof code !== "string" || (options !== undefined && (options === null || typeof options !== "object")) || !code.includes(MARK))
            return Reflect.apply(original, this, arguments);
        const key = createHash("sha256")
            .update(code)
            .update("\0")
            .update(JSON.stringify([options?.filename, options?.lineOffset, options?.columnOffset]))
            .digest("hex");
        const file = resolve(directory, key);
        let cachedData;
        try {
            cachedData = readFileSync(file);
        }
        catch {
            cachedData = undefined;
        }
        // What `vm.runInThisContext` does, with the cached data.
        const script = new vm.Script(code, { ...options, cachedData });
        if (cachedData === undefined || script.cachedDataRejected)
            state.unsaved.push([file, script]);
        return script.runInThisContext(options);
    };
    cached[STATE] = state;
    vm.runInThisContext = cached;
}

/**
 * Writes the code of each module compiled since the last save without cached
 * data: what V8 holds of it now, the functions run so far included. A file
 * another worker wrote meanwhile is replaced whole, never torn.
 */
export function saveCompileCache() {
    const state = vm.runInThisContext[STATE];
    if (!state || state.unsaved.length === 0)
        return;
    const scripts = state.unsaved.splice(0);
    try {
        makeDirectory(state.directory);
        for (const [file, script] of scripts) {
            const partial = `${file}.${process.pid}.${Math.random().toString(36).slice(2)}`;
            writeFileSync(partial, script.createCachedData(), { mode: fileMode() });
            renameSync(partial, file);
        }
    }
    catch {
        // A cache that cannot be written is compiled again next time.
    }
}
