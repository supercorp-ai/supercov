import { createHash, randomBytes } from "node:crypto";
import { readFileSync } from "node:fs";
import { dirname, isAbsolute, relative, resolve, sep } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { appendEvidenceRecord, atomicWriteFileSync } from "./atomic.mjs";
import { inferTestProvenance, testTags } from "./provenance.mjs";
import { serverEvidencePath } from "./transport.mjs";

let cachedProcessInstanceToken;
function processInstanceToken() {
    if (cachedProcessInstanceToken)
        return cachedProcessInstanceToken;
    let random = "";
    try {
        random = randomBytes(3).toString("hex");
    }
    catch {
        random = Math.floor(Math.random() * 16777215).toString(16);
    }
    cachedProcessInstanceToken = `${random}${process.hrtime.bigint().toString(36).slice(-5)}`;
    return cachedProcessInstanceToken;
}
function localFile(file) {
    if (!file)
        return undefined;
    const absolute = file.startsWith("file:") ? fileURLToPath(file) : file;
    return relative(process.cwd(), absolute).split(sep).join("/");
}
export function runnerTestId(identity) {
    const parts = [
        identity.runner,
        localFile(identity.file) ?? "unknown",
        identity.line ?? 0,
        identity.column ?? 0,
        identity.name,
    ];
    // Preserve existing top-level, uniquely registered test IDs. Nested tests
    // and repeated registrations need more than a shared source/name identity.
    if (identity.role === "setup")
        parts.push("role", "setup");
    if (identity.parentTestId)
        parts.push("parent", identity.parentTestId);
    if (identity.registrationOrdinal)
        parts.push("registration", identity.registrationOrdinal);
    // Titles can themselves contain separator text. Domain-separate and encode
    // the extended identity structurally so it cannot alias a literal title.
    const key = identity.role === "setup" || identity.parentTestId || identity.registrationOrdinal
        ? JSON.stringify(["registration-v2", ...parts])
        : parts.join("\0");
    return `${identity.runner}:${createHash("sha256").update(key).digest("hex").slice(0, 24)}`;
}
export function runnerExecutionScope(identity) {
    const testId = runnerTestId(identity);
    const retry = identity.retry ?? 0;
    // Same hazard as the Playwright shim: a pooled runner's fresh workers can
    // share a pid, so the worker identity carries a per-process token.
    const workerId = `${identity.runner}-${process.env["JEST_WORKER_ID"] ?? process.pid}-${processInstanceToken()}`;
    const testKey = createHash("sha256").update(testId).digest("hex").slice(0, 24);
    return {
        version: 1,
        runId: process.env["SUPERCOV_RUN_ID"] ?? "unscoped",
        workerId,
        testId,
        testKey,
        retry,
        // Different processes/worker threads can execute the same registration.
        // Their files must not collide even though the stable test ID is shared.
        attemptId: `${testKey}-${retry}-${processInstanceToken()}`,
    };
}
export function callerLocation(boundary = callerLocation) {
    // Omit the registration wrapper by identity. A user file may have the same
    // basename as a runtime adapter. Parse the location suffix, not individual
    // path segments: spaces, parentheses and Unicode are valid filenames.
    try {
        const error = {};
        Error.captureStackTrace(error, boundary);
        if (typeof error.stack !== "string")
            return {};
        const entry = error.stack.split("\n")[1]?.trim();
        const match = entry && /(?:^at (?:async )?| \()((?:file:\/\/|\/|[A-Za-z]:[\\/]|\\\\).*):(\d+):(\d+)\)?$/.exec(entry);
        if (!match)
            return {};
        return {
            file: match[1],
            line: Number(match[2]),
            column: Number(match[3]),
        };
    }
    catch {
        // Custom formatters must not prevent registration. Missing provenance
        // is explicit, never replaced with an unrelated deeper caller.
        return {};
    }
}
export function readScopedServerEvidence(scope, evidencePath = serverEvidencePath(scope)) {
    try {
        return readFileSync(evidencePath, "utf8")
            .split("\n")
            .filter(Boolean)
            .map((line) => JSON.parse(line))
            .filter((record) => record.scope?.attemptId === scope.attemptId);
    }
    catch {
        return [];
    }
}
export function writeRunnerEvidence(identity, status, scope, evidenceDirectoryOverride, phases = [], serverEvidenceSource, snapshot = false) {
    const evidenceDirectory = evidenceDirectoryOverride ?? process.env["SUPERCOV_EVIDENCE_DIR"];
    if (!evidenceDirectory)
        return;
    const testFile = localFile(identity.file);
    const payload = {
        testId: scope.testId,
        scope,
        test: identity.name,
        ...(testFile ? { testFile } : {}),
        title: identity.name.split(" > ").at(-1) ?? identity.name,
        retry: identity.retry ?? 0,
        status,
        ...(identity.role ? { role: identity.role } : {}),
        provenance: inferTestProvenance({
            runner: identity.runner,
            file: testFile,
            explicitKind: process.env["SUPERCOV_TEST_KIND"],
            tags: identity.role ? [] : testTags(identity.tags, [identity.name]),
        }),
        ...(phases.length > 0 ? { phases } : {}),
        runtime: [],
        browser: [],
        server: serverEvidenceSource
            ? readScopedServerEvidence(scope, serverEvidenceSource)
            : readScopedServerEvidence(scope),
    };
    if (snapshot) {
        atomicWriteFileSync(resolve(process.cwd(), evidenceDirectory, `attempt-${scope.attemptId}`, "mcdc.json"), JSON.stringify(payload));
    } else {
        appendEvidenceRecord(resolve(process.cwd(), evidenceDirectory), identity.runner.replace(/[^A-Za-z0-9_-]/g, "_"), payload);
    }
}

// A failing test's stack as its author would have seen it, for the runners
// that print a stack as it is (node:test, AVA).
//
// - A frame of Supercov's is one in a file of its own, straight under a
//   `.supercov` directory or its `node_modules`. The copy lies under a
//   `.supercov` directory too, and every frame in it was once dropped for
//   that: a failing node:test test printed no frame of its own file.
//   `AsyncLocalStorage.run` is Node's frame for running a test under
//   Supercov, and goes with the frames around it.
// - The test ran in the copy, and a frame of a test file named it there.
// - Node names a module's file by its URL and a CommonJS file by its path.
//   A rewritten file is named through its source map, by path either way:
//   `at seal (/home/me/shop/src/crypto.js:3:11)` for `file:///home/…`.
// - What is left is cut to the length the author's limit allows; the
//   runtime made room for its own frames while they were on the stack.
const OWN_FRAME = /[\\/]\.supercov[\\/](?:node_modules[\\/])?[^\\/:]+\.[cm]?js:\d+/u;
const CARRIER_FRAME = /^\s*at AsyncLocalStorage\.run \(node:internal\//u;
const FRAME_PATH = /^(\s*at (?:.*? \()?)((?:\/|[A-Za-z]:[\\/])[^()]*?)(:\d+:\d+\)?)$/u;
const packageTypes = new Map();
function loadedAsModule(file) {
    if (/\.m[jt]sx?$/u.test(file))
        return true;
    if (/\.c[jt]sx?$/u.test(file))
        return false;
    const visited = [];
    let directory = dirname(file);
    let type;
    for (;;) {
        if (packageTypes.has(directory)) {
            type = packageTypes.get(directory);
            break;
        }
        visited.push(directory);
        try {
            type = JSON.parse(readFileSync(resolve(directory, "package.json"), "utf8")).type === "module";
            break;
        }
        catch {
            // No manifest here, or not one that parses: the next directory up.
        }
        const parent = dirname(directory);
        if (parent === directory) {
            type = false;
            break;
        }
        directory = parent;
    }
    for (const seen of visited)
        packageTypes.set(seen, type);
    return type;
}
//
// `whereItRan` leaves each file where the stack has it: for a printer that
// takes its working directory off a frame itself, and would not know the
// project's.
export function stackAsWritten(stack, whereItRan = false) {
    const copy = process.env["SUPERCOV_PROJECT_ROOT"];
    const project = whereItRan ? undefined : process.env["SUPERCOV_SOURCE_PROJECT_ROOT"];
    const placed = (text) => copy && project && copy !== project ? text.split(copy).join(project) : text;
    const lines = stack.split("\n");
    const own = lines.map((line) => /^\s*at /u.test(line) && OWN_FRAME.test(line));
    const limit = Error.stackTraceLimit;
    let frames = 0;
    return lines
        .filter((line, index) => !own[index] && !(CARRIER_FRAME.test(line) && (own[index - 1] || own[index + 1])))
        .map((line) => {
        const frame = FRAME_PATH.exec(placed(line));
        if (!frame || !project || !isAbsolute(frame[2]) || !frame[2].startsWith(project + sep) || !loadedAsModule(frame[2]))
            return placed(line);
        return `${frame[1]}${pathToFileURL(frame[2]).href}${frame[3]}`;
    })
        .filter((line) => !/^\s*at /u.test(line) || typeof limit !== "number" || !Number.isFinite(limit) || ++frames <= limit)
        .join("\n");
}
