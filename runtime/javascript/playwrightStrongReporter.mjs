// Strong mutation's reporter for Playwright (playwrightStrong.mjs explains the
// pass). In the runner process it maps each repetition of a test to the test
// the plan names, marks each test's baseline and each killed mutant as
// results come (the fixture reads them: a repetition after a failed baseline
// or a kill skips), and at the end turns the repetitions' outcomes into each
// mutant's verdict, in the results file `mutants test` reads.
import { appendFileSync, existsSync, readFileSync } from "node:fs";
import { baselineOf, key, loadRows, strongPass, writeJson } from "./playwrightStrong.mjs";

const WHY = {
    "background": "its code ran in server work no request named",
    "module-level": "it is a server module's top-level code",
    "by-time": "its code ran for a request that named no test",
    "timeout-retry": "a test ran out of time with it switched on, and ran again alone with twice the time",
    "canary": "a canary failed in the server it shared",
    "unfinished": "not every candidate test ran in the shared pass",
};
// Outcomes that ended this long before a pass's first error outside any
// test (a server that exited) are trusted; later ones may be its doing.
const ERROR_MARGIN_MS = 1_000;

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

export default class SupercovPlaywrightStrongReporter {
    constructor() {
        this.pass = strongPass();
        this.started = Date.now();
        this.statuses = new Map();
        this.repetitions = {};
        this.byOriginal = new Map();
    }
    printsToStdio() {
        return false;
    }
    onBegin(_config, suite) {
        if (!this.pass)
            return;
        this.rows = loadRows(this.pass);
        // A repetition of a test has an id of its own; the plan names the
        // first. Repetitions share the test's project, file and titles.
        const where = (test) => JSON.stringify(test.titlePath());
        const firsts = new Map();
        const all = suite.allTests();
        for (const test of all) {
            if (!test.repeatEachIndex)
                firsts.set(where(test), test.id);
        }
        for (const test of all) {
            const original = test.repeatEachIndex ? firsts.get(where(test)) : test.id;
            if (!original)
                continue;
            this.repetitions[test.id] = original;
            this.byOriginal.set(`${original}\0${test.repeatEachIndex}`, test.id);
        }
        writeJson(`${this.pass.round}/repetitions.json`, this.repetitions);
    }
    rowOf(test) {
        const original = this.repetitions[test.id];
        const file = original && this.rows.tests[original]?.file;
        const row = file && this.rows.files[file]?.[test.repeatEachIndex];
        return row && row.tests.includes(original) ? { row, original } : undefined;
    }
    attempt(repetition) {
        const path = `${this.pass.round}/attempts/${key(repetition)}.json`;
        try {
            return JSON.parse(readFileSync(path, "utf8"));
        }
        catch {
            return undefined;
        }
    }
    onError(error) {
        // Outside any test: a web server that exited, a global setup that
        // threw. The first one's time and words.
        this.globalError ??= { at: Date.now(), message: String(error?.message ?? error).split("\n")[0] };
    }
    onTestEnd(test, result) {
        if (!this.pass)
            return;
        this.statuses.set(test.id, result.status);
        const found = this.rowOf(test);
        if (!found)
            return;
        const { row, original } = found;
        if (row.kind === "baseline" && !this.rows.job) {
            writeJson(`${this.pass.round}/baseline/${key(original)}`, result.status === "passed" ? "passed" : "failed");
            return;
        }
        // The rest of the mutant's repetitions skip.
        if (row.kind === "mutant" && result.status === "failed" && this.attempt(test.id))
            writeJson(`${this.pass.round}/killed/${key(row.id)}`, original);
    }
    onEnd() {
        if (!this.pass)
            return;
        this.finish();
        // A killed mutant fails its test, and that is the point: the
        // command's outcome is the pass's, not the tests'.
        return { status: "passed" };
    }
    finish() {
        const { pass, rows } = this;
        const write = (record) => appendFileSync(pass.resultsPath, `${JSON.stringify(record)}\n`);
        for (const record of rows.immediate)
            write(record);
        // Where a mutant ran past its hit limit, by attempt; and in a process
        // that had it on everywhere, by mutant alone.
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
        const baselineFailed = new Set();
        if (!rows.job) {
            for (const fileRows of Object.values(rows.files)) {
                for (const original of fileRows[0].tests) {
                    if (baselineOf(pass, original) !== "passed")
                        baselineFailed.add(original);
                }
            }
        }
        // Each repetition's outcome.
        const outcomeOf = (original, index, mutant) => {
            const repetition = this.byOriginal.get(`${original}\0${index}`);
            const status = repetition && this.statuses.get(repetition);
            const attempt = repetition && this.attempt(repetition);
            if (!attempt) {
                if (baselineFailed.has(original))
                    return { kind: "baseline-failed" };
                return { kind: "unrun" };
            }
            const ranAway = attempt.pageTimeout === true ||
                (attempt.attemptId && timedOut.has(attempt.attemptId)) ||
                (mutant && timedOutAnywhere.has(mutant));
            const base = { startedAt: attempt.startedAt, endedAt: attempt.endedAt ?? Date.now() };
            if (ranAway)
                return { ...base, kind: "hit-limit" };
            if (status === "passed")
                return { ...base, kind: "passed" };
            if (status === "timedOut")
                return { ...base, kind: "timed-out" };
            if (status === "failed" || status === "interrupted")
                return { ...base, kind: "failed" };
            return { kind: "unrun" };
        };
        // Each mutant's repetitions, in every file it has a row in, and the
        // canaries'.
        const mutants = new Map();
        const canaries = [];
        for (const fileRows of Object.values(rows.files)) {
            fileRows.forEach((row, index) => {
                if (row.kind === "baseline")
                    return;
                for (const original of row.tests) {
                    const outcome = { test: original, ...outcomeOf(original, index, row.mutant) };
                    if (row.kind === "canary") {
                        canaries.push(outcome);
                        continue;
                    }
                    if (!mutants.has(row.id))
                        mutants.set(row.id, { row, tests: [], outcomes: [] });
                    const entry = mutants.get(row.id);
                    entry.tests.push(original);
                    entry.outcomes.push(outcome);
                }
            });
        }
        const restarts = [];
        const restart = (id, mutant, tests, reason, budgetFactor = 1) => {
            const runnable = tests.filter((test) => !baselineFailed.has(test));
            if (!runnable.length) {
                write({ id, status: "error", reason: "its candidate tests fail without any mutant", tests: 0, durationMs: 0 });
                return;
            }
            restarts.push({ id, mutant, tests: runnable, reason, budgetFactor });
        };
        // A canary that failed: every mutant that ran before it ended is run
        // again, one mutant per server start.
        const failedCanaries = canaries.filter((canary) => canary.kind === "failed" || canary.kind === "timed-out" || canary.kind === "hit-limit");
        const doubtUntil = failedCanaries.length
            ? Math.max(...failedCanaries.map((canary) => canary.endedAt))
            : -Infinity;
        // After an error outside any test (the shared server exited), what
        // ended later may be its doing: such a mutant runs again alone.
        const trustedUntil = this.globalError ? this.globalError.at - ERROR_MARGIN_MS : Infinity;
        let perRequest = 0;
        for (const [id, { row, tests, outcomes }] of mutants) {
            const ran = outcomes.filter((outcome) => outcome.startedAt !== undefined);
            if (!rows.job && ran.some((outcome) => outcome.startedAt <= doubtUntil)) {
                restart(id, row.mutant, tests, "canary");
                continue;
            }
            if (!rows.job) {
                const trusted = outcomes.filter((outcome) => outcome.startedAt === undefined || outcome.endedAt < trustedUntil);
                const killedBefore = trusted.some((outcome) => outcome.kind === "failed" || outcome.kind === "hit-limit");
                if (!killedBefore && outcomes.some((outcome) => outcome.kind === "unrun" || (outcome.endedAt !== undefined && outcome.endedAt >= trustedUntil))) {
                    restart(id, row.mutant, tests, "unfinished");
                    continue;
                }
            }
            perRequest += 1;
            const verdict = this.verdict(id, row, outcomes, tests, restart);
            const durationMs = ran.length
                ? Math.max(...ran.map((outcome) => outcome.endedAt)) - Math.min(...ran.map((outcome) => outcome.startedAt))
                : 0;
            if (verdict)
                write({ id, ...verdict, tests: ran.length, durationMs });
        }
        for (const job of rows.restarts)
            restart(job.id, job.mutant, job.tests, job.reason);
        for (const job of restarts)
            write({ restart: job });
        write({
            summary: true,
            runner: "playwright",
            pass: pass.name,
            ...(this.globalError ? { error: this.globalError.message } : {}),
            durationMs: Date.now() - this.started,
            mutants: perRequest,
            restarts: restarts.length,
            canaries: {
                ran: canaries.filter((canary) => canary.startedAt !== undefined).length,
                failed: failedCanaries.map((canary) => canary.test),
            },
        });
    }
    /** A mutant's verdict from its repetitions, or `undefined` once it is sent to run again alone. */
    verdict(id, row, outcomes, tests, restart) {
        const { rows } = this;
        const job = rows.job;
        const alone = job ? `; ran alone, with the server started with it switched on: ${WHY[job.reason] ?? job.reason}` : "";
        const killer = outcomes.find((outcome) => outcome.kind === "failed");
        if (killer)
            return { status: "killed", killedBy: killer.test, ...(alone ? { reason: alone.slice(2) } : {}) };
        const ranAway = outcomes.find((outcome) => outcome.kind === "hit-limit");
        if (ranAway)
            return { status: "timeout", killedBy: ranAway.test, reason: `evaluated past its hit limit${alone}` };
        const slow = outcomes.find((outcome) => outcome.kind === "timed-out");
        if (slow) {
            if (!job) {
                restart(id, row.mutant, tests, "timeout-retry", 2);
                return undefined;
            }
            return { status: "timeout", killedBy: slow.test, reason: `ran out of time${alone}` };
        }
        // Run alone, and the command failed outside any test (the server
        // it started with the mutant exited): the mutant broke the app.
        if (job && this.globalError && !outcomes.every((outcome) => outcome.kind === "passed"))
            return { status: "killed", reason: `the command failed with it switched on (${this.globalError.message})${alone}` };
        const unrun = outcomes.filter((outcome) => outcome.kind === "unrun");
        if (unrun.length)
            return { status: "error", reason: `${unrun.length} of its candidate test(s) did not run${alone}` };
        if (outcomes.every((outcome) => outcome.kind === "baseline-failed"))
            return { status: "error", reason: "its candidate tests fail without any mutant" };
        const others = rows.otherRunners[id];
        if (others)
            return { status: "error", reason: `no Playwright test killed it, and its other candidates ran under ${others.join(", ")}, which this pass does not run` };
        return { status: "survived", ...(alone ? { reason: alone.slice(2) } : {}) };
    }
}

