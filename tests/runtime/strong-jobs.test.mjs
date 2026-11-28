import assert from "node:assert/strict";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, statSync, symlinkSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import test from "node:test";
import { spawn, spawnSync } from "node:child_process";
import { readdirSync } from "node:fs";
import { delimiter } from "node:path";
import { WASM_OUT, avaArguments, commandFor, drive, jestArguments, mochaArguments, outcomes, packageIsCommonJs, slots, turns, vitestArguments, withoutNpx } from "../../runtime/javascript/strongJobs.mjs";

const runner = resolve(import.meta.dirname, "fixtures/strong-jobs-runner.mjs");

// A strong pass's driver over a runner whose tests do what `rules` says
// (fixtures/strong-jobs-runner.mjs), each mutant's verdict by id.
async function pass({ tests, mutants, listed, rules, files, workers = 4, aloneGraceMs }) {
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
            workers,
            environment: { ...process.env, FAKE_SUITE: join(root, "suite.json") },
            graceMs: 1_500,
            baselineGraceMs: 1_500,
            ...(aloneGraceMs === undefined ? {} : { aloneGraceMs }),
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
    test_("v1", "t/v.spec.js", 1, "playwright"),
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
        // b's tests are slow: a job runs only its mutant's own.
        "*": { b1: "slow", b2: "slow,subset-fail", c1: "fail", d1: "hang" },
        [on("kill-a")]: { a2: "fail" },
        [on("kill-b")]: { b1: "slow,fail" },
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
    assert.match(verdicts.elsewhere.reason, /ran under playwright/);
    assert.match(verdicts["partly-elsewhere"].reason, /other candidates ran under playwright/);
    assert.equal(verdicts.nothing.reason, "no-runnable-candidate");
    // b2 alone fails without the mutant too: the kill was no kill, and the
    // whole file let the mutant live.
    assert.equal(summary.overturned, 1);
    assert.ok(summary.confirmations >= 2, JSON.stringify(summary));
    assert.ok(summary.stopped >= 2, JSON.stringify(summary));
});

// A mutant's next file is the one that killed most for its cost so far: once
// the costlier file kills the first mutants, the later ones run it first and
// never need the cheap one that kills nothing.
test("a mutant runs the file likeliest to kill it first, as the pass learns", async () => {
    const tests = [test_("a1", "t/a.test.js", 1), test_("b1", "t/b.test.js", 50)];
    const mutants = Array.from({ length: 20 }, (_, n) => ({ id: `m${n}`, file: "src/a.js", candidates: [{ test: 0, reason: "infected" }, { test: 1, reason: "infected" }] }));
    const listed = mutants.map(({ id }) => ({ id }));
    const rules = Object.fromEntries(listed.map((_, index) => [`src/a.js#${index}`, { b1: "fail" }]));
    const { verdicts, summary } = await pass({ tests, mutants, listed, rules, files: { "t/a.test.js": ["a1"], "t/b.test.js": ["b1"] }, workers: 1 });
    assert.ok(Object.values(verdicts).every((verdict) => verdict.status === "killed"), JSON.stringify(verdicts));
    // Two baselines; the first eight mutants (under way at once) run both
    // files, the twelve after them the killing one alone.
    assert.equal(summary.jobs, 2 + 8 * 2 + 12, JSON.stringify(summary));
});

// A job of a mutant's later batch waits behind every first file's.
test("jobs wait their turn by priority, first come first served among equals", async () => {
    const run = slots(1);
    const order = [];
    let release;
    const first = run(() => new Promise((done) => { release = done; }));
    const queued = [["b", 2], ["c", 0], ["d", 1], ["e", 0]].map(([name, priority]) => run(async () => order.push(name), priority));
    release();
    await Promise.all([first, ...queued]);
    assert.deepEqual(order, ["c", "e", "d", "b"]);
});

// Every job holds a lock the others wait on (Cargo's on the build directory
// where each job builds Rust compiled to wasm): side by side, the last jobs
// wait past a limit set by a baseline that ran alone. Each runs again alone,
// and the verdicts are the ones a pass that ran them one at a time reaches.
// Side by side, the limit's grace is short, so the waits pass it; alone, it
// is the pass's own 10 s: with every test file of the suite running at once,
// a 1 s run alone took more than 3 x 1 s + 1.5 s, and four survivors were
// timeouts.
test("a job past its limit beside others runs again alone before it is a timeout", async () => {
    const tests = [test_("w1", "t/w.test.js"), test_("w2", "t/w.test.js")];
    const mutants = Array.from({ length: 8 }, (_, n) => ({ id: `m${n}`, file: "src/a.js", candidates: [{ test: 0, reason: "infected" }] }));
    const listed = mutants.map(({ id }) => ({ id }));
    const rules = { "*": { w1: "locked" }, "src/a.js#0": { w1: "locked,fail" }, "src/a.js#7": { w1: "locked,fail" } };
    const { verdicts, summary } = await pass({ tests, mutants, listed, rules, files: { "t/w.test.js": ["w1", "w2"] }, workers: 8, aloneGraceMs: 10_000 });
    const status = Object.fromEntries(Object.entries(verdicts).map(([id, verdict]) => [id, verdict.status]));
    assert.deepEqual(status, { m0: "killed", m1: "survived", m2: "survived", m3: "survived", m4: "survived", m5: "survived", m6: "survived", m7: "killed" }, JSON.stringify(summary));
    assert.ok(summary.alone >= 1, JSON.stringify(summary));
});

// The machine slows down once the baselines have run (other work on it):
// every job then takes 2.5 s, past the limit their 1 ms baselines set. One
// job at a time, so none waited on another: each runs again alone, its limit
// from its tests run again without the mutant, and the verdicts are the ones
// a machine as slow throughout reaches. Before, every one was a timeout.
test("a job past its limit on a machine slower than at its baseline is judged at the machine's speed now", async () => {
    const tests = [test_("w1", "t/w.test.js"), test_("w2", "t/w.test.js")];
    const mutants = Array.from({ length: 3 }, (_, n) => ({ id: `m${n}`, file: "src/a.js", candidates: [{ test: 0, reason: "infected" }] }));
    const listed = mutants.map(({ id }) => ({ id }));
    const rules = { "*": { w1: "loaded" }, "src/a.js#0": { w1: "loaded,fail" }, "src/a.js#2": { w1: "hang" } };
    const { verdicts, summary } = await pass({ tests, mutants, listed, rules, files: { "t/w.test.js": ["w1", "w2"] }, workers: 1 });
    const status = Object.fromEntries(Object.entries(verdicts).map(([id, verdict]) => [id, verdict.status]));
    assert.deepEqual(status, { m0: "killed", m1: "survived", m2: "timeout" }, JSON.stringify(summary));
    assert.equal(summary.alone, 3, JSON.stringify(summary));
});

// Once the pass is told to stop, its jobs are stopped under their tests: a
// job that ends then (here, one that marks the stop and exits, as a job the
// stop reached first) did not finish its file, and was taken for a kill.
// Now nothing is judged from then on, and the mutant is left for the next
// pass.
test("a job that ends once the pass is told to stop decides nothing", async () => {
    const tests = [test_("w1", "t/w.test.js")];
    const mutants = [{ id: "m0", file: "src/a.js", candidates: [{ test: 0, reason: "infected" }] }];
    const rules = { "src/a.js#0": { w1: "stops" } };
    const { verdicts, summary } = await pass({ tests, mutants, listed: [{ id: "m0" }], rules, files: { "t/w.test.js": ["w1"] }, workers: 1 });
    assert.deepEqual(verdicts, {}, JSON.stringify(summary));
    assert.ok(summary, "the pass still says what it did");
});

// Alone work waits for the shared work under way, and holds back the work
// that comes after it, first come first served; shared work knows whether it
// had company.
test("work alone waits for the machine to be free", async () => {
    const machine = turns();
    const order = [];
    let release;
    const first = machine.shared((turn) => new Promise((done) => { release = () => done(turn); }));
    const second = machine.shared(async (turn) => { order.push("second"); return turn; });
    const alone = machine.alone(async (turn) => { order.push("alone"); return turn; });
    const late = machine.shared(async (turn) => { order.push("late"); return turn; });
    const otherAlone = machine.alone(async () => order.push("other alone"));
    await second;
    release();
    const [firstTurn, secondTurn, aloneTurn, lateTurn] = await Promise.all([first, second, alone, late, otherAlone]);
    assert.deepEqual(order, ["second", "alone", "late", "other alone"]);
    assert.equal(firstTurn.crowded, true);
    assert.equal(secondTurn.crowded, true);
    assert.equal(aloneTurn.crowded, false);
    assert.equal(lateTurn.crowded, false);
});

// Ctrl-C reaches the driver, not its jobs, which run in process groups of
// their own: they stop with it. And a finished job leaves nothing on disk.
for (const signal of ["SIGINT", "SIGTERM"]) {
    test(`every job stops when the pass gets ${signal}`, async (t) => {
        const root = mkdtempSync(join(tmpdir(), "supercov-strong-jobs-"));
        t.after(() => rmSync(root, { recursive: true, force: true }));
        mkdirSync(join(root, ".supercov/frontend-evidence"), { recursive: true });
        writeFileSync(join(root, ".supercov/frontend-evidence/mutants.json"), JSON.stringify({ files: { "src/a.js": [{ id: "hang" }] } }));
        writeFileSync(join(root, "suite.json"), JSON.stringify({ files: { "t/a.test.js": ["a1"] }, rules: { "src/a.js#0": { a1: "hang" } } }));
        writeFileSync(join(root, "plan.json"), JSON.stringify({ tests: [test_("a1", "t/a.test.js")], mutants: [{ id: "hang", file: "src/a.js", candidates: [{ test: 0, reason: "infected" }] }] }));
        const pids = join(root, "pids");
        const driver = spawn(process.execPath, [resolve(import.meta.dirname, "../../runtime/javascript/strongJobs.mjs"), "--", process.execPath, runner], {
            cwd: root,
            env: { ...process.env, FAKE_SUITE: join(root, "suite.json"), FAKE_PIDS: pids, SUPERCOV_STRONG_PLAN: join(root, "plan.json"), SUPERCOV_STRONG_RESULTS: join(root, "results.jsonl") },
            stdio: "ignore",
        });
        const ended = new Promise((done) => driver.once("exit", (code) => done(code)));
        const started = () => (existsSync(pids) ? readFileSync(pids, "utf8").split("\n").filter(Boolean).map(Number) : []);
        // The baseline's process, then the mutant's, which never ends.
        while (started().length < 2)
            await new Promise((done) => setTimeout(done, 50));
        await new Promise((done) => setTimeout(done, 200));
        // The baseline's job is over: its directory is gone.
        assert.deepEqual(readdirSync(join(root, "results.jsonl.jobs")).filter((name) => /^\d+$/.test(name)), ["2"]);
        driver.kill(signal);
        assert.equal(await ended, signal === "SIGINT" ? 130 : 143);
        const alive = (pid) => {
            try {
                process.kill(pid, 0);
                return true;
            }
            catch {
                return false;
            }
        };
        for (let wait = 0; wait < 40 && started().some(alive); wait += 1)
            await new Promise((done) => setTimeout(done, 50));
        assert.deepEqual(started().filter(alive), []);
    });
}

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
        // A tap test file in JavaScript runs by itself, as tap's runner runs it.
        assert.deepEqual(commandFor(["npx", "tap"], "test/a.js", root, "tap"), [process.execPath, "test/a.js"]);
        assert.deepEqual(commandFor(["npx", "tap"], "test/a.ts", root, "tap"), ["npx", "tap"]);
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

test("a command that starts Jest or Mocha itself names its arguments to them", () => {
    assert.deepEqual(jestArguments(["/p/node_modules/.bin/jest", "--env", "node"]), ["--env", "node"]);
    assert.deepEqual(jestArguments(["node", "node_modules/jest-cli/bin/jest.js", "-c", "x.js"]), ["-c", "x.js"]);
    assert.equal(jestArguments(["npm", "test"]), undefined);
    assert.deepEqual(mochaArguments(["/p/node_modules/.bin/mocha", "--check-leaks"]), ["--check-leaks"]);
    assert.deepEqual(mochaArguments(["node", "node_modules/mocha/bin/_mocha"]), []);
    assert.equal(mochaArguments(["npm", "test"]), undefined);
    assert.deepEqual(vitestArguments(["/p/node_modules/.bin/vitest", "run", "--project", "browser"]), ["run", "--project", "browser"]);
    assert.deepEqual(vitestArguments(["node", "node_modules/vitest/vitest.mjs", "run"]), ["run"]);
    assert.equal(vitestArguments(["npm", "test"]), undefined);
    assert.deepEqual(avaArguments(["/p/node_modules/.bin/ava", "--serial"]), ["--serial"]);
    assert.deepEqual(avaArguments(["node", "node_modules/ava/entrypoints/cli.mjs"]), []);
    assert.equal(avaArguments(["npm", "test"]), undefined);
    assert.equal(avaArguments(["node", "java.js"]), undefined);
    const root = mkdtempSync(join(tmpdir(), "supercov-package-"));
    try {
        assert.equal(packageIsCommonJs(root), true);
        writeFileSync(join(root, "package.json"), '{"type":"module"}');
        assert.equal(packageIsCommonJs(root), false);
        writeFileSync(join(root, "package.json"), '{"type":"commonjs"}');
        assert.equal(packageIsCommonJs(root), true);
    }
    finally {
        rmSync(root, { recursive: true, force: true });
    }
});

// The wasm output wrapper (strongWasmOut.cjs, embedded in strongJobs.mjs):
// the tool writes elsewhere, each file is renamed into place only where it
// changed, and the wrapper notes that it ran.
test("a rebuild of the same wasm glue changes nothing in place", () => {
  assert.equal(WASM_OUT, readFileSync(resolve(import.meta.dirname, "../../runtime/javascript/strongWasmOut.cjs"), "utf8"));
  const root = mkdtempSync(join(tmpdir(), "supercov-wasm-out-"));
  try {
    const real = join(root, "real");
    const wrappers = join(root, "wrappers");
    mkdirSync(real);
    mkdirSync(wrappers);
    // A stand-in wasm-bindgen: writes its glue, and a snippet, into --out-dir.
    writeFileSync(join(real, "wasm-bindgen"), `#!/bin/sh\nwhile [ "$1" != "--out-dir" ]; do shift; done\nmkdir -p "$2/snippets"\nprintf "glue $GLUE" > "$2/glue.js"\nprintf snippet > "$2/snippets/a.js"\n`, { mode: 0o755 });
    writeFileSync(join(wrappers, "wasm-bindgen"), WASM_OUT, { mode: 0o755 });
    const build = (glue) => spawnSync(join(wrappers, "wasm-bindgen"), ["--target", "nodejs", "--out-dir", "pkg", "a.wasm"], {
      cwd: root,
      env: { ...process.env, GLUE: glue, PATH: `${wrappers}${delimiter}${real}${delimiter}${process.env.PATH}` },
      encoding: "utf8",
    });
    assert.equal(build("one").status, 0);
    assert.equal(readFileSync(join(root, "pkg/glue.js"), "utf8"), "glue one");
    assert.equal(readFileSync(join(root, "pkg/snippets/a.js"), "utf8"), "snippet");
    assert.ok(existsSync(join(wrappers, "used")));
    const before = statSync(join(root, "pkg/glue.js")).ino;
    // The same glue again: the file in place is the one there before.
    assert.equal(build("one").status, 0);
    assert.equal(statSync(join(root, "pkg/glue.js")).ino, before);
    // Other glue: renamed into place.
    assert.equal(build("two").status, 0);
    assert.equal(readFileSync(join(root, "pkg/glue.js"), "utf8"), "glue two");
    assert.deepEqual(spawnSync("ls", [root], { encoding: "utf8" }).stdout.split("\n").filter((name) => name.includes("supercov")), []);
  }
  finally {
    rmSync(root, { recursive: true, force: true });
  }
});
