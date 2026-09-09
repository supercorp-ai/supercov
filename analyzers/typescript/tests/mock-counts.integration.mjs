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
  "mock count evidence separates instances, reset windows, snapshots and argument values",
  {
    skip: process.env.SUPERCOV_ASSERTED_INTEGRATION !== "1",
  },
  (t) => {
    const root = mkdtempSync(resolve(tmpdir(), "supercov-mock-counts-"));
    t.after(() => {
      if (!process.env.SUPERCOV_KEEP_ASSERTED_FIXTURE)
        rmSync(root, { recursive: true, force: true });
    });
    cpSync(resolve(import.meta.dirname, "fixtures/mock-counts"), root, {
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
    const suite = ["--test", "--test-concurrency=1", "tests/core.test.mjs"];
    ok(run(process.execPath, suite));
    const core = resolve(root, "src/core.mjs"),
      original = readFileSync(core, "utf8");
    const oracle = [];
    for (const [name, before, after, survives] of [
      [
        "count ignores payload",
        "console.log('live');",
        "console.log('changed payload');",
        true,
      ],
      ["count detects missing call", "console.log('live');", "", false],
      [
        "count detects extra call",
        "console.log('live');",
        "console.log('live'); console.log('extra');",
        false,
      ],
      [
        "reset discards earlier calls",
        "console.log('before reset');",
        "",
        true,
      ],
      ["reset retains later calls", "console.log('after reset');", "", false],
      [
        "wrong receiver is not observed",
        "console.error('wrong receiver');",
        "",
        true,
      ],
      [
        "wrong receiver switched to observed receiver",
        "console.error('wrong receiver');",
        "console.log('wrong receiver');",
        false,
      ],
      [
        "replacement sees current instance",
        "console.log('replaced');",
        "",
        false,
      ],
      [
        "restored mock misses later console call",
        "console.log('restored');",
        "",
        true,
      ],
      [
        "restoring previous instance reconnects it",
        "console.log('previous');",
        "",
        false,
      ],
      [
        "count snapshot checks earlier call",
        "console.log('before snapshot');",
        "",
        false,
      ],
      [
        "count snapshot misses later call",
        "console.log('after snapshot');",
        "",
        true,
      ],
      [
        "history snapshot keeps pre-reset call",
        "console.log('before history');",
        "",
        false,
      ],
      [
        "live history after reset checks later call",
        "console.log('after history');",
        "",
        false,
      ],
      [
        "repeated occurrences count separately",
        "console.log('twice');",
        "",
        false,
      ],
      [
        "count ignores argument transformation",
        "console.log(value);",
        "console.log('different');",
        true,
      ],
      ["self count checks no number", "console.log('self count');", "", true],
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
    const report = query("--limit", "1000"),
      page = query("--evidence", "/tests", "--limit", "1000");
    assert.equal(report.pagination.hasMore, false);
    assert.equal(page.pagination.hasMore, false);
    assert.equal(page.items.length, 19);
    const sourceLines = readFileSync(
      resolve(root, "tests/core.test.mjs"),
      "utf8",
    ).split("\n");
    const grouped = new Map();
    for (const item of page.items) {
      assert.ok(item.value);
      for (const ob of item.value.observations) {
        if (ob.mock?.kind !== "call-count") continue;
        const line = Number(ob.assertionSource.split(":").at(-2));
        const name = [
          ...sourceLines
            .slice(0, line)
            .join("\n")
            .matchAll(/test\('([^']+)'/g),
        ].at(-1)[1];
        if (!grouped.has(name)) grouped.set(name, []);
        grouped.get(name).push(ob.mock.countEvidence);
      }
    }
    const claims = (name) => {
      const rows = grouped.get(name);
      assert.ok(rows?.length, name);
      return rows;
    };
    const checked = (name) => {
      const rows = claims(name);
      assert.ok(
        rows.every((r) => r?.status === "source-checked"),
        JSON.stringify({ name, rows }),
      );
      return rows;
    };
    const owners = (claim) =>
      claim.calls.map(
        (call) =>
          report.sites.find((row) => row.site.id === call.site)?.site.owner ??
          "<test>",
      );
    assert.deepEqual(owners(checked("live")[0]), ["live"]);
    assert.deepEqual(owners(checked("reset")[0]), ["afterReset"]);
    assert.ok(checked("reset")[0].resetAt);
    assert.deepEqual(owners(checked("wrong receiver")[0]), []);
    assert.deepEqual(
      checked("replacement").map((r) => r.observedCount),
      [0, 1],
    );
    assert.notEqual(
      checked("replacement")[0].instance,
      checked("replacement")[1].instance,
    );
    assert.deepEqual(owners(checked("restore")[0]), []);
    assert.equal(checked("restore")[0].installedAtRead, false);
    assert.deepEqual(
      checked("restore previous").map((r) => r.observedCount),
      [1, 0],
    );
    assert.deepEqual(owners(checked("saved spy")[0]), ["<test>"]);
    assert.equal(checked("saved spy")[0].installedAtRead, false);
    assert.deepEqual(owners(checked("count snapshot")[0]), ["beforeSnapshot"]);
    assert.deepEqual(checked("history snapshot").map(owners), [
      ["beforeHistory"],
      ["afterHistory"],
    ]);
    assert.equal(checked("history snapshot")[0].resetAt, undefined);
    assert.ok(checked("history snapshot")[1].resetAt);
    assert.deepEqual(owners(checked("repeated occurrence")[0]), [
      "twice",
      "twice",
    ]);
    assert.notEqual(
      ...checked("repeated occurrence")[0].calls.map((c) => c.action),
    );
    assert.deepEqual(owners(checked("parameter")[0]), ["parameter"]);
    for (const name of [
      "conditional limit",
      "async limit",
      "escape limit",
      "self count limit",
      "opaque producer limit",
      "reassigned function limit",
      "replacement callback limit",
      "mutated snapshot limit",
    ])
      assert.ok(
        claims(name).every((r) => r?.status === "unresolved" && r.reason),
        name,
      );
    for (const rows of grouped.values())
      for (const row of rows)
        if (row.status === "source-checked") {
          assert.equal(row.model, "node-sync-console-count-v1");
          assert.equal(row.expectedCount, row.observedCount);
          assert.equal(row.calls.length, row.observedCount);
        }
    assert.ok(
      report.sites.every((row) => row.candidate.status === "unresolved"),
      "count evidence is not general value protection",
    );
    const hints = query("--pragmas");
    assert.equal(hints.summary.analyzerSupported, 0);
    assert.equal(hints.summary.unresolved, 1);
    assert.equal(report.assertionScore, null);
    t.diagnostic(
      JSON.stringify({
        id,
        oracle,
        checkedObservations: [...grouped.values()]
          .flat()
          .filter((r) => r.status === "source-checked").length,
      }),
    );
  },
);
