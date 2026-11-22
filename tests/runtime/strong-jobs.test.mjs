import assert from "node:assert/strict";
import { mkdirSync, mkdtempSync, readFileSync, rmSync, symlinkSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import test from "node:test";
import { commandFor, drive, outcomes, withoutNpx } from "../../runtime/javascript/strongJobs.mjs";

const runner = resolve(import.meta.dirname, "fixtures/strong-jobs-runner.mjs");

// A strong pass's driver over a runner whose tests do what `rules` says
// (fixtures/strong-jobs-runner.mjs), each mutant's verdict by id.
async function pass({ tests, mutants, listed, rules, files }) {
    const root = mkdtempSync(join(tmpdir(), "supercov-strong-jobs-"));
    try {
        mkdirSync(join(root, ".supercov/frontend-evidence"), { recursive: true });
        writeFileSync(join(root, ".supercov/frontend-evidence/mutants.json"), JSON.stringify({ files: { "src/a.js": listed } }));
        writeFileSync(join(root, "suite.json"), JSON.stringify({ files, rules }));
        const resultsPath = join(root, "results.jsonl");
        await drive({
            command: [process.execPath, runner],
            plan: { tests, mutants },
            resultsPath,
            root,
            workers: 4,
            environment: { ...process.env, FAKE_SUITE: join(root, "suite.json") },
            graceMs: 1_500,
            baselineGraceMs: 1_500,
        });
        const records = readFileSync(resultsPath, "utf8").split("\n").filter(Boolean).map((line) => JSON.parse(line));
        return {
            verdicts: Object.fromEntries(records.filter((record) => record.id).map((record) => [record.id, record])),
            summary: records.find((record) => record.summary),
        };
    }
    finally {
        rmSync(root, { recursive: true, force: true });
    }
}

const test_ = (id, file, durationMs = 1, runner = "node:test") => ({ id, file, name: `${file} > ${id}`, runner, durationMs, runnable: true });
const tests = [
    test_("a1", "t/a.test.js"), test_("a2", "t/a.test.js"), test_("a3", "t/a.test.js"),
    test_("b1", "t/b.test.js", 2_000), test_("b2", "t/b.test.js", 2_000),
    test_("c1", "t/c.test.js"), test_("c2", "t/c.test.js"),
    test_("d1", "t/d.test.js"),
    test_("v1", "t/v.test.js", 1, "vitest"),
];
const at = (id) => tests.findIndex((one) => one.id === id);
const mutant = (id, candidates, extra = {}) => ({ id, file: "src/a.js", candidates: candidates.map((name) => ({ test: at(name), reason: "infected" })), ...extra });
const files = {
    "t/a.test.js": ["a1", "a2", "a3"],
    "t/b.test.js": ["b1", "b2"],
    "t/c.test.js": ["c1", "c2"],
    "t/d.test.js": ["d1"],
};

test("each mutant runs against its candidates, file by file, and the first failure kills it", async () => {
    const mutants = [
        mutant("kill-a", ["a2"]),
        mutant("survive", ["a1"]),
        mutant("kill-b", ["b1"]),
        mutant("overturned", ["b2"]),
        mutant("hang", ["a3"]),
        mutant("loop", ["a3"]),
        mutant("crash", ["a1"]),
        mutant("c", ["c1"]),
        mutant("d", ["d1"]),
        mutant("whole", ["a1"], { wholeFiles: ["t/a.test.js", "t/gone.test.js"] }),
        mutant("second-file", ["a1", "c2"]),
        mutant("unplaceable", ["a1"]),
        mutant("elsewhere", ["v1"]),
        mutant("partly-elsewhere", ["a1", "v1"]),
        mutant("nothing", []),
        mutant("not-listed", ["a1"]),
        mutant("decided", [], { decided: "survived" }),
    ];
    const listed = mutants.filter((one) => one.id !== "not-listed").map((one) => ({ id: one.id, ...(one.id === "unplaceable" ? { unplaceable: "function-source" } : {}) }));
    const index = (id) => listed.findIndex((one) => one.id === id);
    const on = (id) => `src/a.js#${index(id)}`;
    const rules = {
        // Without any mutant: b2 fails unless b1 ran first, c1 fails, d1
        // never ends.
        "*": { b2: "subset-fail", c1: "fail", d1: "hang" },
        [on("kill-a")]: { a2: "fail" },
        [on("kill-b")]: { b1: "fail" },
        [on("hang")]: { a3: "hang" },
        [on("loop")]: { a3: "loop" },
        [on("crash")]: { a1: "crash" },
        [on("c")]: { c2: "fail" },
        [on("whole")]: { a3: "fail" },
        [on("second-file")]: { c2: "fail" },
    };
    const { verdicts, summary } = await pass({ tests, mutants, listed, rules, files });
    const status = Object.fromEntries(Object.entries(verdicts).map(([id, verdict]) => [id, verdict.status]));
    assert.deepEqual(status, {
        "kill-a": "killed",
        survive: "survived",
        "kill-b": "killed",
        overturned: "survived",
        hang: "timeout",
        loop: "timeout",
        crash: "killed",
        c: "killed",
        d: "error",
        whole: "killed",
        "second-file": "killed",
        unplaceable: "error",
        elsewhere: "error",
        "partly-elsewhere": "error",
        nothing: "survived",
        "not-listed": "error",
    });
    assert.equal(verdicts["kill-a"].killedBy, "t/a.test.js > a2");
    assert.equal(verdicts["kill-b"].killedBy, "t/b.test.js > b1");
    assert.equal(verdicts.crash.reason, "the test file did not finish");
    assert.equal(verdicts.c.killedBy, "t/c.test.js > c2");
    assert.equal(verdicts.whole.killedBy, "t/a.test.js > a3");
    assert.equal(verdicts["second-file"].killedBy, "t/c.test.js > c2");
    assert.match(verdicts.d.reason, /t\/d\.test\.js does not finish without a mutant/);
    assert.match(verdicts.unplaceable.reason, /function-source/);
    assert.match(verdicts.elsewhere.reason, /ran under vitest/);
    assert.match(verdicts["partly-elsewhere"].reason, /other candidates ran under vitest/);
    assert.equal(verdicts.nothing.reason, "no-runnable-candidate");
    // b2 alone fails without the mutant too: the kill was no kill, and the
    // whole file let the mutant live.
    assert.equal(summary.overturned, 1);
    assert.ok(summary.confirmations >= 2, JSON.stringify(summary));
    assert.ok(summary.stopped >= 2, JSON.stringify(summary));
});

test("a test's outcome is its last attempt's that says one", () => {
    const directory = mkdtempSync(join(tmpdir(), "supercov-outcomes-"));
    try {
        mkdirSync(join(directory, "attempt-x"));
        writeFileSync(join(directory, "a.mcdc.jsonl"), [
            { testId: "jest", status: "unknown" },
            { testId: "jest", status: "passed" },
            { testId: "retried", status: "failed", retry: 0 },
            { testId: "retried", status: "passed", retry: 1 },
            { testId: "late", status: "passed", retry: 1 },
            { testId: "late", status: "failed", retry: 0 },
            { testId: "evidence-only", status: "unknown" },
            { testId: "setup", status: "failed", role: "setup" },
            { noId: true },
        ].map((record) => JSON.stringify(record)).join("\n") + "\n{cut short");
        writeFileSync(join(directory, "attempt-x/mcdc.json"), JSON.stringify({ testId: "bun", status: "failed" }));
        writeFileSync(join(directory, "other.txt"), "not evidence");
        assert.deepEqual(Object.fromEntries(outcomes(directory)), {
            jest: "passed", retried: "passed", late: "passed", "evidence-only": "unknown", bun: "failed",
        });
        assert.deepEqual([...outcomes(join(directory, "missing"))], []);
    }
    finally {
        rmSync(directory, { recursive: true, force: true });
    }
});

test("a job's command names its file where the runner takes one, and runs a project's tool without npx", () => {
    const root = mkdtempSync(join(tmpdir(), "supercov-command-"));
    try {
        mkdirSync(join(root, "node_modules/.bin"), { recursive: true });
        writeFileSync(join(root, "node_modules/.bin/mocha"), "");
        assert.deepEqual(commandFor(["bun", "test"], "t/a.test.ts", root), ["bun", "test", "./t/a.test.ts"]);
        assert.deepEqual(commandFor(["/usr/bin/bun", "test", "--bail"], "t/a.test.ts", root), ["/usr/bin/bun", "test", "--bail", "./t/a.test.ts"]);
        assert.deepEqual(commandFor(["bun", "run", "test"], "t/a.test.ts", root), ["bun", "run", "test"]);
        const tool = join(root, "node_modules/.bin/mocha");
        if (process.platform !== "win32") {
            assert.deepEqual(withoutNpx(["npx", "mocha", "--check-leaks"], root), [tool, "--check-leaks"]);
            assert.deepEqual(withoutNpx(["npx", "-y", "mocha"], root), [tool]);
        }
        for (const command of [["npx", "jest"], ["npx", "mocha@10"], ["npx", "@scope/tool"], ["npx", "--package", "x"], ["npx"], ["node", "mocha"], ["npx", "-y"]])
            assert.deepEqual(withoutNpx(command, root), command);
    }
    finally {
        rmSync(root, { recursive: true, force: true });
    }
});
