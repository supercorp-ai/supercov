// The strong pass's driver for the runners whose processes are started with
// a mutant on (docs/design/strong-mutation.md): Jest, node:test, Mocha, AVA,
// node-tap, bun:test and Vitest Browser Mode. The engine starts it in place of
// the run's command, `node strongJobs.mjs -- <command>`, in the instrumented
// copy, with the plan (SUPERCOV_STRONG_PLAN) and where the verdicts go
// (SUPERCOV_STRONG_RESULTS).
//
// Each job is one start of the command for one test file: the mutant on in
// every process from its start (SUPERCOV_ACTIVE_MUTANT), only the job's tests
// run (SUPERCOV_STRONG_JOB, read by the runner's adapter through
// strongJob.mjs), and the first failure stops it. The tests' outcomes are the
// adapter's own evidence records, in the job's own evidence directory. A
// fresh process per job is how these runners isolate a test file anyway: no
// mutant's module state, timers or process-wide changes reach another's.
//
// For each mutant: its candidate tests by test file, the cheapest first; the
// next file only while it lives. Each file runs first without a mutant (its
// baseline), with every test a mutant will run there; a test that fails there
// is no kill, and a file whose baseline fails or leaves a test out (a subtest
// whose parent was not chosen) runs whole from then on. A kill by a job that
// ran other tests than its baseline did is confirmed by running the job's
// tests again without the mutant. A job past 3x its baseline and 10 s is
// stopped: a timeout, unless a test had already failed.
import { fork, spawn } from "node:child_process";
import { appendFileSync, existsSync, mkdirSync, readFileSync, readdirSync, writeFileSync } from "node:fs";
import { availableParallelism, cpus } from "node:os";
import { basename, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

// A file whose baseline job took at most this long, the start of the command
// included, runs all its baseline's tests beside a mutant's own candidates
// rather than confirm a kill apart (`tryFile`).
const UNION_MS = 1_500;
const RUNNERS = new Set(["jest", "mocha", "node:test", "ava", "tap", "bun:test", "vitest-browser"]);

function parallelism() {
    const given = Number(process.env.SUPERCOV_STRONG_WORKERS);
    if (Number.isSafeInteger(given) && given > 0)
        return given;
    const available = typeof availableParallelism === "function" ? availableParallelism() : cpus().length;
    return Math.max(1, available - 1);
}

// At most `size` jobs at once, first come first served.
function slots(size) {
    let free = size;
    const waiting = [];
    return async function run(work) {
        if (free > 0)
            free -= 1;
        else
            await new Promise((resolve) => waiting.push(resolve));
        try {
            return await work();
        }
        finally {
            const next = waiting.shift();
            if (next)
                next();
            else
                free += 1;
        }
    };
}

// Every file under `directory`, recursively.
function filesUnder(directory) {
    let entries;
    try {
        entries = readdirSync(directory, { withFileTypes: true });
    }
    catch {
        return [];
    }
    return entries.flatMap((entry) => entry.isDirectory()
        ? filesUnder(join(directory, entry.name))
        : [join(directory, entry.name)]);
}

// Each test's outcome in a job's evidence: the last attempt's, where any
// record says more than `unknown` (Jest writes coverage and outcome apart),
// in the order the tests ended.
export function outcomes(directory) {
    const records = [];
    for (const path of filesUnder(directory)) {
        let text;
        try {
            text = readFileSync(path, "utf8");
        }
        catch {
            continue;
        }
        const lines = path.endsWith(".jsonl") ? text.split("\n") : path.endsWith(".json") ? [text] : [];
        for (const line of lines) {
            if (!line.trim())
                continue;
            try {
                records.push(JSON.parse(line));
            }
            catch {
                // A line cut short by a stopped process says nothing.
            }
        }
    }
    const tests = new Map();
    for (const record of records) {
        if (typeof record?.testId !== "string" || (record.role && record.role !== "test"))
            continue;
        const seen = tests.get(record.testId);
        const retry = record.retry ?? 0;
        const status = record.status ?? "unknown";
        if (!seen) {
            tests.set(record.testId, { status, retry });
            continue;
        }
        if (status === "unknown" && seen.status !== "unknown")
            continue;
        if (retry >= seen.retry || seen.status === "unknown")
            tests.set(record.testId, { status, retry: Math.max(retry, seen.retry) });
    }
    return new Map([...tests].map(([id, { status }]) => [id, status]));
}

// The command for one job: Bun's runner takes the file as an argument, as it
// loads every test file into one process; the other runners' adapters keep
// their runner from loading another file.
export function commandFor(command, file, root = process.cwd(), runner = undefined) {
    const [program, first] = command;
    // A tap test file is a program of its own, as tap's runner runs it (and
    // StrykerJS's tap runner): run by itself, beside other jobs, it shares
    // none of the working files tap's command keeps in the project.
    if (runner === "tap" && /\.[cm]?js$/.test(file))
        return [process.execPath, file];
    if (program && /^bun(?:\.exe)?$/.test(basename(program)) && first === "test")
        return [...command, `./${file}`];
    return withoutNpx(command, root);
}

// `npx <tool> ...` where the project has the tool: the tool itself. npx
// costs a start of npm's CLI before each job's, a third of a small suite's
// job.
export function withoutNpx(command, root) {
    const [program, ...rest] = command;
    if (!program || !/^npx(?:\.cmd)?$/.test(basename(program)) || process.platform === "win32")
        return command;
    const at = rest.findIndex((word) => !/^(?:-y|--yes|--no-install|--offline)$/.test(word));
    const tool = rest[at];
    if (at < 0 || !tool || tool.startsWith("-") || tool.includes("@") || tool.includes("/"))
        return command;
    const binary = join(root, "node_modules", ".bin", tool);
    return existsSync(binary) ? [binary, ...rest.slice(at + 1)] : command;
}

// The arguments a command gives Jest, where it starts Jest itself (`jest`,
// `npx jest`, `node_modules/.bin/jest`): a resident Jest parses them as
// Jest's CLI would.
export function jestArguments(command) {
    const at = command.findIndex((word) => /(?:^|[\\/])jest(?:\.js)?$/.test(word) || /jest-cli[\\/]bin[\\/]jest\.js$/.test(word));
    return at < 0 ? undefined : command.slice(at + 1);
}

// The arguments a command gives Mocha, where it starts Mocha itself.
export function mochaArguments(command) {
    const at = command.findIndex((word) => /(?:^|[\\/])_?mocha(?:\.js)?$/.test(word));
    return at < 0 ? undefined : command.slice(at + 1);
}

// Whether a project's .js files are CommonJS (its package.json has no
// `"type": "module"`).
export function packageIsCommonJs(root) {
    try {
        return JSON.parse(readFileSync(join(root, "package.json"), "utf8")).type !== "module";
    }
    catch {
        return true;
    }
}

// Resident runners (strongJestHost.mjs, strongMochaHost.mjs), each running one
// job at a time. A host whose job passes its limit is stopped, and the next
// job starts another.
function residentHosts({ script, args, root, environment, log }) {
    const idle = [];
    const all = new Set();
    const host = () => new Promise((resolveHost, rejectHost) => {
        const child = fork(fileURLToPath(new URL(`./${script}`, import.meta.url)), [], {
            cwd: root,
            env: environment,
            stdio: ["ignore", "pipe", "pipe", "ipc"],
            detached: process.platform !== "win32",
        });
        // Its output is drained, and kept for whoever reads why a job failed.
        const keep = (chunk) => {
            try {
                appendFileSync(log, chunk);
            }
            catch {
                // The log is for reading only.
            }
        };
        child.stdout.on("data", keep);
        child.stderr.on("data", keep);
        all.add(child);
        child.once("exit", () => all.delete(child));
        child.on("message", function started(message) {
            if (message?.type === "started")
                child.send({ type: "init", args: args ?? [] });
            else if (message?.type === "ready") {
                child.off("message", started);
                if (message.error) {
                    stop(child);
                    rejectHost(new Error(message.error));
                }
                else
                    resolveHost(child);
            }
        });
        child.once("error", rejectHost);
    });
    let number = 0;
    return {
        async run(job, limitMs) {
            let child;
            try {
                child = idle.pop() ?? await host();
            }
            catch (error) {
                try {
                    appendFileSync(log, `[supercov] the resident runner did not start: ${error?.stack ?? error}\n`);
                }
                catch {
                    // The log is for reading only.
                }
                return { timedOut: false, error, notStarted: true };
            }
            const id = ++number;
            return new Promise((resolveRun) => {
                let settled = false;
                const settle = (outcome) => {
                    if (settled)
                        return;
                    settled = true;
                    clearTimeout(timer);
                    child.off("message", answered);
                    child.off("exit", exited);
                    resolveRun(outcome);
                };
                const answered = (message) => {
                    if (message?.type === "done" && message.id === id) {
                        idle.push(child);
                        settle({ timedOut: false, error: message.error });
                    }
                };
                const exited = () => settle({ timedOut: false, error: "the resident runner ended" });
                const timer = setTimeout(() => {
                    stop(child);
                    settle({ timedOut: true });
                }, limitMs);
                child.on("message", answered);
                child.once("exit", exited);
                child.send({ type: "job", id, ...job });
            });
        },
        close() {
            for (const child of all)
                stop(child);
        },
    };
}

function readTimeouts(path) {
    try {
        return readFileSync(path, "utf8").split("\n").filter(Boolean).map((line) => {
            try {
                return JSON.parse(line).mutant;
            }
            catch {
                return undefined;
            }
        });
    }
    catch {
        return [];
    }
}

function stop(child) {
    if (child.exitCode !== null || child.signalCode !== null)
        return;
    try {
        if (process.platform === "win32")
            spawn("taskkill", ["/pid", String(child.pid), "/T", "/F"], { stdio: "ignore" });
        else
            process.kill(-child.pid, "SIGKILL");
    }
    catch {
        try {
            child.kill("SIGKILL");
        }
        catch {
            // Already gone.
        }
    }
}

export async function drive({ command, plan, resultsPath, root = process.cwd(), workers = parallelism(), environment = process.env, graceMs = 10_000, baselineGraceMs = 60_000 }) {
    const started = Date.now();
    const write = (record) => appendFileSync(resultsPath, `${JSON.stringify(record)}\n`);
    const work = `${resultsPath}.jobs`;
    mkdirSync(work, { recursive: true });
    const tests = plan.tests;
    const byId = new Map(tests.map((test) => [test.id, test]));
    const handled = (test) => RUNNERS.has(test?.runner);
    // Every test file of the run under these runners: a job's adapter runs
    // none of their tests but the job's.
    const known = [...new Set(tests.filter(handled).map((test) => test.file).filter(Boolean))].sort();
    const testsOf = new Map();
    const runnerOf = new Map(tests.filter(handled).map((test) => [test.file, test.runner]));
    for (const test of tests) {
        if (!handled(test) || !test.file)
            continue;
        if (!testsOf.has(test.file))
            testsOf.set(test.file, []);
        testsOf.get(test.file).push(test);
    }

    // The copy's own list: the mutant at each index must be the run's, and
    // one the instrumenter could not place has no switch to turn on.
    let listed = {};
    try {
        listed = JSON.parse(readFileSync(join(root, ".supercov/frontend-evidence/mutants.json"), "utf8")).files ?? {};
    }
    catch {
        listed = {};
    }
    const idOf = (file, entry) => entry.id ||
        `${file}:${entry.start?.[0]}:${entry.start?.[1]}-${entry.end?.[0]}:${entry.end?.[1]}:${entry.mutator}:${entry.replacement}`;
    const indexes = new Map();
    for (const [file, entries] of Object.entries(listed))
        entries.forEach((entry, index) => indexes.set(idOf(file, entry), { index, entry }));

    const run = slots(workers);
    // A command that starts Jest or Mocha itself: its jobs run in resident
    // runners, one per worker, rather than start the command each.
    const residentAllowed = environment.SUPERCOV_STRONG_RESIDENT !== "0";
    const jestArgs = residentAllowed ? jestArguments(withoutNpx(command, root)) : undefined;
    const mochaArgs = residentAllowed ? mochaArguments(withoutNpx(command, root)) : undefined;
    const jestHosts = residentHosts({ script: "strongJestHost.mjs", args: jestArgs, root, environment, log: join(work, "jest-hosts.log") });
    const mochaHosts = residentHosts({ script: "strongMochaHost.mjs", args: mochaArgs, root, environment, log: join(work, "mocha-hosts.log") });
    const commonJs = packageIsCommonJs(root);
    const broken = new Set();
    const residentFor = (file) => {
        const runner = runnerOf.get(file);
        if (broken.has(runner))
            return undefined;
        if (runner === "jest" && jestArgs)
            return jestHosts;
        // A Mocha suite of ES modules, which cannot be loaded again in one
        // process, starts the command per job.
        if (runner === "mocha" && mochaArgs && (/\.cjs$/.test(file) || (commonJs && /\.js$/.test(file))))
            return mochaHosts;
        return undefined;
    };
    const profile = { jobs: 0, baselines: 0, confirmations: 0, overturned: 0, stopped: 0, jobMs: 0 };
    let jobNumber = 0;

    // One start of the command: `tests` of `file` (null: all of them), with
    // `mutant` on (null: none), stopped at `limitMs`.
    function job({ mutant, file, tests: chosen, bail, limitMs }) {
        return run(() => new Promise((resolveJob) => {
            const number = ++jobNumber;
            profile.jobs += 1;
            const directory = join(work, String(number));
            const evidence = join(directory, "evidence");
            mkdirSync(evidence, { recursive: true });
            const spec = join(directory, "job.json");
            writeFileSync(spec, JSON.stringify({ files: { [file]: chosen === null ? null : [...chosen] }, known, bail, mutant: mutant ? `${mutant.file}#${mutant.index}` : null }));
            const results = join(directory, "results.jsonl");
            const log = join(directory, "output.log");
            const begun = Date.now();
            const resident = residentFor(file);
            // A runner's own command, started for this job alone.
            const spawnJob = () => {
                const [program, ...args] = commandFor(command, file, root, runnerOf.get(file));
                let output;
                try {
                    output = [];
                    const overrides = {
                        // What tap's runner gives each test file it starts.
                        ...(runnerOf.get(file) === "tap" ? { TAP_CHILD_ID: String(number) } : {}),
                        SUPERCOV_EVIDENCE_DIR: evidence,
                        SUPERCOV_STRONG_JOB: spec,
                        SUPERCOV_ACTIVE_MUTANT: mutant ? `${mutant.file}#${mutant.index}` : "-",
                        SUPERCOV_STRONG_RESULTS: results,
                        // Only this driver reads the plan: Vitest's and
                        // Playwright's configurations would drive their own.
                        SUPERCOV_STRONG_PLAN: "",
                    };
                    // Supercov's preload, in this process too, gives every child
                    // this process's own Supercov settings over the ones it is
                    // given: the job's are this process's while it starts.
                    const saved = Object.fromEntries(Object.keys(overrides).map((name) => [name, process.env[name]]));
                    Object.assign(process.env, overrides);
                    let child;
                    try {
                        child = spawn(program, args, {
                            cwd: root,
                            env: { ...environment, ...overrides },
                            stdio: ["ignore", "pipe", "pipe"],
                            detached: process.platform !== "win32",
                            shell: process.platform === "win32",
                            windowsHide: true,
                        });
                    }
                    finally {
                        for (const [name, value] of Object.entries(saved)) {
                            if (value === undefined)
                                delete process.env[name];
                            else
                                process.env[name] = value;
                        }
                    }
                    let size = 0;
                    const keep = (chunk) => {
                        // A job's output is kept for whoever reads why it failed,
                        // up to a megabyte.
                        if (size < 1 << 20) {
                            output.push(chunk);
                            size += chunk.length;
                        }
                    };
                    child.stdout.on("data", keep);
                    child.stderr.on("data", keep);
                    let timedOut = false;
                    const timer = setTimeout(() => {
                        timedOut = true;
                        profile.stopped += 1;
                        stop(child);
                    }, limitMs);
                    const finish = (code, error) => {
                        clearTimeout(timer);
                        profile.jobMs += Date.now() - begun;
                        try {
                            writeFileSync(log, Buffer.concat(output));
                        }
                        catch {
                            // The log is for reading; a verdict does not need it.
                        }
                        const mutantKey = mutant ? `${mutant.file}#${mutant.index}` : undefined;
                        resolveJob({
                            statuses: outcomes(evidence),
                            timedOut,
                            mutantTimedOut: mutantKey !== undefined && readTimeouts(`${results}.timeouts.jsonl`).includes(mutantKey),
                            exitCode: code,
                            error,
                            durationMs: Date.now() - begun,
                            directory,
                        });
                    };
                    child.on("error", (error) => finish(null, error));
                    child.on("close", (code) => finish(code));
                }
                catch (error) {
                    resolveJob({ statuses: new Map(), timedOut: false, mutantTimedOut: false, exitCode: null, error, durationMs: 0, directory });
                }
            };
            if (!resident) {
                spawnJob();
                return;
            }
            // A resident runner runs the job (strongJestHost.mjs,
            // strongMochaHost.mjs); one that cannot start leaves its runner's
            // jobs to the command.
            const environmentOfJob = {
                SUPERCOV_EVIDENCE_DIR: evidence,
                SUPERCOV_STRONG_JOB: spec,
                SUPERCOV_STRONG_RESULTS: results,
                SUPERCOV_STRONG_PLAN: "",
            };
            resident.run({ file: join(root, file), mutant: mutant ? { file: mutant.file, index: mutant.index } : null, environment: environmentOfJob }, limitMs).then(({ timedOut, error, notStarted }) => {
                if (notStarted) {
                    broken.add(runnerOf.get(file));
                    spawnJob();
                    return;
                }
                if (timedOut)
                    profile.stopped += 1;
                profile.jobMs += Date.now() - begun;
                const mutantKey = mutant ? `${mutant.file}#${mutant.index}` : undefined;
                resolveJob({
                    statuses: outcomes(evidence),
                    timedOut,
                    mutantTimedOut: mutantKey !== undefined && readTimeouts(`${results}.timeouts.jsonl`).includes(mutantKey),
                    exitCode: error ? 1 : 0,
                    error,
                    durationMs: Date.now() - begun,
                    directory,
                });
            });
        }));
    }

    // What a file does without a mutant: once with every test a mutant will
    // run there, and, where that cannot stand for the file, once whole.
    const files = new Map();
    const fileState = (file) => {
        if (!files.has(file))
            files.set(file, { union: new Set(), wholeNeeded: false, subset: undefined, whole: undefined });
        return files.get(file);
    };
    const confirmations = new Map();
    const cost = (file, chosen) => (chosen ? [...chosen] : (testsOf.get(file) ?? []).map((test) => test.id))
        .reduce((sum, id) => sum + (byId.get(id)?.durationMs ?? 1_000), 0);
    const baselineLimit = (file, chosen) => 3 * cost(file, chosen) + baselineGraceMs;
    const passedAll = (result, chosen) => !result.timedOut && [...chosen].every((id) => result.statuses.get(id) === "passed");
    async function wholeBaseline(file) {
        const state = fileState(file);
        state.whole ??= (async () => {
            profile.baselines += 1;
            const result = await job({ mutant: null, file, tests: null, bail: false, limitMs: baselineLimit(file, null) });
            const passing = new Set([...result.statuses].filter(([, status]) => status === "passed").map(([id]) => id));
            return { ...result, passing, finished: !result.timedOut && passing.size > 0 };
        })();
        return state.whole;
    }
    // The baseline a mutant's job in `file` is judged by: the subset's where
    // it stood for the file, else the whole file's.
    async function baselineFor(file) {
        const state = fileState(file);
        if (!state.wholeNeeded && state.union.size > 0) {
            state.subset ??= (async () => {
                profile.baselines += 1;
                const chosen = new Set(state.union);
                const result = await job({ mutant: null, file, tests: chosen, bail: false, limitMs: baselineLimit(file, chosen) });
                return { ...result, chosen, finished: passedAll(result, chosen) };
            })();
            const subset = await state.subset;
            if (subset.finished)
                return subset;
            state.wholeNeeded = true;
        }
        return { ...(await wholeBaseline(file)), chosen: null };
    }
    // The tests a kill ran, again without the mutant: whether the killing
    // test fails without it too.
    async function failsWithout(file, chosen, killer, limitMs) {
        const key = `${file}\0${[...chosen].sort().join("\0")}`;
        if (!confirmations.has(key)) {
            profile.confirmations += 1;
            confirmations.set(key, job({ mutant: null, file, tests: chosen, bail: false, limitMs }));
        }
        const result = await confirmations.get(key);
        return result.timedOut || result.statuses.get(killer) !== "passed";
    }

    // A mutant against one file's tests: killed, timeout, or alive.
    async function tryFile(mutant, group) {
        let baseline = group.whole ? { ...(await wholeBaseline(group.file)), chosen: null } : await baselineFor(group.file);
        if (!baseline.finished)
            return { status: "error", reason: `${group.file} does not finish without a mutant, so it could not test this one`, tests: 0 };
        let whole = group.whole || baseline.chosen === null;
        for (;;) {
            // Where the baseline's tests take little time beside a start of
            // the command, the job runs them all: a kill is then judged by a
            // baseline that ran the same tests, and needs no confirmation.
            const cheap = !whole && baseline.chosen && baseline.durationMs <= UNION_MS;
            const chosen = whole ? null : new Set(cheap ? [...baseline.chosen, ...group.tests] : group.tests);
            const excluded = whole ? (testId) => !baseline.passing.has(testId) : () => false;
            const limitMs = 3 * baseline.durationMs + graceMs;
            // A test that fails without the mutant must not stop the job.
            const bail = !whole || [...baseline.statuses.values()].every((status) => status === "passed");
            const result = await job({ mutant, file: group.file, tests: chosen, bail, limitMs });
            const ran = [...result.statuses].filter(([id, status]) => status !== "skipped" && (chosen === null || chosen.has(id)));
            const killer = ran.find(([id, status]) => status === "failed" && !excluded(id))?.[0];
            if (result.mutantTimedOut)
                return { status: "timeout", reason: "the mutant's evaluations passed its limit", tests: ran.length };
            if (killer === undefined) {
                if (result.timedOut)
                    return { status: "timeout", reason: `${group.file} did not finish within its limit`, tests: ran.length };
                // A test the baseline ran that this job never reported: the
                // test file stopped short.
                const expected = whole ? [...baseline.passing] : [...chosen];
                if (expected.some((id) => !result.statuses.has(id)))
                    return { status: "killed", killedBy: group.file, reason: "the test file did not finish", tests: ran.length };
                return { status: "alive", tests: ran.length };
            }
            const name = byId.get(killer)?.name ?? killer;
            // Judged by a baseline that ran the same tests: certain.
            const same = whole || (baseline.chosen !== null && baseline.chosen.size === chosen.size && [...chosen].every((id) => baseline.chosen.has(id)));
            if (same || !(await failsWithout(group.file, chosen, killer, baselineLimit(group.file, chosen))))
                return { status: "killed", killedBy: name, tests: ran.length };
            // The killing test fails without the mutant too, with only these
            // tests before it: the mutant runs again with its file whole.
            profile.overturned += 1;
            baseline = { ...(await wholeBaseline(group.file)), chosen: null };
            if (!baseline.finished)
                return { status: "error", reason: `${group.file} does not finish without a mutant, so it could not test this one`, tests: 0 };
            whole = true;
        }
    }

    async function runMutant(mutant, groups, elsewhere) {
        const begun = Date.now();
        let ran = 0;
        for (const group of groups) {
            const verdict = await tryFile(mutant, group);
            ran += verdict.tests;
            if (verdict.status !== "alive") {
                write({ id: mutant.id, status: verdict.status, ...(verdict.killedBy ? { killedBy: verdict.killedBy } : {}), tests: ran, durationMs: Date.now() - begun, ...(verdict.reason ? { reason: verdict.reason } : {}) });
                return;
            }
        }
        // A mutant that candidates under another runner may still kill is
        // not a survivor this pass can tell.
        if (elsewhere.size > 0) {
            write({ id: mutant.id, status: "error", tests: ran, durationMs: Date.now() - begun, reason: `its other candidates ran under ${[...elsewhere].sort().join(", ")}, which this pass does not run` });
            return;
        }
        write({ id: mutant.id, status: "survived", tests: ran, durationMs: Date.now() - begun });
    }

    const pending = [];
    for (const planned of plan.mutants) {
        if (planned.decided)
            continue;
        const found = indexes.get(planned.id);
        if (!found) {
            write({ id: planned.id, status: "error", reason: "the copy lists no such mutant: the source changed since the run", tests: 0, durationMs: 0 });
            continue;
        }
        if (found.entry.unplaceable) {
            write({ id: planned.id, status: "error", reason: `no switch could be placed (${found.entry.unplaceable})`, tests: 0, durationMs: 0 });
            continue;
        }
        const mutant = { ...planned, index: found.index };
        const byFile = new Map();
        const elsewhere = new Set();
        for (const candidate of planned.candidates) {
            const test = tests[candidate.test];
            if (!handled(test) || !test.file) {
                elsewhere.add(test?.runner ?? "an unknown runner");
                continue;
            }
            if (!byFile.has(test.file))
                byFile.set(test.file, { file: test.file, tests: [], cost: 0, whole: false });
            const group = byFile.get(test.file);
            group.tests.push(test.id);
            group.cost += test.durationMs ?? 1_000;
        }
        for (const file of planned.wholeFiles ?? []) {
            if (!testsOf.has(file))
                continue;
            if (!byFile.has(file))
                byFile.set(file, { file, tests: [], cost: cost(file, null), whole: true });
            byFile.get(file).whole = true;
        }
        const groups = [...byFile.values()].sort((a, b) => a.cost - b.cost);
        if (!groups.length) {
            write(elsewhere.size
                ? { id: planned.id, status: "error", tests: 0, durationMs: 0, reason: `its candidates ran under ${[...elsewhere].sort().join(", ")}, which this pass does not run` }
                : { id: planned.id, status: "survived", reason: "no-runnable-candidate", tests: 0, durationMs: 0 });
            continue;
        }
        for (const group of groups) {
            const state = fileState(group.file);
            if (!group.whole)
                group.tests.forEach((id) => state.union.add(id));
        }
        pending.push([mutant, groups, elsewhere]);
    }
    // Every file's baseline runs the tests of all its mutants: none starts
    // before all are known.
    await Promise.all(pending.map((task) => runMutant(...task)));
    jestHosts.close();
    mochaHosts.close();
    write({ summary: true, driver: "jobs", workers, ...profile, durationMs: Date.now() - started });
    return 0;
}

// `node strongJobs.mjs -- <command>`
if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
    // Bun leaves the `--` out of process.argv.
    const at = process.argv.indexOf("--", 2);
    const command = process.argv.slice(at < 0 ? 2 : at + 1);
    const planPath = process.env.SUPERCOV_STRONG_PLAN;
    const resultsPath = process.env.SUPERCOV_STRONG_RESULTS;
    if (!command.length || !planPath || !resultsPath) {
        console.error("[supercov] strongJobs.mjs: a strong pass starts it with its plan, its results and -- <command>");
        process.exit(2);
    }
    const plan = JSON.parse(readFileSync(planPath, "utf8"));
    process.exitCode = await drive({ command, plan, resultsPath });
}
