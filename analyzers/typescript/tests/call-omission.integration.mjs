import test from "node:test";
import assert from "node:assert/strict";
import {
  cpSync,
  mkdtempSync,
  mkdirSync,
  readFileSync,
  readdirSync,
  rmSync,
  symlinkSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { resolve } from "node:path";
import { spawnSync } from "node:child_process";

const repository = resolve(import.meta.dirname, "../../..");
test(
  "missing-call pragma resolves one specified edit through an ordinary archive, never changes test outcomes",
  { skip: process.env.SUPERCOV_ASSERTED_INTEGRATION !== "1" },
  (t) => {
    const root = mkdtempSync(resolve(tmpdir(), "supercov-call-omission-"));
    if (process.env.SUPERCOV_KEEP_ASSERTED_FIXTURE) t.diagnostic(root);
    t.after(() => {
      if (!process.env.SUPERCOV_KEEP_ASSERTED_FIXTURE)
        rmSync(root, { recursive: true, force: true });
    });
    cpSync(resolve(import.meta.dirname, "fixtures/call-omission"), root, {
      recursive: true,
    });
    mkdirSync(resolve(root, "node_modules"));
    symlinkSync(
      resolve(
        repository,
        "analyzers/typescript/node_modules",
        process.env.SUPERCOV_ASSERTED_TEST_COMPILER === "7.0.2"
          ? "typescript-native"
          : "typescript",
      ),
      resolve(root, "node_modules/typescript"),
    );
    const env = { ...process.env, SUPERCOV_PACKAGE_ROOT: repository };
    delete env.NODE_TEST_CONTEXT;
    const run = (cmd, args) =>
      spawnSync(cmd, args, {
        cwd: root,
        env,
        encoding: "utf8",
        timeout: 60000,
        maxBuffer: 16 * 1024 * 1024,
      });
    const ok = (r) => {
      assert.equal(r.status, 0, r.stderr || r.stdout);
      return r.stdout;
    };
    const suite = ["--test", "--test-reporter=tap", "tests/core.test.mjs"];
    const binary = resolve(repository, "target/debug/supercov");
    assert.match(ok(run(process.execPath, suite)), /# pass 2/);
    assert.match(
      ok(run(binary, ["--", process.execPath, ...suite])),
      /# pass 2/,
    );
    const [id] = readdirSync(resolve(root, ".supercov/runs"));
    const query = (...args) =>
      JSON.parse(ok(run(binary, ["runs", id, ...args, "--json"]))).data;
    const report = query("assertions", "--pragmas");
    assert.equal(report.pragmas.length, 4);
    const [positive, wrong, invalid, later] = report.pragmas;
    assert.equal(
      positive.validation,
      "analyzer-supported",
      JSON.stringify(positive),
    );
    assert.equal(positive.reason, "modeled-callback-omission-rejected");
    assert.equal(positive.hint.callOmission.originalCount, 1);
    assert.equal(positive.hint.callOmission.omittedCount, 0);
    assert.equal(positive.hint.witness, "passed");
    assert.equal(wrong.validation, "unresolved");
    assert.equal(wrong.reason, "omission-target-not-in-selected-history");
    assert.equal(invalid.validation, "unresolved");
    assert.equal(later.validation, "unresolved");
    assert.match(later.reason, /earlier-registration/);
    for (const p of report.pragmas)
      assert.equal(Object.hasOwn(p, "strength"), false);
    assert.equal(report.assertionScore, null);
    assert.equal(query("assertions").summary.semanticallyVerifiedSites, 0);
    const detail = query("test", "first logger count");
    assert.equal(detail.tests[0].phases.length, 3);
    assert.ok(detail.tests[0].phases.every((p) => p.status === "passed"));

    const file = resolve(root, "src/logger.mjs"),
      source = readFileSync(file, "utf8");
    writeFileSync(
      file,
      source.replace(
        "(...args) => console.log('prefix', ...format(args))",
        "() => undefined",
      ),
    );
    const mutated = run(process.execPath, suite);
    assert.equal(mutated.status, 1, mutated.stdout);
    assert.match(mutated.stdout, /ERR_ASSERTION/);
    // Neither deleting a pragma nor keeping a wrong pragma changes the rejection.
    const testFile = resolve(root, "tests/core.test.mjs"),
      tests = readFileSync(testFile, "utf8");
    writeFileSync(testFile, tests.replace(/^.*\/\/ observes:.*\n/gm, ""));
    const withoutHint = run(process.execPath, suite);
    assert.equal(withoutHint.status, mutated.status);
    writeFileSync(file, source.replace("'prefix'", "'different payload'"));
    assert.match(ok(run(process.execPath, suite)), /# pass 2/);
    writeFileSync(file, source);
    assert.match(ok(run(process.execPath, suite)), /# pass 2/);
    const stale = run(binary, [
      "runs",
      id,
      "assertions",
      "--pragmas",
      "--json",
    ]);
    assert.notEqual(
      stale.status,
      0,
      "comment changes must invalidate the old source witness",
    );

    // Parameter initializers execute before the modeled body and can replace a
    // shared method. The original and omitted callback both pass this variant.
    const withSetup = tests.replace(
      "(t) => {",
      "(t, setup = (getLogger().info = () => console.log('replacement'))) => {",
    );
    writeFileSync(testFile, withSetup);
    assert.match(ok(run(process.execPath, suite)), /# pass 2/);
    assert.match(
      ok(run(binary, ["--", process.execPath, ...suite])),
      /# pass 2/,
    );
    const nextId = readdirSync(resolve(root, ".supercov/runs")).find(
      (candidate) => candidate !== id,
    );
    const guarded = JSON.parse(
      ok(run(binary, ["runs", nextId, "assertions", "--pragmas", "--json"])),
    ).data;
    assert.ok(
      guarded.pragmas.every((p) => p.validation !== "analyzer-supported"),
    );
    assert.equal(
      guarded.pragmas[0].hint.callOmission.reason,
      "omission-requires-direct-synchronous-count-assertion",
    );
    writeFileSync(
      file,
      source.replace(
        "(...args) => console.log('prefix', ...format(args))",
        "() => undefined",
      ),
    );
    assert.match(ok(run(process.execPath, suite)), /# pass 2/);
  },
);
