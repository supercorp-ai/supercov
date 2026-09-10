import test from "node:test";
import assert from "node:assert/strict";
import {
  cpSync, mkdirSync, mkdtempSync, symlinkSync, readFileSync,
  writeFileSync, readdirSync, rmSync,
} from "node:fs";
import { resolve } from "node:path";
import { tmpdir } from "node:os";
import { spawnSync } from "node:child_process";

test("native assertion operand roles and callable witnesses survive ordinary archives", {
  skip: process.env.SUPERCOV_ASSERTED_INTEGRATION !== "1",
}, (t) => {
  const repository = resolve(import.meta.dirname, "../../..");
  const root = mkdtempSync(resolve(tmpdir(), "supercov-assertion-roles-"));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  cpSync(resolve(import.meta.dirname, "fixtures/assertion-roles"), root, { recursive: true });
  mkdirSync(resolve(root, "node_modules"));
  symlinkSync(resolve(repository, "analyzers/typescript/node_modules",
    process.env.SUPERCOV_ASSERTED_TEST_COMPILER === "7.0.2" ? "typescript-native" : "typescript"),
  resolve(root, "node_modules/typescript"));
  const env = { ...process.env, SUPERCOV_PACKAGE_ROOT: repository };
  for (const key of ["NODE_OPTIONS", "NODE_PATH", "NODE_TEST_CONTEXT", "NODE_V8_COVERAGE"]) delete env[key];
  const run = (command, args, expected = 0) => {
    const r = spawnSync(command, args, { cwd: root, env, encoding: "utf8", timeout: 60000, maxBuffer: 16 * 1024 * 1024 });
    assert.equal(r.error, undefined);
    assert.equal(r.status, expected, r.stderr + r.stdout);
    return r.stdout;
  };
  const suite = ["--test", "--test-concurrency=1", "--test-reporter=tap", "tests/core.test.mjs"];
  assert.match(run(process.execPath, suite), /# pass 10\n/);
  const binary = resolve(repository, "target/debug/supercov");
  assert.match(run(binary, ["--", process.execPath, ...suite]), /# pass 10\n/);
  const [runId] = readdirSync(resolve(root, ".supercov/runs"));
  const query = (args) => JSON.parse(run(binary, ["runs", runId, "assertions", ...args, "--json"])).data;
  const report = query(["--limit", "100"]);
  assert.equal(report.pagination.hasMore, false);
  const evidence = query(["--analysis", report.analysisId, "--evidence", "/tests", "--limit", "100"]);
  assert.equal(evidence.pagination.hasMore, false);
  const facts = evidence.items.map(i => i.value);
  assert.equal(facts.length, 10);
  const observations = facts.flatMap(f => f.observations);
  const observed = (owner) => observations.filter(o => o.boundary === `return:${owner}`);
  for (const owner of ["okDiagnostic", "callableDiagnostic", "equalityDiagnostic", "shadowedValue", "failedValue", "mixedValue"])
    assert.equal(observed(owner).length, 0, `${owner} must not acquire a passing value observation`);
  for (const owner of ["checkedTruthy", "aliasTruthy", "namedTruthy", "namespaceTruthy", "strictTruthy"]) {
    assert.equal(observed(owner).length, 1, owner);
    assert.equal(observed(owner)[0].assertionMethod, "ok", owner);
    assert.equal(observed(owner)[0].strength, "presence", owner);
  }
  assert.equal(observed("checkedValue").length, 1);
  const issues = facts.flatMap(f => f.witnessIssues ?? []);
  assert.ok(issues.some(i => i.kind === "call-failed" && i.observation?.boundary === "return:failedValue"));
  assert.ok(issues.some(i => i.kind === "mixed-call-outcomes" && i.observation?.boundary === "return:mixedValue"));
  const hints = query(["--analysis", report.analysisId, "--pragmas"]);
  assert.equal(hints.pragmas.length, 1);
  assert.equal(hints.pragmas[0].hint.witness, "passed");
  assert.equal(hints.pragmas[0].hint.assertionMethod, "ok");
  assert.equal(report.assertionScore, null);

  const path = resolve(root, "src/core.mjs"), original = readFileSync(path, "utf8");
  writeFileSync(path, original.replace("'ok details'", "undefined").replace("'callable details'", "false").replace("'equality details'", "null"));
  assert.match(run(process.execPath, suite), /# pass 10\n/);
  writeFileSync(path, original.replace("function checkedTruthy() { return true; }", "function checkedTruthy() { return false; }"));
  const failed = run(process.execPath, suite, 1);
  assert.match(failed, /# pass 9\n/);
  assert.match(failed, /# fail 1\n/);
  assert.match(failed, /ERR_ASSERTION/);
  writeFileSync(path, original.replace("throw Error('diagnostic evaluation');", "return 'ignored';"));
  assert.match(run(process.execPath, suite, 1), /Missing expected exception/);
});
