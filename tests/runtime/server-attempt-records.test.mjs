// A server's records of a test's requests (`server/attempts/<attempt>.jsonl`):
// the first hits of each request's phase, written through a descriptor kept
// open, byte for byte as they were written when every turn opened the file.
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { mkdtempSync, readFileSync, readdirSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { resolve } from "node:path";
import { pathToFileURL } from "node:url";
import test from "node:test";

const runtime = pathToFileURL(resolve("runtime/javascript/runtime.mjs")).href;

// Runs `body` in a child with the runtime as `runtime`, a handler wrapped as
// a server's request handler (`serve(cookie, work)` serves one request whose
// cookies name `attempt` and `phase` and runs `work` in it), a fixed clock,
// and a probe file of eight points as `file`.
function child(root, body, env = {}) {
  const result = spawnSync(
    process.execPath,
    [
      "--input-type=module",
      "--eval",
      `let tick = 1700000000000;
       Date.now = () => tick++;
       const runtime = await import(${JSON.stringify(runtime)});
       const fs = process.getBuiltinModule("node:fs");
       const file = runtime.registerProbeV2({ key: "00000000000000aa", points: 8, decisionWidths: [2], defaultCount: 0, selectionPoints: [8, 0], selectionTrees: [] });
       const handler = runtime.withRequestPhase(async (request, work) => { await null; work(); await null; }, "request");
       const cookie = (attempt, phase) => {
         const scope = new URLSearchParams({ v: "1", r: "run-records", w: "worker", t: "test-" + attempt, k: "key", a: "0", i: attempt });
         return "__supercov_scope=" + encodeURIComponent(scope.toString()) + "; __supercov_phase=" + encodeURIComponent(attempt + ":phase:" + phase);
       };
       const serve = (attempt, phase, work) => handler({ headers: { cookie: cookie(attempt, phase) } }, work);
       const turn = () => new Promise((settle) => setImmediate(settle));
       ${body}`,
    ],
    {
      cwd: process.cwd(),
      env: { ...process.env, SUPERCOV_RUN_ID: "run-records", SUPERCOV_SERVER_EVIDENCE_ROOT: root, ...env },
      encoding: "utf8",
    },
  );
  assert.equal(result.status, 0, result.stderr);
  return result.stdout;
}

const attemptFile = (root, attempt) => resolve(root, "run-records", "attempts", `${attempt}.jsonl`);

test("an attempt's records are the lines appendServer wrote, in order, a hit's on the fast path too", () => {
  const root = mkdtempSync(resolve(tmpdir(), "supercov-attempt-records-"));
  try {
    child(
      root,
      `await serve("a1", 1, () => { file.hit(0); file.hit(1); file.hit(0); runtime.mcdcEndV2(file, 0, 1 + 1 + 3, true); });
       await serve("a1", 1, () => { file.hit(1); file.hit(2); });
       await serve("a1", 2, () => { file.hit(0); runtime.coverageHit("plain-id"); runtime.coverageHit("plain-id"); });
       await turn();`,
    );
    const scope = { version: 1, runId: "run-records", workerId: "worker", testId: "test-a1", testKey: "key", retry: 0, attemptId: "a1" };
    // Each phase's first hits once, its decision vector once, the scope said
    // once at the top of the file.
    assert.equal(
      readFileSync(attemptFile(root, "a1"), "utf8"),
      [
        { type: "values", scope },
        { type: "hit", id: "~p00000000000000aa.0", timestampMs: 1700000000000, phaseId: "a1:phase:1" },
        { type: "hit", id: "~p00000000000000aa.1", timestampMs: 1700000000001, phaseId: "a1:phase:1" },
        { type: "decision", id: "~d00000000000000aa.0", vector: { values: [true, false], outcome: true }, timestampMs: 1700000000002, phaseId: "a1:phase:1" },
        { type: "hit", id: "~p00000000000000aa.2", timestampMs: 1700000000003, phaseId: "a1:phase:1" },
        { type: "hit", id: "~p00000000000000aa.0", timestampMs: 1700000000004, phaseId: "a1:phase:2" },
        { type: "hit", id: "plain-id", timestampMs: 1700000000005, phaseId: "a1:phase:2" },
      ]
        .map((record) => `${JSON.stringify(record)}\n`)
        .join(""),
    );
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("an attempt's file removed while the server holds it is written again, not into the removed one", () => {
  const root = mkdtempSync(resolve(tmpdir(), "supercov-attempt-removed-"));
  try {
    child(
      root,
      `await serve("a1", 1, () => file.hit(0));
       await turn();
       fs.rmSync(${JSON.stringify(attemptFile(root, "a1"))});
       await serve("a1", 2, () => file.hit(1));
       await turn();`,
    );
    const records = readFileSync(attemptFile(root, "a1"), "utf8").trim().split("\n").map((line) => JSON.parse(line));
    assert.deepEqual(records.map((record) => [record.id, record.phaseId]), [["~p00000000000000aa.1", "a1:phase:2"]]);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("a server keeps a few attempts' files open, opens each once, and opens one again after a failed write", () => {
  const root = mkdtempSync(resolve(tmpdir(), "supercov-attempt-descriptors-"));
  try {
    const out = child(
      root,
      `const opened = [];
       const openSync = fs.openSync, writeSync = fs.writeSync, closeSync = fs.closeSync;
       let closes = 0, failNext = false;
       fs.openSync = function (...args) { const fd = openSync.apply(this, args); if (String(args[0]).includes("/attempts/")) opened.push(String(args[0]).split("/").pop()); return fd; };
       fs.closeSync = function (...args) { closes += 1; return closeSync.apply(this, args); };
       fs.writeSync = function (...args) { if (failNext) { failNext = false; throw Object.assign(new Error("gone"), { code: "EBADF" }); } return writeSync.apply(this, args); };
       // Forty attempts, one request each, then the first ten again: the
       // oldest were closed once the server held thirty-two.
       for (let attempt = 0; attempt < 40; attempt += 1) await serve("a" + attempt, 1, () => file.hit(0));
       for (let attempt = 0; attempt < 10; attempt += 1) await serve("a" + attempt, 2, () => file.hit(1));
       // The second request of a39 finds its descriptor open, and writes through it.
       await serve("a39", 2, () => file.hit(2));
       // A write that fails: its descriptor is closed, and the file opened again.
       failNext = true;
       await serve("a39", 3, () => file.hit(3));
       await turn();
       process.stdout.write(JSON.stringify({ opened, closes }));`,
    );
    const { opened, closes } = JSON.parse(out);
    const count = (name) => opened.filter((file) => file === name).length;
    assert.equal(count("a39.jsonl"), 2, out);
    assert.equal(count("a0.jsonl"), 2, out);
    assert.equal(count("a20.jsonl"), 1, out);
    assert.ok(closes >= 18, out);
    const ids = readFileSync(attemptFile(root, "a39"), "utf8").trim().split("\n").slice(1).map((line) => JSON.parse(line).id);
    assert.deepEqual(ids, ["~p00000000000000aa.0", "~p00000000000000aa.2", "~p00000000000000aa.3"]);
    assert.equal(readdirSync(resolve(root, "run-records", "attempts")).length, 40);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("on Windows, where a held file cannot be removed, each turn appends by path", () => {
  const root = mkdtempSync(resolve(tmpdir(), "supercov-attempt-windows-"));
  try {
    const out = child(
      root,
      `Object.defineProperty(process, "platform", { value: "win32" });
       const openSync = fs.openSync, appendFileSync = fs.appendFileSync;
       let opens = 0, appends = 0;
       fs.openSync = function (...args) { if (String(args[0]).includes("/attempts/")) opens += 1; return openSync.apply(this, args); };
       fs.appendFileSync = function (...args) { if (String(args[0]).includes("/attempts/")) appends += 1; return appendFileSync.apply(this, args); };
       await serve("a1", 1, () => file.hit(0));
       await serve("a1", 2, () => file.hit(0));
       process.stdout.write(JSON.stringify({ opens, appends }));`,
    );
    assert.deepEqual(JSON.parse(out), { opens: 0, appends: 2 });
    assert.equal(readFileSync(attemptFile(root, "a1"), "utf8").trim().split("\n").length, 3);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("the evidence root is read once per environment object", () => {
  const root = mkdtempSync(resolve(tmpdir(), "supercov-attempt-root-"));
  const other = mkdtempSync(resolve(tmpdir(), "supercov-attempt-root-other-"));
  try {
    child(
      root,
      `await serve("a1", 1, () => file.hit(0));
       // A suite that gives the process another environment object is read again.
       process.env = { ...process.env, SUPERCOV_SERVER_EVIDENCE_ROOT: ${JSON.stringify(other)} };
       await serve("a2", 1, () => file.hit(0));
       await turn();`,
    );
    assert.deepEqual(readdirSync(resolve(root, "run-records", "attempts")), ["a1.jsonl"]);
    assert.deepEqual(readdirSync(resolve(other, "run-records", "attempts")), ["a2.jsonl"]);
  } finally {
    rmSync(root, { recursive: true, force: true });
    rmSync(other, { recursive: true, force: true });
  }
});
