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
import { runnerExecutionScope, writeRunnerEvidence } from "./runnerEvidence.mjs";

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
