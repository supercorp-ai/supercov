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
  "awaited capture pragmas expose source support without fabricating runtime witnesses",
  {
    skip: process.env.SUPERCOV_ASSERTED_INTEGRATION !== "1",
  },
  (t) => {
    const root = mkdtempSync(resolve(tmpdir(), "supercov-awaited-capture-"));
    t.after(() => {
      if (!process.env.SUPERCOV_KEEP_ASSERTED_FIXTURE)
        rmSync(root, { recursive: true, force: true });
    });
    cpSync(resolve(import.meta.dirname, "fixtures/awaited-capture"), root, {
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
      "dir",
    );
    const env = { ...process.env, SUPERCOV_PACKAGE_ROOT: repository };
    delete env.NODE_TEST_CONTEXT;
    const exec = (command, args) => {
      const result = spawnSync(command, args, {
        cwd: root,
        env,
        encoding: "utf8",
        timeout: 60000,
        maxBuffer: 16 * 1024 * 1024,
      });
      assert.equal(result.status, 0, result.stderr || result.stdout);
      return result.stdout;
    };
    const suite = ["--test", "--test-reporter=tap", "tests/core.test.mjs"];
    assert.match(exec(process.execPath, suite), /# pass 3/);
    const binary = resolve(repository, "target/debug/supercov");
    assert.match(exec(binary, ["--", process.execPath, ...suite]), /# pass 3/);
    const [run] = readdirSync(resolve(root, ".supercov/runs"));
    const query = (...args) =>
      JSON.parse(exec(binary, ["runs", run, ...args, "--json"])).data;
    const report = query("assertions", "--pragmas");
    assert.equal(report.pragmas.length, 3);
    assert.equal(report.assertionScore, null);
    assert.ok(
      report.protocol.capabilities.includes("awaited-observation-sources-v1"),
    );
    for (const item of report.pragmas) {
      const hint = item.hint;
      assert.equal(hint.awaitedObservation.model, "node-child-capture-poll-v1");
      assert.equal(hint.awaitedObservation.pattern, "/Listening on port/");
      assert.deepEqual(
        hint.awaitedObservation.captures.map((c) => c.stream),
        ["stdout", "stderr"],
      );
      assert.equal(hint.witness, "unavailable");
      assert.equal(hint.witnessIssue, "observation-capture-unavailable");
      assert.equal(item.validation, "unresolved");
      assert.equal(
        item.reason,
        hint.issue === "target-not-in-inventory"
          ? "target-not-in-inventory"
          : "observation-capture-unavailable",
      );
      assert.equal(Object.hasOwn(item, "strength"), false);
    }
    for (const title of [
      "awaited child capture",
      "second caller of the same helper",
      "invalid pragma is analysis only",
    ]) {
      const detail = query("test", title);
      assert.equal(detail.tests.length, 1);
      const phases = detail.tests[0].phases;
      assert.equal(phases.length, 1);
      assert.equal(phases[0].status, "passed");
      assert.match(phases[0].operation, /^node:assert\/strict\./);
    }
    const normal = query("assertions");
    assert.equal(normal.summary.semanticallyVerifiedSites, 0);
    // A query's source summary must not survive source drift.
    const file = resolve(root, "tests/process.mjs");
    const text = readFileSync(file, "utf8");
    writeFileSync(file, text + "\n// changed after recording\n");
    const stale = spawnSync(
      binary,
      ["runs", run, "assertions", "--pragmas", "--json"],
      {
        cwd: root,
        env,
        encoding: "utf8",
        timeout: 60000,
      },
    );
    assert.notEqual(stale.status, 0);
    assert.match(
      stale.stderr + stale.stdout,
      /fingerprint|changed|stale|mismatch/i,
    );
  },
);
