import assert from "node:assert/strict";
import test from "node:test";
import {
  canonicalRuntimeAnalysis,
  median,
  semanticReport,
  workload,
} from "./rust-asserted-scaling-calibration.mjs";

test("cross-run phase renaming is bijective and preserves every observation", () => {
  const link = {
    point: "p",
    phase: "one",
    attempt: "test",
    operation: "at file:1:0",
    assertionSource: "assert_eq!(f(), 1)",
    statement: "file:1:0",
  };
  const raw = {
    executionLinks: [link, { ...link, point: "decoy" }],
    facts: { sites: ["p", "decoy"] },
  };
  const canonical = canonicalRuntimeAnalysis(raw);
  assert.deepEqual(
    canonical,
    canonicalRuntimeAnalysis({
      ...raw,
      executionLinks: raw.executionLinks.map((l) => ({
        ...l,
        phase: "renamed",
      })),
    }),
  );
  assert.notDeepEqual(
    canonical,
    canonicalRuntimeAnalysis({ ...raw, executionLinks: [link] }),
  );
  assert.notDeepEqual(
    canonical,
    canonicalRuntimeAnalysis({ ...raw, executionLinks: [link, link] }),
  );
  assert.throws(() =>
    canonicalRuntimeAnalysis({
      ...raw,
      executionLinks: [link, { ...link, phase: "split" }],
    }),
  );
  assert.throws(() =>
    canonicalRuntimeAnalysis({
      ...raw,
      executionLinks: [link, { ...link, attempt: "other test" }],
    }),
  );
  assert.throws(() =>
    canonicalRuntimeAnalysis({
      ...raw,
      executionLinks: [link, { ...link, operation: "other assertion" }],
    }),
  );
  assert.equal(raw.executionLinks[0].phase, "one");
});

test("the held-out generator is deterministic and retains every family", () => {
  for (const groups of [8, 32, 128]) {
    const fixture = workload(groups);
    assert.deepEqual(fixture, workload(groups));
    assert.equal(
      fixture.files["src/lib.rs"].match(/pub fn /g).length,
      4 * groups,
    );
    assert.equal(
      fixture.files["tests/contract.rs"].match(/#\[test\]/g).length,
      3 * groups,
    );
    assert.equal(fixture.expected.uncalled, groups);
    assert.equal(fixture.expected.witnessedBoundaries, groups);
    assert.equal(Object.keys(fixture.hashes).length, 3);
    assert.ok(
      Object.values(fixture.hashes).every((h) => /^[a-f0-9]{64}$/.test(h)),
    );
  }
  assert.notDeepEqual(workload(8).hashes, workload(32).hashes);
  for (const groups of [0, -1, 129, 1.5, NaN])
    assert.throws(() => workload(groups));
});

test("query parity removes only named timing fields, never facts or verdicts", () => {
  const report = {
    loadMs: 1,
    analysisAndJoinMs: 2,
    sourceReadVerifyAnalyzeMs: 3,
    queryTimings: { sourceAnalyzeMs: 2 },
    analysis: { sites: ["a"] },
    sourceAnalysis: { witnesses: ["w"] },
    sourceResolutions: [{ site: "a", status: "evident" }],
    newUnknownField: "must remain compared",
  };
  const semantic = semanticReport(report);
  assert.deepEqual(Object.keys(semantic).sort(), [
    "analysis",
    "newUnknownField",
    "sourceAnalysis",
    "sourceResolutions",
  ]);
  assert.notDeepEqual(
    semantic,
    semanticReport({ ...report, sourceResolutions: [] }),
  );
  assert.deepEqual(semantic, semanticReport({ ...report, loadMs: 1000 }));
  assert.equal(report.loadMs, 1);
});

test("reported medians preserve raw values without mutating samples", () => {
  const values = [4, 1, 9, 2];
  assert.equal(median(values), 3);
  assert.equal(median([7, 1, 3]), 3);
  assert.deepEqual(values, [4, 1, 9, 2]);
});
