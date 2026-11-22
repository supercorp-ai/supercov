// One job of a strong pass run by the command itself (strongJobs.mjs): which
// test files and tests this process of the runner runs, and whether it stops
// at the first failure. The adapters ask it as a file loads and as each test
// is registered or starts; outside a strong pass there is no job and every
// test runs as written.
//
// SUPERCOV_STRONG_JOB names a JSON file:
//   { "files": { "<test file>": ["<test id>", ...] | null }, "known": [...],
//     "bail": true }
// A file in `files` runs its listed tests, or every test where the list is
// null. A file in `known` (every test file of the measured run) and not in
// `files` runs none. Any other file -- a helper the runner loads as a test
// file, a file the measured run did not have -- runs as written.
import { readFileSync } from "node:fs";
import { relative, sep } from "node:path";
import { fileURLToPath } from "node:url";

const jobKey = Symbol.for("supercov.strongJob");

function localFile(file) {
    if (!file)
        return undefined;
    const absolute = String(file).startsWith("file:") ? fileURLToPath(file) : String(file);
    return relative(process.cwd(), absolute).split(sep).join("/");
}

function read() {
    const path = typeof process !== "undefined" ? process.env?.SUPERCOV_STRONG_JOB : undefined;
    if (!path)
        return null;
    let spec;
    try {
        spec = JSON.parse(readFileSync(path, "utf8"));
    }
    catch {
        return null;
    }
    const files = new Map(Object.entries(spec.files ?? {}).map(([file, tests]) => [file, tests === null ? null : new Set(tests)]));
    const known = new Set(spec.known ?? []);
    let failed = false;
    return {
        bail: spec.bail === true,
        // Whether the runner should load this test file at all.
        runsFile(file) {
            const local = localFile(file);
            return local === undefined || files.has(local) || !known.has(local);
        },
        // Whether this test of this file runs.
        selects(file, testId) {
            const local = localFile(file);
            if (local === undefined)
                return true;
            if (!files.has(local))
                return !known.has(local);
            const tests = files.get(local);
            return tests === null || tests.has(testId);
        },
        // A test failed: with `bail`, every later test is skipped.
        fail() {
            failed = true;
        },
        get stopped() {
            return this.bail && failed;
        },
    };
}

/** This process's job, or null outside a strong pass's job. */
export function strongJob() {
    // One job per process, whichever copy of this module asks.
    if (!(jobKey in globalThis))
        Object.defineProperty(globalThis, jobKey, { value: read(), configurable: true });
    return globalThis[jobKey];
}
