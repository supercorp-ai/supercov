import test from "node:test";
import assert from "node:assert/strict";
import { cpSync, mkdirSync, mkdtempSync, symlinkSync, readFileSync, writeFileSync, readdirSync, rmSync } from "node:fs";
import { resolve } from "node:path";
import { tmpdir } from "node:os";
import { spawnSync } from "node:child_process";

test("native exception completion cannot supply ordinary returned-value or factory-throw credit", {
  skip: process.env.SUPERCOV_ASSERTED_INTEGRATION !== "1",
}, (t) => {
  const repository = resolve(import.meta.dirname, "../../..");
  const root = mkdtempSync(resolve(tmpdir(), "supercov-exception-first-operand-"));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  cpSync(resolve(import.meta.dirname, "fixtures/exception-first-operand"), root, { recursive: true });
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
  assert.match(run(process.execPath, suite), /^# pass 8$/m);
  const binary = resolve(repository, "target/debug/supercov");
  assert.match(run(binary, ["--", process.execPath, ...suite]), /^# pass 8$/m);
  const runs = readdirSync(resolve(root, ".supercov/runs"));
  assert.equal(runs.length, 1);
  const [runId] = runs;
  const query = (args) => JSON.parse(run(binary, ["runs", runId, "assertions", ...args, "--json"])).data;
  const report = query(["--limit", "100"]);
  assert.equal(report.pagination.hasMore, false);
  const evidence = query(["--analysis", report.analysisId, "--evidence", "/tests", "--limit", "100"]);
  assert.equal(evidence.pagination.hasMore, false);
  const facts = evidence.items.map(i => i.value);
  assert.equal(facts.length, 8);
  // Origin of a callable/promise is not an ordinary checked return value.
  // Simple true exception checks also stay unresolved until their actual
  // invocation/completion channel is modeled, rather than guessing by owner.
  assert.deepEqual(facts.flatMap(f => f.observations), []);
  assert.equal(report.sites.length, 10);
  for (const { candidate } of report.sites) {
    assert.equal(candidate.status, "unresolved");
    assert.equal(candidate.reason.kind, "limit:operand-shape");
    assert.match(candidate.reason.detail, /completion.*producer.*unresolved/);
  }
  const pragmas = query(["--analysis", report.analysisId, "--pragmas"]);
  assert.equal(pragmas.pragmas.length, 1);
  assert.equal(pragmas.pragmas[0].hint.witness, "passed");
  assert.equal(pragmas.pragmas[0].validation, "unresolved");
  assert.equal(report.assertionScore, null);

  const path = resolve(root, "src/core.mjs"), original = readFileSync(path, "utf8");
  const variants = [
    [original.replace('return 101;', ''), 0],
    [original.replace('return 102;', ''), 0],
    [original.replace('return 104;', ''), 0],
    [original.replace('() => 103', '() => {}'), 0],
    [original.replace('return 101;', 'return false;').replace('return 102;', 'return undefined;'), 0],
    [original.replace('return 104;', 'return null;'), 0],
    [original.replace('() => 103', '() => undefined'), 0],
    [original.replace('return 101;', "throw Error('unexpected');"), 1, 'ERR_ASSERTION'],
    [original.replace('return 102;', "throw Error('unexpected');"), 1, 'ERR_ASSERTION'],
    [original.replace("throw Error('boom');", 'return 105;'), 1, 'ERR_ASSERTION'],
    [original.replace("throw Error('rejected boom');", 'return 106;'), 1, 'ERR_ASSERTION'],
    [original.replace("return () => { throw Error('callback boom'); };", "throw Error('callback boom');"), 1, 'ERR_TEST_FAILURE'],
    [original.replace('export async function rejectedPromise()', 'export function rejectedPromise()'), 1, 'ERR_TEST_FAILURE'],
    [original.replace('return 104;', "throw Error('unexpected');"), 1, 'ERR_ASSERTION'],
    [original.replace('return () => 103;', 'return 103;'), 1, 'ERR_INVALID_ARG_TYPE'],
  ];
  for (const [source, expected, code] of variants) {
    assert.notEqual(source, original);
    writeFileSync(path, source);
    const native = run(process.execPath, suite, expected);
    assert.match(native, new RegExp(`^# pass ${expected ? 7 : 8}$`, "m"));
    assert.match(native, new RegExp(`^# fail ${expected}$`, "m"));
    if (code) assert.ok(native.includes(code), native);
  }
});
