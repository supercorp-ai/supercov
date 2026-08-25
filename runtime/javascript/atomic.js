import { mkdirSync, openSync, closeSync, fsyncSync, renameSync, rmSync, writeFileSync, } from "node:fs";
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
/** Atomically publish a fully prepared file or directory and persist its entry. */
export function atomicRenameSync(source, destination) {
    mkdirSync(dirname(destination), { recursive: true });
    renameSync(source, destination);
    fsyncDirectory(dirname(destination));
}
