import fs, { appendFileSync, chmodSync, existsSync, mkdirSync, openSync, closeSync, fsyncSync, renameSync, rmSync, statSync, writeFileSync, } from "node:fs";
import { randomUUID } from "node:crypto";
import { syncBuiltinESMExports } from "node:module";
import { threadId } from "node:worker_threads";
import { dirname, resolve, sep } from "node:path";
import { Buffer } from "node:buffer";

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
    const writeFileSync = fs.writeFileSync;
    fs.writeFileSync = function (file, ...rest) {
        if (typeof file !== "string" || !resolve(file).startsWith(cacheFolder))
            return writeFileSync.call(this, file, ...rest);
        const partial = resolve(transformCache, `.writing-${process.pid}-${threadId}-${randomUUID()}`);
        try {
            writeFileSync.call(this, partial, ...rest);
            fs.renameSync(partial, file);
        }
        finally {
            fs.rmSync(partial, { force: true });
        }
    };
    syncBuiltinESMExports();
}
