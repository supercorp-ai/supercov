import test from "node:test";
import assert from "node:assert/strict";
import { inspect } from "node:util";
import {
  cpSync,
  mkdtempSync,
  mkdirSync,
  readdirSync,
  readFileSync,
  writeFileSync,
  symlinkSync,
  existsSync,
  rmSync,
} from "node:fs";
import { resolve } from "node:path";
import { tmpdir } from "node:os";
import { spawnSync } from "node:child_process";
const repository = resolve(import.meta.dirname, "../../..");

test("native predicate summaries retain quoted-string uncertainty and exact String/regexp semantics", () => {
  for (const text of ["", "hello", "a_b-42", "with space", "x".repeat(64)])
    for (const colors of [true, false])
      for (const compact of [true, false]) {
        const formatted = inspect(text, { colors, compact, depth: null });
        assert.ok(formatted.includes("'"));
        assert.notEqual(formatted, text);
      }
  for (const object of [{}, { a: 1 }, { b: "different", c: null }])
    assert.equal(String(object), "[object Object]");
  assert.equal(
    /a: 1/.test(
      inspect({ a: 1 }, { depth: null, colors: false, compact: false }),
    ),
    true,
  );
  assert.equal(
    /a: 1/.test(
      inspect({ a: 1 }, { depth: null, colors: true, compact: false }),
    ),
    false,
  );
  assert.equal(/a: 1/.test(String({ a: 1 })), false);
});

test(
  "public payload queries use the original witness without lending it to changed or unsupported predicates",
  { skip: process.env.SUPERCOV_ASSERTED_INTEGRATION !== "1" },
  (t) => {
    const root = mkdtempSync(resolve(tmpdir(), "supercov-payload-native-"));
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
        "fixtures/payload-native-predicates/core.test.mjs",
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
    const binary = resolve(repository, "target/debug/supercov"),
      suite = [
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
    const capture = () => {
      const dir = resolve(root, ".supercov/runs"),
        before = new Set(existsSync(dir) ? readdirSync(dir) : []);
      assert.match(
        ok(run(binary, ["--", process.execPath, ...suite])),
        /# pass 2/,
      );
      const id = readdirSync(dir).find((r) => !before.has(r));
      const query = (more) =>
        JSON.parse(
          ok(
            run(binary, [
              "runs",
              id,
              "assertions",
              "--pragmas",
              ...more,
              "--json",
            ]),
          ),
        ).data;
      const first = query([]),
        pragmas = [...first.pragmas];
      let page = first;
      while (page.pagination.hasMore) {
        page = query([
          "--analysis",
          first.analysisId,
          "--offset",
          String(page.pagination.nextOffset),
        ]);
        pragmas.push(...page.pragmas);
      }
      assert.equal(pragmas.length, first.pagination.total);
      return { ...first, pragmas };
    };
    const sourceFile = resolve(root, "src/logger.mjs"),
      testFile = resolve(root, "tests/core.test.mjs");
    const source = readFileSync(sourceFile, "utf8"),
      tests = readFileSync(testFile, "utf8");
    const first = capture();
    const supported = first.pragmas.filter(
      (p) => p.validation === "analyzer-supported",
    );
    assert.equal(
      supported.length,
      6,
      JSON.stringify(
        first.pragmas.map((p) => [
          p.hint.raw,
          p.reason,
          p.hint.payloadSensitivity,
        ]),
        null,
        2,
      ),
    );
    assert.equal(first.assertionScore, null);
    for (const p of supported) {
      const e = p.hint.payloadSensitivity;
      assert.equal(e.model, "node-closed-payload-sensitivity-v2");
      assert.equal(p.hint.witness, "passed");
      assert.equal(Object.hasOwn(p, "strength"), false);
      if (e.original.projection.argumentIndex === 1) {
        assert.equal(e.original.outcome, "not-rejected");
        for (const v of e.variants.filter(
          (v) => v.change !== "condition-false",
        )) {
          assert.equal(v.check.actual.kind, "quoted-string");
          assert.equal(v.check.outcome, "rejected");
        }
      } else {
        assert.equal(e.original.outcome, "witnessed-pass");
        assert.equal(e.original.actual.kind, "opaque-string");
        assert.deepEqual(e.original.expected, {
          kind: "substring-pattern",
          value: "a: 1",
        });
        const v = e.variants.find((v) => v.change === "condition-false");
        assert.equal(v.check.outcome, "rejected");
        assert.deepEqual(v.check.actual, {
          kind: "string",
          value: "[object Object]",
        });
        assert.equal(
          v.check.projection.coercion.rule,
          "plain-object-default-string",
        );
        assert.ok(
          e.variants.every(
            (v) => !v.check || v.check.outcome !== "witnessed-pass",
          ),
        );
      }
    }
    for (const [from, to] of [
      ["typeof arg === 'object'", "true"],
      ["typeof arg === 'object'", "false"],
      ["typeof arg === 'object'", "typeof arg !== 'object'"],
      ["mode === 'verbose'", "false"],
    ]) {
      writeFileSync(sourceFile, source.replace(from, to));
      const changed = run(process.execPath, suite);
      assert.equal(changed.status, 1);
      assert.match(changed.stdout, /ERR_ASSERTION/);
    }
    writeFileSync(sourceFile, source);
    for (const pattern of ["/a: 1/i", "/a: 1/g", "/a: [0-9]/", "/(a: 1)/"]) {
      writeFileSync(testFile, tests.replace("/a: 1/", pattern));
      const r = capture();
      assert.ok(
        r.pragmas
          .filter((p) => p.hint.assertionMethod === "match")
          .every((p) => p.validation !== "analyzer-supported"),
      );
    }
    writeFileSync(
      testFile,
      tests.replace("String(infoCalls[0].arguments[2])", "'a: 1'"),
    );
    assert.ok(
      capture()
        .pragmas.filter((p) => p.hint.assertionMethod === "match")
        .every((p) => p.validation !== "analyzer-supported"),
    );
    // Custom conversion and a shadowed String cannot inherit the native rule.
    writeFileSync(
      testFile,
      tests.replace("{ a: 1 }", "{ a: 1, toString: () => 'a: 1' }"),
    );
    assert.ok(
      capture().pragmas.every((p) => p.validation !== "analyzer-supported"),
    );
    writeFileSync(
      testFile,
      tests.replace(
        "const cases = [",
        "const String = () => 'a: 1';\nconst cases = [",
      ),
    );
    assert.ok(
      capture().pragmas.every((p) => p.validation !== "analyzer-supported"),
    );
    // A missing witness cannot be substituted with a guessed original rendering.
    writeFileSync(
      testFile,
      tests
        .replace(
          "    // observes: src/logger.mjs#formatData typeof arg === 'object'; check value\n    // observes: src/logger.mjs#selectLogger mode === 'verbose'; check value\n    assert.match",
          "    if (false) {\n    // observes: src/logger.mjs#formatData typeof arg === 'object'; check value\n    // observes: src/logger.mjs#selectLogger mode === 'verbose'; check value\n    assert.match",
        )
        .replace("/a: 1/);", "/a: 1/);\n    }"),
    );
    assert.ok(
      capture()
        .pragmas.filter((p) => p.hint.assertionMethod === "match")
        .every(
          (p) =>
            p.hint.witness !== "passed" &&
            p.validation !== "analyzer-supported",
        ),
    );
    // Outside the narrow quoting domain, transformed strings remain opaque.
    const outsideTests = tests.replaceAll('"hello"', '"hello!"');
    assert.notEqual(outsideTests, tests);
    writeFileSync(testFile, outsideTests);
    const outside = capture().pragmas.filter(
      (p) => p.hint.assertionMethod === "equal",
    );
    assert.ok(outside.length);
    assert.ok(
      outside.every((p) =>
        p.hint.payloadSensitivity?.variants
          ?.filter((v) => v.change !== "condition-false")
          .every((v) => v.status === "unresolved"),
      ),
    );
    writeFileSync(testFile, tests.replace(/^.*\/\/ observes:.*\n/gm, ""));
    assert.match(ok(run(process.execPath, suite)), /# pass 2/);
  },
);
