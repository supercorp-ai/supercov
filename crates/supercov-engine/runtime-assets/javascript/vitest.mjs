import { createHash } from "node:crypto";
import { relative, resolve, sep } from "node:path";
import { afterAll, afterEach, beforeEach } from "vitest";
import { coverageSnapshot as localCoverageSnapshot, coverageSnapshotColumns as localCoverageSnapshotColumns, activateCoverageScope as localActivateCoverageScope, enableRuntimeSnapshotEvidence as localEnableRuntimeSnapshotEvidence, resetCoverage as localResetCoverage, takeNodeAssertionPhases as localTakeNodeAssertionPhases, } from "./runtime.mjs";
import { inferTestProvenance, testTags } from "./provenance.mjs";
import { appendEvidenceRecord, installCompileCache, saveCompileCache } from "./atomic.mjs";
const evidenceDirectory = process.env["SUPERCOV_EVIDENCE_DIR"];
const emittedSetupFiles = new Set();
const attempts = new Map();
const activeScopes = new Map();
// When each test began, for the time it took: the strong pass orders a
// mutant's tests by it and sets their time limits from it.
const startedAt = new Map();
// The clock as it is now: a test that fakes timers fakes `performance.now`
// too, and a duration read from it can come out negative.
const realNow = performance.now.bind(performance);
const runtimeGlobal = globalThis;
const runtime = runtimeGlobal.__SUPERCOV_DIRECT_RUNTIME__ ?? {
    activateCoverageScope: localActivateCoverageScope,
    coverageSnapshot: localCoverageSnapshot,
    coverageSnapshotColumns: localCoverageSnapshotColumns,
    enableRuntimeSnapshotEvidence: localEnableRuntimeSnapshotEvidence,
    resetCoverage: localResetCoverage,
    takeNodeAssertionPhases: localTakeNodeAssertionPhases,
};
// Vitest persists one in-memory runtime snapshot per test. Writing the same
// events through the server JSONL transport would be both redundant and
// unattributed, and turns hot unit-test loops into synchronous filesystem IO.
runtime.enableRuntimeSnapshotEvidence();
// Each worker compiles the instrumented modules from what the workers before
// it compiled, and leaves what it compiled for the ones after.
installCompileCache(process.env["SUPERCOV_COMPILE_CACHE"]);
afterAll(saveCompileCache);
function attemptStatus(state) {
    if (state === "pass")
        return "passed";
    if (state === "fail")
        return "failed";
    if (state === "skip" || state === "todo")
        return "skipped";
    return "unknown";
}
function titlePath(task) {
    const names = [task.name];
    let suite = task.suite;
    while (suite?.name) {
        names.unshift(suite.name);
        suite = suite.suite;
    }
    return names;
}
function writeEvidence(payload) {
    if (!evidenceDirectory)
        return;
    appendEvidenceRecord(resolve(process.cwd(), evidenceDirectory), "vitest-worker", payload);
}
// Work a test file did while none of its tests ran: a `beforeAll` of a
// later `describe`, an `afterAll`, async work a test left going. It is the
// file's, as its module setup is, and no test's: written as one more setup
// record of the file. A test's afterEach clears what it ran, so what is
// there at the next test's start, or at the file's end, is that work.
const fileWork = new Map();
function writeFileWork(file, testFile, title) {
    const snapshot = runtime.coverageSnapshotColumns();
    if (!snapshot.hits.length && !snapshot.decisions.length)
        return;
    const nth = (fileWork.get(file.id) ?? 0) + 1;
    fileWork.set(file.id, nth);
    writeEvidence({
        testId: `vitest:${file.id}:between:${nth}`,
        test: `${testFile} > ${title}`,
        testFile,
        title,
        retry: 0,
        status: "passed",
        provenance: inferTestProvenance({
            runner: "vitest",
            file: testFile,
            project: file.projectName,
            explicitKind: process.env["SUPERCOV_TEST_KIND"],
        }),
        role: "setup",
        runtime: [snapshot],
        browser: [],
        server: [],
    });
}
// Declared without parameters: Vitest 4 reads a hook's first parameter as
// its fixtures and refuses one that is not destructured. Vitest 1-3 pass
// the file's suite, Vitest 4 a context holding it.
afterAll(function () {
    const context = arguments[0];
    const file = context?.file ?? context;
    if (!file?.filepath)
        return;
    const testFile = relative(process.cwd(), file.filepath).split(sep).join("/");
    if (emittedSetupFiles.has(testFile))
        writeFileWork(file, testFile, "after the tests");
});
beforeEach((context) => {
    const task = context.task;
    const testFile = relative(process.cwd(), task.file.filepath)
        .split(sep)
        .join("/");
    if (!emittedSetupFiles.has(testFile)) {
        emittedSetupFiles.add(testFile);
        // The snapshot as its evidence carries it, its events as columns.
        const setupSnapshot = runtime.coverageSnapshotColumns();
        if (setupSnapshot.hits.length || setupSnapshot.decisions.length) {
            writeEvidence({
                testId: `vitest:${task.file.id}:setup`,
                test: `${testFile} > module setup`,
                testFile,
                title: "module setup",
                retry: 0,
                status: "passed",
                provenance: inferTestProvenance({
                    runner: "vitest",
                    file: testFile,
                    project: task.file.projectName,
                    explicitKind: process.env["SUPERCOV_TEST_KIND"],
                }),
                role: "setup",
                runtime: [setupSnapshot],
                browser: [],
                server: [],
            });
        }
    }
    else {
        writeFileWork(task.file, testFile, "between tests");
    }
    const testId = `vitest:${task.id}`;
    const retry = attempts.get(testId) ?? 0;
    attempts.set(testId, retry + 1);
    const testKey = createHash("sha256").update(testId).digest("hex").slice(0, 24);
    const scope = {
        version: 1,
        runId: process.env["SUPERCOV_RUN_ID"] ?? "unscoped",
        workerId: `vitest-${process.env["VITEST_POOL_ID"] ?? process.pid}`,
        testId,
        testKey,
        retry,
        attemptId: `${testKey}-${retry}`,
    };
    activeScopes.set(task.id, scope);
    startedAt.set(task.id, realNow());
    runtime.activateCoverageScope(scope);
    runtime.resetCoverage(testId);
});
afterEach((context) => {
    const task = context.task;
    const testFile = relative(process.cwd(), task.file.filepath)
        .split(sep)
        .join("/");
    const scope = activeScopes.get(task.id);
    const retry = scope?.retry ?? task.result?.retryCount ?? 0;
    const began = startedAt.get(task.id);
    startedAt.delete(task.id);
    const payload = {
        testId: scope?.testId ?? `vitest:${task.id}`,
        ...(scope ? { scope } : {}),
        test: [...titlePath(task)].join(" > "),
        testFile,
        title: task.name,
        retry,
        status: attemptStatus(task.result?.state),
        ...(began === undefined ? {} : { durationMs: Math.max(0, Math.round(realNow() - began)) }),
        provenance: inferTestProvenance({
            runner: "vitest",
            file: testFile,
            project: task.file.projectName,
            explicitKind: process.env["SUPERCOV_TEST_KIND"],
            tags: testTags(task.tags, [...titlePath(task)]),
        }),
        ...(scope ? { phases: runtime.takeNodeAssertionPhases(scope) } : {}),
        // Its events as columns, as Playwright's are written: a test's first
        // hits written as objects repeated every key and phase for each, and
        // serializing them was most of what a test's teardown cost.
        runtime: [runtime.coverageSnapshotColumns()],
        browser: [],
        server: [],
    };
    writeEvidence(payload);
    activeScopes.delete(task.id);
    runtime.activateCoverageScope();
    // What runs from here to the next test is the file's (writeFileWork).
    runtime.resetCoverage(`vitest:${task.file.id}:between`);
});
