import test from "node:test";
import assert from "node:assert/strict";
import {
  cpSync,
  mkdtempSync,
  readFileSync,
  writeFileSync,
  rmSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { resolve } from "node:path";
import { input } from "./fixture-input.mjs";
import { analyze } from "../dist/analyze.js";
import ts from "typescript";

const json = (path, value) => writeFileSync(path, JSON.stringify(value));
function setup(t) {
  const root = mkdtempSync(resolve(tmpdir(), "supercov-witnessed-body-"));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  cpSync(resolve(import.meta.dirname, "fixtures/basic"), root, {
    recursive: true,
  });
  const f = input(t, root);
  f.typescript = ts;
  const sourcePath = resolve(root, "tests/core.test.ts");
  // Same call positions, but no name-based test declaration remains. Actual
  // archive ownership is tested separately through the public subprocess suite.
  const source = readFileSync(sourcePath, "utf8")
    .replace("{ test }", "{ test as run_ }")
    .replaceAll("test(", "run_(");
  writeFileSync(sourcePath, source);
  const rows = f.rows.map((r) => ({ ...r, line: 0, runner: "node:test" }));
  json(resolve(f.inputDirectory, "cov/index.json"), rows);
  return { f, source, sourcePath, rows };
}

test("exact passing calls recover bodies, not titles, row inputs or native test scope", (t) => {
  const { f, rows } = setup(t);
  json(
    resolve(f.inputDirectory, "cov/index.json"),
    rows.map((r) => ({
      ...r,
      title: "an opaque wrapper renamed this",
      name: "same title",
    })),
  );
  const result = analyze(f);
  assert.equal(result.facts.tests.length, 6);
  assert.equal(result.diagnostics.witnessedBodyLinks.length, 5);
  assert.equal(result.diagnostics.unlinkedTests.length, 1);
  assert.equal(result.diagnostics.linkedByTitle, 0);
  assert.ok(
    result.facts.tests[0].observations.some(
      (o) => o.boundary === "return:increment",
    ),
  );
  for (const fact of result.facts.tests.slice(0, 5))
    assert.deepEqual(fact.witnessIssues, [
      { kind: "test-registration-scope-unverified" },
    ]);
  assert.deepEqual(result.facts.tests[5].witnessIssues, [
    { kind: "test-source-unlinked" },
  ]);
  assert.ok(
    result.diagnostics.witnessedBodyLinks.every(
      (l) => l.assertions.length === 1,
    ),
  );
});

test("body recovery requires complete exact native call evidence in one callback", (t) => {
  const { f } = setup(t);
  const path = resolve(f.inputDirectory, "cov/T1.phases.json");
  const [phase] = JSON.parse(readFileSync(path, "utf8"));
  const [other] = JSON.parse(
    readFileSync(resolve(f.inputDirectory, "cov/T2.phases.json"), "utf8"),
  );
  for (const phases of [
    [],
    [{ ...phase, source: phase.source.replace(/:\d+$/, ":99") }],
    [{ ...phase, op: "node:assert/strict.ok" }],
    [{ ...phase, op: "node:assert.equal" }],
    [{ ...phase, source: `runtime-stack:${phase.source}` }],
    [{ ...phase, status: undefined }],
    [{ ...phase, status: "failed" }],
    [phase, { ...phase, status: "failed" }],
    [phase, other],
  ]) {
    json(path, phases);
    const result = analyze(f);
    assert.deepEqual(
      result.facts.tests[0].witnessIssues,
      [{ kind: "test-source-unlinked" }],
      JSON.stringify(phases),
    );
    assert.equal(result.facts.tests[0].observations.length, 0);
  }
  json(path, [phase, phase]);
  assert.equal(
    analyze(f).diagnostics.witnessedBodyLinks[0].assertions.length,
    1,
  );
  rmSync(path);
  assert.deepEqual(analyze(f).facts.tests[0].witnessIssues, [
    { kind: "test-source-unlinked" },
    { kind: "capture-unavailable" },
  ]);
});

test("identifier spelling, other runners and nested functions cannot supply a body link", (t) => {
  const { f, source, sourcePath, rows } = setup(t);
  for (const replacement of [
    source.replace("'node:assert/strict'", "'./fake-assert'"),
    source.replace("() => {", "(assert) => {"),
    // The assertion keeps its exact position on the next line but is now
    // nested in a callback whose relationship to the test is not established.
    source.replace(
      "run_('direct value', () => {",
      "run_('direct value', () => { (() => {",
    ),
  ]) {
    const edited = replacement.includes("(() => {")
      ? replacement.replace("\n})", "\n})() })")
      : replacement;
    writeFileSync(sourcePath, edited);
    assert.deepEqual(analyze(f).facts.tests[0].witnessIssues, [
      { kind: "test-source-unlinked" },
    ]);
  }
  writeFileSync(sourcePath, source);
  json(
    resolve(f.inputDirectory, "cov/index.json"),
    rows.map((r) => ({ ...r, runner: "vitest" })),
  );
  assert.equal(analyze(f).diagnostics.witnessedBodyLinks.length, 0);
});

test("two attempts at the same callback never borrow a witness or acquire a source row", (t) => {
  const { f, rows } = setup(t);
  const extra = { ...rows[0], id: "T7", title: "a different wrapper input" };
  json(resolve(f.inputDirectory, "cov/index.json"), [...rows, extra]);
  cpSync(
    resolve(f.inputDirectory, "cov/T1.lcov"),
    resolve(f.inputDirectory, "cov/T7.lcov"),
  );
  cpSync(
    resolve(f.inputDirectory, "cov/T1.phases.json"),
    resolve(f.inputDirectory, "cov/T7.phases.json"),
  );
  let result = analyze(f);
  assert.deepEqual(result.facts.tests[6].witnessIssues, [
    { kind: "test-registration-scope-unverified" },
  ]);
  assert.equal(result.diagnostics.witnessedBodyLinks.length, 6);
  json(resolve(f.inputDirectory, "cov/T7.phases.json"), []);
  result = analyze(f);
  assert.equal(result.facts.tests[0].observations.length, 1);
  assert.deepEqual(result.facts.tests[6].witnessIssues, [
    { kind: "test-source-unlinked" },
  ]);
  assert.equal(result.facts.tests[6].observations.length, 0);
});
