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
