import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { existsSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { key, writeJson } from "../../runtime/javascript/playwrightStrong.mjs";

const reporter = new URL("../../runtime/javascript/playwrightStrongReporter.mjs", import.meta.url).pathname;

/**
 * A round's judge over the files a round leaves (`work`: its pass.json;
 * `attempts`; `reached`: the mutants a request carried to its server;
 * `everywhere`: where a server started with a mutant ran it; `errors`
 * outside any test; `stoppedAt`, where a budget stopped it; `started`, that
 * a round of the pass began a test with no mutant on everywhere), run as
 * the engine runs it: its records, and what it said.
 */
function judge({ name = "main", work, attempts = [], reached = [], everywhere = [], errors = [], stoppedAt, claimed = {}, started = true }) {
  const directory = mkdtempSync(join(tmpdir(), "supercov-judge-"));
  try {
    const results = join(directory, "results.jsonl");
    const round = join(directory, "playwright", name);
    writeFileSync(join(directory, "plan.json"), "{}");
    writeJson(`${round}/pass.json`, { immediate: [], restarts: [], deferred: [], otherRunners: {}, setupOnly: {}, concurrency: { serialFiles: [] }, timeoutFactor: 1.5, tests: {}, ...work });
    for (const test of Object.keys(work.tests ?? {}))
      writeJson(`${directory}/playwright/baseline/${key(test)}`, "passed");
    if (started)
      writeJson(`${directory}/playwright/started-without-mutant`, 1_000);
    attempts.forEach((attempt, at) => writeJson(`${round}/attempts/${at}.json`, { kind: "mutant", invocation: "1", startedAt: 1_000, endedAt: 2_000, ...attempt }));
    writeFileSync(`${results}.reached.jsonl`, reached.map((mutant) => `${JSON.stringify({ mutant })}\n`).join(""));
    writeFileSync(`${results}.everywhere.jsonl`, everywhere.map((line) => `${JSON.stringify(line)}\n`).join(""));
    errors.forEach((error, at) => writeJson(`${round}/errors/${at}.json`, error));
    for (const [invocation, id] of Object.entries(claimed))
      writeJson(`${round}/invocations/${invocation}.json`, { id });
    if (stoppedAt !== undefined)
      writeFileSync(join(directory, "stopped-at"), String(stoppedAt));
    const restart = name === "main" ? undefined : join(directory, `${name}.json`);
    const ran = spawnSync(process.execPath, ["--input-type=module", "-e", `import(${JSON.stringify(reporter)}).then((m) => m.judge())`], {
      encoding: "utf8",
      env: {
        ...process.env,
        SUPERCOV_STRONG_PLAN: join(directory, "plan.json"),
        SUPERCOV_STRONG_RESULTS: results,
        ...(restart ? { SUPERCOV_STRONG_RESTART: restart } : {}),
        ...(stoppedAt !== undefined ? { SUPERCOV_STRONG_STOPPED: "1" } : {}),
      },
    });
    assert.equal(ran.status, 0, ran.stderr);
    const records = readFileSync(results, "utf8").split("\n").filter(Boolean).map((line) => JSON.parse(line));
    return {
      verdicts: records.filter((record) => record.status),
      progress: records.filter((record) => record.progress),
      restarts: records.filter((record) => record.restart).map((record) => record.restart),
      said: ran.stderr,
    };
  }
  finally {
    rmSync(directory, { recursive: true, force: true });
  }
}

const tests = { t1: { planId: "p1", jobs: 1 }, t2: { planId: "p2", jobs: 1 } };
const passed = (id, original) => ({ id, original, outcome: "passed" });

test("a survivor no attempt ran on its server records no progress, and runs alone with every candidate", () => {
  // `m` ran in its candidates' server when measured, and no request of its
  // attempts carried it there; `n` ran in one.
  const { verdicts, progress, restarts, said } = judge({
    work: {
      kind: "main",
      tests,
      entries: {
        m: { id: "m", mutant: "a.js#1", tests: ["t1", "t2"], serverRan: true },
        n: { id: "n", mutant: "a.js#2", tests: ["t1", "t2"], serverRan: true },
      },
    },
    attempts: [passed("m", "t1"), passed("m", "t2"), passed("n", "t1"), passed("n", "t2")],
    reached: ["a.js#2"],
  });
  assert.deepEqual(verdicts.map((verdict) => [verdict.id, verdict.status]), [["n", "survived"]]);
  // Its passes passed without it: not its progress, which a later pass
  // would take for a survivor.
  assert.deepEqual(progress, []);
  assert.deepEqual(restarts.map((job) => [job.kind, job.batch, job.mutants.map((entry) => [entry.id, entry.tests, entry.reason])]), [["alone", true, [["m", ["t1", "t2"], "unreached"]]]]);
  assert.match(said, /warning: 1 mutant\(s\) lived through every candidate test on the shared server, though no attempt ran them there/);
});

test("a mutant left unfinished that no attempt ran yet records no progress, and its next round runs every candidate", () => {
  const { progress, restarts } = judge({
    work: {
      kind: "main",
      tests,
      entries: {
        m: { id: "m", mutant: "a.js#1", tests: ["t1", "t2"], serverRan: true },
        n: { id: "n", mutant: "a.js#2", tests: ["t1", "t2"], serverRan: true },
        o: { id: "o", mutant: "a.js#3", tests: ["t1", "t2"] },
      },
    },
    attempts: [passed("m", "t1"), passed("n", "t1"), passed("o", "t1")],
    reached: ["a.js#2"],
  });
  // `n` ran, and `o` runs in any request: their passes count.
  assert.deepEqual(progress.map((record) => [record.id, record.passed]), [["n", ["p1"]], ["o", ["p1"]]]);
  assert.deepEqual(restarts.map((job) => [job.kind, job.mutants.map((entry) => [entry.id, entry.tests])]), [["rest", [["m", ["t1", "t2"]], ["n", ["t2"]], ["o", ["t2"]]]]]);
});

test("a budget's stop leaves a mutant no attempt ran without a verdict or progress", () => {
  const work = {
    kind: "main",
    tests,
    entries: { m: { id: "m", mutant: "a.js#1", tests: ["t1", "t2"], serverRan: true } },
  };
  const stopped = judge({ work, attempts: [passed("m", "t1"), passed("m", "t2")], stoppedAt: 5_000 });
  assert.deepEqual(stopped.verdicts, []);
  assert.deepEqual(stopped.progress, []);
  // Its round alone, stopped before any test: no verdict, nothing recorded.
  const alone = judge({
    name: "restart-0",
    work: { kind: "alone", batch: true, tests, entries: { m: { id: "m", mutant: "a.js#1", tests: ["t1", "t2"], reason: "unreached", serverRan: true } } },
    claimed: { 1: "m" },
    stoppedAt: 5_000,
  });
  assert.deepEqual(alone.verdicts, []);
  assert.deepEqual(alone.progress, []);
});

test("alone, an error outside any test kills its mutant only where it came before the round began to stop", () => {
  const work = { kind: "alone", tests, entries: { m: { id: "m", mutant: "a.js#1", tests: ["t1"], reason: "background" } } };
  const after = judge({ name: "restart-0", work, errors: [{ at: 6_000, message: "the web server exited" }], stoppedAt: 5_000 });
  assert.deepEqual(after.verdicts, []);
  const before = judge({ name: "restart-0", work, errors: [{ at: 4_000, message: "the web server exited" }], stoppedAt: 5_000 });
  assert.deepEqual(before.verdicts.map((verdict) => [verdict.id, verdict.status]), [["m", "killed"]]);
  assert.match(before.verdicts[0].reason, /^the command failed with it switched on \(the web server exited\)/);
});

// Where no round of the pass got as far as a test without a mutant (the
// pass's copy does not build, so its web server never starts), a command
// that fails with the mutant switched on says nothing of the mutant: no
// verdict, never a kill. On a Next.js suite whose switched copy did not
// build, five mutants run alone were `killed` this way.
test("alone, an error outside any test is no kill where the command never started without a mutant", () => {
  const work = { kind: "alone", tests, entries: { m: { id: "m", mutant: "a.js#1", tests: ["t1"], reason: "module-level" } } };
  const never = judge({ name: "restart-0", work, errors: [{ at: 4_000, message: "Error: Process from config.webServer was not able to start. Exit code: 1" }], stoppedAt: 5_000, started: false });
  assert.deepEqual(never.verdicts.map((verdict) => [verdict.id, verdict.status]), [["m", "error"]]);
  assert.match(never.verdicts[0].reason, /no run of it in this pass got as far as a test without a mutant/);
});

// An invocation whose command failed before any test began (its web server
// would not start) ends failed, so the pass records the failure and shows
// and keeps the command's output; one whose tests ran ends passed whatever
// they did, as a killed mutant fails its test by design. Before, both ended
// passed, and a server that never started left only "its tests did not
// finish" behind.
test("an invocation that failed before any test began ends failed; one whose tests ran ends passed", async () => {
  const { default: Reporter } = await import(reporter);
  const directory = mkdtempSync(join(tmpdir(), "supercov-judge-end-"));
  try {
    const ending = ({ erred, begun }) => {
      const one = Object.create(Reporter.prototype);
      Object.assign(one, { pass: { shared: join(directory, "playwright"), round: join(directory, "playwright", "main") }, work: { kind: "main" }, running: new Map(), erred, begun });
      one.writeStatus = () => { };
      return one.onEnd();
    };
    assert.equal(ending({ erred: true, begun: false }), undefined);
    assert.deepEqual(ending({ erred: true, begun: true }), { status: "passed" });
    assert.deepEqual(ending({ erred: false, begun: false }), { status: "passed" });
  }
  finally {
    rmSync(directory, { recursive: true, force: true });
  }
});

// A test that began in a round with no mutant on everywhere marks that the
// command starts without one; a test of a round alone does not.
test("a test that began without a mutant on everywhere marks the command as starting", async () => {
  const { default: Reporter } = await import(reporter);
  for (const [kind, marked] of [["main", true], ["rest", true], ["alone", false]]) {
    const directory = mkdtempSync(join(tmpdir(), "supercov-judge-begin-"));
    try {
      const one = Object.create(Reporter.prototype);
      Object.assign(one, { pass: { shared: join(directory, "playwright"), round: join(directory, "playwright", kind) }, work: { kind }, running: new Map() });
      one.writeStatus = () => { };
      one.onTestBegin({ id: "t", title: "t" });
      assert.equal(existsSync(join(directory, "playwright", "started-without-mutant")), marked, kind);
    }
    finally {
      rmSync(directory, { recursive: true, force: true });
    }
  }
});
