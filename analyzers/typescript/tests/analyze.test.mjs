import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync, writeFileSync, readdirSync, rmSync } from "node:fs";
import { resolve } from "node:path";
import ts from "typescript";
import { analyze } from "../dist/analyze.js";
const json = (file, data) => writeFileSync(file, JSON.stringify(data));
import { input } from "./fixture-input.mjs";
const run = (options) => analyze(options);

test("real source and assertion syntax produce facts, not verdicts", (t) => {
  const setup = input(t),
    { facts, diagnostics } = run(setup);
  assert.equal(facts.schema, 1);
  assert.equal(facts.tests.length, 6);
  assert.equal(diagnostics.compilerVersion, ts.version);
  const increment = facts.sites.find((s) => s.owner === "increment");
  assert.ok(increment.bounds.some((b) => b.boundary === "return:increment"));
  const wrapper = facts.sites.find((s) => s.owner === "wrapper");
  assert.ok(increment.reached.includes(wrapper.id));
  assert.ok(
    facts.tests[0].observations.some(
      (o) => o.boundary === "return:increment" && o.strength === "value",
    ),
  );
  assert.equal(facts.tests[5].observations.length, 0);
  const decision = facts.sites.find((s) => s.kind === "decision").decision;
  assert.deepEqual(decision.outcomes, { true: ["T3"], false: ["T4"] });
  assert.ok(Object.hasOwn(decision, "earlyExitDownstream"));
  const cancellation = facts.sites.find((s) => s.method === "clearTimeout");
  assert.ok(cancellation.derive.some((d) => d.requiresTotal?.length));
  for (const site of facts.sites)
    assert.equal(Object.hasOwn(site, "status"), false);
});

test("JS-ASSERT-005: missing assertion witnesses must reach the join as an analysis limit", (t) => {
  const setup = input(t);
  const baseline = run(setup);
  const evidenceFiles = Object.fromEntries(
    Object.entries(setup.evidenceFiles).filter(
      ([file]) =>
        !file.endsWith(".phases.json") && !file.endsWith(".statements.json"),
    ),
  );
  const result = run({ ...setup, evidenceFiles });
  assert.ok(result.diagnostics.suppressedObservations.length > 0);
  assert.equal(result.facts.tests[0].observations.length, 0);
  const site = result.facts.sites.find((site) => site.owner === "increment");
  assert.ok(site.coveredBy.length > 0);
  // The bug is witness provenance, not an unsupported operand shape. It now
  // crosses the join boundary as typed test-owned facts, not a fake shape.
  assert.deepEqual(result.facts.tests[0].witnessIssues, [
    { kind: "capture-unavailable" },
  ]);
  assert.deepEqual(result.facts.sites, baseline.facts.sites);
  rmSync(resolve(setup.inputDirectory, "cov/T1.phases.json"));
  assert.deepEqual(run(setup).facts.tests[0].witnessIssues, [
    { kind: "capture-unavailable" },
  ]);
});

test("rejected assertion calls retain distinct witness reasons without supplying observations", (t) => {
  const setup = input(t);
  const path = resolve(setup.inputDirectory, "cov/T1.phases.json");
  const [phase] = JSON.parse(readFileSync(path, "utf8"));
  for (const [phases, kind] of [
    [[], "call-not-recorded"],
    [[{ ...phase, source: "tests/core.test.ts:999:1" }], "call-not-recorded"],
    [[{ ...phase, op: "node:assert/strict.ok" }], "call-not-recorded"],
    [[{ ...phase, status: "failed" }], "call-failed"],
    [[{ ...phase, status: undefined }], "call-incomplete"],
    [[{ ...phase, status: "running" }], "call-incomplete"],
    [[phase, { ...phase, status: "failed" }], "mixed-call-outcomes"],
    [[phase, { ...phase, status: undefined }], "mixed-call-outcomes"],
  ]) {
    json(path, phases);
    const result = run(setup).facts.tests[0];
    assert.equal(result.observations.length, 0, kind);
    assert.equal(result.witnessIssues[0].kind, kind);
    assert.equal(result.witnessIssues[0].source, phase.source);
    assert.equal(
      result.witnessIssues[0].observation.boundary,
      "return:increment",
    );
  }
  json(path, [phase, phase]);
  const passed = run(setup).facts.tests[0];
  assert.equal(passed.observations.length, 1);
  assert.equal(passed.witnessIssues, undefined);
});

test("analysis is repeatable, read-only, and independent of prototype environment variables", (t) => {
  const setup = input(t);
  const before = readFileSync(
    resolve(setup.inputDirectory, "inventory.json"),
    "utf8",
  );
  const files = readdirSync(setup.inputDirectory);
  const a = run(setup);
  const previous = process.env.SRC_DIR;
  process.env.SRC_DIR = "not-this-project";
  try {
    assert.deepEqual(run(setup), a);
  } finally {
    if (previous === undefined) delete process.env.SRC_DIR;
    else process.env.SRC_DIR = previous;
  }
  assert.equal(
    readFileSync(resolve(setup.inputDirectory, "inventory.json"), "utf8"),
    before,
  );
  assert.deepEqual(readdirSync(setup.inputDirectory), files);
});

test("failed runtime tests cannot become facts and repeated calls do not retain them", (t) => {
  const setup = input(t);
  assert.equal(run(setup).facts.tests.length, 6);
  json(
    resolve(setup.inputDirectory, "cov/index.json"),
    setup.rows.map((r) => ({ ...r, ok: false })),
  );
  assert.equal(run(setup).facts.tests.length, 0);
});

test("the caller may explicitly supply the compiler API", (t) => {
  const setup = input(t);
  assert.equal(
    run({ ...setup, typescript: { ...ts, version: "explicit compiler" } })
      .diagnostics.compilerVersion,
    "explicit compiler",
  );
});

test("invalid inputs fail rather than returning an empty successful analysis", (t) => {
  const setup = input(t);
  assert.throws(() => run({}), /in-memory evidenceFiles are required/);
  assert.throws(
    () =>
      run({
        projectRoot: setup.projectRoot,
        inputDirectory: setup.inputDirectory,
      }),
    /in-memory evidenceFiles are required/,
  );
  json(resolve(setup.inputDirectory, "inventory.json"), {});
  assert.throws(() => run(setup), /sites array/);
  json(resolve(setup.inputDirectory, "inventory.json"), { sites: [] });
  assert.throws(
    () => run({ ...setup, tsconfig: "missing.json" }),
    /Cannot read file/,
  );
});
