import test from "node:test";
import assert from "node:assert/strict";
import {
  cpSync,
  mkdirSync,
  mkdtempSync,
  symlinkSync,
  readdirSync,
  readFileSync,
  writeFileSync,
  rmSync,
} from "node:fs";
import { resolve } from "node:path";
import { tmpdir } from "node:os";
import { spawnSync } from "node:child_process";

test(
  "custom registrations retain witnessed bodies separately from unknown test scope",
  { skip: process.env.SUPERCOV_ASSERTED_INTEGRATION !== "1" },
  (t) => {
    const repository = resolve(import.meta.dirname, "../../..");
    const root = mkdtempSync(resolve(tmpdir(), "supercov-unlinked-tests-"));
    t.after(() => rmSync(root, { recursive: true, force: true }));
    cpSync(resolve(import.meta.dirname, "fixtures/unlinked-tests"), root, {
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
    const run = (command, args) => {
      const r = spawnSync(command, args, {
        cwd: root,
        env,
        encoding: "utf8",
        timeout: 60000,
        maxBuffer: 16 * 1024 * 1024,
      });
      assert.equal(r.error, undefined);
      assert.equal(r.status, 0, r.stderr || r.stdout);
      return r.stdout;
    };
    const binary = resolve(repository, "target/debug/supercov");
    const suite = [
      "--test",
      "--test-concurrency=1",
      "--test-reporter=tap",
      "tests/aliased.test.mjs",
      "tests/direct.test.mjs",
      "tests/wrapped.test.mjs",
    ];
    assert.match(run(process.execPath, suite), /# pass 5/);
    assert.match(run(binary, ["--", process.execPath, ...suite]), /# pass 5/);
    const [id] = readdirSync(resolve(root, ".supercov/runs"));
    const query = (args) =>
      JSON.parse(run(binary, ["runs", id, "assertions", ...args, "--json"]))
        .data;
    const first = query([]),
      sites = [...first.sites];
    let page = first;
    while (page.pagination.hasMore) {
      page = query([
        "--analysis",
        first.analysisId,
        "--offset",
        String(page.pagination.nextOffset),
      ]);
      sites.push(...page.sites);
    }
    const evidence = (pointer) => {
      const page = query([
        "--analysis",
        first.analysisId,
        "--evidence",
        pointer,
      ]);
      const items = [...page.items];
      let next = page;
      while (next.pagination.hasMore) {
        next = query([
          "--analysis",
          first.analysisId,
          "--evidence",
          pointer,
          "--offset",
          String(next.pagination.nextOffset),
        ]);
        items.push(...next.items);
      }
      return items.map((i) => i.value);
    };
    const facts = evidence("/tests");
    assert.equal(facts.length, 5, JSON.stringify(facts));
    const unlinked = facts.filter((t) =>
      t.witnessIssues?.some((i) => i.kind === "test-source-unlinked"),
    );
    assert.equal(unlinked.length, 1);
    assert.ok(
      unlinked.every(
        (t) =>
          t.file === "tests/aliased.test.mjs" && t.observations.length === 0,
      ),
    );
    const witnessed = facts.filter((t) =>
      t.witnessIssues?.some(
        (i) => i.kind === "test-registration-scope-unverified",
      ),
    );
    assert.equal(witnessed.length, 2);
    assert.ok(
      witnessed[0].observations.some((o) => o.boundary === "return:aliased"),
    );
    assert.equal(evidence("/diagnostics/witnessedBodyLinks").length, 2);
    assert.equal(evidence("/diagnostics/linkedTests")[0], 4);
    assert.equal(evidence("/diagnostics/runtimeTests")[0], 5);
    assert.equal(evidence("/diagnostics/unlinkedTests").length, 1);
    const candidate = (owner) =>
      sites.find((s) => s.site.owner === owner && s.site.category === "return")
        .candidate;
    for (const owner of ["witnessless"]) {
      const c = candidate(owner);
      assert.equal(c.reason.kind, "limit:assertion-witness");
      assert.equal(c.status, "unresolved");
      assert.ok(c.witnessIssues.some((i) => i.kind === "test-source-unlinked"));
    }
    assert.ok(
      candidate("aliased").witnessIssues.some(
        (i) => i.kind === "test-registration-scope-unverified",
      ),
    );
    assert.equal(candidate("independent").status, "evident");
    assert.equal(candidate("unchecked").reason.kind, "gap:not-asserted");
    assert.equal(candidate("untouched").reason.kind, "gap:not-reached");
    const hints = query(["--pragmas"]);
    assert.equal(hints.pragmas.length, 2);
    // The real assertion now owns the hint. It cannot prove the registrar's
    // row selection, failure propagation or the whole test's other observers.
    for (const pragma of hints.pragmas) {
      assert.equal(pragma.validation, "unresolved");
      assert.equal(pragma.reason, "test-registration-scope-unverified");
      assert.equal(pragma.hint.witness, "passed");
      assert.equal(pragma.hint.issue, null);
    }
    assert.equal(first.assertionScore, null);
    // A real counterexample to erasing the scope limit: this changes the
    // observed value and makes the assertion throw, but the wrapper catches
    // it and the native test still passes. Only the disposable fixture changes.
    const production = resolve(root, "src/core.mjs");
    const original = readFileSync(production, "utf8");
    assert.equal(original.split("return 7;").length, 2);
    writeFileSync(production, original.replace("return 7;", "return 0;"));
    assert.match(
      run(process.execPath, [
        "--test",
        "--test-reporter=tap",
        "tests/wrapped.test.mjs",
      ]),
      /# pass 1/,
    );
    t.diagnostic(
      JSON.stringify({
        id,
        compiler: first.analyzer.compiler.version,
        tests: facts.length,
        unlinked: unlinked.length,
      }),
    );
  },
);
