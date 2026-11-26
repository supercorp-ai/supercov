// Strong mutation's reporter for Playwright (playwrightStrong.mjs explains the
// pass). In each invocation's runner process it maps each copy of a test to
// the test the plan names, settles an attempt whose worker ended before its
// fixture could, says what the invocation is doing (a status file the engine
// reads for its progress line, and a line on the command's output for each
// mutant attempt), and keeps the first error outside any test.
// Once the command has ended, with every invocation it started (a harness may
// start several at once, each with a share of the tests), the engine runs
// this module (`judge`), which gives each mutant the round left without a
// verdict its verdict, or the round that runs it next: one verdict a mutant,
// from whichever invocations ran it, and what was cut short is unfinished.
import { appendFileSync, existsSync, readFileSync, realpathSync, statSync } from "node:fs";
import { dirname, join } from "node:path";
import { pathToFileURL } from "node:url";
import { attemptPath, baselineOf, entries, invocation, EVERYWHERE, firstWords, giveVerdict, key, loadPass, outcomeOf, placeOf, queuedWork, ranByRequests, readJson, reasonOf, serialFile, settleAttempt, strongPass, unreached, verdictOf, withoutCopy, writeJson } from "./playwrightStrong.mjs";

// How often, at most, the status file is written.
const STATUS_MS = 1_000;
// How often, at most, the work left is counted for it.
const QUEUED_MS = 5_000;

/** Each JSON file in a directory of the pass, or none. */
function readAll(directory) {
    return entries(directory).flatMap((name) => {
        try {
            return [readJson(`${directory}/${name}`)];
        }
        catch {
            return [];
        }
    });
}

function readJsonOr(path) {
    try {
        return readJson(path);
    }
    catch {
        return undefined;
    }
}

/** Ids for a message: the first few, and how many more. */
function listed(ids) {
    const shown = ids.slice(0, 5).join(", ");
    return ids.length > 5 ? `${shown} and ${ids.length - 5} more` : shown;
}
function readLines(path) {
    if (!existsSync(path))
        return [];
    return readFileSync(path, "utf8")
        .split("\n")
        .filter((line) => line.trim())
        .flatMap((line) => {
            try {
                return [JSON.parse(line)];
            }
            catch {
                return [];
            }
        });
}

function seconds(ms) {
    return `${(ms / 1000).toFixed(1)}s`;
}

export default class SupercovPlaywrightStrongReporter {
    constructor() {
        this.pass = strongPass();
        this.running = new Map();
        this.counts = { baselines: 0, controls: 0, failedControls: 0, attempts: 0, killed: 0, timedOut: 0, passed: 0 };
        this.writtenAt = 0;
        // The tests this invocation runs, and the baseline time of the runs
        // it ended: its pace, for the time its work left takes.
        this.held = [];
        // Those of them in a file with a serial describe.
        this.serial = new Set();
        this.doneMs = 0;
        this.queuedAt = 0;
    }
    // It prints its own lines, and says it prints: Playwright adds a reporter
    // of its own where none does. The command's output is a log beside the
    // pass's results, not `mutants test`'s stdout.
    printsToStdio() {
        return true;
    }
    onBegin(_config, suite) {
        if (!this.pass)
            return;
        this.work = loadPass(this.pass);
        this.startedAt = Date.now();
        // A copy of a test has an id of its own; the plan names the test as
        // declared. Copies share its project, file and titles: a worker finds
        // the test's id by them, one small file a test.
        let copies = 0;
        for (const test of suite.allTests()) {
            copies += 1;
            if (withoutCopy(test.titlePath()).copy)
                continue;
            writeJson(`${this.pass.round}/originals/${key(placeOf(test))}`, test.id);
            if (this.work.tests[test.id]?.jobs) {
                this.held.push(test.id);
                if (serialFile(test.location.file))
                    this.serial.add(test.id);
            }
        }
        this.copies = copies;
        const mutants = Object.keys(this.work.entries).length;
        const tests = Object.values(this.work.tests).filter((test) => test.jobs).length;
        console.log(`[supercov] strong pass, ${describeRound(this.work)}: ${mutants} mutant(s) over ${tests} test(s), ${copies} test case(s) in this invocation`);
        this.writeStatus(true);
        // Said while a long test runs, too.
        this.timer = setInterval(() => this.writeStatus(true), 5_000);
        this.timer.unref?.();
    }
    onTestBegin(test) {
        if (!this.pass)
            return;
        this.running.set(test.id, { title: test.title, begunAt: Date.now() });
        this.writeStatus(false);
    }
    onError(error) {
        // Outside any test: a web server that exited, a global setup that
        // threw. Each invocation's first one's time and words.
        if (!this.pass || this.erred)
            return;
        this.erred = true;
        writeJson(`${this.pass.round}/errors/${process.pid}.json`, { pid: process.pid, at: Date.now(), message: firstWords(error?.message ?? error) });
    }
    onTestEnd(test, result) {
        if (!this.pass)
            return;
        this.running.delete(test.id);
        const path = attemptPath(this.pass, invocation(), test.id);
        let attempt;
        try {
            attempt = readJson(path);
        }
        catch {
            attempt = undefined;
        }
        if (!attempt) {
            this.writeStatus(false);
            return;
        }
        // Its worker ended before its fixture did (a mutant that crashed it):
        // the reporter settles it.
        if (attempt.endedAt === undefined) {
            const { outcome, message } = outcomeOf(result.status, result.errors ?? []);
            attempt = settleAttempt(this.pass, { ...attempt, outcome, message });
        }
        this.lastEndedAt = Date.now();
        this.say(attempt);
        this.writeStatus(false);
    }
    /** A line on the command's output for each attempt with a mutant, and each control that failed. */
    say(attempt) {
        const took = seconds((attempt.endedAt ?? Date.now()) - attempt.startedAt);
        this.firstStartedAt ??= attempt.startedAt;
        this.doneMs += attempt.baselineMs ?? this.work.tests[attempt.original]?.durationMs ?? 0;
        if (attempt.kind !== "mutant") {
            this.counts[attempt.kind === "baseline" ? "baselines" : "controls"] += 1;
            if (attempt.outcome === "failed" || attempt.outcome === "timed-out") {
                this.counts.failedControls += 1;
                console.log(`[supercov] ${attempt.kind} failed, with no mutant: ${JSON.stringify(attempt.title)} (${took}): ${attempt.message ?? attempt.outcome}`);
            }
            return;
        }
        this.counts.attempts += 1;
        this.firstMutantAt ??= attempt.startedAt;
        if (attempt.outcome === "passed")
            this.counts.passed += 1;
        else if (attempt.outcome === "failed")
            this.counts.killed += 1;
        else if (attempt.outcome === "timed-out")
            this.counts.timedOut += 1;
        const said = {
            "passed": "lives",
            "failed": "fails",
            "timed-out": `ran out of its ${seconds(attempt.limitMs)} limit (its baseline ${seconds(attempt.baselineMs)})`,
        }[attempt.outcome] ?? attempt.outcome;
        // A cold attempt's failure decides nothing: it runs again once a
        // control has passed on its server.
        const cold = attempt.cold && (attempt.outcome === "failed" || attempt.outcome === "timed-out") ? ", before any control here: runs again after one" : "";
        console.log(`[supercov] ${attempt.id}: ${JSON.stringify(attempt.title)} ${said} (${took}${cold})`);
    }
    /**
     * What this invocation is doing, for the engine's progress line: the
     * tests running and since when, what ended so far, and when the last
     * one did. Written at most once a second, unless `now`.
     */
    writeStatus(now) {
        if (!this.pass || (!now && Date.now() - this.writtenAt < STATUS_MS))
            return;
        this.writtenAt = Date.now();
        const running = [...this.running].map(([id, { title, begunAt }]) => {
            let attempt;
            try {
                attempt = readJson(attemptPath(this.pass, invocation(), id));
            }
            catch {
                attempt = undefined;
            }
            return {
                title,
                begunAt,
                kind: attempt?.kind,
                mutant: attempt?.id ?? undefined,
                startedAt: attempt?.startedAt,
                limitMs: attempt?.limitMs,
            };
        });
        if (now && this.work && Date.now() - this.queuedAt >= QUEUED_MS)
            this.countQueued();
        try {
            writeJson(`${this.pass.shared}/status/${process.pid}.json`, {
                round: this.pass.name,
                kind: this.work?.kind,
                pid: process.pid,
                startedAt: this.startedAt,
                updatedAt: Date.now(),
                copies: this.copies,
                counts: this.counts,
                firstMutantAt: this.firstMutantAt,
                lastEndedAt: this.lastEndedAt,
                running,
                ...(this.queued ? { queued: this.queued } : {}),
            });
        }
        catch {
            // The status is for the progress line; the pass goes on without it.
        }
    }
    /**
     * The work left in this invocation's tests (`queuedWork`), and the time
     * it takes at the pace so far: the baseline time of the runs it ended
     * over the time they took, which counts what runs besides the tests (a
     * copy with nothing to run, a new worker after a kill).
     */
    countQueued() {
        this.queuedAt = Date.now();
        try {
            const queued = queuedWork(this.pass, this.work, this.held, this.serial);
            const spent = (this.lastEndedAt ?? 0) - (this.firstStartedAt ?? 0);
            if (this.doneMs > 0 && spent > 0)
                queued.leftMs = Math.round((queued.ms * spent) / this.doneMs);
            this.queued = queued;
        }
        catch {
            // Counted again at the next status.
        }
    }
    onEnd() {
        if (!this.pass)
            return;
        clearInterval(this.timer);
        this.running.clear();
        this.writeStatus(true);
        // A killed mutant fails a test, and that is the point: the command's
        // outcome is the pass's, not the tests'. The verdicts are the
        // judge's, once every invocation has ended.
        return { status: "passed" };
    }
    /**
     * The round's verdicts left, from what its invocations left on disk:
     * each attempt, the baselines, the kills not yet confirmed, and the
     * errors outside any test; and the rounds to run next.
     */
    finish() {
        const { pass } = this;
        const work = (this.work = loadPass(pass));
        const kind = work.kind;
        const round = pass.round;
        const attempts = readAll(`${round}/attempts`);
        const errors = readAll(`${round}/errors`).sort((a, b) => a.at - b.at);
        const globalError = errors[0];
        const started = Math.min(statSync(`${round}/pass.json`).mtimeMs, ...attempts.map((attempt) => attempt.startedAt));
        const write = (record) => appendFileSync(pass.resultsPath, `${JSON.stringify(record)}\n`);
        let decided = 0;
        for (const record of work.immediate) {
            if (giveVerdict(pass, record))
                decided += 1;
        }
        // Where a mutant ran past its hit limit in a server, by attempt; and
        // in a process that had it on everywhere, by mutant alone.
        const timedOut = new Set();
        const timedOutAnywhere = new Set();
        for (const line of readLines(`${pass.resultsPath}.timeouts.jsonl`)) {
            if (line.pass !== (pass.restart ?? "main"))
                continue;
            if (line.attemptId)
                timedOut.add(line.attemptId);
            else
                timedOutAnywhere.add(line.mutant);
        }
        const byMutant = new Map();
        const controls = { ran: 0, failed: [] };
        for (const attempt of attempts) {
            if (attempt.kind !== "mutant") {
                if (attempt.kind === "control" && attempt.endedAt !== undefined) {
                    controls.ran += 1;
                    if (attempt.outcome === "failed" || attempt.outcome === "timed-out")
                        controls.failed.push(attempt.original);
                }
                continue;
            }
            if (!byMutant.has(attempt.id))
                byMutant.set(attempt.id, []);
            byMutant.get(attempt.id).push(attempt);
        }
        // What runs next: one mutant alone (with the server started with it,
        // where a server ran its code outside any request), and one test at
        // a time for kills to confirm and timeouts' second tries; the rest of
        // what did not finish, as this round ran it.
        const alone = [];
        const serial = [];
        const rest = [];
        const aloneAgain = [];
        const aloneEach = [];
        const next = (entry, reason, tests) => {
            const runnable = tests.filter((test) => baselineOf(pass, test) !== "failed");
            if (!runnable.length) {
                write({ id: entry.id, status: "error", reason: "its candidate tests fail without any mutant", tests: 0, durationMs: 0 });
                return;
            }
            const job = { id: entry.id, mutant: entry.mutant, tests: runnable, reason, ...(entry.serverRan ? { serverRan: true } : {}) };
            // What an alone round left runs alone again, as it was.
            if (kind === "alone")
                aloneEach.push({ ...job, reason: entry.reason });
            else if (EVERYWHERE.includes(reason))
                alone.push(job);
            else if (reason === "unfinished")
                rest.push(job);
            else
                serial.push(job);
        };
        // A round the budget or a signal stopped: from the moment it began
        // to stop (`stopped-at`, beside the results), its servers went down
        // under the tests still running, and a test that ended after it
        // says nothing of its mutant. So, from an error outside any test.
        const stopped = process.env["SUPERCOV_STRONG_STOPPED"] === "1";
        const stoppedAt = Number(readJsonOr(join(dirname(pass.resultsPath), "stopped-at"))) || Infinity;
        const trustedUntil = Math.min(globalError ? globalError.at : Infinity, stoppedAt);
        // One mutant an invocation: which invocation took each, and where a
        // server of that invocation ran it (an invocation whose server was
        // started before it, outside it, never ran its mutant there).
        const claimedBy = new Map();
        for (const name of entries(`${round}/invocations`)) {
            const taken = readJsonOr(`${round}/invocations/${name}`);
            if (taken?.id)
                claimedBy.set(taken.id, name.replace(/\.json$/, ""));
        }
        const ranInServer = new Set(readLines(`${pass.resultsPath}.everywhere.jsonl`).map((line) => `${line.invocation}\0${line.mutant}`));
        const ranByRequest = ranByRequests(pass);
        // Survivors no attempt ran: they run again alone.
        const notRan = [];
        for (const [id, entry] of Object.entries(work.entries)) {
            if (existsSync(`${pass.shared}/verdicts/${key(id)}`)) {
                decided += 1;
                continue;
            }
            const invocation = claimedBy.get(id);
            if (work.batch && invocation === undefined) {
                aloneAgain.push(entry);
                continue;
            }
            const own = byMutant.get(id) ?? [];
            const ended = own.filter((attempt) => attempt.endedAt !== undefined);
            const ranAway = own.find((attempt) => timedOut.has(attempt.attemptId) || timedOutAnywhere.has(entry.mutant));
            // Kills, and attempts that ran out of their time limit.
            const failed = ended.filter((attempt) => attempt.outcome === "failed" || attempt.outcome === "timed-out");
            const after = reasonOf(kind, entry.reason);
            const suffix = after ? `; ${after}` : "";
            const durationMs = ended.length
                ? Math.max(...ended.map((attempt) => attempt.endedAt)) - Math.min(...ended.map((attempt) => attempt.startedAt))
                : 0;
            const verdict = (record) => {
                if (giveVerdict(pass, { id, ...record, tests: ended.length, durationMs }))
                    decided += 1;
            };
            // A kill beside another mutant's attempt, or whose control failed
            // without the mutant: its killing test first, alone, then the rest.
            const doubted = existsSync(`${round}/confirm/${key(id)}`) ? "confirm" : existsSync(`${round}/doubt/${key(id)}`) ? "doubt" : undefined;
            if (doubted && failed.length) {
                const first = failed[0].original;
                next(entry, doubted, [first, ...entry.tests.filter((test) => test !== first)]);
                continue;
            }
            // A kill no control came after (the round ended first): as sure as a
            // kill on a server no other mutant's attempt shared, where it ended
            // before the round began to stop.
            // A cold attempt's failure (no control had passed before it on
            // its server) is no kill: the round ended before it ran again.
            const killer = failed.find((attempt) => !attempt.beside && !attempt.cold && attempt.endedAt < trustedUntil);
            if (killer) {
                const { status, reason } = verdictOf(killer);
                verdict({ status, killedBy: killer.original, reason: `${reason}${suffix}` });
                continue;
            }
            if (ranAway) {
                verdict({ status: "timeout", killedBy: ranAway.original, reason: `evaluated past its hit limit${suffix}` });
                continue;
            }
            // Alone, the command failed outside any test: the mutant broke the
            // app (a server that would not start with it).
            // (Where one invocation of several failed so, it runs alone with
            // the whole command started with it, which says whether it was the
            // mutant's doing.)
            if (kind === "alone" && !work.batch && globalError && !ended.length) {
                verdict({ status: "killed", reason: `the command failed with it switched on (${globalError.message})${suffix}` });
                continue;
            }
            // Its invocation's server never ran it: alone, the whole command
            // started with it.
            if (work.batch && !ranInServer.has(`${invocation}\0${entry.mutant}`)) {
                aloneEach.push(entry);
                continue;
            }
            const passed = new Set(ended.filter((attempt) => attempt.outcome === "passed" && attempt.endedAt < trustedUntil).map((attempt) => attempt.original));
            const runnable = entry.tests.filter((test) => baselineOf(pass, test) !== "failed");
            const left = runnable.filter((test) => !passed.has(test));
            if (!runnable.length) {
                verdict({ status: "error", reason: "its candidate tests fail without any mutant" });
                continue;
            }
            if (left.length) {
                next(entry, "unfinished", left);
                continue;
            }
            if (unreached(kind, entry, ranByRequest)) {
                notRan.push(id);
                next(entry, "unreached", runnable);
                continue;
            }
            verdict(entry.unrun
                ? { status: "error", reason: `no Playwright test killed it, and ${entry.unrun}` }
                : { status: "survived", ...(after ? { reason: after } : {}) });
        }
        for (const job of work.restarts)
            next(job, job.reason, job.tests);
        // Said where the pass's output is read: a verdict a server's switch
        // missed is a Supercov fault, and other survivors may share it.
        if (notRan.length)
            console.error(`[supercov] warning: ${notRan.length} mutant(s) lived through every candidate test on the shared server, though no attempt ran them there: the requests that reached their code did not carry their test's mutant. Each runs again alone, with the server started with it switched on: ${listed(notRan)}`);
        if (kind === "alone") {
            const killedAlone = readLines(pass.resultsPath)
                .filter((record) => work.entries[record.id]?.reason === "unreached" && (record.status === "killed" || record.status === "timeout"))
                .map((record) => record.id);
            if (killedAlone.length)
                console.error(`[supercov] warning: ${killedAlone.length} mutant(s) the shared server let live were killed alone, with the server started with them switched on; their verdicts are the kills: ${listed(killedAlone)}`);
        }
        // What each mutant left open passed: its candidates that ran with it
        // and passed (before any error outside a test), by the plan's ids. A
        // later pass, or a later round, resumes there. A round of mutants a
        // server ran from its start lends nothing to a request's own switch.
        if (kind !== "alone") {
            for (const id of Object.keys(work.entries)) {
                if (existsSync(`${pass.shared}/verdicts/${key(id)}`))
                    continue;
                const passed = (byMutant.get(id) ?? [])
                    .filter((attempt) => attempt.outcome === "passed" && attempt.endedAt < trustedUntil)
                    .map((attempt) => work.tests[attempt.original]?.planId ?? attempt.original);
                if (passed.length)
                    write({ progress: true, id, passed: [...new Set(passed)].sort() });
            }
        }
        // A round of what an earlier one left that ran to its end and decided
        // nothing would only ask for the same again. One the budget or a
        // signal stopped leaves its mutants without a verdict, for the next
        // pass (a confirmation round stopped before its first test made two
        // mutants of a 60 s pass `error`s).
        if (kind !== "main" && !decided && !stopped) {
            for (const job of rest)
                write({ id: job.id, status: "error", reason: "its tests did not finish, in two rounds", tests: 0, durationMs: 0 });
            rest.splice(0, rest.length);
        }
        // A mutant run alone with the whole command started with it, whose
        // tests did not finish: there is no way left to run it.
        if (kind === "alone" && !work.batch && !stopped) {
            for (const job of aloneEach)
                write({ id: job.id, status: "error", reason: "its tests did not finish, run alone", tests: 0, durationMs: 0 });
            aloneEach.splice(0, aloneEach.length);
        }
        // What this invocation had no room for runs in the next, as this
        // one would have run it.
        if (work.deferred.length)
            write({ restart: { kind: "main", reason: "more copies than one invocation holds", mutants: work.deferred } });
        // The mutants a server must start with: one round of them, each
        // invocation of the command taking one; those no invocation took
        // again so, unless none took any; and each one its server did not run
        // (or every one, where no invocation took one) alone with the whole
        // command started with it.
        const takenAny = claimedBy.size > 0;
        for (const entry of aloneAgain)
            (takenAny ? alone : aloneEach).push(entry);
        if (alone.length)
            write({ restart: { kind: "alone", everywhere: true, batch: true, reason: "server-wide", mutants: alone, otherRunners: work.otherRunners, setupOnly: work.setupOnly } });
        for (const job of aloneEach)
            write({ restart: { kind: "alone", everywhere: true, reason: job.reason, mutants: [job], otherRunners: work.otherRunners, setupOnly: work.setupOnly } });
        if (rest.length)
            write({ restart: { kind: "rest", reason: "unfinished", mutants: rest, otherRunners: work.otherRunners, setupOnly: work.setupOnly } });
        if (serial.length)
            write({ restart: { kind: "serial", reason: [...new Set(serial.map((job) => job.reason))].join(", "), mutants: serial, otherRunners: work.otherRunners, setupOnly: work.setupOnly } });
        const mutated = attempts.filter((attempt) => attempt.kind === "mutant");
        write({
            summary: true,
            runner: "playwright",
            pass: pass.name,
            kind,
            ...(work.concurrency.workers ? { workers: work.concurrency.workers } : {}),
            serialFiles: work.concurrency.serialFiles.length,
            ...(globalError ? { error: globalError.message } : {}),
            invocations: new Set(attempts.map((attempt) => attempt.invocation)).size,
            workerProcesses: new Set(attempts.map((attempt) => attempt.worker)).size,
            durationMs: Date.now() - started,
            attempts: mutated.length,
            timedOut: mutated.filter((attempt) => attempt.outcome === "timed-out").length,
            timeoutFactor: work.timeoutFactor,
            baselines: attempts.filter((attempt) => attempt.kind === "baseline").length,
            decided,
            next: { alone: alone.length, rest: rest.length, serial: serial.length },
            controls,
        });
    }
}

/** A round as its first line says it. */
function describeRound(work) {
    if (work.kind === "main")
        return work.job ? "the main round's rest" : "main round";
    if (work.kind === "alone")
        return `alone (${EVERYWHERE.includes(work.job?.reason) ? "the server started with the mutant" : work.job?.reason})`;
    return `${work.kind} round (${work.job?.reason})`;
}

/** Decides the round's verdicts: run by the engine once the command has ended. */
export function judge() {
    const reporter = new SupercovPlaywrightStrongReporter();
    if (!reporter.pass || !existsSync(`${reporter.pass.round}/pass.json`))
        return;
    reporter.finish();
}

if (process.argv[1] && import.meta.url === pathToFileURL(realpathSync(process.argv[1])).href)
    judge();
