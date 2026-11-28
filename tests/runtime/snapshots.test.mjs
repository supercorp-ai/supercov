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
  const { columnarEvents, eventsOfColumns, wholeSnapshot } = await import("../../runtime/javascript/snapshots.mjs");
  // A page writes its columns compact (runtime.mjs, compactSnapshot); written
  // out in full, they are what the collector stored before.
  const columns = () => {
    const written = JSON.parse(globalThis.__SUPERCOV_COLUMNS_SNAPSHOT__());
    assert.ok(Array.isArray(written.idPrefixes), JSON.stringify(written));
    return wholeSnapshot(written);
  };
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

test("a test's record written with its pages' kept texts is its JSON.stringify", async () => {
  const { keepSnapshotText, recordLine } = await import("../../runtime/javascript/snapshots.mjs");
  // As a page writes a snapshot of columns: integer keys, escapes, lone
  // surrogates, numbers of every form, and a string shaped like the record's
  // own placeholder.
  const page = {
    decisions: [{ id: "src/a.ts:3:9", vectors: [{ values: [true, null, false], outcome: true }] }],
    hits: ["src/a.ts:1:1-1:9", "line\nbreak \"quoted\" \\   \ud800 é"],
    eventColumns: {
      types: ["hit", "decision"], phases: ["att:phase:1"], environments: ["browser"],
      id: [0, 1, "src/a.ts:3:9"], type: [0, 0, 1], phase: [0, -1, 0], environment: [0, 0, 0],
      t: [1793741378313, -3, 0.5],
      vector: { 10: { values: [1e21, 1e-7, -1.5], outcome: false }, 2: { values: [true], outcome: true } },
    },
    comparisons: [{ id: "c", outcomes: ["lt", "\u0000supercov-snapshots\u0000"] }],
  };
  const text = JSON.stringify(page);
  const read = JSON.parse(text);
  keepSnapshotText(read, text);
  const empty = { decisions: [], hits: [], events: [] };
  const record = {
    testId: "t1",
    test: 'title "browser":"\u0000supercov-snapshots\u0000"',
    phases: [{ id: "p", browser: [] }],
    browser: [read, empty, read],
    span: [1, 2],
  };
  assert.equal(recordLine(record), JSON.stringify(record));
  assert.equal(recordLine({ ...record, browser: [] }), JSON.stringify({ ...record, browser: [] }));
});
