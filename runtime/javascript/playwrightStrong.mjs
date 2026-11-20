// Strong mutation for Playwright (docs/design/strong-mutation.md, browser
// tests against a server).
//
// One Playwright invocation runs the plan. The generated config repeats every
// test (`repeatEach`) as often as the busiest test file has rows: a file's row
// k is what every test of the file does in its k-th repetition -- run with
// one mutant, run with none (the baseline, row 0, and canaries), or skip. A
// test's repetition finds its row from its file and `repeatEachIndex`, so
// nothing is claimed at run time and no two repetitions can take the same
// work. The attempt's mutant goes into its scope, which reaches the app's
// server with every request (header, cookies, child processes) and the page
// through an init script, so the workers of one run each test their own
// mutant against one server.
//
// A mutant the server must start with (its code ran in work no request named,
// or is a server module's top level, or ran for a request that named no test)
// gets no row: it is run alone afterwards, in an invocation of its own with
// SUPERCOV_ACTIVE_MUTANT set, which the engine starts for each `restart`
// record the reporter writes (javascript_run.rs). So do a test that ran out
// of time (again, with twice the time) and every verdict a failing canary
// puts in doubt.
//
// This module holds what the config, the fixture (playwright.mjs) and the
// reporter (playwrightStrongReporter.mjs) share.
import { existsSync, mkdirSync, readFileSync, renameSync, rmSync, writeFileSync } from "node:fs";
import { createHash } from "node:crypto";
import { basename, dirname, resolve } from "node:path";

const TIMEOUT_FACTOR = 1.5;
const TIMEOUT_CONSTANT_MS = 5_000;
const DEFAULT_DURATION_MS = 1_000;
const CANARY_FILES = 3;
const POLL_MS = 50;

/** What a page writes to its console when its mutant passed the hit limit. */
export const PAGE_TIMEOUT_MARKER = "SUPERCOV_MUTANT_TIMEOUT";

/** The pass this process belongs to, or `undefined` outside strong mode. */
export function strongPass() {
    const planPath = process.env["SUPERCOV_STRONG_PLAN"];
    const resultsPath = process.env["SUPERCOV_STRONG_RESULTS"];
    if (!planPath || !resultsPath)
        return undefined;
    const restart = process.env["SUPERCOV_STRONG_RESTART"] || undefined;
    const name = restart ? basename(restart).replace(/\.json$/, "") : "main";
    return {
        planPath,
        resultsPath,
        restart,
        name,
        round: resolve(dirname(resultsPath), "playwright", name),
    };
}

export function key(text) {
    return createHash("sha256").update(String(text)).digest("hex").slice(0, 24);
}

export function writeJson(path, value) {
    mkdirSync(dirname(path), { recursive: true });
    const draft = `${path}.${process.pid}.draft`;
    writeFileSync(draft, JSON.stringify(value));
    renameSync(draft, path);
}

function readJson(path) {
    return JSON.parse(readFileSync(path, "utf8"));
}

function budgetOf(durations, factor = 1) {
    const longest = Math.max(DEFAULT_DURATION_MS, ...durations.map((ms) => ms ?? DEFAULT_DURATION_MS));
    return Math.ceil((longest * TIMEOUT_FACTOR + TIMEOUT_CONSTANT_MS) * factor);
}

/**
 * The pass's rows: for each test file, what each repetition of its tests
 * does. Worked out once, in the runner, from the plan (or the restart job).
 */
function buildRows(pass, root) {
    const plan = readJson(pass.planPath);
    const tests = {};
    plan.tests.forEach((test) => {
        if (test.runner === "playwright" && test.file)
            tests[test.id] = { file: test.file, durationMs: test.durationMs ?? null };
    });
    if (pass.restart) {
        const job = readJson(pass.restart);
        const files = {};
        for (const id of job.tests) {
            const test = tests[id];
            if (!test)
                continue;
            files[test.file] ??= [{ kind: "mutant", id: job.id, mutant: job.mutant, tests: [], budgetMs: 0 }];
            files[test.file][0].tests.push(id);
        }
        for (const [, [row]] of Object.entries(files))
            row.budgetMs = budgetOf(row.tests.map((id) => tests[id].durationMs), job.budgetFactor ?? 1);
        return { pass: pass.name, job, tests, files, immediate: [], restarts: [], otherRunners: {} };
    }

    // The copy's own list: the mutant at each index must be the run's, and
    // one the instrumenter could not place has no switch to turn on.
    let listed = {};
    try {
        listed = readJson(`${root}/.supercov/frontend-evidence/mutants.json`).files ?? {};
    }
    catch {
        listed = {};
    }
    const idOf = (file, entry) => entry.id ||
        `${file}:${entry.start?.[0]}:${entry.start?.[1]}-${entry.end?.[0]}:${entry.end?.[1]}:${entry.mutator}:${entry.replacement}`;
    const indexes = new Map();
    for (const [file, entries] of Object.entries(listed))
        entries.forEach((entry, index) => indexes.set(idOf(file, entry), { index, entry }));

    const immediate = [];
    const restarts = [];
    const otherRunners = {};
    // Each file's mutants, in the plan's order, with their candidates there.
    const byFile = new Map();
    const baseline = new Map();
    const needBaseline = (file, id) => {
        if (!baseline.has(file))
            baseline.set(file, new Set());
        baseline.get(file).add(id);
    };
    for (const mutant of plan.mutants) {
        if (mutant.decided)
            continue;
        const found = indexes.get(mutant.id);
        if (!found || found.entry.unplaceable) {
            immediate.push({
                id: mutant.id,
                status: "error",
                reason: found ? `no switch could be placed (${found.entry.unplaceable})` : "the copy lists no such mutant: the source changed since the run",
                tests: 0,
                durationMs: 0,
            });
            continue;
        }
        const own = [];
        const others = new Set();
        for (const candidate of mutant.candidates) {
            const test = plan.tests[candidate.test];
            if (test && tests[test.id])
                own.push(test.id);
            else if (test)
                others.add(test.runner);
        }
        if (others.size)
            otherRunners[mutant.id] = [...others].sort();
        if (!own.length) {
            immediate.push(others.size
                ? { id: mutant.id, status: "error", reason: `its candidates ran under ${[...others].sort().join(", ")}, which this pass does not run`, tests: 0, durationMs: 0 }
                : { id: mutant.id, status: "survived", reason: "no-runnable-candidate", tests: 0, durationMs: 0 });
            continue;
        }
        const switched = `${mutant.file}#${found.index}`;
        for (const id of own)
            needBaseline(tests[id].file, id);
        if (mutant.serverWide) {
            restarts.push({ id: mutant.id, mutant: switched, tests: own, reason: mutant.serverWide, budgetFactor: 1 });
            continue;
        }
        const grouped = new Map();
        for (const id of own) {
            const file = tests[id].file;
            if (!grouped.has(file))
                grouped.set(file, []);
            grouped.get(file).push(id);
        }
        for (const [file, ids] of grouped) {
            if (!byFile.has(file))
                byFile.set(file, []);
            byFile.get(file).push({
                kind: "mutant",
                id: mutant.id,
                mutant: switched,
                tests: ids,
                budgetMs: budgetOf(ids.map((id) => tests[id].durationMs)),
            });
        }
    }
    const files = {};
    for (const [file, ids] of baseline) {
        files[file] = [{ kind: "baseline", id: null, mutant: null, tests: [...ids], budgetMs: null }, ...(byFile.get(file) ?? [])];
    }
    // Canaries: rows with no mutant among the mutated ones, in the files
    // with the most mutants. One that fails means what one attempt did to
    // the shared server reached another's (or the test is flaky).
    const busiest = [...byFile.entries()]
        .sort((a, b) => b[1].length - a[1].length || (a[0] < b[0] ? -1 : 1))
        .slice(0, CANARY_FILES);
    for (const [file, mutated] of busiest) {
        const canary = () => ({ kind: "canary", id: null, mutant: null, tests: files[file][0].tests, budgetMs: null });
        const rows = files[file];
        if (mutated.length >= 2)
            rows.splice(1 + Math.floor(mutated.length / 2), 0, canary());
        rows.push(canary());
    }
    return { pass: pass.name, tests, files, immediate, restarts, otherRunners };
}

/** The rows of this pass: built and written by the runner, read elsewhere. */
let rowsCache;
export function loadRows(pass) {
    rowsCache ??= readJson(`${pass.round}/rows.json`);
    return rowsCache;
}

/**
 * The generated config in strong mode: every test repeated once per row,
 * no retries (a retry would be a repetition of its own), no failure limit
 * (a killed mutant fails a test), and the strong reporter alone.
 */
export function strongConfig(normalized, root, reporterPath) {
    const pass = strongPass();
    let rows;
    if (process.env.TEST_WORKER_INDEX === undefined) {
        rows = buildRows(pass, root);
        rmSync(pass.round, { recursive: true, force: true });
        writeJson(`${pass.round}/rows.json`, rows);
        rowsCache = rows;
    }
    else {
        rows = loadRows(pass);
    }
    const repeatEach = Math.max(1, ...Object.values(rows.files).map((fileRows) => fileRows.length));
    const strongProject = (project) => ({ ...project, repeatEach, retries: 0 });
    return {
        ...normalized,
        ...(Array.isArray(normalized.projects) ? { projects: normalized.projects.map(strongProject) } : {}),
        repeatEach,
        retries: 0,
        maxFailures: 0,
        reporter: [["dot"], [reporterPath]],
    };
}

/** How a test did with no mutant: `passed`, `failed`, or `undefined` before it ended. */
export function baselineOf(pass, original) {
    const marker = `${pass.round}/baseline/${key(original)}`;
    try {
        return readJson(marker);
    }
    catch {
        return undefined;
    }
}

const sleep = (ms) => new Promise((done) => setTimeout(done, ms));

let repetitionsCache;
async function originalTestId(pass, testInfo) {
    if (!testInfo.repeatEachIndex)
        return testInfo.testId;
    for (let waited = 0; waited < 10_000; waited += POLL_MS) {
        if (!repetitionsCache?.[testInfo.testId] && existsSync(`${pass.round}/repetitions.json`))
            repetitionsCache = readJson(`${pass.round}/repetitions.json`);
        if (repetitionsCache?.[testInfo.testId])
            return repetitionsCache[testInfo.testId];
        await sleep(POLL_MS);
    }
    return undefined;
}

/**
 * What this repetition of a test does: `undefined` outside strong mode,
 * `null` to skip, else the mutant (`file#index`, `-` for none) and its time
 * budget. A row after the baseline waits for the test's own baseline: a test
 * that fails with no mutant cannot tell one.
 */
export async function strongClaim(testInfo) {
    const pass = strongPass();
    if (!pass)
        return undefined;
    const started = Date.now();
    const rows = loadRows(pass);
    const original = await originalTestId(pass, testInfo);
    const test = original && rows.tests[original];
    const row = test && rows.files[test.file]?.[testInfo.repeatEachIndex];
    if (!row || !row.tests.includes(original))
        return null;
    const configured = testInfo.timeout;
    if (row.kind !== "baseline" && !rows.job) {
        // Waiting is not the test's time.
        testInfo.setTimeout(0);
        const deadline = Date.now() + Math.max(120_000, budgetOf([test.durationMs]) * 4);
        while (baselineOf(pass, original) === undefined && Date.now() < deadline)
            await sleep(POLL_MS);
        if (baselineOf(pass, original) !== "passed")
            return null;
    }
    if (row.kind === "mutant" && existsSync(`${pass.round}/killed/${key(row.id)}`))
        return null;
    // The test's time from here: a mutant's from the test's measured
    // duration, a canary's as configured.
    if (row.kind === "mutant")
        testInfo.setTimeout(Date.now() - started + row.budgetMs);
    else if (configured > 0)
        testInfo.setTimeout(Date.now() - started + configured);
    else
        testInfo.setTimeout(0);
    const claimed = { original, kind: row.kind, id: row.id, mutant: row.mutant ?? "-", startedAt: Date.now() };
    writeJson(`${pass.round}/attempts/${key(testInfo.testId)}.json`, claimed);
    return claimed;
}

/** The attempt's end, and whether its page said the mutant ran away. */
export function strongFinish(testInfo, claimed, details) {
    const pass = strongPass();
    if (!pass || !claimed)
        return;
    writeJson(`${pass.round}/attempts/${key(testInfo.testId)}.json`, { ...claimed, ...details, endedAt: Date.now() });
}

/**
 * Switches a document's mutant on before its scripts run: an init script.
 * Every copy of the runtime in the page reads the state from here.
 */
export function activatePageMutant({ file, index, marker }) {
    const stateKey = Symbol.for("supercov.strongMutation");
    let strong = globalThis[stateKey];
    if (strong === undefined) {
        strong = { slots: new Map(), file: undefined, index: -1, hits: 0, limit: 1e6 };
        Object.defineProperty(globalThis, stateKey, { value: strong, configurable: true });
    }
    const previous = strong.slots.get(strong.file);
    if (previous !== undefined)
        previous.a = -1;
    strong.file = file;
    strong.index = index;
    strong.hits = 0;
    const slot = strong.slots.get(file);
    if (slot !== undefined)
        slot.a = index;
    strong.onTimeout = () => {
        try {
            console.error(marker);
        }
        catch {
            // A page that replaced its console.
        }
    };
}
