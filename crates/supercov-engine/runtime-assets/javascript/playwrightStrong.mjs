// Strong mutation for Playwright (docs/design/strong-mutation.md, browser
// tests against a server).
//
// One invocation of the command runs the whole plan. Every test some mutant
// needs is declared several times over as the spec loads (its copies, each
// in a describe of its own, `[supercov copy <k>]`), so that one worker runs
// them one after another with the browser it already has; a copy takes its
// work when it starts: the next mutant whose candidate it is, in a queue on
// disk that every worker and every invocation of the pass share. A mutant
// goes from candidate to candidate as copies take it, and stops at its first
// kill; nothing waits for a stage to end, and no worker restarts for a
// mutant. The first run of a test in each worker is a control, with no
// mutant: the pass's baseline the first time, and after a kill (a failed
// test gives Playwright's next test a new worker) the check that the test
// still passes without the mutant on the same server, which confirms the
// kill. A harness may start several invocations at once, each with a share
// of the tests; they share the pass's files and decide the same way.
//
// The attempt's mutant goes into its scope, which reaches the app's server
// with every request (header, cookies, child processes) and the page through
// an init script. A mutant the server must start with (its code ran in work
// no request named, or is a server module's top level, or ran for a request
// that named no test) is run alone afterwards, in an invocation of its own
// with SUPERCOV_ACTIVE_MUTANT set, which the engine starts for each `restart`
// record the judge writes (javascript_run.rs). So do a test that only ran
// out of time (again, with twice the time), a kill that ran beside another
// mutant's attempt or whose control failed (one test at a time), and the
// mutants one invocation had no room for.
//
// This module holds what the config, the fixture (playwright.mjs) and the
// reporter (playwrightStrongReporter.mjs) share.
import { appendFileSync, existsSync, mkdirSync, readdirSync, readFileSync, realpathSync, renameSync, rmSync, writeFileSync } from "node:fs";
import { createHash } from "node:crypto";
import { basename, dirname, resolve, sep } from "node:path";

const TIMEOUT_FACTOR = 1.5;
const TIMEOUT_CONSTANT_MS = 5_000;
const DEFAULT_DURATION_MS = 1_000;
const POLL_MS = 50;
// Playwright makes every test case of every file it loads before the first
// test runs, in the runner: an invocation holds about this many copies, and
// the mutants past them run in a further invocation.
const COPIES_PER_INVOCATION = 20_000;
// A test whose control fails this often in one invocation is not run again
// there: its mutants wait for a later invocation.
const CONTROL_FAILURES = 2;

/** What a page writes to its console when its mutant passed the hit limit. */
export const PAGE_TIMEOUT_MARKER = "SUPERCOV_MUTANT_TIMEOUT";

/** A copy's describe: `[supercov copy <k>]`, k from 1. */
const COPY_TITLE = /^\[supercov copy (\d+)\]$/;
export function copyTitle(copy) {
    return `[supercov copy ${copy}]`;
}

/**
 * How many test cases one invocation holds: `COPIES_PER_INVOCATION`, or
 * `SUPERCOV_PLAYWRIGHT_COPIES`.
 */
export function copiesPerInvocation() {
    const asked = Number(process.env["SUPERCOV_PLAYWRIGHT_COPIES"]);
    return Number.isInteger(asked) && asked > 0 ? asked : COPIES_PER_INVOCATION;
}

/** The pass this process belongs to, or `undefined` outside strong mode. */
export function strongPass() {
    const planPath = process.env["SUPERCOV_STRONG_PLAN"];
    const resultsPath = process.env["SUPERCOV_STRONG_RESULTS"];
    if (!planPath || !resultsPath)
        return undefined;
    const restart = process.env["SUPERCOV_STRONG_RESTART"] || undefined;
    const name = restart ? basename(restart).replace(/\.json$/, "") : "main";
    const shared = resolve(dirname(resultsPath), "playwright");
    return {
        planPath,
        resultsPath,
        restart,
        name,
        // What every invocation of every round shares: the baselines, the
        // verdicts given, each process's status.
        shared,
        round: resolve(shared, name),
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

function readJsonOr(path, fallback) {
    try {
        return readJson(path);
    }
    catch {
        return fallback;
    }
}

/** Each entry of a directory of the pass, or none. */
export function entries(directory) {
    try {
        return readdirSync(directory).sort();
    }
    catch {
        return [];
    }
}

/** Makes a directory that marks something done once: `false` where it was already made. */
export function mark(path) {
    try {
        mkdirSync(path);
        return true;
    }
    catch (error) {
        if (error?.code === "ENOENT") {
            mkdirSync(dirname(path), { recursive: true });
            return mark(path);
        }
        if (error?.code === "EEXIST")
            return false;
        throw error;
    }
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

/**
 * The projects others depend on (`dependencies`, `teardown`): their tests,
 * a login that stores the session others start from, run as they are in
 * every invocation, with no mutant, whether or not a mutant needs them.
 */
function setupProjectsOf(config) {
    const names = new Set();
    for (const project of Array.isArray(config?.projects) ? config.projects : []) {
        for (const name of project.dependencies ?? [])
            names.add(name);
        if (project.teardown)
            names.add(project.teardown);
    }
    return [...names].sort();
}

/** Whether the project runs a file's tests side by side: every project, or the config, says `fullyParallel`. */
function fullyParallelOf(config) {
    const projects = Array.isArray(config?.projects) && config.projects.length ? config.projects : [{}];
    return projects.every((project) => project.fullyParallel ?? config?.fullyParallel ?? false);
}

// A plan test's id as Playwright knows it: a Rust wasm run names a test by
// its file too (`<file>::<id>`).
function playwrightId(test) {
    return test.file && test.id.startsWith(`${test.file}::`) ? test.id.slice(test.file.length + 2) : test.id;
}

/** A spec file as both the runner and a test's declaration name it: its real path. */
const realFiles = new Map();
function realFile(path) {
    if (!realFiles.has(path)) {
        let real = path;
        try {
            real = realpathSync(path);
        }
        catch {
            real = path;
        }
        realFiles.set(path, real);
    }
    return realFiles.get(path);
}

/** What a test's copies are counted by where it is declared: its spec file and title. */
function declarationKey(file, title) {
    return `${realFile(file)}\0${title}`;
}

/**
 * The pass's work, worked out in the runner from the plan (or the job the
 * round runs): each mutant to run with its candidate tests in the order it
 * takes them (those whose own evidence infected it first, cheapest first),
 * each test's queue of mutants (those it is the likeliest killer of first),
 * and how many copies each test needs: one for each mutant that may take it,
 * one more for each control a kill may call for, and the first control.
 */
function buildPass(pass, root, config) {
    const plan = readJson(pass.planPath);
    const tests = {};
    plan.tests.forEach((test) => {
        if (test.runner === "playwright" && test.file)
            tests[playwrightId(test)] = { file: test.file, title: test.title ?? null, durationMs: test.durationMs ?? null, jobs: 0 };
    });
    // `workers` set only where the round runs one test at a time.
    const concurrency = { workers: undefined, serialFiles: serialFilesOf(plan, tests, fullyParallelOf(config)) };
    const job = pass.restart ? readJson(pass.restart) : null;
    const kind = job?.kind ?? "main";
    const immediate = [];
    const restarts = [];
    const deferred = [];
    const otherRunners = { ...(job?.otherRunners ?? {}) };
    const mutants = [];
    if (job && job.kind !== "main") {
        for (const entry of job.mutants) {
            const all = [...entry.tests, ...(entry.rest ?? [])].filter((id) => tests[id]);
            mutants.push({ id: entry.id, mutant: entry.mutant, tests: [...new Set(all)], reason: entry.reason, budgetFactor: entry.budgetFactor ?? 1 });
        }
        // A serial round runs one test at a time: what it kills, nothing
        // beside it did.
        if (kind === "serial")
            concurrency.workers = 1;
    }
    else {
        // A main round's later invocation runs the mutants the one before had
        // no room for, as the main round would.
        const only = job ? new Set(job.mutants.map((mutant) => mutant.id)) : undefined;
        // The copy's own list: the mutant at each index must be the run's, and
        // one the instrumenter could not place has no switch to turn on.
        const listed = readJsonOr(`${root}/.supercov/frontend-evidence/mutants.json`, {}).files ?? {};
        const idOf = (file, entry) => entry.id ||
            `${file}:${entry.start?.[0]}:${entry.start?.[1]}-${entry.end?.[0]}:${entry.end?.[1]}:${entry.mutator}:${entry.replacement}`;
        const indexes = new Map();
        for (const [file, fileEntries] of Object.entries(listed))
            fileEntries.forEach((entry, index) => indexes.set(idOf(file, entry), { index, entry }));
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
            const ordered = [...new Set([...own.filter((test) => test.infected), ...own.filter((test) => !test.infected)].map((test) => test.id))];
            const switched = `${mutant.file}#${found.index}`;
            if (mutant.serverWide) {
                restarts.push({ id: mutant.id, mutant: switched, tests: ordered, reason: mutant.serverWide, budgetFactor: 1 });
                continue;
            }
            mutants.push({ id: mutant.id, mutant: switched, tests: ordered, budgetFactor: 1 });
        }
    }
    // Each test's queue, and the copies the round's mutants need, as many as
    // an invocation holds; the mutants past them wait for the next.
    const limit = copiesPerInvocation();
    const queues = new Map();
    let copies = 0;
    const entries = {};
    for (const entry of mutants) {
        const added = entry.tests.reduce((sum, id) => sum + 2 + (queues.has(id) ? 0 : 1), 0);
        if (copies && copies + added > limit && kind === "main") {
            deferred.push({ id: entry.id });
            continue;
        }
        copies += added;
        entries[entry.id] = entry;
        entry.tests.forEach((id, at) => {
            if (!queues.has(id))
                queues.set(id, []);
            queues.get(id).push({ id: entry.id, at });
        });
    }
    const declared = {};
    for (const [id, queue] of queues) {
        // The likeliest killers' mutants first; the plan's order between equals.
        queue.sort((a, b) => a.at - b.at);
        const test = tests[id];
        test.jobs = queue.length;
        test.queue = queue.map((job) => job.id);
        const where = declarationKey(resolve(root, test.file), test.title);
        declared[where] = Math.max(declared[where] ?? 0, 2 * queue.length + 1);
    }
    return {
        pass: pass.name,
        kind,
        job,
        // A round whose server runs one mutant everywhere has no control: the
        // test cannot run without it there.
        everywhere: kind === "alone",
        // One mutant an invocation: each runner takes one as it starts, and
        // the processes it starts (its web server) run with it on.
        batch: kind === "alone" && Boolean(job?.batch),
        // A main round's first control of a test is its baseline.
        baselines: kind === "main",
        tests,
        declared,
        entries,
        setupProjects: setupProjectsOf(config),
        concurrency,
        immediate,
        restarts,
        otherRunners,
        deferred,
    };
}

/** The round's work: built and written by the runner, read by its judge. */
let passCache;
export function loadPass(pass) {
    passCache ??= readJson(`${pass.round}/pass.json`);
    return passCache;
}

/**
 * What a worker reads: the round's shape (`worker.json`: the tests, their
 * copies and how they run side by side), each test's queue of mutants
 * (`queues/<test>.json`) and each mutant it takes (`mutants/<mutant>.json`),
 * never the plan.
 */
function writeRoundFiles(pass, work) {
    const tests = {};
    for (const [id, test] of Object.entries(work.tests)) {
        if (!test.jobs)
            continue;
        tests[id] = { file: test.file, title: test.title, durationMs: test.durationMs, jobs: test.jobs };
        writeJson(`${pass.round}/queues/${key(id)}.json`, test.queue);
    }
    for (const entry of Object.values(work.entries))
        writeJson(`${pass.round}/mutants/${key(entry.id)}.json`, entry);
    writeJson(`${pass.round}/worker.json`, {
        kind: work.kind,
        everywhere: work.everywhere,
        batch: work.batch,
        baselines: work.baselines,
        tests,
        declared: work.declared,
        setupProjects: work.setupProjects,
        concurrency: work.concurrency,
    });
}

let workerCache;
function loadWorker(pass) {
    workerCache ??= readJson(`${pass.round}/worker.json`);
    return workerCache;
}

const queueCache = new Map();
function queueOf(pass, test) {
    if (!queueCache.has(test))
        queueCache.set(test, { ids: readJsonOr(`${pass.round}/queues/${key(test)}.json`, []), done: 0 });
    return queueCache.get(test);
}

const mutantCache = new Map();
function mutantOf(pass, id) {
    if (!mutantCache.has(id))
        mutantCache.set(id, readJson(`${pass.round}/mutants/${key(id)}.json`));
    return mutantCache.get(id);
}

/**
 * Whether this process lists the tests (`playwright test --list`) rather
 * than run them: it reads no plan and writes nothing, and lists the
 * project's tests as they are, once each.
 */
function listing() {
    return process.argv.includes("--list");
}

/** Whether this process runs the strong pass's tests: not outside it, and not to list them. */
export function copying() {
    return Boolean(strongPass()) && !listing();
}

/**
 * The generated config in strong mode: no retries (a retry would be a copy
 * of its own), no failure limit (a killed mutant fails a test), the
 * project's own workers (one in a serial round), and the strong reporter
 * alone.
 */
export function strongConfig(normalized, root, reporterPath) {
    if (listing())
        return normalized;
    const pass = strongPass();
    let concurrency;
    if (process.env.TEST_WORKER_INDEX === undefined) {
        // Every invocation of the round (a harness may start several at
        // once, each with a share of the tests) works out the same work from
        // the same plan and writes it alike; none removes what another
        // wrote. The round's directory is new with the round.
        const work = buildPass(pass, root, normalized);
        // This invocation, for its workers: they share its server.
        process.env["SUPERCOV_PLAYWRIGHT_INVOCATION"] = String(process.pid);
        writeJson(`${pass.round}/pass.json`, work);
        writeRoundFiles(pass, work);
        passCache = work;
        if (work.batch)
            takeAloneMutant(pass, work, root);
        concurrency = work.concurrency;
    }
    else {
        concurrency = loadWorker(pass).concurrency;
    }
    const strongProject = (project) => ({ ...project, repeatEach: 1, retries: 0 });
    return {
        ...normalized,
        ...(Array.isArray(normalized.projects) ? { projects: normalized.projects.map(strongProject) } : {}),
        repeatEach: 1,
        retries: 0,
        maxFailures: 0,
        ...(concurrency.workers ? { workers: concurrency.workers } : {}),
        // The strong reporter alone: `mutants test --json` owns stdout.
        reporter: [[reporterPath]],
    };
}

/**
 * In a round of mutants a server must start with, each invocation's runner
 * takes the next one no other took, and switches it on for every process it
 * starts from here (`webServer`, its workers): several mutants run alone at
 * once, one per invocation of a harness that starts several. An invocation
 * with none left runs nothing.
 */
function takeAloneMutant(pass, work, root) {
    const fileOf = (id) => realFile(resolve(root, work.tests[id]?.file ?? ""));
    const named = namedSpecs(Object.keys(work.tests).map(fileOf));
    const here = (entry) => !named || entry.tests.some((id) => named.has(fileOf(id)));
    for (const entry of work.job.mutants) {
        if (!here(entry) || !mark(`${pass.round}/alone/${key(entry.id)}`))
            continue;
        process.env["SUPERCOV_ACTIVE_MUTANT"] = entry.mutant;
        writeJson(`${pass.round}/invocations/${process.pid}.json`, { id: entry.id, mutant: entry.mutant });
        return;
    }
    writeJson(`${pass.round}/invocations/${process.pid}.json`, { id: null });
}

/**
 * The plan's spec files this invocation's command line names, as a harness
 * that shares the tests out names them (`a.spec.ts:12`, or a folder of
 * them); `undefined` where it names none of them (every test is its).
 */
function namedSpecs(specs) {
    const named = new Set();
    for (const arg of process.argv.slice(2)) {
        if (arg.startsWith("-"))
            continue;
        const path = realFile(resolve(arg.replace(/(:\d+)+$/, "")));
        for (const spec of specs) {
            if (spec === path || spec.startsWith(`${path}${sep}`))
                named.add(spec);
        }
    }
    return named.size ? named : undefined;
}

/** The mutant this invocation runs, in a round of one mutant an invocation. */
let aloneMutant;
function invocationMutant(pass) {
    aloneMutant ??= readJsonOr(`${pass.round}/invocations/${invocation()}.json`, { id: null }).id;
    return aloneMutant;
}

/**
 * Declares each test of the round as many times as its copies: Playwright's
 * test declaration, wrapped, on the prototype every `test` object (and each
 * one `test.extend` makes later) binds. Copies keep the test's title and
 * place (a `file:line` filter takes them all); each is in a describe of its
 * own, `[supercov copy <k>]`, so that its full title is new. A serial
 * describe runs as a default one: in order, in one worker, but a failed test
 * (a kill) does not skip the rest; a test that leans on an earlier one's
 * state is caught by its control.
 */
export function installCopies(baseTest) {
    if (!copying())
        return false;
    const symbol = Object.getOwnPropertySymbols(baseTest).find((one) => one.description === "testType");
    const prototype = symbol && Object.getPrototypeOf(baseTest[symbol]);
    const declare = prototype?._createTest;
    const describe = prototype?._describe;
    const configure = prototype?._configure;
    if (typeof declare !== "function" || typeof describe !== "function" || typeof configure !== "function")
        throw new Error("Supercov's strong pass cannot declare copies of tests with this version of Playwright");
    if (prototype.__supercovCopies)
        return true;
    Object.defineProperty(prototype, "__supercovCopies", { value: true });
    prototype._createTest = function (type, location, title, fnOrDetails, fn) {
        declare.call(this, type, location, title, fnOrDetails, fn);
        if (type === "skip" || type === "fixme")
            return;
        const suite = this._currentSuite?.(location, "test()");
        const copies = copiesOf(suite?._requireFile ?? location?.file, title);
        for (let copy = 1; copy < copies; copy++)
            describe.call(this, "default", location, copyTitle(copy), () => declare.call(this, type, location, title, fnOrDetails, fn));
    };
    prototype._describe = function (type, ...rest) {
        return describe.call(this, type === "serial" ? "default" : type === "serial.only" ? "only" : type, ...rest);
    };
    prototype._configure = function (location, options) {
        return configure.call(this, location, options?.mode === "serial" ? { ...options, mode: "default" } : options);
    };
    return true;
}

let declaredCopies;
function copiesOf(file, title) {
    if (!file)
        return 1;
    declaredCopies ??= readJsonOr(`${strongPass().round}/worker.json`, { declared: {} }).declared;
    return declaredCopies[declarationKey(file, title)] ?? 1;
}

/** How a test did with no mutant: `passed`, `failed`, or `undefined` before it ended. */
export function baselineOf(pass, original) {
    try {
        return readJson(`${pass.shared}/baseline/${key(original)}`);
    }
    catch {
        return undefined;
    }
}

const sleep = (ms) => new Promise((done) => setTimeout(done, ms));

/**
 * Where a test is, the same for each of its copies and in every invocation:
 * its project and title path, without its copy's describe. The reporter, from
 * a test case.
 */
export function placeOf(test) {
    return JSON.stringify(withoutCopy(test.titlePath().slice(1)).path);
}

/** A title path without its copy's describe, and which copy it is: 0 for the test as declared. */
export function withoutCopy(path) {
    const at = path.length - 2;
    const found = at >= 0 ? COPY_TITLE.exec(path[at]) : null;
    return found ? { path: [...path.slice(0, at), path[at + 1]], copy: Number(found[1]) } : { path, copy: 0 };
}

/** The test the plan names for a copy, as its invocation's reporter wrote it. */
async function originalTestId(pass, testInfo) {
    const { path, copy } = withoutCopy(testInfo.titlePath);
    if (!copy)
        return testInfo.testId;
    const file = `${pass.round}/originals/${key(JSON.stringify([testInfo.project.name, ...path]))}`;
    for (let waited = 0; waited < 10_000; waited += POLL_MS) {
        if (existsSync(file))
            return readJson(file);
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
 * One test of `file` at a time, across the round's workers: the file's lock,
 * a directory, held by this worker's process until the test ends. A lock
 * whose holder died is taken over.
 */
async function lockFile(pass, file) {
    // Per invocation: another invocation's tests reach a server of their own.
    const locks = `${pass.round}/locks/${invocation()}`;
    const lock = `${locks}/${key(file)}`;
    mkdirSync(locks, { recursive: true });
    for (;;) {
        if (mark(lock)) {
            writeFileSync(`${lock}/owner`, String(process.pid));
            return lock;
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

/** Where an invocation keeps its attempt of a test case: several may run the same test. */
export function attemptPath(pass, at, testId) {
    return `${pass.round}/attempts/${at}-${key(testId)}.json`;
}

export function invocation() {
    return process.env["SUPERCOV_PLAYWRIGHT_INVOCATION"] ?? "0";
}

// The tests that passed without a mutant in this worker (a control): a test
// runs with a mutant only in a worker where it passed without one.
const passedHere = new Set();

/** Whether a mutant needs no more attempts: a verdict given, killed, or held for a later round. */
function settled(pass, id) {
    const at = key(id);
    return existsSync(`${pass.shared}/verdicts/${at}`) || existsSync(`${pass.round}/killed/${at}`) || existsSync(`${pass.round}/held/${at}`);
}

/**
 * Whether another attempt runs the mutant now, and since when: the earliest
 * start of those running, or `undefined` where none does.
 */
function inFlightSince(pass, id) {
    let since;
    for (const entry of entries(`${pass.round}/inflight/${key(id)}`)) {
        if (!alive(Number(entry.split("-")[0])))
            continue;
        let started;
        try {
            started = Number(readFileSync(`${pass.round}/inflight/${key(id)}/${entry}`, "utf8")) || 0;
        }
        catch {
            // It ended as this looked.
            continue;
        }
        since = since === undefined ? started : Math.min(since, started);
    }
    return since;
}

/**
 * The next mutant for a copy of `test`: the first of its queue not settled
 * and not yet tried against it, one no other attempt runs first. Taken by
 * making its mark, which only one copy can.
 */
function takeJob(pass, test, only, budgetMs) {
    const queue = queueOf(pass, test);
    const busy = [];
    for (let at = queue.done; at < queue.ids.length; at++) {
        const id = queue.ids[at];
        if (only !== undefined && id !== only)
            continue;
        const tried = `${pass.round}/claims/${key(id)}/${key(test)}`;
        if (existsSync(tried) || settled(pass, id)) {
            // Nothing undoes either: no copy looks at it again.
            if (at === queue.done)
                queue.done += 1;
            continue;
        }
        const since = inFlightSince(pass, id);
        if (since !== undefined) {
            // One that has run longer than this test would take may hang:
            // a second attempt would only wait as long.
            if (Date.now() - since < budgetMs)
                busy.push(id);
            continue;
        }
        if (mark(tried))
            return id;
    }
    // Every mutant left runs elsewhere now: one of them here too, rather
    // than leave this test's copies unused while its attempt runs.
    for (const id of busy) {
        if (only === undefined && !settled(pass, id) && mark(`${pass.round}/claims/${key(id)}/${key(test)}`))
            return id;
    }
    return undefined;
}

/** Whether a test has mutants left to take, or kills to confirm, in this invocation. */
function workLeft(pass, test) {
    if (entries(`${pass.round}/pending/${invocation()}/${key(test)}`).length)
        return true;
    const queue = queueOf(pass, test);
    for (let at = queue.done; at < queue.ids.length; at++) {
        const id = queue.ids[at];
        if (!existsSync(`${pass.round}/claims/${key(id)}/${key(test)}`) && !settled(pass, id))
            return true;
    }
    return false;
}

/**
 * Notes that an attempt runs, and whether another mutant's attempt runs
 * beside it in this invocation (its server): each attempt that starts looks
 * at those running, and marks them and itself, so of two that overlap one
 * sees the other.
 */
function startRunning(pass, testId, mutant) {
    const running = `${pass.round}/running/${invocation()}`;
    const mine = `${process.pid}-${key(testId)}`;
    writeJson(`${running}/${mine}`, { mutant, testId });
    let beside = false;
    for (const entry of entries(running)) {
        if (entry === mine || entry.endsWith(".draft") || !alive(Number(entry.split("-")[0])))
            continue;
        const other = readJsonOr(`${running}/${entry}`, undefined);
        if (!other || other.mutant === mutant || (other.mutant === "-" && mutant === "-"))
            continue;
        beside = true;
        mark(`${pass.round}/beside/${invocation()}-${key(other.testId)}`);
    }
    return { running: `${running}/${mine}`, beside };
}

/**
 * What this copy of a test does: `undefined` outside strong mode, `null` to
 * skip, else its attempt: a control (no mutant, `-`) or the mutant it took
 * (`file#index`), and its time budget. A file whose tests the measured run
 * never ran side by side runs one at a time here too.
 */
export async function strongClaim(testInfo) {
    const pass = strongPass();
    if (!pass)
        return undefined;
    const started = Date.now();
    const shape = loadWorker(pass);
    // A project others start from runs as it is.
    if (shape.setupProjects.includes(testInfo.project.name))
        return undefined;
    const original = await originalTestId(pass, testInfo);
    const test = original && shape.tests[original];
    if (!test)
        return null;
    const configured = testInfo.timeout;
    // Waiting is not the test's time.
    testInfo.setTimeout(0);
    const unreliable = `${pass.round}/unreliable/${invocation()}/${key(original)}`;
    if (entries(unreliable).length >= CONTROL_FAILURES)
        return null;
    let attempt;
    if (!shape.everywhere && !passedHere.has(original)) {
        // The test's first run in this worker, or the first since it failed
        // here: without a mutant, the round's baseline or a kill's control.
        if (!workLeft(pass, original))
            return null;
        attempt = shape.baselines && baselineOf(pass, original) === undefined
            ? { kind: "baseline", id: null, mutant: "-" }
            : { kind: "control", id: null, mutant: "-" };
    }
    else {
        // A later round's test that failed without any mutant in the main one.
        if (baselineOf(pass, original) === "failed")
            return null;
        const id = takeJob(pass, original, shape.batch ? invocationMutant(pass) ?? null : undefined, budgetOf([test.durationMs]));
        if (!id)
            return null;
        const entry = mutantOf(pass, id);
        attempt = { kind: "mutant", id, mutant: entry.mutant, budgetMs: budgetOf([test.durationMs], entry.budgetFactor ?? 1) };
        mark(`${pass.round}/inflight/${key(id)}`);
        writeFileSync(`${pass.round}/inflight/${key(id)}/${process.pid}-${key(testInfo.testId)}`, String(Date.now()));
    }
    const lock = shape.concurrency.workers !== 1 && shape.concurrency.serialFiles.includes(test.file)
        ? await lockFile(pass, test.file)
        : undefined;
    // The test's time from here: a mutant's from the test's measured
    // duration and the time Playwright gives an assertion and an action to
    // fail in (a failing `expect` waits for its own timeout), a control's as
    // configured.
    if (attempt.kind === "mutant") {
        const expectTimeout = testInfo.project?.expect?.timeout ?? 5_000;
        const actionTimeout = testInfo.project?.use?.actionTimeout ?? 0;
        // Playwright itself ends the test at its configured timeout (the
        // project's, or its own `test.slow()` / `describe.configure`), and an
        // assertion may wait up to there with a timeout of its own: that is
        // the floor.
        const budget = Math.max(attempt.budgetMs + expectTimeout + actionTimeout, configured > 0 ? configured : 0);
        testInfo.setTimeout(Date.now() - started + budget);
    }
    else if (configured > 0)
        testInfo.setTimeout(Date.now() - started + configured);
    const { running, beside } = startRunning(pass, testInfo.testId, attempt.mutant);
    const claimed = {
        original,
        title: test.title,
        testId: testInfo.testId,
        invocation: invocation(),
        worker: process.pid,
        kind: attempt.kind,
        id: attempt.id,
        mutant: attempt.mutant,
        startedAt: Date.now(),
        lock,
        running,
        besideAtStart: beside,
    };
    writeJson(attemptPath(pass, invocation(), testInfo.testId), claimed);
    return claimed;
}

const TEST_TIMEOUT = /Test timeout of \d+ms exceeded/;

export function stripAnsi(text) {
    return String(text).replace(/\u001b\[[0-9;]*m/g, "");
}

/** An error's message, on one line, short enough to keep. */
export function firstWords(message) {
    const text = stripAnsi(message ?? "").replace(/\s+/g, " ").trim();
    return text.length > 300 ? `${text.slice(0, 299)}…` : text;
}

/**
 * How an attempt ended, from its errors: `passed`, `failed` (an assertion or
 * an action failed, or the test threw), `timed-out` (only its time ran out),
 * or `skipped`; and why it failed.
 */
export function outcomeOf(status, errors) {
    const timeoutOnly = errors.length > 0 && errors.every((error) => TEST_TIMEOUT.test(stripAnsi(error?.message ?? "")));
    const reported = errors.find((error) => !TEST_TIMEOUT.test(stripAnsi(error?.message ?? ""))) ?? errors[0];
    const message = reported ? firstWords(reported.message ?? reported.value) : undefined;
    if (status === "passed")
        return { outcome: "passed" };
    if (status === "skipped")
        return { outcome: "skipped" };
    if (status === "timedOut" && timeoutOnly)
        return { outcome: "timed-out", message };
    return { outcome: "failed", message };
}

/** Writes a mutant's verdict into the results as soon as it is known, once. */
export function giveVerdict(pass, record) {
    if (!mark(`${pass.shared}/verdicts/${key(record.id)}`))
        return false;
    appendFileSync(pass.resultsPath, `${JSON.stringify(record)}\n`);
    return true;
}

/** The candidate tests of a mutant that passed with it. */
function passedWith(pass, id) {
    return new Set(entries(`${pass.round}/passed/${key(id)}`));
}

/** A mutant every candidate test lets live, now that one more passed with it: its verdict. */
function surviveIfDone(pass, id) {
    if (settled(pass, id) || existsSync(`${pass.round}/confirm/${key(id)}`) || existsSync(`${pass.round}/doubt/${key(id)}`))
        return;
    const entry = mutantOf(pass, id);
    const passed = passedWith(pass, id);
    // Every candidate passed with it: one that failed without it is left to
    // the round's judge, once its controls are all known.
    if (!entry.tests.every((test) => passed.has(key(test))))
        return;
    const ran = entry.tests.length;
    const others = readJsonOr(`${pass.round}/pass.json`, { otherRunners: {} }).otherRunners?.[id];
    giveVerdict(pass, others
        ? { id, status: "error", reason: `no Playwright test killed it, and its other candidates ran under ${others.join(", ")}, which this pass does not run`, tests: ran, durationMs: 0 }
        : { id, status: "survived", ...(entry.reason ? { reason: reasonOf(loadWorker(pass).kind, entry.reason) } : {}), tests: ran, durationMs: 0 });
}

export const WHY = {
    "background": "its code ran in server work no request named",
    "module-level": "it is a server module's top-level code",
    "by-time": "its code ran for a request that named no test",
    "timeout-retry": "a test ran out of time with it switched on, and ran again alone with twice the time",
    "unfinished": "not every candidate test ran in the shared pass",
    "confirm": "a test killed it beside other mutants' attempts",
    "doubt": "the test that killed it failed without it too, on the same server",
};
export const EVERYWHERE = ["background", "module-level", "by-time"];

/** What a verdict from a later round says of it. */
export function reasonOf(kind, reason) {
    if (kind === "alone")
        return `ran alone${EVERYWHERE.includes(reason) ? ", with the server started with it switched on" : ""}: ${WHY[reason] ?? reason}`;
    if (kind === "serial")
        return `ran one test at a time: ${WHY[reason] ?? reason}`;
    return undefined;
}

/**
 * The attempt's end: its outcome kept, and what it settles. A control that
 * passes confirms the kills before it on this server; one that fails puts
 * them in doubt. A mutated attempt that fails kills its mutant (confirmed by
 * the test's next control, where the round has controls); one that passes
 * may be its mutant's last candidate. `details` says what the fixture saw:
 * the attempt's id and whether its page said the mutant ran away.
 */
export function strongFinish(testInfo, claimed, details) {
    const pass = strongPass();
    if (!pass || !claimed)
        return;
    const { outcome, message } = outcomeOf(testInfo.status, testInfo.errors ?? []);
    settleAttempt(pass, { ...claimed, ...details, outcome, message });
}

/**
 * What an attempt that ended settles: the fixture's, or the reporter's for
 * an attempt whose worker ended before its fixture could (a mutant that
 * crashed it).
 */
export function settleAttempt(pass, attempt) {
    rmSync(attempt.running, { force: true });
    if (attempt.lock)
        rmSync(attempt.lock, { recursive: true, force: true });
    const beside = attempt.besideAtStart || existsSync(`${pass.round}/beside/${attempt.invocation}-${key(attempt.testId)}`);
    const ended = { ...attempt, beside, endedAt: Date.now() };
    writeJson(attemptPath(pass, attempt.invocation, attempt.testId), ended);
    const test = attempt.original;
    const shape = loadWorker(pass);
    if (attempt.kind !== "mutant") {
        settleControl(pass, ended, shape);
        return ended;
    }
    rmSync(`${pass.round}/inflight/${key(attempt.id)}/${attempt.worker}-${key(attempt.testId)}`, { force: true });
    const entry = mutantOf(pass, attempt.id);
    const verdict = (status, reason) => ({ id: attempt.id, status, killedBy: test, reason: `${reason}${suffix(shape, entry)}`, tests: 1, durationMs: ended.endedAt - ended.startedAt });
    if (attempt.pageTimeout === true) {
        mark(`${pass.round}/killed/${key(attempt.id)}`);
        giveVerdict(pass, verdict("timeout", "evaluated past its hit limit"));
        return ended;
    }
    if (ended.outcome === "passed") {
        passedHere.add(test);
        mark(`${pass.round}/passed/${key(attempt.id)}/${key(test)}`);
        // One mutant an invocation: a survivor waits for the judge, who
        // knows whether its server ran it.
        if (!shape.batch)
            surviveIfDone(pass, attempt.id);
        return ended;
    }
    if (ended.outcome === "timed-out") {
        // Only its time ran out: run again, alone, with twice the time.
        if (shared(shape.kind))
            mark(`${pass.round}/held/${key(attempt.id)}`);
        else if (giveVerdict(pass, verdict("timeout", "ran out of time")))
            mark(`${pass.round}/killed/${key(attempt.id)}`);
        return ended;
    }
    if (ended.outcome !== "failed")
        return ended;
    // A kill: no other copy takes the mutant now.
    passedHere.delete(test);
    mark(`${pass.round}/killed/${key(attempt.id)}`);
    if (beside)
        mark(`${pass.round}/confirm/${key(attempt.id)}`);
    else if (shape.everywhere)
        giveVerdict(pass, verdict("killed", ended.message ?? "the test failed"));
    else {
        // Confirmed by the test's next control on this server.
        writeJson(`${pass.round}/pending/${attempt.invocation}/${key(test)}/${key(attempt.id)}`, { id: attempt.id, test, message: ended.message, endedAt: ended.endedAt, durationMs: ended.endedAt - ended.startedAt });
    }
    return ended;
}

function suffix(shape, entry) {
    const said = reasonOf(shape.kind, entry.reason);
    return said ? `; ${said}` : "";
}

/**
 * A run with no mutant: the test's baseline (the first in a main round), or
 * a control after a kill. Passing, it lets the test take mutants in this
 * worker and confirms the kills before it on this server; failing, it puts
 * them in doubt, and a test that fails so twice in an invocation is not run
 * there again.
 */
function settleControl(pass, ended, shape) {
    const test = ended.original;
    const passed = ended.outcome === "passed";
    if (ended.kind === "baseline" || shape.baselines) {
        // A test passes without a mutant once a control passed; it fails
        // without one where none did.
        if (passed)
            writeJson(`${pass.shared}/baseline/${key(test)}`, "passed");
        else if (ended.outcome !== "skipped" && baselineOf(pass, test) === undefined)
            writeJson(`${pass.shared}/baseline/${key(test)}`, "failed");
    }
    const pending = `${pass.round}/pending/${ended.invocation}/${key(test)}`;
    for (const name of entries(pending)) {
        const kill = readJsonOr(`${pending}/${name}`, undefined);
        // A kill that ended after this control started waits for the next.
        if (!kill || kill.endedAt > ended.startedAt)
            continue;
        rmSync(`${pending}/${name}`, { force: true });
        if (passed && !ended.beside) {
            const entry = mutantOf(pass, kill.id);
            giveVerdict(pass, { id: kill.id, status: "killed", killedBy: kill.test, reason: `${kill.message ?? "the test failed"}${suffix(shape, entry)}`, tests: 1, durationMs: kill.durationMs });
        }
        else
            mark(`${pass.round}/doubt/${key(kill.id)}`);
    }
    if (passed) {
        passedHere.add(test);
        return;
    }
    if (ended.outcome === "skipped")
        return;
    passedHere.delete(test);
    mark(`${pass.round}/unreliable/${ended.invocation}/${key(test)}`);
    writeFileSync(`${pass.round}/unreliable/${ended.invocation}/${key(test)}/${ended.worker}-${Date.now()}`, "");
}

/** Whether a round runs mutants side by side on a shared server: the main round, and the rest of what it left. */
export function shared(kind) {
    return kind === "main" || kind === "rest";
}

/**
 * Whether the attempt's mutant ran in this worker's own process: code the
 * test itself runs, in a process the strong pass otherwise keeps. Such a
 * worker is not used again.
 */
export function ranInWorker(claimed, attemptId) {
    if (!claimed || claimed.kind !== "mutant")
        return false;
    const strong = globalThis[Symbol.for("supercov.strongMutation")];
    return Boolean(strong?.requestHits?.get?.(`${attemptId}\0${claimed.mutant}`));
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
