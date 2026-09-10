import test from "node:test";
import assert from "node:assert/strict";
import { cpSync, mkdirSync, mkdtempSync, symlinkSync, readFileSync, writeFileSync, readdirSync, rmSync } from "node:fs";
import { resolve } from "node:path";
import { tmpdir } from "node:os";
import { spawnSync } from "node:child_process";

test("native exception assertions keep completion, matcher and diagnostic roles separate", {
  skip: process.env.SUPERCOV_ASSERTED_INTEGRATION !== "1",
}, (t) => {
  const repository = resolve(import.meta.dirname, "../../..");
  const root = mkdtempSync(resolve(tmpdir(), "supercov-exception-roles-"));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  cpSync(resolve(import.meta.dirname, "fixtures/exception-roles"), root, { recursive: true });
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
  assert.match(run(process.execPath, suite), /^# pass 5$/m);
  const binary = resolve(repository, "target/debug/supercov");
  assert.match(run(binary, ["--", process.execPath, ...suite]), /^# pass 5$/m);
  const runs = readdirSync(resolve(root, ".supercov/runs"));
  assert.equal(runs.length, 1);
  const [runId] = runs;
  const query = (args) => JSON.parse(run(binary, ["runs", runId, "assertions", ...args, "--json"])).data;
  const report = query(["--limit", "100"]);
  assert.equal(report.pagination.hasMore, false);
  const evidence = query(["--analysis", report.analysisId, "--evidence", "/tests", "--limit", "100"]);
  assert.equal(evidence.pagination.hasMore, false);
  const facts = evidence.items.map(i => i.value);
  assert.equal(facts.length, 5);
  const observations = facts.flatMap(f => f.observations);
  for (const owner of ["noThrowMessage", "noRejectMessage"])
    assert.ok(!observations.some(o => o.boundary.endsWith(`:${owner}`)), owner);
  for (const owner of ["throwsMessage", "rejectsMessage", "expectedError"])
    assert.ok(!observations.some(o => o.boundary === `throw:${owner}`), owner);
  // A matcher-producing operand remains relevant. Do not repair the unused
  // no-error argument by dropping every exception assertion's second operand.
  assert.ok(observations.some(o => o.boundary === "return:expectedError"));
  assert.equal(report.assertionScore, null);

  const path = resolve(root, "src/core.mjs"), original = readFileSync(path, "utf8");
  const variants = [
    [original.replaceAll("diagnostic", "other diagnostic").replace("return 101;", "return false;").replace("return 102;", "return undefined;"), 0],
    [original.replace("return 'no-throw diagnostic';", "return 9001;").replace("return 'no-reject diagnostic';", "return { unused: true };"), 0],
    [original.replace("return 101;", "throw Error('unexpected synchronous failure');"), 1],
    [original.replace("return 102;", "throw Error('unexpected asynchronous failure');"), 1],
    [original.replace("return /^Error: boom$/;", "return /^other$/;"), 1],
    [original.replace("return 'throws diagnostic';", "return 'boom';"), 1, "ERR_AMBIGUOUS_ARGUMENT"],
    [original.replace("return 'rejects diagnostic';", "return 'rejected boom';"), 1, "ERR_AMBIGUOUS_ARGUMENT"],
  ];
  for (const [source, expected, code = "ERR_ASSERTION"] of variants) {
    assert.notEqual(source, original);
    writeFileSync(path, source);
    const native = run(process.execPath, suite, expected);
    assert.match(native, new RegExp(`^# pass ${expected ? 4 : 5}$`, "m"));
    assert.match(native, new RegExp(`^# fail ${expected}$`, "m"));
    if (expected) assert.ok(native.includes(code));
  }
});
