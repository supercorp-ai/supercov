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
const repository = resolve(import.meta.dirname, "../../..");

test(
  "payload guidance distinguishes selected values, not execution or nearby assertions",
  { skip: process.env.SUPERCOV_ASSERTED_INTEGRATION !== "1" },
  (t) => {
    const root = mkdtempSync(
      resolve(tmpdir(), "supercov-payload-sensitivity-"),
    );
    t.after(() => {
      if (!process.env.SUPERCOV_KEEP_ASSERTED_FIXTURE)
        rmSync(root, { recursive: true, force: true });
    });
    if (process.env.SUPERCOV_KEEP_ASSERTED_FIXTURE) t.diagnostic(root);
    cpSync(resolve(import.meta.dirname, "fixtures/count-sensitivity"), root, {
      recursive: true,
    });
    cpSync(
      resolve(
        import.meta.dirname,
        "fixtures/payload-sensitivity/core.test.mjs",
      ),
      resolve(root, "tests/core.test.mjs"),
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
    for (const k of [
      "NODE_OPTIONS",
      "NODE_PATH",
      "NODE_TEST_CONTEXT",
      "NODE_V8_COVERAGE",
    ])
      delete env[k];
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
    const query = (id) => {
      const first = JSON.parse(
        ok(run(binary, ["runs", id, "assertions", "--pragmas", "--json"])),
      ).data;
      const pragmas = [...first.pragmas];
      let page = first;
      while (page.pagination.hasMore) {
        page = JSON.parse(
          ok(
            run(binary, [
              "runs",
              id,
              "assertions",
              "--pragmas",
              "--analysis",
              first.analysisId,
              "--offset",
              String(page.pagination.nextOffset),
              "--json",
            ]),
          ),
        ).data;
        pragmas.push(...page.pragmas);
      }
      assert.equal(pragmas.length, first.pagination.total);
      return { ...first, pragmas };
    };
    const capture = () => {
      const runs = resolve(root, ".supercov/runs"),
        before = new Set(existsSync(runs) ? readdirSync(runs) : []);
      assert.match(
        ok(run(binary, ["--", process.execPath, ...suite])),
        /# pass 4/,
      );
      const id = readdirSync(runs).find((x) => !before.has(x));
      return { id, report: query(id) };
    };
    const sourceFile = resolve(root, "src/logger.mjs"),
      testFile = resolve(root, "tests/core.test.mjs");
    const source = readFileSync(sourceFile, "utf8"),
      tests = readFileSync(testFile, "utf8");
    assert.match(ok(run(process.execPath, suite)), /# pass 4/);
    const { id, report } = capture();
    const supported = report.pragmas.filter(
      (p) => p.validation === "analyzer-supported",
    );
    assert.equal(
      supported.length,
      6,
      JSON.stringify(
        report.pragmas.map((p) => [
          p.hint.raw,
          p.reason,
          p.hint.payloadSensitivity?.reason,
        ]),
        null,
        2,
      ),
    );
    assert.equal(report.assertionScore, null);
    for (const p of supported) {
      assert.equal(p.hint.witness, "passed");
      assert.equal(Object.hasOwn(p, "strength"), false);
      const e = p.hint.payloadSensitivity;
      assert.equal(e.original.outcome, "not-rejected");
      assert.ok(e.original.projection.callSource.startsWith("src/logger.mjs:"));
      assert.ok(
        e.original.projection.instance.startsWith("tests/core.test.mjs:"),
      );
      const index = e.original.projection.argumentIndex;
      if (index === 0)
        assert.ok(e.variants.every((v) => v.check?.outcome === "not-rejected"));
      if (index === 1) {
        assert.equal(e.variants[0].check.actual.kind, "undefined");
        assert.equal(e.variants[0].check.outcome, "rejected");
      }
      if (index === 2) {
        assert.equal(e.original.expected.kind, "object");
        for (const v of e.variants.filter(
          (v) => v.change !== "condition-false",
        )) {
          assert.equal(v.check.actual.kind, "opaque-string");
          assert.equal(v.check.outcome, "rejected");
        }
      }
    }
    assert.ok(
      supported.some(
        (p) =>
          p.hint.payloadSensitivity.original.projection.historySelections
            ?.length,
      ),
    );
    // Independent native counterexamples; not input to the source checker.
    const mapperStart = source.indexOf("(arg) => {"),
      mapperEnd = source.indexOf("\n});", mapperStart);
    assert.ok(mapperStart > 0 && mapperEnd > mapperStart);
    const emptyMapper =
      source.slice(0, mapperStart) + "(arg) => {" + source.slice(mapperEnd);
    for (const mutant of [
      emptyMapper,
      source.replace("mode === 'verbose'", "true"),
      source.replace("mode === 'verbose'", "mode !== 'verbose'"),
    ]) {
      writeFileSync(sourceFile, mutant);
      const native = run(process.execPath, suite);
      assert.equal(native.status, 1);
      assert.match(native.stdout, /ERR_ASSERTION/);
    }
    writeFileSync(sourceFile, source);
    writeFileSync(testFile, tests.replace(/^.*\/\/ observes:.*\n/gm, ""));
    assert.match(ok(run(process.execPath, suite)), /# pass 4/);
    assert.notEqual(
      run(binary, ["runs", id, "assertions", "--pragmas", "--json"]).status,
      0,
    );
    // A tautological or discarded operand is not rescued by a source hint.
    for (const replacement of [
      "assert.equal(infoCalls[0].arguments[1], infoCalls[0].arguments[1]);",
      "assert.equal('hello', 'hello');",
    ]) {
      writeFileSync(
        testFile,
        tests.replace(
          "assert.equal(infoCalls[0].arguments[1], 'hello');",
          replacement,
        ),
      );
      const { report: negative } = capture();
      const changedLine =
        readFileSync(testFile, "utf8")
          .split("\n")
          .findIndex((l) => l.includes(replacement)) + 1;
      const hints = negative.pragmas.filter((p) =>
        p.hint.assertionSource?.startsWith(
          `tests/core.test.mjs:${changedLine}:`,
        ),
      );
      assert.ok(hints.length);
      assert.ok(hints.every((p) => p.validation !== "analyzer-supported"));
    }
    // Earlier rejection cannot be attributed to a later payload assertion.
    const earlier = tests.replace(
      "    // observes: src/logger.mjs#formatData args.map; check value\n    assert.equal(infoCalls[0].arguments[1], 'hello');",
      "    if (mode === 'normal') { assert.deepEqual(infoCalls[0].arguments[2], { a: 1 }); }\n    // observes: src/logger.mjs#selectLogger mode === 'verbose'; check value\n    assert.equal(infoCalls[0].arguments[1], 'hello');",
    );
    writeFileSync(testFile, earlier);
    const { report: preceding } = capture();
    assert.ok(
      preceding.pragmas.some((p) =>
        p.hint.payloadSensitivity?.variants?.some((v) =>
          v.reason?.includes("earlier-payload-assertion-rejects"),
        ),
      ),
    );
    writeFileSync(testFile, tests);
    // An opaque formatted value must not be equated with itself as an abstract tag.
    const opaque = tests
      .replace("if (mode === 'normal') {", "if (mode === 'verbose') {")
      .replace(
        "assert.deepEqual(infoCalls[0].arguments[2], { a: 1 });",
        "assert.deepEqual(infoCalls[0].arguments[2], infoCalls[0].arguments[2]);",
      );
    writeFileSync(testFile, opaque);
    const { report: opaqueReport } = capture();
    assert.ok(
      opaqueReport.pragmas.some((p) =>
        p.hint.payloadSensitivity?.reason?.includes(
          "payload-predicate-undecidable-in-model",
        ),
      ),
    );
  },
);
