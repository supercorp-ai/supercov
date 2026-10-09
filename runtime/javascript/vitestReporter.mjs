import fs from "node:fs";
import { syncBuiltinESMExports } from "node:module";
import { basename, isAbsolute, relative, resolve, sep } from "node:path";
import { fileURLToPath } from "node:url";
import { inferTestProvenance } from "./provenance.mjs";
import { appendEvidenceRecord } from "./atomic.mjs";
// The files of the copy Supercov rewrote, by path inside it, and where each
// is kept as its author wrote it or as a tool of the command changed it.
const copyRoots = (() => {
    const copy = fileURLToPath(new URL("../../", import.meta.url));
    try {
        return [...new Set([copy, fs.realpathSync(copy)])];
    }
    catch {
        return [copy];
    }
})();
let rewritten;
function rewrittenFile(path) {
    if (rewritten === undefined) {
        try {
            rewritten = new Set(JSON.parse(fs.readFileSync(new URL("./authored-sources.json", import.meta.url), "utf8")));
        }
        catch {
            rewritten = new Set();
        }
    }
    if (typeof path !== "string" || !isAbsolute(path))
        return undefined;
    for (const root of copyRoots) {
        const local = relative(root, path);
        if (local !== ".." && !local.startsWith(`..${sep}`) && !isAbsolute(local)) {
            const file = local.split(sep).join("/");
            return rewritten.has(file) ? file : undefined;
        }
    }
    return undefined;
}
const SOURCE_MAP = "\n//# sourceMappingURL=data:application/json;base64,";
/**
 * A rewritten file's source map names the project's own file, by absolute
 * path, so a Node stack trace reads as the project's. Vitest prints a frame
 * relative to its root, which is the copy: `../../../../lib/crypto.ts:3:11`
 * where it prints `lib/crypto.ts:3:11` without Supercov, and a file outside
 * its module graph gets no code under the frame. Vite is handed the same
 * code with a map that names the file itself.
 */
export function supercovSourceMaps() {
    const { readFile } = fs.promises;
    return {
        name: "supercov:source-maps",
        enforce: "pre",
        async load(id) {
            if (rewrittenFile(id) === undefined)
                return null;
            try {
                const code = await readFile(id, "utf8");
                const found = code.lastIndexOf(SOURCE_MAP);
                if (found < 0)
                    return null;
                const encoded = code.slice(found + SOURCE_MAP.length).trim();
                if (/\s/.test(encoded))
                    return null;
                const map = JSON.parse(Buffer.from(encoded, "base64").toString("utf8"));
                map.sources = [basename(id)];
                return { code: code.slice(0, found + 1), map };
            }
            catch {
                return null;
            }
        },
    };
}
// Vitest reads a file a failure names: to print the code around the nearest
// frame, and (Vitest 4) to look for a source map in it. In the copy that is
// the rewritten file. An assertion wrapped into one long line printed no code
// at all, any other line printed Supercov's, and positions that were already
// the author's were mapped a second time through the file's own map: a test
// file's frame at `:12:35` read `:12:0`. What Vitest itself reads is the text
// the file was rewritten from. What runs is not read here: Vite loads it.
let readsAuthored = false;
function readAuthoredFromVitest() {
    if (readsAuthored)
        return;
    readsAuthored = true;
    const { readFileSync, existsSync } = fs;
    const kept = (name, file) => fileURLToPath(new URL(`./${name}/${file}`, import.meta.url));
    const calledByVitest = () => {
        const holder = {};
        const limit = Error.stackTraceLimit;
        try {
            Error.stackTraceLimit = 1;
            Error.captureStackTrace(holder, supercovReadFileSync);
        }
        finally {
            Error.stackTraceLimit = limit;
        }
        return /[\\/]vitest[\\/]dist[\\/]/.test(String(holder.stack).split("\n")[1] ?? "");
    };
    function supercovReadFileSync(path, ...rest) {
        const file = rewrittenFile(path);
        if (file === undefined || !calledByVitest())
            return Reflect.apply(readFileSync, this, [path, ...rest]);
        const changed = kept(".changed", file);
        return Reflect.apply(readFileSync, this, [existsSync(changed) ? changed : kept(".authored", file), ...rest]);
    }
    fs.readFileSync = supercovReadFileSync;
    syncBuiltinESMExports();
}
function sourcePath(moduleId) {
    const absolute = moduleId.startsWith("file:")
        ? fileURLToPath(moduleId)
        : moduleId;
    return relative(process.cwd(), absolute).split(sep).join("/");
}
function rawAttemptStatus(state, expectedFailure) {
    if (!expectedFailure)
        return state;
    // Vitest reports an already-inverted final state for `it.fails`: pass
    // means the body failed as expected, while fail means it unexpectedly
    // passed. Supercov's cross-runner contract stores actual + expected, so
    // undo Vitest's inversion at the adapter boundary.
    if (state === "passed")
        return "failed";
    if (state === "failed")
        return "passed";
    return state;
}
/** Records final runner outcomes, including tests that never execute hooks. */
export default class SupercovVitestReporter {
    reportedAttempts = new Set();
    constructor(configureProjects) {
        this.configureProjects = configureProjects;
    }
    onInit(vitest) {
        this.configureProjects?.(vitest.projects);
        readAuthoredFromVitest();
    }
    onBrowserInit(project) {
        this.configureProjects?.([project]);
    }
    onTestCaseResult(testCase) {
        const evidenceDirectory = process.env["SUPERCOV_EVIDENCE_DIR"];
        if (!evidenceDirectory)
            return;
        const result = testCase.result();
        if (result.state === "pending")
            return;
        const diagnostic = testCase.diagnostic();
        const testFile = sourcePath(testCase.module.moduleId);
        const retry = diagnostic?.retryCount ?? 0;
        const payload = {
            testId: `vitest:${testCase.id}`,
            test: testCase.fullName,
            testFile,
            title: testCase.name,
            retry,
            status: rawAttemptStatus(result.state, testCase.options.fails),
            expectedStatus: testCase.options.fails ? "failed" : "passed",
            flaky: diagnostic?.flaky ?? false,
            provenance: inferTestProvenance({
                runner: "vitest",
                file: testFile,
                project: testCase.project.name,
                explicitKind: process.env["SUPERCOV_TEST_KIND"],
            }),
            runtime: [],
            browser: [],
            server: [],
        };
        this.reportedAttempts.add(`${testCase.id}:${retry}`);
        appendEvidenceRecord(resolve(process.cwd(), evidenceDirectory), "vitest-status", payload);
    }
    /** Vitest 2 compatibility; Vitest 3+ uses onTestCaseResult above. */
    onFinished(files = []) {
        const evidenceDirectory = process.env["SUPERCOV_EVIDENCE_DIR"];
        if (!evidenceDirectory)
            return;
        const visit = (task, inheritedFile) => {
            const file = task.file ?? inheritedFile ?? task;
            if (task.type === "test" && task.result?.state) {
                const testFile = sourcePath(file.filepath ?? task.filepath ?? "unknown");
                const retry = task.result.retryCount ?? 0;
                if (this.reportedAttempts.has(`${task.id}:${retry}`)) {
                    return;
                }
                const names = [task.name ?? task.id ?? "test"];
                let suite = task.suite;
                while (suite?.name) {
                    names.unshift(suite.name);
                    suite = suite.suite;
                }
                const expectedFailure = Boolean(task.fails ?? task.options?.fails);
                const payload = {
                    testId: `vitest:${task.id ?? names.join(" > ")}`,
                    test: names.join(" > "),
                    testFile,
                    title: task.name ?? names.at(-1) ?? "test",
                    retry,
                    status: rawAttemptStatus(task.result.state === "pass"
                        ? "passed"
                        : task.result.state === "fail"
                            ? "failed"
                            : "skipped", expectedFailure),
                    expectedStatus: expectedFailure ? "failed" : "passed",
                    provenance: inferTestProvenance({
                        runner: "vitest",
                        file: testFile,
                        project: file.projectName,
                        explicitKind: process.env["SUPERCOV_TEST_KIND"],
                    }),
                    runtime: [],
                    browser: [],
                    server: [],
                };
                appendEvidenceRecord(resolve(process.cwd(), evidenceDirectory), "vitest-status", payload);
            }
            for (const child of task.tasks ?? [])
                visit(child, file);
        };
        for (const file of files)
            visit(file, file);
    }
}
