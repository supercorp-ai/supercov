import test from "node:test";
import assert from "node:assert/strict";
import {
  cpSync,
  mkdtempSync,
  mkdirSync,
  readdirSync,
  readFileSync,
  writeFileSync,
  symlinkSync,
  rmSync,
  existsSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { resolve } from "node:path";
import { spawnSync } from "node:child_process";

const repository = resolve(import.meta.dirname, "../../..");
test(
  "closed count guidance binds routing questions to existing assertions across table rows",
  { skip: process.env.SUPERCOV_ASSERTED_INTEGRATION !== "1" },
  (t) => {
    const root = mkdtempSync(resolve(tmpdir(), "supercov-count-sensitivity-"));
    t.after(() => {
      if (!process.env.SUPERCOV_KEEP_ASSERTED_FIXTURE)
        rmSync(root, { recursive: true, force: true });
    });
    if (process.env.SUPERCOV_KEEP_ASSERTED_FIXTURE) t.diagnostic(root);
    cpSync(resolve(import.meta.dirname, "fixtures/count-sensitivity"), root, {
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
    for (const key of [
      "NODE_OPTIONS",
      "NODE_PATH",
      "NODE_TEST_CONTEXT",
      "NODE_V8_COVERAGE",
    ])
      delete env[key];
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
      let page = first;
      const pragmas = [...first.pragmas];
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
      const runs = resolve(root, ".supercov/runs");
      const old = new Set(existsSync(runs) ? readdirSync(runs) : []);
      assert.match(
        ok(run(binary, ["--", process.execPath, ...suite])),
        /# pass 6/,
      );
      const id = readdirSync(resolve(root, ".supercov/runs")).find(
        (id) => !old.has(id),
      );
      return {
        id,
        report: query(id),
      };
    };
    const sourceFile = resolve(root, "src/logger.mjs"),
      testFile = resolve(root, "tests/core.test.mjs");
    const source = readFileSync(sourceFile, "utf8"),
      tests = readFileSync(testFile, "utf8");
    assert.match(ok(run(process.execPath, suite)), /# pass 6/);
    const { id, report } = capture();
    assert.equal(report.pragmas.length, 24);
    const supported = report.pragmas.filter(
      (p) => p.validation === "analyzer-supported",
    );
    assert.ok(
      supported.length >= 12,
      JSON.stringify(
        report.pragmas.map((p) => [
          p.hint.target?.snippet,
          p.reason,
          p.hint.countSensitivity?.reason,
        ]),
      ),
    );
    assert.ok(
      supported.some((p) =>
        p.hint.countSensitivity.variants.some((v) => v.outcome === "rejected"),
      ),
    );
    assert.ok(
      supported.some((p) =>
        p.hint.countSensitivity.variants.some(
          (v) => v.outcome === "not-rejected",
        ),
      ),
    );
    for (const p of supported) {
      assert.equal(p.reason, "modeled-count-sensitivity");
      assert.equal(p.hint.witness, "passed");
      assert.equal(
        p.hint.countSensitivity.originalCount,
        p.hint.countSensitivity.expectedCount,
      );
      assert.ok(p.hint.countSensitivity.allocations.length > 0);
      assert.equal(Object.hasOwn(p, "strength"), false);
    }
    assert.equal(report.assertionScore, null);

    // Exact supported condition edits are independently exercised natively.
    for (const [from, to] of [
      ["mode === 'quiet'", "true"],
      ["mode === 'quiet'", "false"],
      ["destination === 'terminal' ? verboseStderr", "true ? verboseStderr"],
      ["destination === 'terminal' ? verboseStderr", "false ? verboseStderr"],
      [
        "destination === 'terminal' ? normalStderr",
        "destination !== 'terminal' ? normalStderr",
      ],
    ]) {
      writeFileSync(sourceFile, source.replace(from, to));
      const changed = run(process.execPath, suite);
      assert.equal(changed.status, 1, changed.stdout);
      assert.match(changed.stdout, /ERR_ASSERTION/);
    }
    writeFileSync(sourceFile, source);
    writeFileSync(testFile, tests.replace(/^.*\/\/ observes:.*\n/gm, ""));
    assert.match(ok(run(process.execPath, suite)), /# pass 6/);
    assert.notEqual(
      run(binary, ["runs", id, "assertions", "--pragmas", "--json"]).status,
      0,
    );

    // Effects outside the selected prefix must not earn shared-allocation permission.
    for (const variant of [
      tests.replace(
        "const cases = [",
        "selectLogger({mode:'normal',destination:'network'}).info = () => {};\nconst cases = [",
      ),
      tests.replace(
        "const cases = [",
        "test('prior effect', () => { selectLogger({mode:'normal',destination:'network'}).info = () => {}; });\nconst cases = [",
      ),
      tests.replace("const logger =", "t.before(() => {}); const logger ="),
      tests.replace("(t) => {", "(t, setup = console.log) => {"),
    ]) {
      writeFileSync(testFile, variant);
      // May fail or pass natively; checking cannot turn a rejected scope into credit.
      const old = new Set(readdirSync(resolve(root, ".supercov/runs")));
      run(binary, ["--", process.execPath, ...suite]);
      const next = readdirSync(resolve(root, ".supercov/runs")).find(
        (id) => !old.has(id),
      );
      const guarded = query(next);
      assert.ok(
        guarded.pragmas.every((p) => p.validation !== "analyzer-supported"),
        JSON.stringify(guarded.pragmas),
      );
    }
    writeFileSync(testFile, tests);
    // Native aliases can mutate receivers/captured state despite `const` and an
    // unchanged call count. They are not source-closure ownership certificates.
    for (const replacement of [
      "const popAlias = Array['prototype'].pop; const makeMethod = () => popAlias; const silent = { info: makeMethod(), error: () => {} };",
      "const makeMethod = (state = { map: Array['prototype'].pop, length: 2, 0: 'x', 1: 'y' }) => () => state.map(() => {}); const silent = { info: makeMethod(), error: () => {} };",
    ]) {
      writeFileSync(
        sourceFile,
        source.replace(
          "const silent = { info: () => {}, error: () => {} };",
          replacement,
        ),
      );
      assert.match(ok(run(process.execPath, suite)), /# pass 6/);
      const guarded = capture().report;
      assert.ok(
        guarded.pragmas.every((p) => p.validation !== "analyzer-supported"),
      );
    }
    writeFileSync(sourceFile, source);
    // Do not run through an earlier primitive assertion that would already reject
    // a changed route and then attribute that failure to a later count assertion.
    const priorSource = source.replace(
      "info: () => {}, error: () => {}",
      "info: () => 1, error: () => {}",
    );
    const priorTest = tests.replace(
      "logger.info('hello', { a: 1 });",
      "assert.equal(logger.info('hello', { a: 1 }), mode === 'quiet' ? 1 : undefined);",
    );
    writeFileSync(sourceFile, priorSource);
    writeFileSync(testFile, priorTest);
    assert.match(ok(run(process.execPath, suite)), /# pass 6/);
    const prior = capture().report.pragmas.filter(
      (p) => p.hint.countSensitivity?.conditionText === "mode === 'quiet'",
    );
    assert.ok(
      prior.some((p) =>
        p.hint.countSensitivity.variants?.some((v) =>
          v.reason?.includes("earlier-primitive-assertion-rejects"),
        ),
      ),
    );
    assert.ok(
      prior.every((p) =>
        p.hint.countSensitivity.variants?.every(
          (v) => v.outcome !== "rejected",
        ),
      ),
    );
    writeFileSync(sourceFile, priorSource.replace("mode === 'quiet'", "true"));
    assert.equal(run(process.execPath, suite).status, 1);
    writeFileSync(sourceFile, source);
    writeFileSync(testFile, tests);
  },
);
