// Strong mutation's setup file for Vitest (docs/design/strong-mutation.md).
//
// The strong reporter (vitestStrongReporter.mjs) runs the suite in rounds. A
// round can run one test file several times, each copy with its own mutant;
// the copies cannot be told apart by Vitest's own results (they share the
// file's path), so each copy claims its assignment from the round's
// directory, and reports its tests' outcomes there itself.
//
// This file runs in the test file's isolated module graph before the test
// file's imports: it switches the copy's mutant on, so a mutant evaluated
// while a module loads is in place too, and skips every test the assignment
// did not choose. After the copy's first chosen test fails, the rest are
// skipped: the mutant is killed.
import { appendFileSync, mkdirSync, openSync, closeSync, readFileSync, existsSync } from "node:fs";
import { createHash } from "node:crypto";
import { afterAll, afterEach, beforeAll, beforeEach, inject } from "vitest";
import * as localRuntime from "./runtime.mjs";

const runtime = globalThis.__SUPERCOV_DIRECT_RUNTIME__ ?? localRuntime;

/** The test file's path from the project root, as the plan names it. */
function thisFile(root) {
    const state = globalThis.__vitest_worker__;
    const filepath = state?.filepath ?? state?.current?.file?.filepath;
    if (!filepath)
        return undefined;
    return filepath.startsWith(root)
        ? filepath.slice(root.length).replace(/^[\\/]+/, "").split("\\").join("/")
        : filepath;
}

/** This copy's assignment: the first of the file's that no other copy took. */
function claim() {
    let provided;
    try {
        provided = inject("supercovStrong");
    }
    catch {
        return undefined;
    }
    if (!provided?.round)
        return undefined;
    const file = thisFile(provided.root);
    if (!file)
        return undefined;
    const directory = `${provided.round}/${createHash("sha256").update(file).digest("hex").slice(0, 24)}`;
    for (let index = 0; existsSync(`${directory}/${index}.json`); index += 1) {
        try {
            closeSync(openSync(`${directory}/${index}.claim`, "wx"));
        }
        catch {
            continue;
        }
        return { ...JSON.parse(readFileSync(`${directory}/${index}.json`, "utf8")), out: `${directory}/${index}.out` };
    }
    return undefined;
}

const assignment = claim();
const chosen = new Set(assignment?.tests ?? []);
let killed = false;
// Set once the active mutant was evaluated past the hit limit.
let limitReached = false;
let limitReported = false;
// The clock as it was when this file loaded: a test file may fake Date.
const realNow = Date.now.bind(Date);
const report = (record) => {
    if (assignment)
        appendFileSync(assignment.out, `${JSON.stringify(record)}\n`);
};

// An error no test catches -- work a test left running, a rejected promise
// nobody awaited -- fails the run as Vitest reports it, and is the mutant's
// doing where the same file without a mutant raised none. A mutant that
// made a loop endless outside any test throws its timeout error there.
function outsideTests(error) {
    if (!assignment)
        return;
    const timeout = runtime.isMutantTimeout?.(error) ||
        /SUPERCOV_MUTANT_TIMEOUT|MutantTimeout/.test(`${error?.name} ${error?.code} ${error?.message}`);
    report({ outside: true, timeout, message: `${error?.message ?? error}`.slice(0, 300) });
}
if (assignment) {
    process.on("uncaughtException", outsideTests);
    process.on("unhandledRejection", outsideTests);
    report({ claimed: true, at: realNow() });
    if (assignment.mutant) {
        runtime.setMutantHitLimit?.(assignment.hitLimit ?? 1_000_000);
        runtime.setActiveMutant(assignment.mutant.file, assignment.mutant.index);
        // Code under test may catch the hit limit's error and carry on (a
        // stream that turns an error into a frame): the limit having been
        // reached is the mutant's timeout all the same.
        const strong = globalThis[Symbol.for("supercov.strongMutation")];
        if (strong)
            strong.onTimeout = () => {
                limitReached = true;
            };
    }
}

// When the file's tests start: what came before was loading it.
beforeAll(() => {
    if (assignment)
        report({ started: true, at: realNow() });
});

beforeEach((context) => {
    if (!assignment) {
        // A copy with nothing to do (more copies than assignments).
        context.skip();
        return;
    }
    const id = `vitest:${context.task.id}`;
    if (killed || (!assignment.wholeFile && !chosen.has(id)))
        context.skip();
});

afterEach((context) => {
    if (!assignment)
        return;
    const result = context.task.result;
    if (result?.state === "skip")
        return;
    // `state` is not final before the hooks after a test are done; an error
    // already recorded is what failed it.
    const errors = result?.errors ?? [];
    const failed = result?.state === "fail" || errors.length > 0;
    const limitNow = limitReached && !limitReported;
    if (limitNow)
        limitReported = true;
    const timeout = limitNow || errors.some((error) => runtime.isMutantTimeout?.(error) ||
        /SUPERCOV_MUTANT_TIMEOUT|MutantTimeout/.test(`${error?.name} ${error?.code} ${error?.message}`) ||
        /Test timed out in \d+ms/.test(`${error?.message}`));
    report({ test: `vitest:${context.task.id}`, failed: failed || limitNow, timeout, ms: result?.duration });
    if ((failed || limitNow) && assignment.mutant)
        killed = true;
});

// Work a mutant left going after the tests (a loop that never ends between
// awaits, timers that re-arm) keeps evaluating the mutant: Vitest would have
// to stop the worker by force, and a run waiting on it never ends. A short
// look after the tests tells that from code that runs now and then. Most
// copies leave nothing evaluating the mutant: a brief probe that sees no
// steady evaluation ends the look there, so only a copy whose mutant is
// still being evaluated over and over waits the whole window.
const LEFT_RUNNING_PROBE_MS = 25;
const LEFT_RUNNING_WINDOW_MS = 300;
const LEFT_RUNNING_EVALUATIONS = 100;
// Half the rate the window looks for: a few late evaluations (a promise the
// tests left settling) end the look; work that keeps going does not.
const LEFT_RUNNING_PROBE_EVALUATIONS = Math.ceil(LEFT_RUNNING_EVALUATIONS * LEFT_RUNNING_PROBE_MS / LEFT_RUNNING_WINDOW_MS / 2);
// Timers as they were when this file loaded: a test file that fakes the
// clock (vi.useFakeTimers) and leaves it faked must not hold the look up.
const realSetTimeout = globalThis.setTimeout;
const pause = (ms) => new Promise((resolve) => realSetTimeout(resolve, ms));
afterAll(async () => {
    if (!assignment)
        return;
    if (assignment.mutant && runtime.mutantHitCount) {
        const before = runtime.mutantHitCount();
        await pause(LEFT_RUNNING_PROBE_MS);
        if (runtime.mutantHitCount() - before >= LEFT_RUNNING_PROBE_EVALUATIONS)
            await pause(LEFT_RUNNING_WINDOW_MS - LEFT_RUNNING_PROBE_MS);
        const evaluations = runtime.mutantHitCount() - before;
        if (limitReached && !limitReported) {
            limitReported = true;
            report({ outside: true, timeout: true, message: "the mutant was evaluated past the hit limit outside any test" });
        }
        if (evaluations > LEFT_RUNNING_EVALUATIONS)
            report({
                outside: true,
                timeout: true,
                message: `the mutant was evaluated ${evaluations} times in ${LEFT_RUNNING_WINDOW_MS} ms after the tests ended`,
            });
    }
    report({ done: true, at: realNow() });
    if (assignment.mutant)
        runtime.clearActiveMutant();
});
