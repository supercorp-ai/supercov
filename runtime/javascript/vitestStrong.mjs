// Strong mutation's setup file for Vitest (docs/design/strong-mutation.md).
//
// The strong reporter (vitestStrongReporter.mjs) runs the suite in stages. A
// stage can run one test file many times, each copy with its own mutant; the
// copies cannot be told apart by Vitest's own results (they share the file's
// path), so each copy claims its assignment from the stage's directory as it
// starts, and reports its tests' outcomes there itself.
//
// This file runs in the test file's isolated module graph before the test
// file's imports: it switches the copy's mutant on, so a mutant evaluated
// while a module loads is in place too, and skips every test the assignment
// did not choose. After the copy's first chosen test fails, the rest are
// skipped: the mutant is killed.
import { appendFileSync, readFileSync, existsSync } from "node:fs";
import { createHash, randomBytes } from "node:crypto";
import { isMainThread } from "node:worker_threads";
import { afterAll, afterEach, beforeAll, beforeEach, inject, vi } from "vitest";

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

/**
 * This copy's assignment: the next of its file's. Each copy appends a token
 * of its own to the file's claims (an append is one atomic write), and the
 * line it landed on is the assignment it takes.
 */
function claim() {
    let provided;
    try {
        provided = inject("supercovStrong");
    }
    catch {
        return undefined;
    }
    if (!provided?.stage)
        return undefined;
    const file = thisFile(provided.root);
    if (!file)
        return undefined;
    const key = createHash("sha256").update(file).digest("hex").slice(0, 24);
    const directory = `${provided.stage}/${key}`;
    // Where Supercov's spawner starts the workers, the strong reporter told
    // this copy its assignment already.
    let index = provided.claim;
    if (index === undefined) {
        const token = randomBytes(12).toString("hex");
        try {
            appendFileSync(`${directory}/claims`, `${token}\n`);
            index = readFileSync(`${directory}/claims`, "utf8").split("\n").indexOf(token);
        }
        catch {
            return undefined;
        }
    }
    // A copy beyond its file's assignments has nothing to run.
    if (index < 0 || !existsSync(`${directory}/${index}.json`))
        return { surplus: true };
    return {
        ...JSON.parse(readFileSync(`${directory}/${index}.json`, "utf8")),
        // The mutant got its verdict from another copy meanwhile.
        skip: existsSync(`${directory}/${index}.skip`),
        out: `${directory}/${index}.out`,
        claimed: { log: `${provided.stage}/claimed`, line: `${key}/${index}` },
    };
}

// What a file before this one in the same worker reported to is not this
// file's copy.
if (globalThis.__vitest_worker__)
    globalThis.__vitest_worker__.supercovReport = undefined;
const claimed = claim();
// A copy with nothing to run -- beyond its file's assignments, or one whose
// mutant another copy has decided -- ends here, before its test file loads.
if (claimed?.surplus || claimed?.skip) {
    if (claimed.skip) {
        const at = Date.now();
        appendFileSync(claimed.out, `${JSON.stringify({ claimed: true, at, up: Math.round(performance.now()), pid: isMainThread ? process.pid : undefined, skipped: true })}\n${JSON.stringify({ done: true, at })}\n`);
        try {
            appendFileSync(claimed.claimed.log, `${claimed.claimed.line}\n`);
        }
        catch {
            // The stage is over.
        }
    }
    throw new Error("supercov: this copy has nothing to run");
}
// Loaded only by a copy that runs.
const runtime = globalThis.__SUPERCOV_DIRECT_RUNTIME__ ?? (await import("./runtime.mjs"));
const assignment = claimed;
const chosen = new Set(assignment?.tests ?? []);
let killed = false;
// Set once the active mutant was evaluated past the hit limit.
let limitReached = false;
let limitReported = false;
// The clock as it was when this file loaded: a test file may fake Date.
const realNow = Date.now.bind(Date);
// How often the copy's mutant has been evaluated so far: a failure before its
// first evaluation is the program's without the mutant, not the mutant's.
const hitsNow = () => assignment?.mutant ? runtime.mutantHitCount?.() : undefined;
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
    report({ outside: true, timeout, message: `${error?.message ?? error}`.slice(0, 300), hits: hitsNow() });
}
// A test file that does not load or set up -- a setup file after this one
// throws, or one of its imports does -- runs none of its tests and none of
// its hooks: Vitest tells its collection, and the copy reports what failed
// with how often its mutant had run by then. Reported once per worker: a
// worker that runs files one after another reports each to its own copy.
const workerState = globalThis.__vitest_worker__;
if (workerState)
    workerState.supercovReport = assignment ? (record) => report({ ...record, hits: hitsNow() }) : undefined;
if (workerState?.rpc && !workerState.supercovCollected) {
    workerState.supercovCollected = true;
    const rpc = workerState.rpc;
    workerState.rpc = new Proxy(rpc, {
        get(target, name) {
            const value = Reflect.get(target, name);
            if (name !== "onCollected" || typeof value !== "function")
                return value;
            return (files, ...rest) => {
                const error = files?.find?.((file) => file?.result?.state === "fail")?.result?.errors?.[0];
                if (error)
                    workerState.supercovReport?.({ loadError: true, message: messageOf(error) });
                return value.call(target, files, ...rest);
            };
        },
    });
}

// An error as a verdict's reason says it.
function messageOf(error) {
    return `${error?.message ?? error}`.slice(0, 300);
}

// The first hook of `suite` or a suite in it that failed (a `beforeAll`
// that threw): Vitest records it on its suite and skips the suite's tests.
function failedHook(suite) {
    if (!suite || suite.type === "test")
        return undefined;
    const own = suite.result?.errors?.[0];
    if (own)
        return own;
    for (const child of suite.tasks ?? []) {
        const error = failedHook(child);
        if (error)
            return error;
    }
    return undefined;
}

if (assignment) {
    process.on("uncaughtException", outsideTests);
    process.on("unhandledRejection", outsideTests);
    // A worker process of its own (not a thread of Vitest's) can be
    // stopped alone, should it stop reporting.
    // `up`: how long the worker had been starting (its process, Vitest, the
    // environment) before this copy claimed.
    report({ claimed: true, at: realNow(), up: Math.round(performance.now()), pid: isMainThread ? process.pid : undefined });
    try {
        appendFileSync(assignment.claimed.log, `${assignment.claimed.line}\n`);
    }
    catch {
        // The stage is over.
    }
    // The copy's own test limit, read as the test file's tests are collected.
    if (assignment.testTimeout)
        vi.setConfig({ testTimeout: assignment.testTimeout });
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
        // Outside a strong pass's stage.
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
    report({ test: `vitest:${context.task.id}`, failed: failed || limitNow, timeout, ms: result?.duration, hits: hitsNow() });
    // A test that fails without the mutant too (its file's baseline says)
    // decides nothing: the copy goes on past it.
    const failsWithout = !limitNow && assignment.continueAfter?.includes(`vitest:${context.task.id}`);
    if ((failed || limitNow) && assignment.mutant && !failsWithout)
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
// No parameter is declared: Vitest 4 reads a suite hook's first one as the
// fixtures it asks for, and passes the suite second (Vitest 1-3, first).
afterAll(async function () {
    if (!assignment)
        return;
    // A hook that failed skipped its suite's tests: what failed, and how
    // often the mutant had run by the file's end.
    const hook = failedHook([...arguments].find((value) => value?.type === "suite"));
    if (hook)
        report({ hook: true, message: messageOf(hook), hits: hitsNow() });
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
    // A worker process whose channel to Vitest closed (the mutant
    // disconnected it) cannot tell Vitest its file ended: it did not.
    report({ done: true, at: realNow(), channelLost: Boolean(process.send) && !process.connected && isMainThread });
    if (assignment.mutant)
        runtime.clearActiveMutant();
});
