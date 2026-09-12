import { test } from "node:test";
import assert from "node:assert/strict";
import {
  mkdtempSync,
  mkdirSync,
  readFileSync,
  writeFileSync,
  readdirSync,
  rmSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { spawnSync } from "node:child_process";
import ts from "typescript";
import { analyze } from "../dist/analyze.js";
import { assertFactsParity } from "./facts-parity.mjs";

const directory = dirname(fileURLToPath(import.meta.url));
const projectRoot = resolve(directory, "fixtures/basic");
const cli = resolve(directory, "../bin/analyze.mjs");
const json = (file, data) => writeFileSync(file, JSON.stringify(data));
function input(t) {
  const path = mkdtempSync(resolve(tmpdir(), "supercov-ts-test-"));
  t.after(() => rmSync(path, { recursive: true, force: true }));
  mkdirSync(resolve(path, "cov"));
  const file = "src/core.ts",
    source = readFileSync(resolve(projectRoot, file), "utf8");
  const sf = ts.createSourceFile(file, source, ts.ScriptTarget.Latest, true);
  const sites = [];
  const position = (offset) => {
    const p = sf.getLineAndCharacterOfPosition(offset);
    return { line: p.line + 1, column: p.character + 1 };
  };
  const add = (node, owner, kind, category, method) =>
    sites.push({
      id: `S${sites.length + 1}`,
      file,
      kind,
      category,
      classification: "contractual",
      start: position(node.getStart(sf)),
      end: position(node.getEnd()),
      pos: node.getStart(sf),
      endPos: node.getEnd(),
      text: node.getText(sf),
      fn: owner,
      owner,
      exported: true,
      ...(method ? { method } : {}),
    });
  const visit = (node, owner = "") => {
    if (ts.isFunctionDeclaration(node)) owner = node.name.text;
    if (ts.isReturnStatement(node)) add(node, owner, "effect", "return");
    if (ts.isIfStatement(node))
      add(node.expression, owner, "decision", "condition");
    if (
      ts.isCallExpression(node) &&
      ts.isIdentifier(node.expression) &&
      ["setTimeout", "clearTimeout"].includes(node.expression.text)
    )
      add(node, owner, "effect", "schedule", node.expression.text);
    if (
      ts.isCallExpression(node) &&
      node.expression.getText(sf) === "console.log"
    )
      add(node, owner, "effect", "log", "log");
    ts.forEachChild(node, (child) => visit(child, owner));
  };
  visit(sf);
  json(resolve(path, "inventory.json"), { sites });
  const testFile = "tests/core.test.ts",
    testSource = readFileSync(resolve(projectRoot, testFile), "utf8");
  const rows = [...testSource.matchAll(/test\('([^']+)'/g)].map(
    (match, index) => ({
      id: `T${index + 1}`,
      name: match[1],
      title: match[1],
      file: testFile,
      line: testSource.slice(0, match.index).split("\n").length,
      ok: true,
    }),
  );
  json(resolve(path, "cov/index.json"), rows);
  const testAst = ts.createSourceFile(
    testFile,
    testSource,
    ts.ScriptTarget.Latest,
    true,
  );
  for (const row of rows) {
    // Controlled witness fixtures: real-run ownership is exercised separately by
    // archive.integration.test.mjs, not implied by these synthetic phases.
    const phases = [];
    const nextLine = rows[rows.indexOf(row) + 1]?.line ?? Infinity;
    const visitAssertion = (node) => {
      const p = testAst.getLineAndCharacterOfPosition(node.getStart(testAst));
      if (
        ts.isCallExpression(node) &&
        node.expression.getText(testAst).startsWith("assert.") &&
        p.line + 1 >= row.line &&
        p.line + 1 < nextLine
      )
        phases.push({
          op: `node:assert/strict.${node.expression.name.text}`,
          source: `${testFile}:${p.line + 1}:${p.character + 1}`,
          status: "passed",
          fns: [],
          decs: [],
          stmts: 0,
        });
      ts.forEachChild(node, visitAssertion);
    };
    visitAssertion(testAst);
    json(resolve(path, `cov/${row.id}.phases.json`), phases);
    writeFileSync(
      resolve(path, `cov/${row.id}.lcov`),
      `SF:${file}\n` +
        source
          .split("\n")
          .map((_, i) => `DA:${i + 1},1`)
          .join("\n") +
        "\nend_of_record\n",
    );
    const condition = sites.find((s) => s.kind === "decision");
    json(resolve(path, `cov/${row.id}.outcomes.json`), {
      [`${file}:${condition.start.line}:${condition.start.column}#0`]:
        row.name === "enabled outcome"
          ? [1, 0]
          : row.name === "disabled outcome"
            ? [0, 1]
            : [0, 0],
    });
  }
  return {
    projectRoot,
    inputDirectory: path,
    coverageRunner: "vitest",
    sites,
    rows,
  };
}
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
  const result = run({ ...setup, runtimeObservations: false });
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
  assert.throws(() => run({}), /required strings/);
  json(resolve(setup.inputDirectory, "inventory.json"), {});
  assert.throws(() => run(setup), /sites array/);
  json(resolve(setup.inputDirectory, "inventory.json"), { sites: [] });
  assert.throws(
    () => run({ ...setup, tsconfig: "missing.json" }),
    /Cannot read file/,
  );
});

test("CLI resolves config-relative paths from an unrelated working directory and refuses overwrites", (t) => {
  const setup = input(t),
    config = resolve(setup.inputDirectory, "analysis.json");
  json(config, { projectRoot, inputDirectory: ".", coverageRunner: "vitest" });
  const child = spawnSync(process.execPath, [cli, "--config", config], {
    cwd: tmpdir(),
    encoding: "utf8",
  });
  assert.equal(child.status, 0, child.stderr);
  assert.equal(JSON.parse(child.stdout).sites.length, setup.sites.length);
  assert.equal(JSON.parse(child.stderr).linkedTests, 6);
  const output = resolve(setup.inputDirectory, "facts.json");
  writeFileSync(output, "keep this");
  const duplicate = spawnSync(
    process.execPath,
    [cli, "--config", config, "--output", output],
    { encoding: "utf8" },
  );
  assert.equal(duplicate.status, 1);
  assert.equal(readFileSync(output, "utf8"), "keep this");
});

test("facts parity rejects lost observations, dropped tests and modified dependency paths", (t) => {
  const { facts } = run(input(t)),
    resolutions = { resolutions: [] };
  // This fixture has raw candidate facts; compare non-candidate sites here.
  facts.sites = facts.sites.filter(
    (s) => !s.derive?.some((d) => d.requiresTotal),
  );
  assert.doesNotThrow(() => assertFactsParity(facts, facts, resolutions));
  for (const change of [
    (f) => f.tests[0].observations.pop(),
    (f) => f.tests.pop(),
    (f) => f.sites[0].bounds.pop(),
  ]) {
    const changed = structuredClone(facts);
    change(changed);
    assert.throws(() => assertFactsParity(changed, facts, resolutions));
  }
});
