// Supercov AVA adapter. The preload imports it in each AVA worker -- a
// worker thread, or a child process under --no-worker-threads -- and it
// patches AVA's own Runner, so a suite runs exactly as written. AVA runs a
// file's tests concurrently, so a test's coverage cannot be told apart by
// time: each test, with its beforeEach and afterEach hooks, runs inside a
// coverage carrier of its own, as node:test subtests do, and what ran under
// it is written with the test's outcome once it settles. Loading the test
// file and before/after hooks run outside any test and count for the run.
// It computes no coverage.
import { realpathSync } from "node:fs";
import { join } from "node:path";
import { pathToFileURL } from "node:url";
import { beginBufferedServerEvidence, flushBufferedServerEvidence, takeNodeAssertionPhases, withCoverageCarrier, } from "./runtime.mjs";
import { runnerExecutionScope, stackAsWritten, writeRunnerEvidence } from "./runnerEvidence.mjs";

// The installed ava package the entrypoint belongs to.
function avaRoot(entrypoint) {
    let real;
    try {
        real = realpathSync(entrypoint).replaceAll("\\", "/");
    }
    catch {
        return undefined;
    }
    const at = real.lastIndexOf("/node_modules/ava/");
    return at < 0 ? undefined : real.slice(0, at + "/node_modules/ava".length);
}

function asWritten(failure, depth = 0) {
    if (!failure || typeof failure !== "object" || depth > 4)
        return;
    try {
        // AVA prints the stack of an error a test threw as it is, and
        // shortens a failed assertion's by its own working directory, which
        // is the copy. A thrown error reaches here as an assertion's cause.
        const shortened = failure.type === "ava" && !(failure.originalError?.cause instanceof Error);
        if (typeof failure.stack === "string")
            failure.stack = stackAsWritten(failure.stack, shortened);
        for (const inner of Array.isArray(failure.errors) ? failure.errors : [])
            asWritten(inner, depth + 1);
    }
    catch {
        // A stack that cannot be written is printed as it is.
    }
}

export async function adaptAva(entrypoint) {
    const evidenceDirectory = process.env.SUPERCOV_EVIDENCE_DIR;
    const root = avaRoot(entrypoint);
    if (!evidenceDirectory || !root)
        return;
    let loaded;
    try {
        loaded = await import(pathToFileURL(join(root, "lib", "runner.js")).href);
    }
    catch {
        return;
    }
    const Runner = loaded.default ?? loaded.Runner;
    const runTest = Runner?.prototype?.runTest;
    if (typeof runTest !== "function" || runTest.supercovAva)
        return;
    Runner.prototype.runTest = function supercovAvaRunTest(task, contextRef) {
        // AVA refuses two tests with one title in a file, so the title names
        // the test.
        const identity = { runner: "ava", file: this.file, name: task.title };
        const scope = runnerExecutionScope(identity);
        beginBufferedServerEvidence(scope);
        const finish = (status) => writeRunnerEvidence(identity, status, scope, evidenceDirectory, takeNodeAssertionPhases(scope), flushBufferedServerEvidence(scope));
        let settled;
        try {
            settled = Promise.resolve(withCoverageCarrier({ version: 1, scope }, () => Reflect.apply(runTest, this, [task, contextRef])));
        }
        catch (error) {
            finish("failed");
            throw error;
        }
        return settled.then((passed) => {
            finish(passed ? "passed" : "failed");
            return passed;
        }, (error) => {
            finish("failed");
            throw error;
        });
    };
    Runner.prototype.runTest.supercovAva = true;
    // AVA prints a failure's stack as it is. The test ran in the copy, so a
    // frame of the test file named it there, and a frame of a rewritten
    // module was named by its path where Node names a module by its URL:
    // `at seal (/home/me/shop/src/crypto.js:3:11)` over
    // `at file:///home/me/shop/.supercov/workspaces/…/test/crypto.test.js:5:18`.
    // A failure is told to the reporter as a state change; its stack is put
    // right on the way.
    const emit = Runner.prototype.emit;
    if (typeof emit === "function")
        Runner.prototype.emit = function supercovAvaEmit(name, data, ...rest) {
            if (name === "stateChange" && data?.err)
                asWritten(data.err);
            return Reflect.apply(emit, this, [name, data, ...rest]);
        };
    // A skipped or todo test never reaches runTest; AVA announces it.
    const start = Runner.prototype.start;
    if (typeof start === "function")
        Runner.prototype.start = function supercovAvaStart(...args) {
            // AVA's emitter hands a listener { data: state }.
            this.on("stateChange", ({ data: state } = {}) => {
                if (state?.type !== "selected-test" || !(state.skip || state.todo))
                    return;
                const identity = { runner: "ava", file: this.file, name: state.title };
                writeRunnerEvidence(identity, "skipped", runnerExecutionScope(identity), evidenceDirectory);
            });
            return Reflect.apply(start, this, args);
        };
}
