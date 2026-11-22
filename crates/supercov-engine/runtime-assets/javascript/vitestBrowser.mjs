// Vitest Browser Mode runs the test file in the browser, so the node setup
// cannot be used: it imports node:fs through atomic.mjs, and Vite externalises
// that for the client. Loading it fails the whole suite before a single test
// collects, which is what a browser-mode project saw.
//
// This setup mirrors the node one's payload contract exactly. What it cannot do
// in the browser -- resolve a path against the project root, infer provenance,
// write a file -- it hands to node over Vitest's browser command channel, so
// that contract has one implementation.
import { afterEach, beforeEach } from "vitest";
import { coverageSnapshot, activateCoverageScope, enableRuntimeSnapshotEvidence, resetCoverage, setActiveMutant, takeNodeAssertionPhases, } from "./runtime.mjs";
// Vitest exposes the command channel at "vitest/browser" from v4 and at
// "@vitest/browser/context" before that. Both are tried so a project is not
// forced onto one Vitest line to be measured.
const browserContext = await import("vitest/browser").catch(() =>
    import("@vitest/browser/context"));
const { commands } = browserContext;
// A strong pass's job (strongJob.mjs): its mutant is switched on here,
// before the test file's imports, and only its tests run, none after its
// first failure. The browser has no environment: node hands both over.
const strong = typeof commands.__supercovStrongJob === "function" ? await commands.__supercovStrongJob() : null;
let strongFailed = false;
if (strong) {
    const at = strong.mutant.lastIndexOf("#");
    if (at > 0)
        setActiveMutant(strong.mutant.slice(0, at), Number(strong.mutant.slice(at + 1)));
    // Past the hit limit the switch throws where it runs; node records it,
    // as the browser can write nothing.
    const state = globalThis[Symbol.for("supercov.strongMutation")];
    if (state)
        state.onTimeout = (file, index) => {
            commands.__supercovMutantTimeout(`${file}#${index}`).catch(() => { });
        };
}
function strongSelects(task) {
    const files = strong.job.files ?? {};
    if (!(strong.file in files))
        return !(strong.job.known ?? []).includes(strong.file);
    const tests = files[strong.file];
    return tests === null || tests.includes(`vitest:${task.id}`);
}
const attempts = new Map();
const activeScopes = new Map();
const emittedSetupFiles = new Set();
enableRuntimeSnapshotEvidence();
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
// The node setup keys an attempt by sha256(testId). Browsers have the same
// digest behind an async API, so the hook awaits it rather than substituting a
// different hash and giving the same test two identities across environments.
async function testKeyOf(testId) {
    const bytes = new TextEncoder().encode(testId);
    const digest = await crypto.subtle.digest("SHA-256", bytes);
    return [...new Uint8Array(digest)]
        .map((byte) => byte.toString(16).padStart(2, "0"))
        .join("")
        .slice(0, 24);
}
// Evidence leaves the browser over Vitest's own command channel: node runs the
// handler, the test awaits it, and the failure is reported rather than
// swallowed -- silently losing a test's evidence is the worst shape a
// measurement bug takes.
async function sendEvidence(payload, suffix) {
    try {
        await commands.__supercovEvidence(payload, suffix);
    }
    catch (error) {
        console.error("[supercov] failed to record browser evidence:", error);
        throw error;
    }
}
beforeEach(async (context) => {
    const task = context.task;
    if (strong && ((strong.job.bail && strongFailed) || !strongSelects(task))) {
        context.skip();
        return;
    }
    // Module imports and shared setup execute before the first test. Save
    // them before resetCoverage clears the snapshot, with a setup identity
    // rather than crediting that execution to the first test.
    if (!emittedSetupFiles.has(task.file.id)) {
        const setupSnapshot = coverageSnapshot();
        if (setupSnapshot.hits.length || setupSnapshot.decisions.length) {
            await sendEvidence({
                testId: `vitest:${task.file.id}:setup`,
                test: `${task.file.name} > module setup`,
                projectName: task.file.projectName,
                title: "module setup",
                retry: 0,
                status: "passed",
                role: "setup",
                runtime: [setupSnapshot],
                browser: [],
                server: [],
            }, `vitest-${task.file.id}-setup`);
        }
        emittedSetupFiles.add(task.file.id);
    }
    const testId = `vitest:${task.id}`;
    const retry = attempts.get(testId) ?? 0;
    attempts.set(testId, retry + 1);
    const testKey = await testKeyOf(testId);
    const scope = {
        version: 1,
        runId: globalThis.__SUPERCOV_RUN_ID__ ?? "unscoped",
        workerId: "vitest-browser",
        testId,
        testKey,
        retry,
        attemptId: `${testKey}-${retry}`,
    };
    activeScopes.set(task.id, scope);
    activateCoverageScope(scope);
    resetCoverage(testId);
});
afterEach(async (context) => {
    const task = context.task;
    const scope = activeScopes.get(task.id);
    if (strong && task.result?.state === "fail")
        strongFailed = true;
    const retry = scope?.retry ?? task.result?.retryCount ?? 0;
    // Node relativises the test file Vitest reports and infers provenance, so
    // both environments describe a test the same way and neither trusts the
    // browser realm to say which file it was.
    const payload = {
        testId: scope?.testId ?? `vitest:${task.id}`,
        ...(scope ? { scope } : {}),
        test: [...titlePath(task)].join(" > "),
        projectName: task.file?.projectName,
        title: task.name,
        retry,
        // A test still running in its afterEach has passed so far; a job
        // reads its outcome from this record.
        status: strong && task.result?.state === "run" ? "passed" : attemptStatus(task.result?.state),
        ...(scope ? { phases: takeNodeAssertionPhases(scope) } : {}),
        // Browser-mode evidence is a runtime snapshot like node's: the code
        // under test runs in the same realm as the probes.
        runtime: [coverageSnapshot()],
        browser: [],
        server: [],
    };
    await sendEvidence(payload, `vitest-${task.id}-${retry}`);
    activeScopes.delete(task.id);
    activateCoverageScope();
});
