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
import { appendFileSync, existsSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { createHash } from "node:crypto";
import { availableParallelism } from "node:os";
import { relative, sep } from "node:path";

const TIMEOUT_FACTOR = 1.5;
const TIMEOUT_CONSTANT_MS = 5_000;
const HIT_LIMIT_FLOOR = 1_000;

function toRelative(root, file) {
    return relative(root, file).split(sep).join("/");
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
    // A mutant's switch, by its id in the copy's list: the index the copy
    // gave it, or why it has none.
    const idOf = (file, entry) => entry.id ||
        `${file}:${entry.start?.[0]}:${entry.start?.[1]}-${entry.end?.[0]}:${entry.end?.[1]}:${entry.mutator}:${entry.replacement}`;
    const indexes = new Map();
    for (const [file, entries] of Object.entries(listed)) {
        entries.forEach((entry, index) => indexes.set(idOf(file, entry), { index, entry }));
    }
    const placed = (mutant) => {
        const found = indexes.get(mutant.id);
        if (!found)
            return { error: "the copy lists no such mutant: the source changed since the run" };
        if (found.entry.unplaceable)
            return { error: `no switch could be placed (${found.entry.unplaceable})` };
        return { index: found.index };
    };

    // Each mutant to run: its candidates grouped by test file, the cheapest
    // file first.
    const pending = [];
    for (let mutant of plan.mutants) {
        if (mutant.decided)
            continue;
        const switchOf = placed(mutant);
        if (switchOf.error) {
            write({ id: mutant.id, status: "error", reason: switchOf.error, tests: 0, durationMs: 0 });
            continue;
        }
        mutant = { ...mutant, index: switchOf.index };
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

    // Rounds run through claims: each assignment is a file in the round's
    // directory, and each copy of a test file Vitest runs takes the next one
    // for its file (vitestStrong.mjs) and reports its tests' outcomes beside it.
    const roundsDirectory = `${root}/.supercov/strong-rounds`;
    rmSync(roundsDirectory, { recursive: true, force: true });
    let roundNumber = 0;
    async function runRound(assignments, timeoutMs) {
        const directory = `${roundsDirectory}/${roundNumber++}`;
        const counts = new Map();
        const placedAt = [];
        for (const assignment of assignments) {
            const key = createHash("sha256").update(assignment.file).digest("hex").slice(0, 24);
            const index = counts.get(key) ?? 0;
            counts.set(key, index + 1);
            mkdirSync(`${directory}/${key}`, { recursive: true });
            writeFileSync(`${directory}/${key}/${index}.json`, JSON.stringify(assignment.claim));
            placedAt.push(`${directory}/${key}/${index}.out`);
        }
        provide(vitest, { root, round: directory });
        setTestTimeout(vitest, Math.ceil(timeoutMs));
        last = await runFiles(assignments.map((assignment) => specOf.get(assignment.file)), false);
        // The outcome of each claim, whichever copy took it.
        return placedAt.map((out) => {
            const outcome = { claimed: false, done: false, tests: [] };
            if (!existsSync(out))
                return outcome;
            for (const line of readFileSync(out, "utf8").split("\n")) {
                if (!line.trim())
                    continue;
                const record = JSON.parse(line);
                if (record.claimed)
                    outcome.claimed = true;
                else if (record.done)
                    outcome.done = true;
                else if (record.test)
                    outcome.tests.push(record);
            }
            return outcome;
        });
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
        const assignments = [...union].map(([file, entry]) => ({
            file,
            claim: { mutant: null, tests: [...entry.tests], wholeFile: entry.whole },
        }));
        const outcomes = await runRound(assignments, 60_000);
        assignments.forEach((assignment, at) => {
            const outcome = outcomes[at];
            if (!outcome.done)
                baselineFailed.add(`${assignment.file}#file`);
            for (const test of outcome.tests) {
                if (test.failed)
                    baselineFailed.add(test.test);
            }
        });
    }

    const oneAtATime = !isolated(vitest);
    const workers = Math.max(1, Number(vitest.config.maxWorkers) || availableParallelism());
    // Copies of one test file in a round, and assignments in all: enough to
    // keep every worker busy while the slowest copy finishes.
    const perFile = oneAtATime ? 1 : workers;
    const perRound = oneAtATime ? 1 : workers * 2;
    const durationOf = new Map(tests.map((test) => [test.id, test.durationMs ?? 1_000]));
    while (pending.length) {
        const used = new Map();
        const round = [];
        for (const job of pending) {
            if (round.length >= perRound)
                break;
            const group = job.files[job.next];
            if ((used.get(group.file) ?? 0) >= perFile || baselineFailed.has(`${group.file}#file`) && !job.skipped)
                continue;
            used.set(group.file, (used.get(group.file) ?? 0) + 1);
            round.push(job);
        }
        // A job whose next file cannot run (it failed without any mutant)
        // moves on, or ends.
        for (const job of pending) {
            const group = job.files[job.next];
            if (!round.includes(job) && baselineFailed.has(`${group.file}#file`)) {
                if (job.next + 1 < job.files.length)
                    job.next += 1;
                else {
                    write({ id: job.mutant.id, status: "error", reason: `${group.file} fails without any mutant`, tests: job.ran, durationMs: 0 });
                    job.done = true;
                }
            }
        }
        let limit = 0;
        const assignments = round.map((job) => {
            const group = job.files[job.next];
            const chosen = group.tests.filter((id) => !baselineFailed.has(id));
            const budget = Math.max(...chosen.map((id) => durationOf.get(id) ?? 1_000), 1_000);
            limit = Math.max(limit, (budget * TIMEOUT_FACTOR + TIMEOUT_CONSTANT_MS) * (job.retried ? 2 : 1));
            if (!job.started)
                job.started = Date.now();
            return {
                file: group.file,
                claim: {
                    mutant: { file: job.mutant.file, index: job.mutant.index },
                    tests: chosen,
                    wholeFile: group.whole,
                    hitLimit: Math.max(HIT_LIMIT_FLOOR, 1_000_000),
                },
            };
        });
        const outcomes = round.length ? await runRound(assignments, limit) : [];
        round.forEach((job, at) => {
            const group = job.files[job.next];
            const outcome = outcomes[at];
            if (!outcome.claimed) {
                // No copy took it: run it again in a later round.
                job.unclaimed = (job.unclaimed ?? 0) + 1;
                if (job.unclaimed > 2) {
                    write({ id: job.mutant.id, status: "error", reason: "no copy of its test file ran it", tests: job.ran, durationMs: Date.now() - job.started });
                    job.done = true;
                }
                return;
            }
            job.ran += outcome.tests.length;
            const failed = outcome.tests.find((test) => test.failed && !baselineFailed.has(test.test));
            let verdict;
            if (failed) {
                if (failed.timeout && !job.retried) {
                    job.retried = true;
                    return;
                }
                verdict = { status: failed.timeout ? "timeout" : "killed", killedBy: failed.test };
            }
            else if (!outcome.done) {
                // The copy started and its file did not finish: the mutant
                // broke it while it loaded or ran.
                verdict = { status: "killed", killedBy: group.file, reason: "the test file did not finish" };
            }
            else if (job.next + 1 < job.files.length) {
                job.next += 1;
                job.retried = false;
                return;
            }
            else {
                verdict = { status: "survived" };
            }
            write({ id: job.mutant.id, ...verdict, tests: job.ran, durationMs: Date.now() - job.started });
            job.done = true;
        });
        for (let index = pending.length - 1; index >= 0; index--) {
            if (pending[index].done)
                pending.splice(index, 1);
        }
        if (!round.length && pending.length) {
            for (const job of pending)
                write({ id: job.mutant.id, status: "error", reason: "could not be scheduled", tests: job.ran, durationMs: 0 });
            break;
        }
    }
    rmSync(roundsDirectory, { recursive: true, force: true });
    write({ summary: true, durationMs: Date.now() - started, isolated: !oneAtATime });
    return last;
}
