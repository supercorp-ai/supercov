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
  async (t) => {
    const root = mkdtempSync(resolve(tmpdir(), "supercov-mock-counts-"));
    if (process.env.SUPERCOV_KEEP_ASSERTED_FIXTURE) t.diagnostic(root);
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
    const suite = [
      "--test",
      "--test-concurrency=1",
      "--test-reporter=tap",
      "tests/core.test.mjs",
      "tests/rows.test.mjs",
      "tests/row-limits.test.mjs",
      "tests/eval.test.mjs",
    ];
    const nativeOutput = ok(run(process.execPath, suite));
    const nativeNames = [...nativeOutput.matchAll(/^# Subtest: (.+)$/gm)].map(
      (match) => match[1],
    );
    assert.equal(nativeNames.length, 66);
    // Verify the native side effect separately from the captured count-only
    // fixture: adding a value assertion there would introduce another oracle.
    const nativeReceiverMutation = JSON.parse(
      ok(
        run(process.execPath, [
          "--input-type=module",
          "-e",
          "import {nativeSharedLogger} from './src/native-methods.mjs'; const logger=nativeSharedLogger(); const before=Object.hasOwn(logger,'length'); logger.info('hello'); console.log(JSON.stringify({before,after:Object.hasOwn(logger,'length'),length:logger.length,same:logger===nativeSharedLogger()}));",
        ]),
      ),
    );
    assert.deepEqual(nativeReceiverMutation, {
      before: false,
      after: true,
      length: 0,
      same: true,
    });
    const oracle = [];
    for (const [name, before, after, survives, file = "core.mjs"] of [
      [
        "count ignores payload",
        "console.log('live');",
        "console.log('changed payload');",
        true,
      ],
      ["count detects missing call", "console.log('live');", "", false],
      [
        "map payload remains unchecked",
        "'mapped payload'",
        "'different mapped payload'",
        true,
        "arrays.mjs",
      ],
      [
        "map callback effect removal detected",
        "console.log(value, index, array.length);",
        "",
        false,
        "arrays.mjs",
      ],
      [
        "map callback evaluation removal detected",
        "console.log('callback evaluated');",
        "",
        false,
        "arrays.mjs",
      ],
      [
        "slice value remains unchecked",
        "'selected'",
        "'different selected value'",
        true,
        "arrays.mjs",
      ],
      [
        "object payload remains unchecked",
        "'object payload'",
        "'different object payload'",
        true,
      ],
      [
        "object payload call removal detected",
        "console.log({ payload: ['object payload', { n: 1 }] });",
        "",
        false,
      ],
      [
        "object argument side effect removal detected",
        "console.log('object argument side effect');",
        "",
        false,
      ],
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
      [
        "factory payload ignored",
        "console.log(prefix, ...format(args))",
        "console.log('different')",
        true,
        "factories.mjs",
      ],
      [
        "factory call removed",
        "console.log(prefix, ...format(args))",
        "void 0",
        false,
        "factories.mjs",
      ],
      [
        "factory call duplicated",
        "console.log(prefix, ...format(args))",
        "(console.log(prefix), console.log(...args))",
        false,
        "factories.mjs",
      ],
      [
        "factory receiver changed",
        "console.error(...args)",
        "console.log(...args)",
        false,
        "factories.mjs",
      ],
      [
        "factory quiet branch inverted",
        "enabled === false",
        "enabled === true",
        false,
        "factories.mjs",
      ],
      [
        "module factory payload ignored",
        "module closure payload",
        "different module payload",
        true,
        "factories.mjs",
      ],
      [
        "nested argument call removed",
        "console.log(console.log('nested call'))",
        "console.log('nested call')",
        false,
        "factories.mjs",
      ],
      [
        "array contents not checked",
        "console.log(...args)",
        "console.log('different array')",
        true,
        "factories.mjs",
      ],
      [
        "captured condition changed",
        "if (enabled) console.log",
        "if (!enabled) console.log",
        false,
        "factories.mjs",
      ],
      [
        "initialization excluded from test mock",
        "console.log('module initialization');",
        "",
        true,
        "effectful.mjs",
      ],
      [
        "post-initialization call checked natively",
        "console.log('after initialization')",
        "void 0",
        false,
        "effectful.mjs",
      ],
    ]) {
      const core = resolve(root, "src", file),
        original = readFileSync(core, "utf8");
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
    const complete = (key, ...args) => {
      const first = query(...args, "--limit", "1000");
      let current = first;
      const items = [...first[key]];
      while (current.pagination.hasMore) {
        const offset = current.pagination.nextOffset;
        assert.ok(
          Number.isInteger(offset) && offset > current.pagination.offset,
        );
        current = query(
          ...args,
          "--limit",
          "1000",
          "--offset",
          String(offset),
          "--analysis",
          first.analysisId,
        );
        items.push(...current[key]);
      }
      return { ...first, [key]: items };
    };
    const report = complete("sites"),
      page = complete("items", "--evidence", "/tests");
    const attempts = complete("items", "--evidence", "/attempts");
    const nativeOccurrences = new Map();
    for (const name of nativeNames)
      nativeOccurrences.set(name, (nativeOccurrences.get(name) ?? 0) + 1);
    assert.ok(
      attempts.items.every((item) => nativeOccurrences.has(item.value.name)),
    );
    for (const [name, count] of nativeOccurrences) {
      const actual = attempts.items.filter(
        (item) => item.value.name === name,
      ).length;
      if (count === 1)
        assert.equal(actual, 1, `unique native attempt missing: ${name}`);
      else assert.ok(actual >= 1 && actual <= count, name);
    }
    // Two native tests with the same title currently collapse in capture. Do not
    // turn that loss into the expected denominator; retain the desired behavior.
    await t.test(
      "capture preserves both same-title row attempts",
      {
        todo: "Duplicate-title capture loses an attempt; row analysis must not guess its identity",
      },
      () =>
        assert.equal(
          attempts.items.filter(
            (item) => item.value.name === "duplicate row same",
          ).length,
          2,
        ),
    );
    assert.ok(page.items.length >= 65 && page.items.length <= 66);
    const sourceLines = readFileSync(
      resolve(root, "tests/core.test.mjs"),
      "utf8",
    ).split("\n");
    const grouped = new Map();
    for (const item of page.items) {
      assert.ok(item.value);
      if (item.value.file !== "tests/core.test.mjs") continue;
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
    assert.deepEqual(owners(checked("object payload count")[0]), [
      "objectPayload",
    ]);
    assert.equal(checked("object payload count")[0].observedCount, 1);
    assert.equal(checked("saved object payload count")[0].observedCount, 1);
    assert.deepEqual(
      checked("closure payload is not invoked").map((r) => r.observedCount),
      [1, 0],
    );
    assert.equal(
      checked("saved object payload count")[0].installedAtRead,
      false,
    );
    assert.deepEqual(
      owners(checked("object argument side effects counted")[0]),
      ["payloadAfterCall", "nestedObjectPayload"],
    );
    assert.equal(
      checked("array map counts callback effects")[0].observedCount,
      2,
    );
    assert.deepEqual(
      checked("array map routes callback effects").map((r) => r.observedCount),
      [1, 1],
    );
    assert.equal(
      checked("array map output payload is not pinned")[0].observedCount,
      1,
    );
    const ordered = checked("array map evaluation order")[0];
    assert.equal(ordered.observedCount, 4);
    assert.notEqual(ordered.calls[0].source, ordered.calls[1].source);
    assert.equal(ordered.calls[2].source, ordered.calls[3].source);
    assert.deepEqual(
      checked("own map method is not array map").map((r) => r.observedCount),
      [0, 1],
    );
    const sliced = checked("sliced history survives reset");
    assert.deepEqual(
      sliced.map((r) => r.observedCount),
      [1, 1],
    );
    assert.deepEqual(owners(sliced[0]), ["afterHistory"]);
    assert.deepEqual(
      sliced[0].historySelections.map(({ inputCount, from, to }) => ({
        inputCount,
        from,
        to,
      })),
      [{ inputCount: 2, from: 1, to: 2 }],
    );
    assert.deepEqual(owners(checked("negative sliced history index")[0]), [
      "afterHistory",
    ]);
    const selections = checked("nested and empty sliced history");
    assert.equal(selections[0].historySelections.length, 2);
    assert.deepEqual(owners(selections[0]), ["afterHistory"]);
    assert.equal(selections[1].observedCount, 0);
    assert.equal(selections[1].historySelections[0].inputCount, 3);
    assert.deepEqual(
      checked("slice snapshots before argument effects").map(
        (r) => r.observedCount,
      ),
      [1, 2],
    );
    assert.deepEqual(
      owners(checked("slice snapshots before argument effects")[0]),
      ["live"],
    );
    assert.equal(
      checked("fresh array slice spreads selected values")[0].observedCount,
      1,
    );
    for (const name of [
      "conditional limit",
      "async limit",
      "escape limit",
      "self count limit",
      "reassigned function limit",
      "replacement callback limit",
      "mutated snapshot limit",
      "shared module object limit",
      "native factory method mutates shared receiver",
      "sparse array map limit",
      "mutating array map limit",
      "array map this argument limit",
      "coercing slice index limit",
      "effectful module initialization limit",
      "getter initialization limit",
      "receiver this limit",
      "missing own property limit",
      "closure mutation limit",
      "unmocked object payload limit",
      "getter payload remains unsupported",
    ])
      assert.ok(
        claims(name).every((r) => r?.status === "unresolved" && r.reason),
        name,
      );
    for (const rows of grouped.values())
      for (const row of rows)
        if (row.status === "source-checked") {
          assert.equal(row.model, "node-sync-console-count-v2");
          assert.equal(row.expectedCount, row.observedCount);
          assert.equal(row.calls.length, row.observedCount);
        }
    for (const name of [
      "discarded fresh object",
      "fresh closure",
      "fresh quiet branch",
      "fresh receiver branch",
      "separate closure environments",
      "pure module factory closure",
      "nested argument calls",
      "fresh array spread",
      "captured branch environments",
    ])
      checked(name);
    assert.deepEqual(
      checked("fresh receiver branch").map((r) => r.observedCount),
      [0, 1],
    );
    assert.equal(checked("fresh quiet branch")[0].observedCount, 0);
    assert.equal(checked("separate closure environments")[0].observedCount, 2);
    assert.equal(checked("nested argument calls")[0].observedCount, 2);
    assert.deepEqual(
      checked("captured branch environments").map((r) => r.observedCount),
      [1, 0],
    );
    assert.equal(checked("nested argument calls")[0].calls.length, 2);
    const nested = checked("nested argument calls")[0].calls.map((r) =>
      r.source.split(":").slice(-2).map(Number),
    );
    assert.ok(
      nested[0][0] > nested[1][0] && nested[0][1] < nested[1][1],
      "inner call occurs before outer call",
    );
    assert.match(
      claims("shared module object limit")[0].reason,
      /^shared-module-object-history/,
    );
    assert.match(
      claims("effectful module initialization limit")[0].reason,
      /^effectful-module-initialization/,
    );
    assert.ok(
      report.sites.every((row) => row.candidate.status === "unresolved"),
      "count evidence is not general value protection",
    );
    const hints = query("--pragmas");
    assert.equal(hints.summary.analyzerSupported, 0);
    assert.equal(hints.summary.unresolved, 1);
    assert.equal(report.assertionScore, null);
    const countEvidence = (file) =>
      page.items
        .filter((item) => item.value.file === file)
        .flatMap((item) =>
          item.value.observations
            .filter((ob) => ob.mock?.kind === "call-count")
            .map((ob) => ob.mock.countEvidence),
        );
    const rows = countEvidence("tests/rows.test.mjs");
    assert.equal(rows.length, 6);
    for (const [index, label, counts] of [
      [0, "c", [1, 0]],
      [1, "a", [0, 0]],
      [2, "b", [0, 1]],
    ]) {
      const claims = rows.filter((r) => r.rowBinding?.rowIndex === index);
      assert.equal(claims.length, 2, JSON.stringify(rows));
      assert.deepEqual(
        claims.map((r) => r.observedCount),
        counts,
      );
      for (const claim of claims) {
        assert.equal(claim.status, "source-checked");
        assert.equal(claim.expectedCount, claim.observedCount);
        const binding = claim.rowBinding;
        assert.equal(binding.status, "source-checked");
        assert.equal(binding.model, "node-test-for-of-v1");
        assert.equal(
          binding.bindings.find((b) => b.name === "label").value,
          label,
        );
        assert.ok(
          binding.loop && binding.table && binding.row && binding.title,
        );
      }
    }
    const rowLimits = countEvidence("tests/row-limits.test.mjs");
    assert.ok(rowLimits.length >= 5 && rowLimits.length <= 6);
    assert.ok(
      rowLimits.every(
        (r) =>
          r.status === "unresolved" && r.rowBinding.status === "unresolved",
      ),
    );
    assert.deepEqual(
      [...new Set(rowLimits.map((r) => r.reason))].sort(),
      [
        "row-table-escapes-or-may-change",
        "ambiguous-row-title",
        "unsupported-row-title",
        "unsupported-row-registration",
      ].sort(),
    );
    const evalLimits = countEvidence("tests/eval.test.mjs");
    assert.equal(evalLimits.length, 2);
    assert.ok(evalLimits.every((r) => r.status === "unresolved"));
    assert.ok(
      evalLimits.some((r) => r.reason === "row-table-escapes-or-may-change"),
    );
    assert.ok(
      evalLimits.some((r) =>
        r.reason.startsWith(
          "mutable-target-or-unsupported-module-initialization",
        ),
      ),
    );
    t.diagnostic(
      JSON.stringify({
        id,
        oracle,
        checkedObservations: [...grouped.values()]
          .flat()
          .filter((r) => r.status === "source-checked").length,
        sourceRowCheckedObservations: rows.length,
        nativeTests: 66,
        archivedTests: page.items.length,
      }),
    );
  },
);
