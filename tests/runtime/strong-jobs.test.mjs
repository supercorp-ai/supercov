import assert from "node:assert/strict";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, statSync, symlinkSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import test from "node:test";
import { spawnSync } from "node:child_process";
import { delimiter } from "node:path";
import { WASM_OUT, commandFor, drive, jestArguments, mochaArguments, outcomes, packageIsCommonJs, withoutNpx } from "../../runtime/javascript/strongJobs.mjs";

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
