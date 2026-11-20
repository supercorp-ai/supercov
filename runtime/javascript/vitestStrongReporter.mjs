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
    // Warm workers (below) reset each copy's modules themselves.
    if (vitest.__supercovWarm)
        return true;
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

// SUPERCOV_STRONG_WARM=1: copies run in worker processes Vitest keeps
// between test files (the pool's own `isolate: false`), and each copy's setup
// file resets the modules Vitest evaluated before its test file loads, as an
// isolated worker would start: a copy no longer pays for a new process and
// for the packages Node already loaded. Only the forks and threads pools keep
// workers this way.
function warmUp(vitest) {
    if (process.env["SUPERCOV_STRONG_WARM"] !== "1" || !process.env["SUPERCOV_STRONG_PLAN"])
        return;
    const configs = [vitest.config, ...(vitest.projects ?? []).map((project) => project.config)].filter(Boolean);
    const poolOf = (config) => config.pool ?? "forks";
    if (!configs.every((config) => ["forks", "threads"].includes(poolOf(config)) && !config.browser?.enabled))
        return;
    for (const config of configs) {
        const pool = poolOf(config);
        config.poolOptions ??= {};
        config.poolOptions[pool] = { ...config.poolOptions[pool], isolate: false };
        config.isolate = false;
    }
    vitest.__supercovWarm = true;
}

// SUPERCOV_STRONG_CPU_PROFILE=<file>: a CPU profile of this process -- the
// one that schedules the copies and serves its workers' modules -- for the
// whole pass, to see what holds the workers up.
async function profileMainProcess() {
    const path = process.env["SUPERCOV_STRONG_CPU_PROFILE"];
    if (!path)
        return async () => { };
    const { Session } = await import("node:inspector");
    const session = new Session();
    session.connect();
    const post = (method, params) => new Promise((resolve, reject) => session.post(method, params, (error, result) => error ? reject(error) : resolve(result)));
    await post("Profiler.enable");
    await post("Profiler.setSamplingInterval", { interval: 1000 });
    await post("Profiler.start");
    return async () => {
        const { profile } = await post("Profiler.stop");
        writeFileSync(path, JSON.stringify(profile));
        session.disconnect();
    };
}

export default class SupercovStrongReporter {
    constructor(configureProjects) {
        this.configureProjects = configureProjects;
    }
    onInit(vitest) {
        this.configureProjects?.(vitest.projects);
        warmUp(vitest);
        const planPath = process.env["SUPERCOV_STRONG_PLAN"];
        const resultsPath = process.env["SUPERCOV_STRONG_RESULTS"];
        if (!planPath || !resultsPath)
            return;
        const original = vitest.runFiles.bind(vitest);
        vitest.runFiles = async (specs) => {
            const stopProfile = await profileMainProcess();
            const last = await drive(vitest, original, specs, JSON.parse(readFileSync(planPath, "utf8")), resultsPath);
            await stopProfile();
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
    // Vitest 2 and later pass specifications; Vitest 1, [project, file].
    const fileOf = (spec) => spec.moduleId ?? (Array.isArray(spec) ? spec[1] : spec);
    const specOf = new Map(specs.map((spec) => [toRelative(root, fileOf(spec)), spec]));
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
    let stalled = false;
    // Where the pass's time goes: each round's wall time against what its
    // copies spent loading (round start to claim), in tests, and after them.
    const profile = { rounds: 0, wallMs: 0, copies: 0, mutantCopies: 0, checkCopies: 0, loadMs: 0, testMs: 0, copyMs: 0, tailMs: 0, testsRun: 0, timeline: [] };
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
        provide(vitest, { root, round: directory, warm: Boolean(vitest.__supercovWarm) });
        setTestTimeout(vitest, Math.ceil(timeoutMs));
        // A round whose workers do not end (a fork that will not
        // terminate) must not hold the pass: past its deadline, what it
        // finished is read and the rest is a timeout.
        let timer;
        const deadline = new Promise((resolve) => {
            timer = setTimeout(() => resolve("deadline"), timeoutMs * 2 + 30_000);
        });
        const roundStarted = Date.now();
        const ran = runFiles(assignments.map((assignment) => specOf.get(assignment.file)), false);
        const finished = await Promise.race([ran.then((result) => ({ result })), deadline]);
        clearTimeout(timer);
        if (finished === "deadline")
            stalled = true;
        else
            last = finished.result;
        const roundEnded = Date.now();
        profile.rounds += 1;
        profile.wallMs += roundEnded - roundStarted;
        let lastDone = roundStarted;
        // The outcome of each claim, whichever copy took it.
        const outcomesOf = placedAt.map((out) => {
            const outcome = { claimed: false, done: false, tests: [], outside: [] };
            if (!existsSync(out))
                return outcome;
            for (const line of readFileSync(out, "utf8").split("\n")) {
                if (!line.trim())
                    continue;
                const record = JSON.parse(line);
                if (record.claimed) {
                    outcome.claimed = true;
                    outcome.claimedAt = record.at;
                }
                else if (record.started)
                    outcome.startedAt = record.at;
                else if (record.done) {
                    outcome.done = true;
                    outcome.doneAt = record.at;
                }
                else if (record.test)
                    outcome.tests.push(record);
                else if (record.outside)
                    outcome.outside.push(record);
            }
            return outcome;
        }).map((outcome, at) => {
            if (outcome.claimedAt) {
                profile.copies += 1;
                if (assignments[at].claim.mutant)
                    profile.mutantCopies += 1;
                profile.loadMs += outcome.claimedAt - roundStarted;
                profile.testsRun += outcome.tests.length;
                for (const test of outcome.tests)
                    profile.testMs += test.ms ?? 0;
                if (outcome.doneAt) {
                    profile.copyMs += outcome.doneAt - outcome.claimedAt;
                    lastDone = Math.max(lastDone, outcome.doneAt);
                }
            }
            return outcome;
        }).map((outcome, at, all) => {
            if (at === all.length - 1)
                profile.tailMs += roundEnded - lastDone;
            return outcome;
        });
        // The first rounds' copies, each as [file, claimed, tests started, done] in ms from
        // the round's start, and the round's end.
        if (profile.timeline.length < 12)
            profile.timeline.push({
                endMs: roundEnded - roundStarted,
                copies: outcomesOf.map((outcome, at) => [
                    assignments[at].file.split("/").pop(),
                    outcome.claimedAt ? outcome.claimedAt - roundStarted : null,
                    outcome.startedAt ? outcome.startedAt - roundStarted : null,
                    outcome.doneAt ? outcome.doneAt - roundStarted : null,
                ]),
            });
        return outcomesOf;
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
    // What each file's baseline ran, in order, and the files it ran whole:
    // a kill by a test whose file's baseline ran the same tests before it
    // is certain; any other is confirmed without the mutant first.
    const baselineOrder = new Map();
    const wholeBaseline = new Set();
    const baseline = async (files) => {
        const assignments = [...files].map(([file, entry]) => ({
            file,
            claim: { mutant: null, tests: [...entry.tests], wholeFile: entry.whole },
        }));
        const outcomes = await runRound(assignments, 60_000);
        return assignments.map((assignment, at) => ({ assignment, outcome: outcomes[at] }));
    };
    const recordBaseline = ({ assignment, outcome }) => {
        baselineOrder.set(assignment.file, outcome.tests.map((test) => test.test));
        if (assignment.claim.wholeFile)
            wholeBaseline.add(assignment.file);
        if (outcome.outside.length)
            baselineFailed.add(`${assignment.file}#outside`);
        if (!outcome.done)
            baselineFailed.add(`${assignment.file}#file`);
        for (const test of outcome.tests)
            if (test.failed)
                baselineFailed.add(test.test);
    };
    if (union.size) {
        // A candidate that fails with only the candidates of its file
        // around it leans on a test before it: its file runs whole, and
        // only what fails then, with nothing mutated, is no kill.
        const widened = new Map();
        for (const result of await baseline(union)) {
            const failed = result.outcome.tests.some((test) => test.failed);
            if ((failed || !result.outcome.done) && !result.assignment.claim.wholeFile)
                widened.set(result.assignment.file, { tests: new Set(), whole: true });
            else
                recordBaseline(result);
        }
        if (widened.size) {
            for (const result of await baseline(widened))
                recordBaseline(result);
            for (const job of pending)
                for (const group of job.files)
                    if (widened.has(group.file))
                        group.whole = true;
        }
    }

    const oneAtATime = !isolated(vitest);
    const workers = Math.max(1, Number(vitest.config.maxWorkers) || availableParallelism());
    // Copies of one test file in a round, and assignments in all: enough to
    // keep every worker busy while the slowest copy finishes.
    // A round ends when its slowest copy does, and the workers wait: the
    // more a round holds, the less of that wait there is per assignment.
    // SUPERCOV_STRONG_ROUND sets how many a round holds, as a multiple of
    // the workers (default 2: larger rounds load the machine, and timing-
    // sensitive tests slow down with it).
    // Warm workers are recycled when a round ends: a larger round keeps
    // them longer.
    const roundFactor = Math.max(1, Number(process.env["SUPERCOV_STRONG_ROUND"]) || (vitest.__supercovWarm ? 6 : 2));
    const perFile = oneAtATime ? 1 : workers * Math.min(roundFactor, 4);
    const perRound = oneAtATime ? 1 : workers * roundFactor;
    const durationOf = new Map(tests.map((test) => [test.id, test.durationMs ?? 1_000]));
    // A mutant runs its cheapest file first; if that does not kill it, all
    // its other files at once: the first file kills most killed mutants,
    // and a survivor then takes one round more, not one per file.
    for (const job of pending) {
        job.waiting = [job.files[0]];
        job.later = job.files.slice(1);
        job.open = 0;
    }
    // Kills among the verdicts so far: how likely a copy is to need a
    // confirmation (withPredecessors below).
    let killedFirst = 0;
    let decidedFirst = 0;
    const finish = (job, verdict) => {
        write({ id: job.mutant.id, ...verdict, tests: job.ran, durationMs: Date.now() - (job.started || Date.now()) });
        job.done = true;
        decidedFirst += 1;
        if (verdict.status === "killed" || verdict.status === "timeout")
            killedFirst += 1;
    };
    // A copy runs only a mutant's candidates, and a test can lean on one
    // the copy left out (state an earlier test built): it fails then with
    // or without the mutant. A kill counts where the file's baseline ran
    // the same tests before the killing one (or the copy ran what the
    // baseline ran whole); any other kill is confirmed by running the same
    // tests without the mutant. Where they fail that way too, it is no
    // kill: the mutant runs again with its test file whole, and only what
    // fails there without the mutant as well is none.
    const confirmations = new Map();
    let overturned = 0;
    const confirmationKey = (file, chosen, whole) => `${file}\0${whole ? "*" : chosen.join("\0")}`;
    const certain = (group, chosen, evidence) => {
        if (group.whole)
            return wholeBaseline.has(group.file);
        const order = baselineOrder.get(group.file);
        if (!order)
            return false;
        if (evidence.test) {
            const at = order.indexOf(evidence.test);
            return at >= 0 && order.slice(0, at).every((id) => chosen.includes(id));
        }
        return order.length === chosen.length && order.every((id) => chosen.includes(id));
    };
    const confirm = (job, group, chosen, evidence, verdict) => {
        const key = confirmationKey(group.file, chosen, group.whole);
        if (!confirmations.has(key))
            confirmations.set(key, {
                file: group.file,
                claim: { mutant: null, tests: chosen, wholeFile: group.whole },
                tries: 0,
                result: undefined,
            });
        (job.confirming ??= []).push({ key, group, evidence, verdict });
    };
    // Each of a job's kills waiting on a confirmation, once it has run.
    const settle = (job) => {
        for (const waiting of [...job.confirming]) {
            const { key, group, evidence, verdict } = waiting;
            const result = confirmations.get(key).result;
            if (!result)
                continue;
            job.confirming.splice(job.confirming.indexOf(waiting), 1);
            const alsoWithout = evidence.test ? result.failed.has(evidence.test)
                : evidence.outside ? result.outside
                    : !result.done;
            if (!alsoWithout) {
                finish(job, verdict);
                return;
            }
            overturned += 1;
            if (!group.whole)
                job.waiting.push({ file: group.file, tests: [], cost: group.cost, whole: true });
            else if (evidence.test)
                baselineFailed.add(evidence.test);
        }
    };
    // A kill whose copy ran every test its file's baseline ran before the
    // killing one needs no confirmation. Where those tests cost less than
    // the confirmation a kill would likely need (a copy of its own: loading
    // the file, then the tests up to the kill), the copy runs them too, in
    // the baseline's order, and any kill it finds is certain.
    let closedCopies = 0;
    const copyOverheadMs = () => profile.copies
        ? (profile.loadMs + profile.copyMs - profile.testMs) / profile.copies
        : 300;
    const withPredecessors = (group, chosen) => {
        if (group.whole || process.env["SUPERCOV_STRONG_PREFIX"] === "0")
            return chosen;
        const order = baselineOrder.get(group.file);
        if (!order)
            return chosen;
        const wanted = new Set(chosen);
        let lastAt = -1;
        order.forEach((id, at) => {
            if (wanted.has(id))
                lastAt = at;
        });
        const extra = order.slice(0, lastAt + 1).filter((id) => !wanted.has(id));
        if (!extra.length)
            return chosen;
        const cost = (ids) => ids.reduce((sum, id) => sum + (durationOf.get(id) ?? 1_000), 0);
        // Half the chosen tests' time: where in them a kill falls, on average.
        const killRate = (killedFirst + 1) / (decidedFirst + 2);
        const confirmation = killRate * (copyOverheadMs() + cost(chosen) / 2);
        if (process.env["SUPERCOV_STRONG_PREFIX"] !== "1" && cost(extra) > confirmation)
            return chosen;
        closedCopies += 1;
        return order.slice(0, lastAt + 1);
    };
    while (pending.length && !stalled) {
        const used = new Map();
        const round = [];
        const checks = [];
        for (const [key, check] of confirmations) {
            if (check.result || check.running || round.length + checks.length >= perRound)
                continue;
            if ((used.get(check.file) ?? 0) >= perFile)
                continue;
            used.set(check.file, (used.get(check.file) ?? 0) + 1);
            check.running = true;
            checks.push({ key, check });
        }
        for (const job of pending) {
            if (round.length + checks.length >= perRound)
                break;
            for (const group of [...job.waiting]) {
                if (round.length + checks.length >= perRound || (oneAtATime && round.length + checks.length))
                    break;
                if (baselineFailed.has(`${group.file}#file`)) {
                    job.waiting.splice(job.waiting.indexOf(group), 1);
                    continue;
                }
                if ((used.get(group.file) ?? 0) >= perFile)
                    continue;
                used.set(group.file, (used.get(group.file) ?? 0) + 1);
                job.waiting.splice(job.waiting.indexOf(group), 1);
                job.open += 1;
                round.push({ job, group });
            }
        }
        let limit = 0;
        const assignments = round.map(({ job, group }) => {
            const chosen = withPredecessors(group, group.tests.filter((id) => !baselineFailed.has(id)));
            const budget = Math.max(...chosen.map((id) => durationOf.get(id) ?? 1_000), 1_000);
            limit = Math.max(limit, (budget * TIMEOUT_FACTOR + TIMEOUT_CONSTANT_MS) * (group.retried ? 2 : 1));
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
        for (const { check } of checks) {
            const budget = Math.max(...check.claim.tests.map((id) => durationOf.get(id) ?? 1_000), 1_000);
            limit = Math.max(limit, budget * TIMEOUT_FACTOR + TIMEOUT_CONSTANT_MS);
            assignments.push({ file: check.file, claim: check.claim });
        }
        profile.checkCopies += checks.length;
        const outcomes = assignments.length ? await runRound(assignments, limit) : [];
        checks.forEach(({ check }, at) => {
            const outcome = outcomes[round.length + at];
            check.running = false;
            check.tries += 1;
            if (!outcome.claimed && !stalled && check.tries < 3)
                return;
            check.result = {
                failed: new Set(outcome.tests.filter((test) => test.failed).map((test) => test.test)),
                outside: outcome.outside.length > 0,
                // A copy no worker took cannot say the tests fail without
                // the mutant: the kill stands.
                done: outcome.done || !outcome.claimed,
            };
        });
        round.forEach(({ job, group }, at) => {
            job.open -= 1;
            if (job.done)
                return;
            const outcome = outcomes[at];
            if (!outcome.claimed) {
                if (stalled)
                    return;
                group.unclaimed = (group.unclaimed ?? 0) + 1;
                if (group.unclaimed > 2)
                    finish(job, { status: "error", reason: "no copy of its test file ran it" });
                else
                    job.waiting.push(group);
                return;
            }
            job.ran += outcome.tests.length;
            const chosen = assignments[at].claim.tests;
            const kill = (evidence, verdict) => {
                if (certain(group, chosen, evidence))
                    finish(job, verdict);
                else if (evidence.test && !group.whole) {
                    // Tests after the failing one cannot change it: the
                    // confirmation runs what ran up to it, which many
                    // mutants' copies share.
                    const ran = outcome.tests.map((test) => test.test);
                    confirm(job, group, ran.slice(0, ran.indexOf(evidence.test) + 1), evidence, verdict);
                }
                else
                    confirm(job, group, chosen, evidence, verdict);
            };
            const failed = outcome.tests.find((test) => test.failed && !baselineFailed.has(test.test));
            const outside = baselineFailed.has(`${group.file}#outside`) ? undefined : outcome.outside[0];
            if (!failed && outside) {
                kill({ outside: true }, outside.timeout
                    ? { status: "timeout", reason: "outside any test: the mutant's evaluations passed its limit" }
                    : { status: "killed", killedBy: group.file, reason: `an error outside any test: ${outside.message}` });
            }
            else if (failed) {
                if (failed.timeout && !group.retried) {
                    group.retried = true;
                    job.waiting.push(group);
                    return;
                }
                kill({ test: failed.test }, { status: failed.timeout ? "timeout" : "killed", killedBy: failed.test });
            }
            else if (!outcome.done) {
                // The copy started and its file did not finish: the mutant
                // broke it while it loaded or ran, or held it past the
                // round's deadline.
                if (stalled)
                    finish(job, { status: "timeout", reason: "its test file did not finish by the round's deadline" });
                else
                    kill({ unfinished: true }, { status: "killed", killedBy: group.file, reason: "the test file did not finish" });
            }
        });
        for (const job of pending) {
            if (job.confirming?.length && !job.done)
                settle(job);
            if (job.done || job.open || job.waiting.length || job.confirming?.length)
                continue;
            if (job.later.length) {
                job.waiting = job.later;
                job.later = [];
            }
            else
                finish(job, { status: "survived" });
        }
        for (let index = pending.length - 1; index >= 0; index--) {
            if (pending[index].done)
                pending.splice(index, 1);
        }
        if (!round.length && !checks.length && pending.length && !stalled) {
            for (const job of pending)
                finish(job, { status: "error", reason: "could not be scheduled" });
            break;
        }
    }
    // SUPERCOV_STRONG_KEEP_ROUNDS=1 keeps each round's assignments and what
    // every copy reported, to read where a verdict came from.
    if (!process.env["SUPERCOV_STRONG_KEEP_ROUNDS"])
        rmSync(roundsDirectory, { recursive: true, force: true });
    write({ summary: true, durationMs: Date.now() - started, isolated: !oneAtATime, warm: Boolean(vitest.__supercovWarm), stalled, left: pending.filter((job) => !job.done).length, confirmations: confirmations.size, overturned, closed: closedCopies, workers, perRound, profile });
    if (stalled) {
        // A worker that will not end keeps Vitest from closing: the mutants
        // left run in the next pass `mutants test` starts.
        process.exit(0);
    }
    return last;
}
