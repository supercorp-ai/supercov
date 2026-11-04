import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { resolve } from "node:path";
import { pathToFileURL } from "node:url";
import test from "node:test";

const runtimeUrl = pathToFileURL(resolve(import.meta.dirname, "../../runtime/javascript/runtime.mjs")).href;

// Each case runs in its own process: the runtime keeps its state on the
// process's global, and one of them pollutes Object.prototype.
function run(body) {
  const result = spawnSync(process.execPath, ["--input-type=module", "-e", `
    const runtime = await import(${JSON.stringify(runtimeUrl)});
    runtime.enableRuntimeSnapshotEvidence();
    const scope = (attempt) => ({ version: 1, runId: "run", workerId: "worker", testId: attempt, testKey: attempt, retry: 0, attemptId: attempt + "-0" });
    ${body}
  `], { encoding: "utf8", env: { ...process.env, SUPERCOV_RUN_ID: "run" } });
  assert.equal(result.status, 0, result.stderr);
  return JSON.parse(result.stdout.trim().split("\n").at(-1));
}

test("recording survives a polluted Object.prototype", () => {
  // axios's own tests set Object.prototype.get to prove axios survives it.
  // The runtime then threw "Getter must be a function" from every probe, and
  // seven tests failed that pass without Supercov.
  const recorded = run(`
    runtime.activateCoverageScope(scope("a"));
    runtime.resetCoverage("a");
    Object.prototype.get = "attacker";
    Object.prototype.set = "attacker";
    try {
      runtime.coverageHit("before");
      runtime.withNodeAssertionPhase("assert.ok", "t.js:1:1", () => runtime.coverageHit("inside"));
      const snapshot = runtime.coverageSnapshot();
      console.log(JSON.stringify({ hits: [...snapshot.hits].sort(), events: snapshot.events.length }));
    } finally {
      delete Object.prototype.get;
      delete Object.prototype.set;
    }
  `);
  assert.deepEqual(recorded.hits, ["before", "inside"]);
  assert.ok(recorded.events >= 2);
});

test("work an earlier test left running does not carry its phase into the next test", () => {
  // axios's HTTP/2 upload tests leave a stream settling after they end. It
  // kept the ended test's assertion phase, the next test's evidence named a
  // phase it never reported, and the whole run could not be read.
  const recorded = run(`
    runtime.activateCoverageScope(scope("a"));
    runtime.resetCoverage("a");
    let later;
    runtime.withNodeAssertionPhase("assert.ok", "t.js:1:1", () => {
      later = new Promise((done) => setTimeout(() => { runtime.coverageHit("late"); done(); }, 20));
      runtime.coverageHit("own");
      return true;
    });
    const own = runtime.coverageSnapshot().events.filter((event) => event.id === "own").map((event) => event.phaseId ?? null);
    runtime.activateCoverageScope(scope("b"));
    runtime.resetCoverage("b");
    await later;
    const late = runtime.coverageSnapshot().events.filter((event) => event.id === "late").map((event) => event.phaseId ?? null);
    console.log(JSON.stringify({ own, late }));
  `);
  assert.deepEqual(recorded.own, ["a-0:assertion:1"], "a test's own phase is kept");
  assert.deepEqual(recorded.late, [null], "an earlier test's phase is not");
});

test("recording survives a test that removes Buffer", () => {
  // axios's toFormData test sets `globalThis.Buffer = undefined` to check it
  // copes; the runtime and the evidence journal read Buffer inside that test.
  const atomicUrl = pathToFileURL(resolve(import.meta.dirname, "../../runtime/javascript/atomic.mjs")).href;
  const recorded = run(`
    const { appendEvidenceRecord } = await import(${JSON.stringify(atomicUrl)});
    const { mkdtempSync, readdirSync, rmSync } = await import("node:fs");
    const { tmpdir } = await import("node:os");
    const directory = mkdtempSync(tmpdir() + "/supercov-buffer-");
    runtime.activateCoverageScope(scope("a"));
    runtime.resetCoverage("a");
    const original = globalThis.Buffer;
    globalThis.Buffer = undefined;
    let carrier;
    try {
      runtime.withNodeAssertionPhase("assert.throws", "t.js:1:1", () => runtime.coverageHit("inside"));
      carrier = runtime.coverageContextEnvironment();
      appendEvidenceRecord(directory, "vitest-worker", { testId: "a" });
      appendEvidenceRecord(directory, "vitest-worker", { testId: "b" });
    } finally {
      globalThis.Buffer = original;
    }
    const journals = readdirSync(directory).length;
    rmSync(directory, { recursive: true, force: true });
    console.log(JSON.stringify({ hits: [...runtime.coverageSnapshot().hits], carrier: Object.keys(carrier), journals }));
  `);
  assert.deepEqual(recorded.hits, ["inside"]);
  assert.deepEqual(recorded.carrier, ["SUPERCOV_CONTEXT"]);
  assert.equal(recorded.journals, 1, "both records went to one journal");
});

test("a promisified exec or execFile still resolves stdout and stderr, and carries the test", () => {
  // commander's tests run their fixtures through util.promisify(execFile) and
  // read { stdout }. Node gives exec and execFile their own promisified form;
  // the runtime's child-process wrappers lacked it, so the promise resolved to
  // the bare stdout string and 38 tests read undefined.
  const recorded = run(`
    const child = await import("node:child_process");
    const { promisify } = await import("node:util");
    runtime.activateCoverageScope(scope("a"));
    const script = "process.stdout.write(process.env.SUPERCOV_CONTEXT ? 'carried' : 'lost'); process.stderr.write('err')";
    const file = await promisify(child.execFile)(process.execPath, ["-e", script]);
    const shell = await promisify(child.exec)(JSON.stringify(process.execPath) + " -e " + JSON.stringify(script));
    const failed = await promisify(child.execFile)(process.execPath, ["-e", "process.stdout.write('out'); process.exit(3)"]).catch((error) => ({ code: error.code, stdout: error.stdout }));
    const pending = promisify(child.execFile)(process.execPath, ["-e", ""]);
    const hasChild = typeof pending.child?.pid === "number";
    await pending;
    console.log(JSON.stringify({ file, shell, failed, hasChild }));
  `);
  assert.deepEqual(recorded.file, { stdout: "carried", stderr: "err" });
  assert.deepEqual(recorded.shell, { stdout: "carried", stderr: "err" });
  assert.deepEqual(recorded.failed, { code: 3, stdout: "out" });
  assert.equal(recorded.hasChild, true);
});

test("a child given its own environment gets only that and the test's carrier", () => {
  // chalk's fixtures run with execa's extendEnv: false so the CI running the
  // tests cannot leak into them. The runtime spread its own environment under
  // the one passed, CI=1 reached the fixture, and two tests saw level 1 for 3.
  const recorded = run(`
    const child = await import("node:child_process");
    runtime.activateCoverageScope(scope("a"));
    process.env.SUPERCOV_TEST_LEAK = "parent";
    const script = "process.stdout.write(JSON.stringify({ leak: process.env.SUPERCOV_TEST_LEAK ?? null, own: process.env.OWN ?? null, carried: Boolean(process.env.SUPERCOV_CONTEXT) }))";
    const given = JSON.parse(child.execFileSync(process.execPath, ["-e", script], { env: { OWN: "1" }, encoding: "utf8" }));
    const inherited = JSON.parse(child.execFileSync(process.execPath, ["-e", script], { encoding: "utf8" }));
    console.log(JSON.stringify({ given, inherited }));
  `);
  assert.deepEqual(recorded.given, { leak: null, own: "1", carried: true });
  assert.deepEqual(recorded.inherited, { leak: "parent", own: null, carried: true });
});

test("the runtime a bundled page carries parses as ES2019", async () => {
  // A browser runner with no Supercov adapter bundles each source with the
  // runtime inlined, and browserify parses up to ES2020: debug's karma suite
  // failed on the runtime's own `??=` before any test ran. Webpack depends on
  // acorn itself, so it resolves from there.
  const { createRequire } = await import("node:module");
  const { readFileSync } = await import("node:fs");
  const acorn = createRequire(createRequire(import.meta.url).resolve("webpack"))("acorn");
  const source = readFileSync(new URL(runtimeUrl), "utf8");
  assert.doesNotThrow(() => acorn.parse(source, { ecmaVersion: 2019, sourceType: "module" }));
});

test("a probe hit again in the same context is not recorded again", () => {
  // Before any async callback begins, the probe clock had no epoch (NaN),
  // which no epoch equals: every repeat at a script's top level built and
  // serialized its record again. A million-iteration lru-cache loop took
  // 177 s under Supercov and 0.1 s without it. Date.now is read only for a
  // record about to be written.
  const recorded = run(`
    const file = runtime.registerProbeV2({ pointIds: ["statement"], decisions: [] });
    const realNow = Date.now;
    let reads = 0;
    Date.now = () => { reads += 1; return realNow(); };
    for (let i = 0; i < 1000; i++) {
      runtime.coverageHitV2(file, 0);
      runtime.optionalSelect("short", "continued", i);
    }
    Date.now = realNow;
    console.log(JSON.stringify({ reads }));
  `);
  assert.ok(recorded.reads <= 4, `recorded ${recorded.reads} times`);
});

test("interleaved requests each keep their own phase, before and after a reset", () => {
  // The async hook reuses the epoch each request's context was given; a
  // reset clears the epochs, and a context made before it must not keep one
  // that now belongs to another context.
  const recorded = run(`
    const file = runtime.registerProbeV2({ pointIds: ["first", "second"], decisions: [] });
    runtime.activateCoverageScope(scope("a"));
    runtime.resetCoverage("a");
    const tick = () => new Promise((done) => setTimeout(done, 1));
    const request = (phaseId, point, rounds) => runtime.withCoverageCarrier({ version: 1, scope: scope("a"), phaseId }, async () => {
      for (let round = 0; round < rounds; round++) {
        await tick();
        runtime.coverageHitV2(file, point);
      }
    });
    const phasesOf = (id) => runtime.coverageSnapshot().events.filter((event) => event.id === id).map((event) => event.phaseId).sort();
    await Promise.all([request("a-0:phase:1", 0, 3), request("a-0:phase:2", 0, 3), request("a-0:phase:2", 1, 2)]);
    const before = { first: phasesOf("first"), second: phasesOf("second") };
    let resume;
    const paused = new Promise((done) => { resume = done; });
    const crossing = runtime.withCoverageCarrier({ version: 1, scope: scope("a"), phaseId: "a-0:phase:3" }, async () => {
      await tick();
      runtime.coverageHitV2(file, 1);
      await paused;
      runtime.coverageHitV2(file, 1);
    });
    await tick(); await tick();
    runtime.resetCoverage("a");
    resume();
    await crossing;
    console.log(JSON.stringify({ before, after: phasesOf("second") }));
  `);
  assert.deepEqual(recorded.before.first, ["a-0:phase:1", "a-0:phase:2"]);
  assert.deepEqual(recorded.before.second, ["a-0:phase:2"]);
  assert.deepEqual(recorded.after, ["a-0:phase:3"], "a hit after a reset is recorded again");
});

test("a reset from another copy of the runtime reaches the async hook's epochs", () => {
  // A bundle can carry its own copy of the runtime. The copies share one
  // state, and the async hook is the first copy's: a reset from another copy
  // has to clear the epochs that hook keeps for each request.
  const recorded = run(`
    // The first copy records first, and so installs the hook.
    runtime.coverageHitV2(runtime.registerProbeV2({ pointIds: ["first"], decisions: [] }), 0);
    const other = await import(${JSON.stringify(runtimeUrl + "?copy")});
    const file = other.registerProbeV2({ pointIds: ["point"], decisions: [] });
    other.activateCoverageScope(scope("a"));
    other.resetCoverage("a");
    const tick = () => new Promise((done) => setTimeout(done, 1));
    let resume;
    const paused = new Promise((done) => { resume = done; });
    const crossing = other.withCoverageCarrier({ version: 1, scope: scope("a"), phaseId: "a-0:phase:1" }, async () => {
      await tick();
      other.coverageHitV2(file, 0);
      await paused;
      other.coverageHitV2(file, 0);
    });
    await tick(); await tick();
    other.resetCoverage("a");
    resume();
    await crossing;
    console.log(JSON.stringify(other.coverageSnapshot().events.filter((event) => event.id === "point").map((event) => event.phaseId)));
  `);
  assert.deepEqual(recorded, ["a-0:phase:1"], "a hit after the other copy's reset is recorded again");
});

test("work outside any request is recorded again for each test", () => {
  // Work no request owns is keyed by the test running when it runs: a timer
  // the server set records again, at a new epoch, for each new test.
  const recorded = run(`
    delete process.env.SUPERCOV_RUN_ID;
    const file = runtime.registerProbeV2({ pointIds: ["background"], decisions: [] });
    const tick = () => new Promise((done) => setTimeout(done, 1));
    const epochs = [];
    globalThis.__SUPERCOV_MCDC_TEST_ID__ = "first";
    await tick();
    runtime.coverageHitV2(file, 0);
    epochs.push(file.hitEpochs[0]);
    globalThis.__SUPERCOV_MCDC_TEST_ID__ = "second";
    await tick();
    runtime.coverageHitV2(file, 0);
    epochs.push(file.hitEpochs[0]);
    console.log(JSON.stringify(epochs));
  `);
  assert.notEqual(recorded[1], recorded[0], "the second test's hit is recorded at its own epoch");
});
