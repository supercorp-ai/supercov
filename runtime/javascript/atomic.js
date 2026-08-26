import { appendFileSync, existsSync, mkdirSync, openSync, closeSync, fsyncSync, renameSync, rmSync, writeFileSync, } from "node:fs";
import { randomUUID } from "node:crypto";
import { dirname } from "node:path";
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
    mkdirSync(dirname(path), { recursive: true });
    const temporary = `${path}.${process.pid}.${randomUUID()}.tmp`;
    let descriptor;
    try {
        descriptor = openSync(temporary, "wx", 0o600);
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
    mkdirSync(dirname(path), { recursive: true });
    const existed = existsSync(path);
    const descriptor = openSync(path, "a", 0o600);
    try {
        writeFileSync(descriptor, data.endsWith("\n") ? data : `${data}\n`);
        fsyncSync(descriptor);
    }
    finally {
        closeSync(descriptor);
    }
    if (!existed)
        fsyncDirectory(dirname(path));
}
/** Append a complete local record; process exit closes it before publication. */
export function appendJsonLineSync(path, data) {
    mkdirSync(dirname(path), { recursive: true });
    appendFileSync(path, data.endsWith("\n") ? data : `${data}\n`, { mode: 0o600 });
}
/** Atomically publish a fully prepared file or directory and persist its entry. */
export function atomicRenameSync(source, destination) {
    mkdirSync(dirname(destination), { recursive: true });
    renameSync(source, destination);
    fsyncDirectory(dirname(destination));
}
