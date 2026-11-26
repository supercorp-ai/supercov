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

test("a snapshot read as columns is what the collector made of a compact one, and its events come back for a merge", async () => {
  const runtime = await import(pathToFileURL(resolve(import.meta.dirname, "../../runtime/javascript/runtime.mjs")).href);
  const { columnarEvents, eventsOfColumns } = await import("../../runtime/javascript/snapshots.mjs");
  const columns = () => JSON.parse(globalThis.__SUPERCOV_COLUMNS_SNAPSHOT__());
  // What the collector stored before: the compact text expanded, then as columns.
  const before = () => columnarEvents(expandSnapshot(JSON.parse(globalThis.__SUPERCOV_COMPACT_SNAPSHOT__(globalThis.__SUPERCOV_COVERAGE_SNAPSHOT__()))));
  runtime.enableRuntimeSnapshotEvidence();
  runtime.resetCoverage();
  // Nothing recorded: no columns, an empty list of events.
  assert.equal(JSON.stringify(columns()), JSON.stringify(before()));
  assert.equal(JSON.stringify(eventsOfColumns(columns())), JSON.stringify(globalThis.__SUPERCOV_COVERAGE_SNAPSHOT__()));
  const file = runtime.registerProbeV2({ key: "00000000000000cc", points: 3, decisionWidths: [2], defaultCount: 0, selectionPoints: [3, 0], selectionTrees: [] });
  const previous = globalThis.__SUPERCOV_PHASE_ID__;
  try {
    runtime.coverageHit("outside-a-phase");
    globalThis.__SUPERCOV_PHASE_ID__ = "attempt:phase:1";
    runtime.activateCoverageScope();
    file.hit(0);
    file.hit(1);
    runtime.mcdcEndV2(file, 0, 1 + 2 + 3, false);
    runtime.compareV2("f.ts:1:2", 1, 2, "<");
    globalThis.__SUPERCOV_PHASE_ID__ = "attempt:phase:2";
    runtime.activateCoverageScope();
    file.hit(0);
    runtime.mcdcEndV2(file, 0, 1 + 1, true);
  } finally {
    globalThis.__SUPERCOV_PHASE_ID__ = previous;
  }
  const read = columns();
  assert.ok(read.eventColumns.id.length >= 5, JSON.stringify(read));
  assert.equal(JSON.stringify(read), JSON.stringify(before()));
  // The events as the runtime built them, which a merge with a document that
  // left reads.
  const objects = globalThis.__SUPERCOV_COVERAGE_SNAPSHOT__();
  assert.equal(JSON.stringify(eventsOfColumns(read).events), JSON.stringify(objects.events));
  assert.equal(JSON.stringify(mergeSnapshots([eventsOfColumns(read)])), JSON.stringify(mergeSnapshots([objects])));
  // Anything without columns passes through.
  assert.equal(eventsOfColumns(objects), objects);
  assert.equal(eventsOfColumns(undefined), undefined);
  runtime.resetCoverage();
});

test("a document's batches and the tail read at the end give the snapshot it would have given whole", async () => {
  const { columnarEvents, keepBatch, snapshotFromBatches } = await import("../../runtime/javascript/snapshots.mjs");
  const rows = [
    ["hit", "a", null, 10, "p:1", "browser"],
    ["hit", "b", null, 12, null, "browser"],
    ["decision", "d", { values: [true], outcome: true }, 13, "p:1", "browser"],
    ["hit", "a", null, 20, "p:2", "browser"],
  ];
  const whole = columnarEvents({
    decisions: [{ id: "d", vectors: [{ values: [true], outcome: true }] }],
    hits: ["a", "b"],
    events: [
      { type: "hit", id: "a", timestampMs: 10, phaseId: "p:1", environment: "browser" },
      { type: "hit", id: "b", timestampMs: 12, environment: "browser" },
      { type: "decision", id: "d", vector: { values: [true], outcome: true }, timestampMs: 13, phaseId: "p:1", environment: "browser" },
      { type: "hit", id: "a", timestampMs: 20, phaseId: "p:2", environment: "browser" },
    ],
    logicals: [],
    comparisons: [{ id: "f.ts:1:2", outcomes: ["lt"] }],
  });
  const kept = new Map();
  keepBatch(kept, { document: "x", generation: 0, events: 0, hits: 0, eventRows: rows.slice(0, 2), hitIds: ["a", "b"] });
  keepBatch(kept, { document: "x", generation: 0, events: 2, hits: 2, eventRows: rows.slice(2, 3), hitIds: [] });
  const tail = { supercovTail: 1, document: "x", generation: 0, events: 3, hits: 2, eventRows: rows.slice(3), hitIds: [], decisions: [{ id: "d", vectors: [{ values: [true], outcome: true }] }], logicals: [], comparisons: [{ id: "f.ts:1:2", outcomes: ["lt"] }] };
  assert.equal(JSON.stringify(snapshotFromBatches(kept.get("x"), tail)), JSON.stringify(whole));
  // Batches that do not run up to the tail, or of another generation, give
  // nothing, and the document is read whole.
  assert.equal(snapshotFromBatches(kept.get("x"), { ...tail, events: 2 }), undefined);
  assert.equal(snapshotFromBatches(kept.get("x"), { ...tail, hits: 1 }), undefined);
  assert.equal(snapshotFromBatches(kept.get("x"), { ...tail, generation: 1 }), undefined);
  assert.equal(snapshotFromBatches(undefined, tail), undefined);
  // A batch that skips some evidence spoils the document's batches until a
  // reset starts them again.
  keepBatch(kept, { document: "x", generation: 0, events: 9, hits: 2, eventRows: [], hitIds: [] });
  assert.equal(snapshotFromBatches(kept.get("x"), tail), undefined);
  keepBatch(kept, { document: "x", generation: 0, events: 4, hits: 2, eventRows: [], hitIds: [] });
  assert.equal(kept.get("x").broken, true);
  keepBatch(kept, { document: "x", generation: 1, events: 0, hits: 0, eventRows: rows.slice(0, 1), hitIds: ["a"] });
  assert.deepEqual(kept.get("x").hitIds, ["a"]);
  // A tail with no comparisons gives a snapshot with none.
  const plain = snapshotFromBatches(kept.get("x"), { supercovTail: 1, document: "x", generation: 1, events: 1, hits: 1, eventRows: [], hitIds: [], decisions: [], logicals: [] });
  assert.equal("comparisons" in plain, false);
  assert.deepEqual(plain.hits, ["a"]);
});
