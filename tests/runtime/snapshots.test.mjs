import assert from "node:assert/strict";
import { resolve } from "node:path";
import { pathToFileURL } from "node:url";
import test from "node:test";
import { expandSnapshot, mergeSnapshots } from "../../runtime/javascript/snapshots.mjs";

// What every event's JSON names once, in first-seen order: the merge as it
// was before it compared repeated prefixes.
function reference(snapshots) {
  const events = new Map();
  for (const snapshot of snapshots)
    for (const event of snapshot?.events ?? [])
      events.set(JSON.stringify(event), event);
  return [...events.keys()];
}
const written = (merged) => merged.events.map((event) => JSON.stringify(event));
const hit = (id, timestampMs, phaseId) => ({ type: "hit", id, timestampMs, ...(phaseId ? { phaseId } : {}), environment: "browser" });

test("a document that restored the one before it is merged as each event once", () => {
  const first = [hit("a", 1, "p:1"), hit("b", 2, "p:1")];
  const second = [...first.map((event) => ({ ...event })), hit("c", 3, "p:2")];
  const third = [...second.map((event) => ({ ...event })), hit("a", 4, "p:3")];
  const snapshots = [{ events: first }, { events: second }, { events: third }];
  assert.deepEqual(written(mergeSnapshots(snapshots)), reference(snapshots));
  assert.equal(mergeSnapshots(snapshots).events.length, 4, "the same hit at another time is another event");
});

test("events that differ only in key order or nested values stay apart", () => {
  const vector = { type: "decision", id: "d", timestampMs: 5, vector: { values: [true, false], outcome: true } };
  const reordered = { id: "a", type: "hit", timestampMs: 1, phaseId: "p:1", environment: "browser" };
  const snapshots = [
    { events: [hit("a", 1, "p:1"), vector] },
    { events: [reordered, { ...vector, vector: { values: [true, true], outcome: true } }] },
    { events: [hit("a", 1, "p:1"), { ...vector }] },
  ];
  assert.deepEqual(written(mergeSnapshots(snapshots)), reference(snapshots));
});

test("a prefix that stops matching part way is compared no further", () => {
  const snapshots = [
    { events: [hit("a", 1), hit("b", 2), hit("c", 3)] },
    { events: [hit("a", 1), hit("x", 9), hit("c", 3), hit("b", 2)] },
    { events: [] },
    null,
    { events: [hit("x", 9), hit("y", 10)] },
  ];
  assert.deepEqual(written(mergeSnapshots(snapshots)), reference(snapshots));
});

test("hits, decisions and logicals are merged as before", () => {
  const merged = mergeSnapshots([
    { hits: ["a", "b"], decisions: [{ id: "d", vectors: [{ values: [true], outcome: true }] }], logicals: [{ id: "l", vectors: [[1]] }] },
    { hits: ["b", "c"], decisions: [{ id: "d", vectors: [{ values: [true], outcome: true }, { values: [false], outcome: false }] }], logicals: [{ id: "l", vectors: [[1], [2]] }] },
  ]);
  assert.deepEqual(merged.hits, ["a", "b", "c"]);
  assert.deepEqual(merged.decisions, [{ id: "d", vectors: [{ values: [true], outcome: true }, { values: [false], outcome: false }] }]);
  assert.deepEqual(merged.logicals, [{ id: "l", vectors: [[1], [2]] }]);
});

test("a compact snapshot expands to what JSON.parse of the whole gave", async () => {
  await import(pathToFileURL(resolve(import.meta.dirname, "../../runtime/javascript/runtime.mjs")).href);
  const compact = globalThis.__SUPERCOV_COMPACT_SNAPSHOT__;
  const snapshot = {
    decisions: [{ id: "d", vectors: [{ values: [true, null], outcome: false }] }],
    hits: ["js:statement:a", "js:statement:b"],
    events: [
      { type: "hit", id: "js:statement:a", timestampMs: 1731000000000, phaseId: "attempt:phase:1", environment: "browser" },
      { type: "hit", id: "js:statement:b", timestampMs: 1731000000001, environment: "browser" },
      { type: "decision", id: "d", vector: { values: [true, null], outcome: false }, timestampMs: 1731000000002, phaseId: "attempt:phase:1", environment: "browser" },
      { type: "hit", id: "js:statement:a", timestampMs: 1731000000003, phaseId: undefined, environment: "worker", note: null, flag: true },
    ],
    logicals: [{ id: "l", vectors: [{ key: "short-falsy" }] }],
  };
  const text = compact(snapshot);
  assert.equal(JSON.stringify(expandSnapshot(JSON.parse(text))), JSON.stringify(snapshot));
  // What it cannot write compactly it writes whole, and that passes through.
  const odd = { hits: [1], events: [] };
  assert.equal(JSON.stringify(expandSnapshot(JSON.parse(compact(odd)))), JSON.stringify(odd));
});

test("comparison outcomes are kept, each once per probe, and left out when there are none", () => {
  const merged = mergeSnapshots([
    { hits: ["a"], comparisons: [{ id: "f.ts:1:2", outcomes: ["lt"] }, { id: "f.ts:3:4-3:9#arith", outcomes: ["differs"] }] },
    { hits: ["a"], comparisons: [{ id: "f.ts:1:2", outcomes: ["lt", "eq"] }] },
  ]);
  assert.deepEqual(merged.comparisons, [
    { id: "f.ts:1:2", outcomes: ["lt", "eq"] },
    { id: "f.ts:3:4-3:9#arith", outcomes: ["differs"] },
  ]);
  assert.equal("comparisons" in mergeSnapshots([{ hits: ["a"] }]), false);
});
