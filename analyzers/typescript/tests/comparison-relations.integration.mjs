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
  "comparison relations distinguish immutable self-comparisons from independent evaluations",
  {
    skip: process.env.SUPERCOV_ASSERTED_INTEGRATION !== "1",
  },
  (t) => {
    const root = mkdtempSync(
      resolve(tmpdir(), "supercov-comparison-relations-"),
    );
    t.after(() => {
      if (!process.env.SUPERCOV_KEEP_ASSERTED_FIXTURE)
        rmSync(root, { recursive: true, force: true });
    });
    cpSync(
      resolve(import.meta.dirname, "fixtures/comparison-relations"),
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
    const run = (command, args) =>
      spawnSync(command, args, {
        cwd: root,
        env,
        encoding: "utf8",
        timeout: 60000,
        maxBuffer: 16 * 1024 * 1024,
      });
    const ok = (result) => {
      assert.equal(result.error, undefined);
      assert.equal(result.status, 0, result.stderr || result.stdout);
      return result.stdout;
    };
    const suite = [
      "--test",
      "--test-concurrency=1",
      "tests/core.test.mjs",
      "tests/loose.test.mjs",
      "tests/typed.test.mts",
    ];
    ok(run(process.execPath, suite));
    const core = resolve(root, "src/core.mjs"),
      original = readFileSync(core, "utf8");
    const oracle = [];
    for (const [name, before, after, survives] of [
      [
        "self comparison accepts a changed value",
        "return 'self'",
        "return 'changed'",
        true,
      ],
      ["deep self comparison accepts NaN", "return 'self'", "return NaN", true],
      [
        "immutable aliases share the changed value",
        "return 'alias'",
        "return 99",
        true,
      ],
      [
        "independent expected literal detects change",
        "return 'fixed'",
        "return 'wrong'",
        false,
      ],
      [
        "overwritten alias detects change",
        "return 'reset'",
        "return 'wrong'",
        false,
      ],
      [
        "repeated calls can disagree",
        "return 'successive'",
        "return calls",
        false,
      ],
      [
        "repeated getters can disagree",
        "return 'getter'",
        "return reads",
        false,
      ],
      [
        "Node strict self equality accepts NaN",
        "return 17",
        "return NaN",
        true,
      ],
      [
        "strict self equality accepts another number",
        "return 17",
        "return 18",
        true,
      ],
      [
        "copy of mutable value is not a live alias",
        "return 23",
        "return 24",
        false,
      ],
      [
        "independent assertion beside self comparison still detects change",
        "return 'checked'",
        "return 'wrong'",
        false,
      ],
      [
        "awaiting a value is not always identity",
        "return 'awaited'",
        "return Promise.resolve('awaited')",
        false,
      ],
      [
        "Node loose self equality accepts NaN",
        "return 'loose'",
        "return NaN",
        true,
      ],
      [
        "Node loose deep self equality accepts another value",
        "return 'loose-deep'",
        "return null",
        true,
      ],
      [
        "explicit Node strict self equality accepts NaN",
        "return 'explicit-strict'",
        "return NaN",
        true,
      ],
      [
        "a locally shadowed helper can reject a self comparison",
        "return 'shadowed'",
        "return 'wrong'",
        false,
      ],
      [
        "erased TypeScript wrappers preserve the changed value",
        "return 'typed'",
        "return 'changed typed'",
        true,
      ],
    ]) {
      assert.equal(original.split(before).length, 2, name);
      try {
        writeFileSync(core, original.replace(before, after));
        const result = run(process.execPath, suite);
        assert.equal(result.error, undefined, name);
        assert.equal(result.signal, null, name);
        assert.equal(
          result.status,
          survives ? 0 : 1,
          `${name}\n${result.stdout}\n${result.stderr}`,
        );
        oracle.push({ name, survives, status: result.status });
      } finally {
        writeFileSync(core, original);
      }
    }
    const binary = resolve(repository, "target/debug/supercov");
    ok(run(binary, ["--", process.execPath, ...suite]));
    const [id] = readdirSync(resolve(root, ".supercov/runs"));
    const query = (...args) =>
      JSON.parse(ok(run(binary, ["runs", id, "assertions", ...args, "--json"])))
        .data;
    const report = query("--limit", "1000");
    assert.equal(report.pagination.hasMore, false);
    const self = report.sites.filter((row) =>
      [
        "selfOnly",
        "aliasOnly",
        "strictSelf",
        "looseSelf",
        "looseDeepSelf",
        "explicitStrictSelf",
        "typedSelf",
      ].includes(row.site.owner),
    );
    assert.equal(self.length, 7);
    for (const row of self)
      assert.equal(row.candidate.status, "unresolved", row.site.owner);
    const independent = report.sites.filter((row) =>
      ["independent", "overwritten", "independentlyChecked"].includes(
        row.site.owner,
      ),
    );
    assert.equal(independent.length, 3);
    for (const row of independent)
      assert.equal(row.candidate.status, "evident", row.site.owner);
    const testPage = query("--evidence", "/tests", "--limit", "1000");
    assert.equal(testPage.pagination.hasMore, false);
    assert.equal(testPage.items.length, 15);
    const observations = testPage.items.flatMap(
      (item) => item.value.observations,
    );
    const selfRelations = observations.filter(
      (ob) => ob.comparison?.relation === "same-immutable-binding",
    );
    assert.ok(selfRelations.some((ob) => ob.boundary === "return:selfOnly"));
    assert.ok(selfRelations.some((ob) => ob.boundary === "return:aliasOnly"));
    assert.ok(
      selfRelations.some(
        (ob) =>
          ob.boundary === "return:strictSelf" &&
          ob.comparison.predicate === "node-same-value",
      ),
    );
    assert.ok(
      selfRelations.some(
        (ob) =>
          ob.boundary === "return:looseSelf" &&
          ob.comparison.predicate === "node-loose-equality",
      ),
    );
    assert.ok(
      selfRelations.some(
        (ob) =>
          ob.boundary === "return:looseDeepSelf" &&
          ob.comparison.predicate === "node-deep-equality",
      ),
    );
    assert.ok(
      selfRelations.some(
        (ob) =>
          ob.boundary === "return:explicitStrictSelf" &&
          ob.comparison.predicate === "node-same-value",
      ),
    );
    assert.ok(
      selfRelations.some(
        (ob) =>
          ob.boundary === "return:typedSelf" &&
          ob.comparison.predicate === "node-deep-strict-equality",
      ),
    );
    assert.ok(
      selfRelations.every(
        (ob) => ob.comparison.actual.binding === ob.comparison.expected.binding,
      ),
    );
    assert.ok(
      observations
        .filter((ob) =>
          [
            "return:successive",
            "return:withGetter",
            "return:strictAlias",
          ].includes(ob.boundary),
        )
        .every((ob) => ob.comparison?.relation !== "same-immutable-binding"),
    );
    const rejectedObservations = testPage.items
      .flatMap((item) => item.value.witnessIssues ?? [])
      .flatMap((issue) => (issue.observation ? [issue.observation] : []));
    const custom = [...observations, ...rejectedObservations].filter(
      (ob) => ob.boundary === "return:shadowed",
    );
    assert.ok(custom.length > 0);
    assert.ok(custom.every((ob) => !ob.comparison));
    const awaitIssues = testPage.items
      .flatMap((item) => item.value.witnessIssues ?? [])
      .filter((issue) => issue.observation?.boundary === "return:awaitedSelf");
    assert.ok(awaitIssues.length > 0);
    assert.ok(
      awaitIssues.every(
        (issue) => issue.observation.comparison?.relation === "unresolved",
      ),
    );
    const hints = query("--pragmas");
    assert.equal(hints.summary.hints, 1);
    assert.equal(hints.summary.analyzerSupported, 0);
    assert.equal(hints.pragmas[0].validation, "unresolved");
    assert.equal(report.assertionScore, null);
    t.diagnostic(JSON.stringify({ id, oracle }));
  },
);
