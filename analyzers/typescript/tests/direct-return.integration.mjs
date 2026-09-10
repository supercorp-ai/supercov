import test from "node:test";
import assert from "node:assert/strict";
import {
  cpSync,
  mkdtempSync,
  mkdirSync,
  readFileSync,
  readdirSync,
  writeFileSync,
  symlinkSync,
  existsSync,
  rmSync,
} from "node:fs";
import { resolve } from "node:path";
import { tmpdir } from "node:os";
import { spawnSync } from "node:child_process";

test(
  "public value guidance checks direct returns without borrowing mock or earlier-call evidence",
  { skip: process.env.SUPERCOV_ASSERTED_INTEGRATION !== "1" },
  (t) => {
    const repository = resolve(import.meta.dirname, "../../..");
    const root = mkdtempSync(resolve(tmpdir(), "supercov-direct-return-"));
    t.after(() => {
      if (!process.env.SUPERCOV_KEEP_ASSERTED_FIXTURE)
        rmSync(root, { recursive: true, force: true });
    });
    if (process.env.SUPERCOV_KEEP_ASSERTED_FIXTURE) t.diagnostic(root);
    cpSync(resolve(import.meta.dirname, "fixtures/direct-return"), root, {
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
    for (const name of [
      "NODE_OPTIONS",
      "NODE_PATH",
      "NODE_TEST_CONTEXT",
      "NODE_V8_COVERAGE",
    ])
      delete env[name];
    const binary = resolve(repository, "target/debug/supercov");
    const suite = [
      "--test",
      "--test-concurrency=1",
      "--test-reporter=tap",
      "tests/core.test.mjs",
    ];
    const run = (cmd, args) =>
      spawnSync(cmd, args, {
        cwd: root,
        env,
        encoding: "utf8",
        timeout: 60000,
        maxBuffer: 32 * 1024 * 1024,
      });
    const ok = (r) => {
      assert.equal(r.status, 0, r.stderr || r.stdout);
      return r.stdout;
    };
    const sourceFile = resolve(root, "src/value.mjs"),
      testFile = resolve(root, "tests/core.test.mjs");
    const source = readFileSync(sourceFile, "utf8"),
      tests = readFileSync(testFile, "utf8");
    assert.match(ok(run(process.execPath, suite)), /# pass 2/);
    const runs = resolve(root, ".supercov/runs");
    const before = new Set(existsSync(runs) ? readdirSync(runs) : []);
    assert.match(
      ok(run(binary, ["--", process.execPath, ...suite])),
      /# pass 2/,
    );
    const id = readdirSync(runs).find((name) => !before.has(name));
    const query = (more) =>
      JSON.parse(ok(run(binary, ["runs", id, "assertions", ...more, "--json"])))
        .data;
    const ordinaryBefore = query([]);
    const first = query(["--pragmas"]);
    const pragmas = [...first.pragmas];
    let page = first;
    while (page.pagination.hasMore) {
      page = query([
        "--pragmas",
        "--analysis",
        first.analysisId,
        "--offset",
        String(page.pagination.nextOffset),
      ]);
      pragmas.push(...page.pragmas);
    }
    assert.equal(pragmas.length, 8);
    const supported = pragmas.filter(
      (p) => p.validation === "analyzer-supported",
    );
    assert.equal(supported.length, 3, JSON.stringify(pragmas, null, 2));
    assert.equal(first.assertionScore, null);
    assert.deepEqual(
      query([]),
      ordinaryBefore,
      "guidance must not alter ordinary coverage/candidate results",
    );
    for (const p of supported) {
      assert.equal(p.reason, "modeled-direct-return-sensitivity");
      assert.equal(p.hint.witness, "passed");
      assert.equal(Object.hasOwn(p, "strength"), false);
      assert.equal(p.hint.payloadSensitivity, undefined);
      const e = p.hint.directReturnSensitivity;
      assert.equal(e.scope, "first-synchronous-test-prefix");
      assert.equal(e.original.outcome, "not-rejected");
      assert.ok(e.original.targetEvaluations > 0);
      assert.ok(e.original.callSource.startsWith("tests/core.test.mjs:"));
      if (e.changeText === "false") {
        assert.deepEqual(
          e.variants.map((v) => [v.change, v.check?.outcome]),
          [["boolean-literal-inverted", "rejected"]],
        );
      } else {
        assert.equal(e.changeText, "items.length === 0");
        const outcomes = Object.fromEntries(
          e.variants.map((v) => [v.change, v.check?.outcome]),
        );
        assert.equal(outcomes["condition-true"], "not-rejected");
        // The alias assertion is preceded by another assertion which can reject;
        // only its unchanged variant may therefore be attributed to this site.
        const callLine = e.original.callSource.replace(/:\d+$/, "");
        const assertionLine = e.assertionSource.replace(/:\d+$/, "");
        if (callLine === assertionLine) {
          assert.equal(outcomes["condition-false"], "rejected");
          assert.equal(outcomes["condition-inverted"], "rejected");
        } else {
          for (const change of ["condition-false", "condition-inverted"]) {
            const variant = e.variants.find((v) => v.change === change);
            assert.equal(variant.status, "unresolved");
            assert.equal(variant.check, null);
            assert.match(variant.reason, /earlier.*reject/);
          }
        }
      }
    }
    assert.ok(pragmas.some((p) => p.hint.witnessIssue === "call-not-recorded"));
    assert.ok(
      pragmas.some((p) =>
        p.hint.directReturnSensitivity?.reason?.includes(
          "target-not-evaluated-by-selected-call",
        ),
      ),
    );
    assert.ok(
      pragmas.some(
        (p) =>
          p.hint.directReturnSensitivity?.reason ===
          "direct-return-operand-not-source-call",
      ),
    );
    assert.ok(
      pragmas.some(
        (p) =>
          p.hint.directReturnSensitivity?.reason ===
          "direct-return-setup-or-earlier-test",
      ),
    );
    assert.ok(
      pragmas.some(
        (p) =>
          p.validation === "invalid" && p.reason === "unsupported-check-recipe",
      ),
    );

    // Mutants exist only in this test's disposable fixture. Run its original
    // assertions unchanged and retain native selected-assertion rejection.
    for (const [from, to] of [
      ["return false", "return true"],
      ["items.length === 0", "false"],
      ["items.length === 0", "items.length !== 0"],
    ]) {
      writeFileSync(sourceFile, source.replace(from, to));
      const native = run(process.execPath, suite);
      assert.equal(native.status, 1, native.stderr || native.stdout);
      const selected = native.stdout.split("# Subtest: ")[1];
      assert.match(selected, /\nnot ok 1 /);
      assert.match(selected, /ERR_ASSERTION/);
    }
    writeFileSync(sourceFile, source);
    // No annotation is enforced by the test runtime.
    writeFileSync(testFile, tests.replaceAll(/^[ \t]*\/\/ observes:.*$/gm, ""));
    assert.match(ok(run(process.execPath, suite)), /# pass 2/);
    writeFileSync(testFile, tests);
    t.diagnostic(
      JSON.stringify({
        id,
        supported: supported.length,
        compiler: first.analyzer?.compiler,
      }),
    );
  },
);
