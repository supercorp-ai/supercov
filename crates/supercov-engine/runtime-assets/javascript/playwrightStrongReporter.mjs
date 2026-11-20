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
    "confirm": "a test killed it beside other mutants' attempts",
    "stage": "its likeliest killer let it live",
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
    // It prints nothing, and says it prints: Playwright adds a reporter of
    // its own where none does, and `mutants test --json` owns stdout.
    printsToStdio() {
        return true;
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
        this.globalError ??= { at: Date.now(), message: firstWords(error?.message ?? error) };
    }
    onTestEnd(test, result) {
        if (!this.pass)
            return;
        // Why it failed, and whether only the test's time ran out: a test
        // that timed out while an assertion or an action was failing failed
        // on its own.
        const errors = result.errors ?? [];
        const timeoutOnly = errors.length > 0 && errors.every((error) => TEST_TIMEOUT.test(stripAnsi(error?.message ?? "")));
        const reported = errors.find((error) => !TEST_TIMEOUT.test(stripAnsi(error?.message ?? ""))) ?? errors[0];
        this.statuses.set(test.id, {
            status: result.status,
            timeoutOnly,
            message: reported ? firstWords(reported.message ?? reported.value) : undefined,
        });
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
        const kind = rows.kind;
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
        // Every attempt that ran with a mutant: a kill beside another's is
        // confirmed alone before it counts.
        const mutated = [];
        for (const repetition of Object.keys(this.repetitions)) {
            const attempt = this.attempt(repetition);
            if (attempt && attempt.mutant !== "-")
                mutated.push({ ...attempt, repetition, endedAt: attempt.endedAt ?? Date.now() });
        }
        const beside = (repetition, attempt) => mutated.some((other) => other.repetition !== repetition &&
            other.mutant !== attempt.mutant &&
            other.startedAt < (attempt.endedAt ?? Date.now()) &&
            attempt.startedAt < other.endedAt);
        // Each repetition's outcome.
        const outcomeOf = (original, index, mutant) => {
            const repetition = this.byOriginal.get(`${original}\0${index}`);
            const ended = repetition && this.statuses.get(repetition);
            const attempt = repetition && this.attempt(repetition);
            if (!attempt) {
                if (baselineFailed.has(original))
                    return { kind: "baseline-failed" };
                return { kind: "unrun" };
            }
            const ranAway = attempt.pageTimeout === true ||
                (attempt.attemptId && timedOut.has(attempt.attemptId)) ||
                (mutant && timedOutAnywhere.has(mutant));
            const base = {
                startedAt: attempt.startedAt,
                endedAt: attempt.endedAt ?? Date.now(),
                message: ended?.message,
                beside: beside(repetition, attempt),
            };
            if (ranAway)
                return { ...base, kind: "hit-limit" };
            if (ended?.status === "passed")
                return { ...base, kind: "passed" };
            if (ended?.status === "timedOut" && ended.timeoutOnly)
                return { ...base, kind: "timed-out" };
            if (ended?.status === "failed" || ended?.status === "interrupted" || ended?.status === "timedOut")
                return { ...base, kind: "failed" };
            return { kind: "unrun" };
        };
        // Each mutant's repetitions, in every file it has a row in, and the
        // canaries'.
        const outcomes = new Map();
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
                    if (!outcomes.has(row.id))
                        outcomes.set(row.id, []);
                    outcomes.get(row.id).push(outcome);
                }
            });
        }
        // What runs next: one mutant alone (with the server started with
        // it, where a server ran its code outside any request), a further
        // pass of survivors' other candidates, and a serial pass, one test
        // at a time, for kills to confirm, timeouts' second tries and what
        // did not finish.
        const alone = [];
        const stage = [];
        const serial = [];
        const next = (entry, reason, tests, budgetFactor = 1) => {
            const runnable = tests.filter((test) => !baselineFailed.has(test));
            if (!runnable.length) {
                write({ id: entry.id, status: "error", reason: "its candidate tests fail without any mutant", tests: 0, durationMs: 0 });
                return;
            }
            const job = { id: entry.id, mutant: entry.mutant, tests: runnable, reason, budgetFactor };
            if (EVERYWHERE.includes(reason) || reason === "canary")
                alone.push(job);
            else if (reason === "stage")
                stage.push(job);
            else
                serial.push(job);
        };
        const failedCanaries = canaries.filter((canary) => ["failed", "timed-out", "hit-limit"].includes(canary.kind));
        const doubtUntil = failedCanaries.length
            ? Math.max(...failedCanaries.map((canary) => canary.endedAt))
            : -Infinity;
        // After an error outside any test (the shared server exited), what
        // ended later may be its doing.
        const trustedUntil = this.globalError ? this.globalError.at - ERROR_MARGIN_MS : Infinity;
        let decided = 0;
        let confirming = 0;
        for (const [id, entry] of Object.entries(rows.entries)) {
            const own = outcomes.get(id) ?? [];
            const ran = own.filter((outcome) => outcome.startedAt !== undefined);
            const all = [...entry.tests, ...(entry.rest ?? [])];
            if (kind === "main" && ran.some((outcome) => outcome.startedAt <= doubtUntil)) {
                next(entry, "canary", all);
                continue;
            }
            const verdict = this.verdict(entry, own, all, trustedUntil, next);
            if (verdict === "confirm")
                confirming += 1;
            if (!verdict || verdict === "confirm")
                continue;
            decided += 1;
            const durationMs = ran.length
                ? Math.max(...ran.map((outcome) => outcome.endedAt)) - Math.min(...ran.map((outcome) => outcome.startedAt))
                : 0;
            write({ id, ...verdict, tests: ran.length, durationMs });
        }
        for (const job of rows.restarts)
            next(job, job.reason, job.tests);
        // A serial pass that decided nothing would only ask for itself again.
        if (kind === "serial" && !decided) {
            for (const job of serial)
                write({ id: job.id, status: "error", reason: `its tests did not finish, run one at a time (${job.reason})`, tests: 0, durationMs: 0 });
            serial.length = 0;
        }
        for (const job of alone)
            write({ restart: { kind: "alone", everywhere: EVERYWHERE.includes(job.reason), reason: job.reason, mutants: [job], otherRunners: rows.otherRunners } });
        if (stage.length)
            write({ restart: { kind: "stage", reason: "stage", mutants: stage, otherRunners: rows.otherRunners } });
        if (serial.length)
            write({ restart: { kind: "serial", reason: [...new Set(serial.map((job) => job.reason))].join(", "), mutants: serial, otherRunners: rows.otherRunners } });
        write({
            summary: true,
            runner: "playwright",
            pass: pass.name,
            kind,
            workers: rows.concurrency.workers,
            ...(this.globalError ? { error: this.globalError.message } : {}),
            durationMs: Date.now() - this.started,
            decided,
            confirming,
            next: { alone: alone.length, stage: stage.length, serial: serial.length },
            canaries: {
                ran: canaries.filter((canary) => canary.startedAt !== undefined).length,
                failed: failedCanaries.map((canary) => canary.test),
            },
        });
    }
    /**
     * A mutant's verdict from its repetitions in this pass; `undefined` when
     * it was sent on to another pass, `"confirm"` when a kill beside other
     * mutants' attempts is to be confirmed one test at a time.
     */
    verdict(entry, outcomes, all, trustedUntil, next) {
        const { rows } = this;
        const kind = rows.kind;
        const shared = kind === "main" || kind === "stage";
        const reason = entry.reason ?? (kind === "main" ? undefined : rows.job?.reason);
        const after = kind === "alone"
            ? `; ran alone${EVERYWHERE.includes(reason) ? ", with the server started with it switched on" : ""}: ${WHY[reason] ?? reason}`
            : kind === "serial"
                ? `; ran one test at a time: ${WHY[reason] ?? reason}`
                : "";
        const trusted = (outcome) => !shared || (!outcome.beside && outcome.endedAt < trustedUntil);
        const failed = outcomes.filter((outcome) => outcome.kind === "failed");
        const killer = failed.find(trusted);
        if (killer)
            return { status: "killed", killedBy: killer.test, reason: `${killer.message ?? "the test failed"}${after}` };
        const ranAway = outcomes.find((outcome) => outcome.kind === "hit-limit");
        if (ranAway)
            return { status: "timeout", killedBy: ranAway.test, reason: `evaluated past its hit limit${after}` };
        if (failed.length) {
            // Its killing test ran beside other mutants' attempts (or after
            // the shared server failed): that test first, alone, then the rest.
            const first = failed[0].test;
            next(entry, failed.some((outcome) => outcome.beside) ? "confirm" : "unfinished", [first, ...all.filter((test) => test !== first)]);
            return "confirm";
        }
        const slow = outcomes.find((outcome) => outcome.kind === "timed-out");
        if (slow) {
            if (shared) {
                next(entry, "timeout-retry", all, 2);
                return undefined;
            }
            return { status: "timeout", killedBy: slow.test, reason: `ran out of time${after}` };
        }
        const unrun = outcomes.filter((outcome) => outcome.kind === "unrun" || (shared && outcome.endedAt !== undefined && outcome.endedAt >= trustedUntil));
        if (unrun.length) {
            // Alone, the command failed outside any test: the mutant broke the
            // app (a server that would not start with it).
            if (kind === "alone" && this.globalError)
                return { status: "killed", reason: `the command failed with it switched on (${this.globalError.message})${after}` };
            next(entry, "unfinished", all);
            return undefined;
        }
        if (outcomes.length && outcomes.every((outcome) => outcome.kind === "baseline-failed"))
            return { status: "error", reason: "its candidate tests fail without any mutant" };
        // Its likeliest killer let it live: its other candidates next.
        if (kind === "main" && entry.rest?.length) {
            next(entry, "stage", entry.rest);
            return undefined;
        }
        const others = rows.otherRunners[entry.id];
        if (others)
            return { status: "error", reason: `no Playwright test killed it, and its other candidates ran under ${others.join(", ")}, which this pass does not run` };
        return { status: "survived", ...(after ? { reason: after.slice(2) } : {}) };
    }
}

const TEST_TIMEOUT = /Test timeout of \d+ms exceeded/;
const EVERYWHERE = ["background", "module-level", "by-time"];

function stripAnsi(text) {
    return String(text).replace(/\u001b\[[0-9;]*m/g, "");
}

/** An error's message, on one line, short enough to keep. */
function firstWords(message) {
    const text = stripAnsi(message ?? "").replace(/\s+/g, " ").trim();
    return text.length > 300 ? `${text.slice(0, 299)}…` : text;
}
