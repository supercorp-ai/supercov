// Strong mutation's driver for Vitest (docs/design/strong-mutation.md).
//
// Vitest hands its reporters the running instance before it runs a file
// (`onInit`). In strong mode this reporter takes the instance's `runFiles`
// over: instead of one run of every file, it runs the plan in rounds through
// the same resident instance -- the project's own pool and workers, no new
// process -- and records each mutant's verdict.
//
// A round gives each test file at most one mutant. A file is evaluated in an
// isolated module graph, so different files of one round can each carry a
// different mutant without one reaching another. A mutant with candidates in
// several files runs one file per round, cheapest file first, and stops at
// its first kill. Where the project does not isolate files, a round holds
// one mutant.
import { appendFileSync, readFileSync } from "node:fs";
import { relative, sep } from "node:path";

const TIMEOUT_FACTOR = 1.5;
const TIMEOUT_CONSTANT_MS = 5_000;
const HIT_LIMIT_FLOOR = 1_000;

function toRelative(root, file) {
    return relative(root, file).split(sep).join("/");
}

function collectTests(task, out = []) {
    if (task.type === "test" || task.type === "custom")
        out.push(task);
    for (const child of task.tasks ?? [])
        collectTests(child, out);
    return out;
}

function isolated(vitest) {
    for (const project of vitest.projects ?? [vitest.getCoreWorkspaceProject?.()]) {
        const config = project?.config;
        if (!config)
            continue;
        const pool = config.pool ?? "forks";
        const options = config.poolOptions?.[pool] ?? {};
        if (options.isolate === false || config.isolate === false)
            return false;
        if (options.singleThread || options.singleFork)
            return false;
    }
    return true;
}

function provide(vitest, value) {
    for (const project of vitest.projects ?? []) {
        project.provide?.("supercovStrong", value);
    }
    vitest.provide?.("supercovStrong", value);
}

function setTestTimeout(vitest, ms) {
    for (const project of vitest.projects ?? []) {
        if (project.config)
            project.config.testTimeout = ms;
    }
    if (vitest.config)
        vitest.config.testTimeout = ms;
}

/** Why a failed test failed: a mutant timeout, a time limit, or a test. */
function failureKind(task) {
    const errors = task.result?.errors ?? [];
    for (const error of errors) {
        const text = `${error?.name ?? ""} ${error?.message ?? ""}`;
        if (/SupercovMutantTimeout/.test(text))
            return "timeout";
        if (/Test timed out in \d+ms/.test(text))
            return "timeout";
    }
    return "killed";
}

export default class SupercovStrongReporter {
    constructor(configureProjects) {
        this.configureProjects = configureProjects;
    }
    onInit(vitest) {
        this.configureProjects?.(vitest.projects);
        const planPath = process.env["SUPERCOV_STRONG_PLAN"];
        const resultsPath = process.env["SUPERCOV_STRONG_RESULTS"];
        if (!planPath || !resultsPath)
            return;
        const original = vitest.runFiles.bind(vitest);
        vitest.runFiles = async (specs) => {
            const last = await drive(vitest, original, specs, JSON.parse(readFileSync(planPath, "utf8")), resultsPath);
            // A killed mutant fails its test, and that is the point: the
            // command's exit code is the pass's, not the rounds'.
            process.exitCode = 0;
            // Vitest 3 and later read what the run gave back.
            return last ?? { testModules: [], unhandledErrors: [] };
        };
    }
}

async function drive(vitest, runFiles, specs, plan, resultsPath) {
    const root = vitest.config.root;
    const specOf = new Map(specs.map((spec) => [toRelative(root, spec.moduleId), spec]));
    const write = (record) => appendFileSync(resultsPath, `${JSON.stringify(record)}\n`);
    const tests = plan.tests;
    const started = Date.now();
    let last;

    // The copy's own list: the mutant at each index must be the run's, and
    // one the instrumenter could not place has no switch to turn on.
    let listed = {};
    try {
        listed = JSON.parse(readFileSync(`${root}/.supercov/frontend-evidence/mutants.json`, "utf8")).files ?? {};
    }
    catch {
        listed = {};
    }
    const placed = (mutant) => {
        const entry = listed[mutant.file]?.[mutant.index];
        if (!entry)
            return "the copy lists no such mutant: the source changed since the run";
        const [, , start, end] = /:(\d+:\d+)-(\d+:\d+):/.exec(mutant.id) ?? [];
        const same = entry.mutator === mutant.mutator &&
            (!start || `${entry.start?.[0]}:${entry.start?.[1]}` === start.replace(/^(\d+):/, "$1:")) ;
        if (!same)
            return "the copy's mutant at this index is another: the source changed since the run";
        if (entry.unplaceable)
            return `no switch could be placed (${entry.unplaceable})`;
        return undefined;
    };

    // Each mutant to run: its candidates grouped by test file, the cheapest
    // file first.
    const pending = [];
    for (const mutant of plan.mutants) {
        if (mutant.decided)
            continue;
        const unplaced = placed(mutant);
        if (unplaced) {
            write({ id: mutant.id, status: "error", reason: unplaced, tests: 0, durationMs: 0 });
            continue;
        }
        const byFile = new Map();
        for (const candidate of mutant.candidates) {
            const test = tests[candidate.test];
            if (!test?.file || !specOf.has(test.file))
                continue;
            if (!byFile.has(test.file))
                byFile.set(test.file, { file: test.file, tests: [], cost: 0, whole: false });
            const group = byFile.get(test.file);
            group.tests.push(test.id);
            group.cost += test.durationMs ?? 1_000;
        }
        for (const file of mutant.wholeFiles ?? []) {
            if (!specOf.has(file))
                continue;
            if (!byFile.has(file))
                byFile.set(file, { file, tests: [], cost: 0, whole: true });
            byFile.get(file).whole = true;
        }
        const files = [...byFile.values()].sort((a, b) => a.cost - b.cost);
        if (!files.length) {
            write({ id: mutant.id, status: "survived", reason: "no-runnable-candidate", tests: 0, durationMs: 0 });
            continue;
        }
        pending.push({ mutant, files, next: 0, ran: 0, retried: false, started: 0 });
    }

    // A baseline: each file with the union of its candidates, no mutant.
    // A test that fails there cannot count as a kill.
    const union = new Map();
    for (const job of pending) {
        for (const group of job.files) {
            const entry = union.get(group.file) ?? { tests: new Set(), whole: false };
            group.tests.forEach((id) => entry.tests.add(id));
            entry.whole ||= group.whole;
            union.set(group.file, entry);
        }
    }
    const baselineFailed = new Set();
    if (union.size) {
        const files = {};
        for (const [file, entry] of union)
            files[file] = { tests: [...entry.tests], wholeFile: entry.whole };
        provide(vitest, { root, files });
        setTestTimeout(vitest, 60_000);
        last = await runFiles([...union.keys()].map((file) => specOf.get(file)), false);
        for (const fileTask of vitest.state.getFiles([...union.keys()].map((file) => specOf.get(file).moduleId))) {
            const file = toRelative(root, fileTask.filepath);
            if (fileTask.result?.state === "fail" && !collectTests(fileTask).some((t) => t.result?.state === "fail"))
                baselineFailed.add(`${file}#file`);
            for (const test of collectTests(fileTask)) {
                if (test.result?.state === "fail")
                    baselineFailed.add(`vitest:${test.id}`);
            }
        }
    }

    const oneAtATime = !isolated(vitest);
    const durationOf = new Map(tests.map((test) => [test.id, test.durationMs ?? 1_000]));
    while (pending.length) {
        // Pack a round: each file at most once.
        const used = new Set();
        const round = [];
        for (const job of pending) {
            if (oneAtATime && round.length)
                break;
            const group = job.files[job.next];
            if (used.has(group.file))
                continue;
            used.add(group.file);
            round.push(job);
        }
        const files = {};
        let limit = 0;
        for (const job of round) {
            const group = job.files[job.next];
            const chosen = group.tests.filter((id) => !baselineFailed.has(id));
            const budget = Math.max(...chosen.map((id) => durationOf.get(id) ?? 1_000), 1_000);
            const factor = job.retried ? 2 : 1;
            limit = Math.max(limit, (budget * TIMEOUT_FACTOR + TIMEOUT_CONSTANT_MS) * factor);
            files[group.file] = {
                mutant: { file: job.mutant.file, index: job.mutant.index },
                tests: chosen,
                wholeFile: group.whole,
                hitLimit: Math.max(HIT_LIMIT_FLOOR, 1_000_000),
            };
            if (!job.started)
                job.started = Date.now();
        }
        provide(vitest, { root, files });
        setTestTimeout(vitest, Math.ceil(limit));
        const roundSpecs = Object.keys(files).map((file) => specOf.get(file));
        last = await runFiles(roundSpecs, false);
        const results = new Map(vitest.state
            .getFiles(roundSpecs.map((spec) => spec.moduleId))
            .map((fileTask) => [toRelative(root, fileTask.filepath), fileTask]));
        for (const job of round) {
            const group = job.files[job.next];
            const fileTask = results.get(group.file);
            const ran = fileTask ? collectTests(fileTask).filter((t) => t.result?.state && t.result.state !== "skip") : [];
            job.ran += ran.length;
            const failed = ran.find((t) => t.result?.state === "fail" && !baselineFailed.has(`vitest:${t.id}`));
            const loadFailed = fileTask?.result?.state === "fail" && !ran.length && !baselineFailed.has(`${group.file}#file`);
            let verdict;
            if (failed) {
                const kind = failureKind(failed);
                if (kind === "timeout" && !job.retried) {
                    job.retried = true;
                    continue;
                }
                verdict = { status: kind, killedBy: `vitest:${failed.id}` };
            }
            else if (loadFailed) {
                verdict = { status: "killed", killedBy: `${group.file}`, reason: "module-load" };
            }
            else if (job.next + 1 < job.files.length) {
                job.next += 1;
                job.retried = false;
                continue;
            }
            else {
                verdict = { status: "survived" };
            }
            write({ id: job.mutant.id, ...verdict, tests: job.ran, durationMs: Date.now() - job.started });
            job.done = true;
        }
        for (let index = pending.length - 1; index >= 0; index--) {
            if (pending[index].done)
                pending.splice(index, 1);
        }
    }
    write({ summary: true, durationMs: Date.now() - started, isolated: !oneAtATime });
    return last;
}
