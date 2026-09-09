import test from "node:test";
import assert from "node:assert/strict";
import {
  cpSync,
  mkdtempSync,
  mkdirSync,
  symlinkSync,
  readFileSync,
  writeFileSync,
  readdirSync,
  rmSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { resolve } from "node:path";
import { spawnSync } from "node:child_process";

const repository = resolve(import.meta.dirname, "../../..");
test(
  "decision evidence distinguishes checked returns from distinguishable outcomes",
  {
    skip: process.env.SUPERCOV_ASSERTED_INTEGRATION !== "1",
  },
  async (t) => {
    const root = mkdtempSync(
      resolve(tmpdir(), "supercov-decision-sensitivity-"),
    );
    t.after(() => {
      if (!process.env.SUPERCOV_KEEP_ASSERTED_FIXTURE)
        rmSync(root, { recursive: true, force: true });
    });
    cpSync(
      resolve(import.meta.dirname, "fixtures/decision-sensitivity"),
      root,
      { recursive: true },
    );
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
    const execute = (command, args) =>
      spawnSync(command, args, {
        cwd: root,
        env,
        encoding: "utf8",
        timeout: 60000,
        maxBuffer: 16 * 1024 * 1024,
      });
    const ok = (result) => {
      assert.equal(result.error, undefined);
      assert.equal(result.signal, null);
      assert.equal(result.status, 0, result.stderr || result.stdout);
      return result.stdout;
    };
    const suite = [
      "--test",
      "--test-concurrency=1",
      "--test-reporter=tap",
      "tests/core.test.mjs",
    ];
    assert.match(ok(execute(process.execPath, suite)), /^# pass 10$/m);
    const core = resolve(root, "src/core.mjs"),
      original = readFileSync(core, "utf8");
    const oracle = [];
    for (const [name, before, after, survives] of [
      ["identical forced true", "if (sameFlag)", "if (true)", true],
      ["identical forced false", "if (sameFlag)", "if (false)", true],
      ["distinct forced true", "if (distinctFlag)", "if (true)", false],
      ["distinct forced false", "if (distinctFlag)", "if (false)", false],
      [
        "identical return value is checked",
        "if (sameFlag) {\n    return 7;",
        "if (sameFlag) {\n    return 8;",
        false,
      ],
      ["masked forced true", "if (maskedFlag)", "if (true)", true],
      ["masked forced false", "if (maskedFlag)", "if (false)", true],
      [
        "masked zero return is checked",
        "if (maskedFlag) {\n    return 1;",
        "if (maskedFlag) {\n    return 0;",
        false,
      ],
      ["transformed forced true", "if (transformedFlag)", "if (true)", true],
      ["transformed forced false", "if (transformedFlag)", "if (false)", true],
      [
        "transformed zero return is checked",
        "if (transformedFlag) {\n    return -1;",
        "if (transformedFlag) {\n    return 0;",
        false,
      ],
      ["effectful forced true", "if (effectfulFlag)", "if (true)", false],
      ["effectful forced false", "if (effectfulFlag)", "if (false)", false],
    ]) {
      assert.equal(original.split(before).length, 2, name);
      try {
        writeFileSync(core, original.replace(before, after));
        const start = performance.now();
        const result = execute(process.execPath, suite);
        const wallMs = performance.now() - start;
        assert.equal(result.error, undefined, name);
        assert.equal(result.signal, null, name);
        assert.equal(
          result.status,
          survives ? 0 : 1,
          name + "\n" + result.stdout + result.stderr,
        );
        if (!survives) assert.match(result.stdout, /ERR_ASSERTION/, name);
        oracle.push({ name, survives, wallMs });
      } finally {
        writeFileSync(core, original);
      }
    }
    const binary = resolve(repository, "target/debug/supercov");
    ok(execute(binary, ["--", process.execPath, ...suite]));
    const runs = readdirSync(resolve(root, ".supercov/runs"));
    assert.equal(runs.length, 1);
    const query = (...args) =>
      JSON.parse(
        ok(execute(binary, ["runs", runs[0], "assertions", ...args, "--json"])),
      ).data;
    const report = query("--limit", "1000");
    assert.equal(report.pagination.hasMore, false);
    const decisions = report.sites.filter((r) => r.site.kind === "decision");
    assert.equal(decisions.length, 5);
    for (const row of decisions) {
      assert.equal(row.candidate.coveredBy, 2);
      assert.equal(row.facts.decision.outcomes.true.length, 1);
      assert.equal(row.facts.decision.outcomes.false.length, 1);
    }
    const page = query("--evidence", "/tests", "--limit", "1000");
    assert.equal(page.pagination.hasMore, false);
    assert.equal(page.items.length, 10);
    for (const item of page.items) {
      assert.ok(item.value);
      assert.equal(item.value.witnessIssues?.length ?? 0, 0);
    }
    assert.ok(
      page.items.filter((item) => item.value.observations.length > 0).length >=
        8,
    );
    assert.equal(report.assertionScore, null);
    t.diagnostic(JSON.stringify({ run: runs[0], oracle, decisions }));
    await t.test(
      "distinct branch outcomes retain their existing evidence",
      () => {
        const result = decisions.find(
          (r) => r.site.owner === "distinct",
        ).candidate;
        assert.equal(result.stuckTrueCaught, true);
        assert.equal(result.stuckFalseCaught, true);
      },
    );
    await t.test(
      "equal branch results do not prove that forcing either outcome is caught",
      {
        todo: "SG-ASSERT-017: outcome sensitivity needs more than passing return witnesses",
      },
      () => {
        const result = decisions.find(
          (r) => r.site.owner === "identical",
        ).candidate;
        assert.notEqual(result.stuckTrueCaught, true);
        assert.notEqual(result.stuckFalseCaught, true);
      },
    );
    await t.test(
      "different results accepted by the same predicate do not prove outcome sensitivity",
      {
        todo: "SG-ASSERT-017: account for the actual assertion predicate",
      },
      () => {
        const result = decisions.find(
          (r) => r.site.owner === "masked",
        ).candidate;
        assert.notEqual(result.stuckTrueCaught, true);
        assert.notEqual(result.stuckFalseCaught, true);
      },
    );
    await t.test(
      "equal return values do not erase separately checked effects",
      () => {
        const result = decisions.find(
          (r) => r.site.owner === "effectful",
        ).candidate;
        assert.equal(result.stuckTrueCaught, true);
        assert.equal(result.stuckFalseCaught, true);
      },
    );
    await t.test(
      "downstream transformations must preserve a difference before claiming detection",
      () => {
        const result = decisions.find(
          (r) => r.site.owner === "transformed",
        ).candidate;
        assert.notEqual(result.stuckTrueCaught, true);
        assert.notEqual(result.stuckFalseCaught, true);
      },
    );
  },
);
