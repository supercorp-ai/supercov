import { createHash, randomBytes } from "node:crypto";
import { mkdirSync, readFileSync } from "node:fs";
import { relative, resolve, sep } from "node:path";
import { fileURLToPath } from "node:url";
import { atomicWriteFileSync } from "./atomic.mjs";
import { inferTestProvenance } from "./provenance.mjs";
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
    const key = [
        identity.runner,
        localFile(identity.file) ?? "unknown",
        identity.line ?? 0,
        identity.column ?? 0,
        identity.name,
    ].join("\0");
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
        attemptId: `${testKey}-${retry}`,
    };
}
export function callerLocation(ignored) {
    const lines = new Error().stack?.split("\n").slice(2) ?? [];
    for (const entry of lines) {
        if (ignored.test(entry) || entry.includes("node:internal"))
            continue;
        const match = /(?:\(|at\s+)(file:\/\/[^:)]+|(?:[A-Za-z]:)?[^():]+):(\d+):(\d+)\)?$/.exec(entry.trim());
        if (!match)
            continue;
        return {
            file: match[1],
            line: Number(match[2]),
            column: Number(match[3]),
        };
    }
    return {};
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
export function writeRunnerEvidence(identity, status, scope, evidenceDirectoryOverride, phases = [], serverEvidenceSource) {
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
        provenance: inferTestProvenance({
            runner: identity.runner,
            file: testFile,
            explicitKind: process.env["SUPERCOV_TEST_KIND"],
        }),
        ...(phases.length > 0 ? { phases } : {}),
        runtime: [],
        browser: [],
        server: serverEvidenceSource
            ? readScopedServerEvidence(scope, serverEvidenceSource)
            : readScopedServerEvidence(scope),
    };
    const directory = resolve(process.cwd(), evidenceDirectory, `${identity.runner.replace(/[^A-Za-z0-9_-]/g, "_")}-${scope.attemptId}`);
    mkdirSync(directory, { recursive: true });
    atomicWriteFileSync(resolve(directory, "mcdc.json"), `${JSON.stringify(payload)}\n`);
}
