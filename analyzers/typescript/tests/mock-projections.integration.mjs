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
  "mock projections retain passing predicates without inventing source-site dependence",
  {
    skip: process.env.SUPERCOV_ASSERTED_INTEGRATION !== "1",
  },
  async (t) => {
    const root = mkdtempSync(resolve(tmpdir(), "supercov-mock-projections-"));
    t.after(() => {
      if (!process.env.SUPERCOV_KEEP_ASSERTED_FIXTURE)
        rmSync(root, { recursive: true, force: true });
    });
    cpSync(resolve(import.meta.dirname, "fixtures/mock-projections"), root, {
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
    delete env.CHOOSE_LOG;
    const execute = (command, args) =>
      spawnSync(command, args, {
        cwd: root,
        env,
        encoding: "utf8",
        timeout: 60000,
        maxBuffer: 16 * 1024 * 1024,
      });
    const ok = (result) => {
      assert.equal(result.status, 0, result.stderr || result.stdout);
      return result.stdout;
    };
    const suite = ["--test", "--test-concurrency=1", "tests/core.test.mjs"];
    const core = resolve(root, "src/core.mjs");
    const original = readFileSync(core, "utf8");
    ok(execute(process.execPath, suite));
    const oracle = [];
    // Native counterexamples change only production code in this disposable fixture.
    // No test rewriting, instrumentation or mutation framework is used as the oracle.
    for (const [name, before, after, survives] of [
      [
        "count ignores arguments",
        "'count payload', 1",
        "'different', 99",
        true,
      ],
      [
        "count detects missing call",
        "console.log('count payload', 1);",
        "",
        false,
      ],
      [
        "count detects added call",
        "console.log('count payload', 1);",
        "console.log('count payload', 1); console.log('extra');",
        false,
      ],
      [
        "length ignores arguments",
        "'length payload'",
        "'different length'",
        true,
      ],
      [
        "argument detects selected value",
        "'argument payload'",
        "'different argument'",
        false,
      ],
      [
        "argument ignores other argument",
        "'argument payload', 1",
        "'argument payload', 99",
        true,
      ],
      [
        "argument ignores extra call",
        "console.log('argument payload', 1);",
        "console.log('argument payload', 1); console.log('extra');",
        true,
      ],
      [
        "selected call ignores other call",
        "'selected second'",
        "'different second'",
        true,
      ],
      [
        "selected call detects its value",
        "'selected first'",
        "'different first'",
        false,
      ],
      [
        "slice ignores excluded call",
        "'slice first'",
        "'different first'",
        true,
      ],
      [
        "slice detects included call",
        "'slice second'",
        "'different second'",
        false,
      ],
      [
        "map ignores unprojected argument",
        "'mapped first', 1",
        "'mapped first', 99",
        true,
      ],
      [
        "map detects projected argument",
        "'mapped first'",
        "'different mapped'",
        false,
      ],
      [
        "conditional ignores unselected mock",
        "'conditional right'",
        "'different right'",
        true,
      ],
      [
        "conditional detects selected mock",
        "'conditional left'",
        "'different left'",
        false,
      ],
      [
        "reset count ignores missing call",
        "console.log('reset payload');",
        "",
        true,
      ],
      [
        "late count ignores missing call",
        "console.log('late payload');",
        "",
        true,
      ],
      [
        "caught failure is not protection",
        "'failed payload'",
        "'different failed'",
        true,
      ],
      [
        "inactive assertion is not protection",
        "'inactive payload'",
        "'different inactive'",
        true,
      ],
      [
        "same named tracker does not observe production",
        "console.log('fake tracker payload');",
        "",
        true,
      ],
      [
        "shadowed receiver does not observe production",
        "console.log('shadowed receiver payload');",
        "",
        true,
      ],
    ]) {
      assert.equal(original.split(before).length, 2, name);
      try {
        writeFileSync(core, original.replace(before, after));
        const result = execute(process.execPath, suite);
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
    ok(execute(binary, ["--", process.execPath, ...suite]));
    const [run] = readdirSync(resolve(root, ".supercov/runs"));
    const query = (...args) =>
      JSON.parse(
        ok(execute(binary, ["runs", run, "assertions", ...args, "--json"])),
      ).data;
    const evidence = query("--evidence", "/tests", "--limit", "1000");
    assert.equal(evidence.pagination.hasMore, false);
    assert.equal(evidence.items.length, 13);
    const tests = evidence.items.map((item) => {
      assert.ok(item.value, "mock test evidence must fit the fixture page");
      return item.value;
    });
    const observations = tests.flatMap((row) => row.observations);
    const projections = observations.filter((ob) => ob.mock);
    await t.test(
      "count, argument, slice and map evidence keep separate source projections",
      () => {
        assert.equal(projections.length, 9);
        assert.equal(
          projections.filter((ob) => ob.mock.kind === "call-count").length,
          4,
        );
        assert.equal(
          projections.filter((ob) => ob.mock.kind === "call-history").length,
          1,
        );
        assert.ok(
          projections.some(
            (ob) => ob.mock.path.join(".") === "mock.calls.[0].arguments.[0]",
          ),
        );
        assert.ok(projections.some((ob) => ob.mock.path.includes("slice(1)")));
        assert.ok(
          projections.some((ob) =>
            ob.mock.path.some((part) => part.startsWith("map(")),
          ),
        );
        assert.ok(
          projections.every(
            (ob) =>
              ob.mock.target === "console.log" && ob.facet === "console.log",
          ),
        );
        assert.ok(
          projections.every((ob) => ob.assertionSource && ob.assertionMethod),
        );
      },
    );
    const report = query("--limit", "1000");
    assert.equal(report.pagination.hasMore, false);
    assert.equal(report.sites.length, report.summary.sites);
    await t.test(
      "mock-only checks do not become argument protection or whole-history witnesses",
      () => {
        for (const row of report.sites.filter(
          (row) => row.candidate.coveredBy > 0,
        )) {
          assert.equal(row.candidate.status, "unresolved", row.site.owner);
          if (!["failedWitness", "inactiveWitness"].includes(row.site.owner))
            assert.equal(
              row.candidate.reason.kind,
              "limit:operand-shape",
              row.site.owner,
            );
        }
        assert.equal(report.summary.semanticallyVerifiedSites, 0);
        assert.equal(report.assertionScore, null);
      },
    );
    t.diagnostic(JSON.stringify({ run, oracle }));
  },
);
