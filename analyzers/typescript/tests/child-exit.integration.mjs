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
import { resolve } from "node:path";
import { tmpdir } from "node:os";
import { spawnSync } from "node:child_process";

const repository = resolve(import.meta.dirname, "../../..");
const fixture = resolve(import.meta.dirname, "fixtures/child-exit");
test(
  "child exit assertions require the selected instance, field and resolver value",
  { skip: process.env.SUPERCOV_ASSERTED_INTEGRATION !== "1" },
  async (t) => {
    const results = [];
    // Each archive contains only this case, so an independent good test cannot
    // hide a bad link. The native counterexample changes the producer, not tests.
    for (const [name, detectsChange] of [
      ["imported", true],
      ["local-helper", true],
      ["direct", true],
      ["scalar", true],
      ["constant", false],
      ["signal", false],
      ["transformed", false],
      ["other-child", false],
      ["settled-first", false],
      ["not-a-child", false],
      ["mutated-result", false],
      ["wrong-await", false],
      ["object-side-effect", false],
      ["shadowed-promise", false],
      ["retained-result", true],
      ["then-mutation", false],
      ["alias-mutation", false],
      ["sibling-reader", true],
      ["sibling-mutation", false],
      ["reflective-mutation", false],
    ]) {
      const root = mkdtempSync(resolve(tmpdir(), "supercov-child-exit-"));
      t.after(() => {
        if (!process.env.SUPERCOV_KEEP_ASSERTED_FIXTURE)
          rmSync(root, { recursive: true, force: true });
      });
      cpSync(resolve(fixture, "package.json"), resolve(root, "package.json"));
      cpSync(resolve(fixture, "src"), resolve(root, "src"), {
        recursive: true,
      });
      cpSync(resolve(fixture, "tests"), resolve(root, "tests"), {
        recursive: true,
      });
      cpSync(
        resolve(fixture, "cases", `${name}.mjs`),
        resolve(root, "tests/case.test.mjs"),
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
      const run = (command, args) => {
        const start = performance.now();
        const result = spawnSync(command, args, {
          cwd: root,
          env,
          encoding: "utf8",
          timeout: 20000,
          maxBuffer: 16 * 1024 * 1024,
        });
        assert.equal(
          result.error,
          undefined,
          `${name}: ${result.error?.message}`,
        );
        assert.equal(result.signal, null, name);
        return { ...result, wallMs: performance.now() - start };
      };
      const ok = (result) => {
        assert.equal(
          result.status,
          0,
          `${name}\n${result.stdout}\n${result.stderr}`,
        );
        return result.stdout;
      };
      const suite = ["--test", "--test-concurrency=1", "tests/case.test.mjs"];
      const baseline = run(process.execPath, suite);
      ok(baseline);
      const source = resolve(root, "src/cli.mjs");
      const original = readFileSync(source, "utf8");
      assert.equal(original.split("process.exit(1)").length, 2);
      let mutant;
      try {
        writeFileSync(
          source,
          original.replace("process.exit(1)", "process.exit(2)"),
        );
        mutant = run(process.execPath, suite);
        assert.equal(
          mutant.status,
          detectsChange ? 1 : 0,
          `${name}\n${mutant.stdout}\n${mutant.stderr}`,
        );
        if (detectsChange) assert.match(mutant.stdout, /ERR_ASSERTION/);
      } finally {
        writeFileSync(source, original);
      }
      const binary = resolve(repository, "target/debug/supercov");
      ok(run(binary, ["--", process.execPath, ...suite]));
      const [id] = readdirSync(resolve(root, ".supercov/runs"));
      const authored = readFileSync(
        resolve(root, "tests/case.test.mjs"),
        "utf8",
      );
      const title = authored.match(/test\('([^']+)'/)[1];
      const assertionLine =
        authored
          .split("\n")
          .findIndex((line) => line.startsWith("  assert.equal(")) + 1;
      assert.ok(assertionLine > 0);
      const detail = JSON.parse(
        ok(run(binary, ["runs", id, "test", title, "--json"])),
      ).data;
      assert.equal(detail.tests.length, 1);
      assert.deepEqual(
        detail.tests[0].phases.map((p) => [p.source, p.status]),
        [[`tests/case.test.mjs:${assertionLine}:3`, "passed"]],
      );
      const query = (...args) =>
        JSON.parse(
          ok(run(binary, ["runs", id, "assertions", ...args, "--json"])),
        ).data;
      const report = query("--file", "src/cli.mjs", "--limit", "100");
      assert.equal(report.pagination.hasMore, false);
      const site = report.sites.find((row) => row.site?.method === "exit");
      assert.ok(site, name);
      assert.ok(site.candidate.coveredBy > 0, name);
      assert.equal(report.assertionScore, null);
      const evidence = query("--evidence", "/tests", "--limit", "100");
      assert.equal(evidence.pagination.hasMore, false);
      const observations = evidence.items.flatMap(
        (item) => item.value.observations,
      );
      const exits = observations.filter((ob) => ob.boundary === "exit");
      assert.equal(exits.length, 1, name);
      const exitSource = exits[0].processExit;
      assert.equal(exitSource.model, "node-child-exit-source-v1", name);
      assert.equal(exitSource.status, "unresolved", name);
      assert.equal(site.candidate.status, "unresolved", name);
      assert.equal(
        site.candidate.reason.kind,
        name === "wrong-await"
          ? "limit:operand-shape"
          : "limit:process-exit-link",
        name,
      );
      if (
        [
          "imported",
          "local-helper",
          "direct",
          "scalar",
          "signal",
          "other-child",
          "mutated-result",
          "retained-result",
          "then-mutation",
          "alias-mutation",
          "sibling-reader",
          "sibling-mutation",
          "reflective-mutation",
        ].includes(name)
      ) {
        assert.ok(exitSource.resolution, JSON.stringify({ name, exitSource }));
        assert.equal(exitSource.resolution.status, "source-checked", name);
        assert.equal(
          exitSource.consumer.status,
          [
            "mutated-result",
            "then-mutation",
            "alias-mutation",
            "sibling-mutation",
            "reflective-mutation",
          ].includes(name)
            ? "unresolved"
            : "source-checked",
          name,
        );
        if (exitSource.consumer.status === "unresolved")
          assert.ok(exitSource.consumer.blockedAt, name);
        assert.equal(
          exitSource.resolution.eventArgument,
          name === "signal" ? "signal" : "code",
          name,
        );
        assert.equal(
          exitSource.resolution.field,
          name === "scalar" ? undefined : name === "signal" ? "signal" : "code",
          name,
        );
        assert.ok(
          exitSource.spawn && exitSource.promise && exitSource.event.source,
          name,
        );
      } else assert.equal(exitSource.resolution, undefined, name);
      if (["imported", "local-helper"].includes(name))
        assert.equal(exitSource.helperCalls.length, 1, name);
      const rejected = {
        constant: "transformed-or-constant-event-value",
        transformed: "transformed-or-constant-event-value",
        "settled-first": "competing-or-unsupported-settlement",
        "not-a-child": "native-child-spawn-unverified",
        "wrong-await": "promise-result-not-awaited-before-projection",
        "object-side-effect": "transformed-or-constant-event-value",
        "shadowed-promise": "native-promise-binding-unverified",
      };
      if (rejected[name]) assert.equal(exitSource.reason, rejected[name], name);
      if (name === "other-child") {
        const [, start, end] = exitSource.spawn.split(":");
        assert.match(authored.slice(Number(start), Number(end)), /\['-e'/);
      }
      results.push({
        name,
        detectsChange,
        nativeStatus: mutant.status,
        baselineMs: baseline.wallMs,
        mutantMs: mutant.wallMs,
        runId: id,
        candidate: site.candidate,
        observations,
      });
      if (
        ["constant", "other-child", "settled-first", "not-a-child"].includes(
          name,
        )
      ) {
        await t.test(
          `${name}: unrelated predicate supplies no producer-value credit`,
          {},
          () => {
            assert.equal(site.candidate.strength, undefined);
            assert.notEqual(site.candidate.status, "evident");
          },
        );
      }
      // A signal check or transformed code can constrain something while still
      // missing this change. Survival alone does not prove no value protection.
      if (["signal", "transformed"].includes(name))
        assert.notEqual(site.candidate.strength, "total");
    }
    t.diagnostic(JSON.stringify({ results }));
  },
);
