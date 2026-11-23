// Strong mutation for Playwright (docs/design/strong-mutation.md, browser
// tests against a server).
//
// An invocation of Playwright runs the plan, or as many of its rows as one
// invocation holds (the rest run in further invocations, which the engine
// starts); a harness may start several invocations at once, each with a
// share of the tests, and they share the pass's files. The generated config repeats every
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
// Playwright makes every repetition of every test of a file it loads before
// the first test runs, in the runner, whether a row names the test or not:
// an invocation holds about this many of them, and the main pass's other
// rows go to further invocations.
const TEST_CASES_PER_INVOCATION = 20_000;
// A baseline, two canaries and one mutant's row at least.
const MIN_REPETITIONS = 4;

/** What a page writes to its console when its mutant passed the hit limit. */
export const PAGE_TIMEOUT_MARKER = "SUPERCOV_MUTANT_TIMEOUT";

/**
 * How many repetitions of each test one invocation holds: what fits
 * `TEST_CASES_PER_INVOCATION` for the plan's Playwright tests, or
 * `SUPERCOV_PLAYWRIGHT_REPETITIONS`.
 */
export function repetitionsPerInvocation(testCount) {
    const asked = Number(process.env["SUPERCOV_PLAYWRIGHT_REPETITIONS"]);
    if (Number.isInteger(asked) && asked >= MIN_REPETITIONS)
        return asked;
    return Math.max(MIN_REPETITIONS, Math.floor(TEST_CASES_PER_INVOCATION / Math.max(1, testCount)));
}

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

export function readJson(path) {
    return JSON.parse(readFileSync(path, "utf8"));
}

function budgetOf(durations, factor = 1) {
    const longest = Math.max(DEFAULT_DURATION_MS, ...durations.map((ms) => ms ?? DEFAULT_DURATION_MS));
    return Math.ceil((longest * TIMEOUT_FACTOR + TIMEOUT_CONSTANT_MS) * factor);
}

/**
 * The files whose tests must not run side by side: those whose tests the
 * measured run ran, two or more of them, and never at once (by their
 * spans), and, where the spans tell nothing (a file with one test, or none
 * spanned), those the project runs one test at a time (not
 * `fullyParallel`). A file whose tests overlapped runs side by side. The
 * strong pass runs the others one test at a time; its workers are the
 * project's own.
 */
function serialFilesOf(plan, tests, fullyParallel) {
    const byFile = new Map();
    for (const { file } of Object.values(tests)) {
        if (!byFile.has(file))
            byFile.set(file, []);
    }
    for (const test of plan.tests) {
        if (tests[playwrightId(test)] && Array.isArray(test.span))
            byFile.get(tests[playwrightId(test)].file).push(test.span);
    }
    const serialFiles = [];
    for (const [file, spans] of byFile) {
        if (spans.length < 2) {
            if (!fullyParallel)
                serialFiles.push(file);
            continue;
        }
        spans.sort((a, b) => a[0] - b[0]);
        if (spans.every((span, at) => at === 0 || spans[at - 1][1] <= span[0]))
            serialFiles.push(file);
    }
    return serialFiles.sort();
}

/** Whether the project runs a file's tests side by side: every project, or the config, says `fullyParallel`. */
function fullyParallelOf(config) {
    const projects = Array.isArray(config?.projects) && config.projects.length ? config.projects : [{}];
    return projects.every((project) => project.fullyParallel ?? config?.fullyParallel ?? false);
}

/** A mutant's rows: one per file its tests are in, each with its tests there. */
function mutantRows(tests, entry) {
    const grouped = new Map();
    for (const id of entry.tests) {
        const file = tests[id].file;
        if (!grouped.has(file))
            grouped.set(file, []);
        grouped.get(file).push(id);
    }
    return [...grouped].map(([file, ids]) => [file, {
        kind: "mutant",
        id: entry.id,
        mutant: entry.mutant,
        tests: ids,
        budgetMs: budgetOf(ids.map((id) => tests[id].durationMs), entry.budgetFactor ?? 1),
    }]);
}

/**
 * The pass's rows: for each test file, what each repetition of its tests
 * does. Worked out once, in the runner, from the plan (or the job the pass
 * runs). The main pass runs each mutant against its likeliest killer alone
 * (a test that infected it, cheapest first): its other candidates run in a
 * later pass only if that one survives.
 */
// A plan test's id as Playwright knows it: a Rust wasm run names a test by
// its file too (`<file>::<id>`).
function playwrightId(test) {
    return test.file && test.id.startsWith(`${test.file}::`) ? test.id.slice(test.file.length + 2) : test.id;
}

function buildRows(pass, root, config) {
    const plan = readJson(pass.planPath);
    const tests = {};
    plan.tests.forEach((test) => {
        if (test.runner === "playwright" && test.file)
            tests[playwrightId(test)] = { file: test.file, durationMs: test.durationMs ?? null };
    });
    // `workers` set only where the pass runs one test at a time.
    const concurrency = { workers: undefined, serialFiles: serialFilesOf(plan, tests, fullyParallelOf(config)) };
    const job = pass.restart ? readJson(pass.restart) : null;
    const perInvocation = repetitionsPerInvocation(Object.keys(tests).length);
    if (job && job.kind !== "main") {
        const files = {};
        const entries = {};
        // A mutant with a row in a file this invocation has filled waits
        // for the next invocation, which runs the same job.
        const deferred = [];
        for (const entry of job.mutants) {
            const known = { ...entry, tests: entry.tests.filter((id) => tests[id]) };
            const rows = mutantRows(tests, known);
            if (rows.some(([file]) => (files[file]?.length ?? 0) >= perInvocation)) {
                deferred.push(entry);
                continue;
            }
            entries[entry.id] = known;
            for (const [file, row] of rows)
                (files[file] ??= []).push(row);
        }
        // A serial pass runs one test at a time: what it kills, nothing
        // beside it did.
        if (job.kind === "serial")
            concurrency.workers = 1;
        return { pass: pass.name, kind: job.kind, job, tests, files, entries, concurrency, immediate: [], restarts: [], otherRunners: job.otherRunners ?? {}, deferred };
    }
    // A main pass's later invocation runs the mutants the one before had no
    // room for, as the main pass would.
    const only = job ? new Set(job.mutants.map((mutant) => mutant.id)) : undefined;

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
    const deferred = [];
    // A file's mutated rows, beside its baseline and two canaries.
    const rowsPerFile = perInvocation - 3;
    const otherRunners = {};
    const entries = {};
    const byFile = new Map();
    const baseline = new Map();
    const needBaseline = (file, id) => {
        if (!baseline.has(file))
            baseline.set(file, new Set());
        baseline.get(file).add(id);
    };
    for (const mutant of plan.mutants) {
        if (mutant.decided || (only && !only.has(mutant.id)))
            continue;
        // A Rust mutant (Rust compiled to wasm) is switched by its index in
        // its file's list, as the pass's build placed it.
        const found = mutant.language === "rust" ? { index: mutant.index, entry: {} } : indexes.get(mutant.id);
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
        // Its candidates, cheapest first (the plan's order), those whose own
        // evidence shows it infected first of all.
        const own = [];
        const others = new Set();
        for (const candidate of mutant.candidates) {
            const test = plan.tests[candidate.test];
            if (test && tests[playwrightId(test)])
                own.push({ id: playwrightId(test), infected: candidate.reason === "infected" });
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
        const ordered = [...own.filter((test) => test.infected), ...own.filter((test) => !test.infected)].map((test) => test.id);
        const switched = `${mutant.file}#${found.index}`;
        if (mutant.serverWide) {
            for (const id of ordered)
                needBaseline(tests[id].file, id);
            restarts.push({ id: mutant.id, mutant: switched, tests: ordered, reason: mutant.serverWide, budgetFactor: 1 });
            continue;
        }
        const entry = { id: mutant.id, mutant: switched, tests: [ordered[0]], rest: ordered.slice(1), budgetFactor: 1 };
        // One row, in its first test's file.
        const [[file, row]] = mutantRows(tests, entry);
        if (!byFile.has(file))
            byFile.set(file, []);
        // A file's rows past what this invocation holds wait for the next.
        if (byFile.get(file).length >= rowsPerFile) {
            deferred.push({ id: mutant.id });
            continue;
        }
        byFile.get(file).push(row);
        entries[mutant.id] = entry;
        for (const id of ordered)
            needBaseline(tests[id].file, id);
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
    return { pass: pass.name, kind: "main", job: null, tests, files, entries, concurrency, immediate, restarts, otherRunners, deferred };
}

/** The rows of this pass: built and written by the runner, read by its reporter. */
let rowsCache;
export function loadRows(pass) {
    rowsCache ??= readJson(`${pass.round}/rows.json`);
    return rowsCache;
}

/**
 * What a worker reads: the pass's shape (`worker.json`: the repetitions,
 * the tests and how they run side by side), and the rows of the one
 * repetition index it serves (`rows/<index>.json`), never the plan or the
 * other rows. Each is small and the same size whatever the plan's.
 */
function writeWorkerFiles(pass, rows, repeatEach) {
    writeJson(`${pass.round}/worker.json`, { repeatEach, tests: rows.tests, concurrency: rows.concurrency, job: Boolean(rows.job) });
    const byIndex = Array.from({ length: repeatEach }, () => ({}));
    for (const [file, fileRows] of Object.entries(rows.files))
        fileRows.forEach((row, index) => { byIndex[index][file] = row; });
    byIndex.forEach((at, index) => writeJson(`${pass.round}/rows/${index}.json`, at));
}

let workerCache;
function loadWorker(pass) {
    workerCache ??= readJson(`${pass.round}/worker.json`);
    return workerCache;
}

const indexCache = new Map();
function rowsAt(pass, index) {
    if (!indexCache.has(index))
        indexCache.set(index, readJson(`${pass.round}/rows/${index}.json`));
    return indexCache.get(index);
}

/**
 * Whether this process lists the tests (`playwright test --list`) rather
 * than run them: it reads no plan and writes nothing, and lists the
 * project's tests as they are, once each.
 */
function listing() {
    return process.argv.includes("--list");
}

/**
 * The generated config in strong mode: every test repeated once per row,
 * no retries (a retry would be a repetition of its own), no failure limit
 * (a killed mutant fails a test), the project's own workers (one in a
 * serial pass), and the strong reporter alone.
 */
export function strongConfig(normalized, root, reporterPath) {
    if (listing())
        return normalized;
    const pass = strongPass();
    let shape;
    if (process.env.TEST_WORKER_INDEX === undefined) {
        // Every invocation of the pass (a harness may start several at
        // once, each with a share of the tests) works out the same rows from
        // the same plan and writes them alike; none removes what another
        // wrote. The pass's directory is new with the pass.
        const rows = buildRows(pass, root, normalized);
        const repeatEach = Math.max(1, ...Object.values(rows.files).map((fileRows) => fileRows.length));
        // This invocation, for its workers: they share its server.
        process.env["SUPERCOV_PLAYWRIGHT_INVOCATION"] = String(process.pid);
        writeJson(`${pass.round}/rows.json`, rows);
        writeWorkerFiles(pass, rows, repeatEach);
        rowsCache = rows;
        shape = { repeatEach, concurrency: rows.concurrency };
    }
    else {
        shape = loadWorker(pass);
    }
    const { repeatEach, concurrency } = shape;
    const strongProject = (project) => ({ ...project, repeatEach, retries: 0 });
    return {
        ...normalized,
        ...(Array.isArray(normalized.projects) ? { projects: normalized.projects.map(strongProject) } : {}),
        repeatEach,
        retries: 0,
        maxFailures: 0,
        ...(concurrency.workers ? { workers: concurrency.workers } : {}),
        // The strong reporter alone: `mutants test --json` owns stdout.
        reporter: [[reporterPath]],
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

/**
 * Where a test is, the same for each of its repetitions and in every
 * invocation: its project and title path. The reporter, from a test case.
 */
export function placeOf(test) {
    return JSON.stringify(test.titlePath().slice(1));
}

/** The test the plan names (its first repetition's id) for a repetition, as its invocation's reporter wrote it. */
async function originalTestId(pass, testInfo) {
    if (!testInfo.repeatEachIndex)
        return testInfo.testId;
    const path = `${pass.round}/originals/${key(JSON.stringify([testInfo.project.name, ...testInfo.titlePath]))}`;
    for (let waited = 0; waited < 10_000; waited += POLL_MS) {
        if (existsSync(path))
            return readJson(path);
        await sleep(POLL_MS);
    }
    return undefined;
}

function alive(pid) {
    try {
        process.kill(pid, 0);
        return true;
    }
    catch (error) {
        return error?.code === "EPERM";
    }
}

/**
 * One test of `file` at a time, across the pass's workers: the file's lock,
 * a directory, held by this worker's process until the test ends. A lock
 * whose holder died is taken over.
 */
async function lockFile(pass, file) {
    // Per invocation: another invocation's tests reach a server of their own.
    const locks = `${pass.round}/locks/${process.env["SUPERCOV_PLAYWRIGHT_INVOCATION"]}`;
    const lock = `${locks}/${key(file)}`;
    mkdirSync(locks, { recursive: true });
    for (;;) {
        try {
            mkdirSync(lock);
            writeFileSync(`${lock}/owner`, String(process.pid));
            return lock;
        }
        catch (error) {
            if (error?.code !== "EEXIST")
                throw error;
        }
        let owner;
        try {
            owner = Number(readFileSync(`${lock}/owner`, "utf8"));
        }
        catch {
            owner = undefined;
        }
        if (owner && owner !== process.pid && !alive(owner))
            rmSync(lock, { recursive: true, force: true });
        await sleep(POLL_MS);
    }
}

/**
 * What this repetition of a test does: `undefined` outside strong mode,
 * `null` to skip, else the mutant (`file#index`, `-` for none) and its time
 * budget. A row after the baseline waits for the test's own baseline: a test
 * that fails with no mutant cannot tell one. A file whose tests the measured
 * run never ran side by side runs one at a time here too.
 */
export async function strongClaim(testInfo) {
    const pass = strongPass();
    if (!pass)
        return undefined;
    const started = Date.now();
    const rows = loadWorker(pass);
    const original = await originalTestId(pass, testInfo);
    const test = original && rows.tests[original];
    const row = test && rowsAt(pass, testInfo.repeatEachIndex)[test.file];
    if (!row || !row.tests.includes(original))
        return null;
    const configured = testInfo.timeout;
    // Waiting is not the test's time.
    testInfo.setTimeout(0);
    if (row.kind !== "baseline" && !rows.job) {
        const deadline = Date.now() + Math.max(120_000, budgetOf([test.durationMs]) * 4);
        while (baselineOf(pass, original) === undefined && Date.now() < deadline)
            await sleep(POLL_MS);
        if (baselineOf(pass, original) !== "passed")
            return null;
    }
    const killed = () => row.kind === "mutant" && existsSync(`${pass.round}/killed/${key(row.id)}`);
    if (killed())
        return null;
    const lock = rows.concurrency.workers !== 1 && rows.concurrency.serialFiles.includes(test.file)
        ? await lockFile(pass, test.file)
        : undefined;
    if (killed()) {
        if (lock)
            rmSync(lock, { recursive: true, force: true });
        return null;
    }
    // The test's time from here: a mutant's from the test's measured
    // duration and the time Playwright gives an assertion and an action to
    // fail in (a failing `expect` waits for its own timeout), a canary's as
    // configured.
    if (row.kind === "mutant") {
        const expectTimeout = testInfo.project?.expect?.timeout ?? 5_000;
        const actionTimeout = testInfo.project?.use?.actionTimeout ?? 0;
        // Playwright itself ends the test at its configured timeout (the
        // project's, or its own `test.slow()` / `describe.configure`), and an
        // assertion may wait up to there with a timeout of its own: that is
        // the floor.
        const budget = Math.max(row.budgetMs + expectTimeout + actionTimeout, configured > 0 ? configured : 0);
        testInfo.setTimeout(Date.now() - started + budget);
    }
    else if (configured > 0)
        testInfo.setTimeout(Date.now() - started + configured);
    const claimed = {
        original,
        index: testInfo.repeatEachIndex,
        testId: testInfo.testId,
        invocation: process.env["SUPERCOV_PLAYWRIGHT_INVOCATION"],
        kind: row.kind,
        id: row.id,
        mutant: row.mutant ?? "-",
        startedAt: Date.now(),
        lock,
    };
    writeJson(`${pass.round}/attempts/${key(testInfo.testId)}.json`, claimed);
    return claimed;
}

/** The attempt's end, and whether its page said the mutant ran away. */
export function strongFinish(testInfo, claimed, details) {
    const pass = strongPass();
    if (!pass || !claimed)
        return;
    // A kill is marked here, before the worker starts its next test: the
    // reporter hears of it later, and the next test of the row would run.
    if (claimed.kind === "mutant" && testInfo.status !== undefined && testInfo.status !== "passed" && testInfo.status !== "skipped")
        writeJson(`${pass.round}/killed/${key(claimed.id)}`, claimed.original);
    writeJson(`${pass.round}/attempts/${key(testInfo.testId)}.json`, { ...claimed, ...details, endedAt: Date.now() });
    if (claimed.lock)
        rmSync(claimed.lock, { recursive: true, force: true });
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
