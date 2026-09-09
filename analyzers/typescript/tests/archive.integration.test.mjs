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
const binary = resolve(repository, "target/debug/supercov");
const enabled = process.env.SUPERCOV_ASSERTED_INTEGRATION === "1";
test(
  "ordinary archive → matching JS source → public per-site query, with native counterexamples",
  { skip: !enabled },
  async (t) => {
    const root = mkdtempSync(
      resolve(tmpdir(), "supercov-js-asserted-integration-"),
    );
    t.after(() => {
      if (!process.env.SUPERCOV_KEEP_ASSERTED_FIXTURE)
        rmSync(root, { recursive: true, force: true });
    });
    cpSync(resolve(import.meta.dirname, "fixtures/archive"), root, {
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
    symlinkSync(
      resolve(repository, "node_modules/vitest"),
      resolve(root, "node_modules/vitest"),
    );
    const vitestPath = resolve(root, "tests/core.vitest.test.mjs");
    writeFileSync(
      vitestPath,
      readFileSync(resolve(root, "tests/core.test.mjs"), "utf8").replace(
        "import test from 'node:test'",
        "import { test } from 'vitest'",
      ),
    );
    // This integration is itself a node:test child; the independent suite must not
    // inherit node's private parent-runner transport mode.
    const environment = { ...process.env, SUPERCOV_PACKAGE_ROOT: repository };
    delete environment.NODE_TEST_CONTEXT;
    const exec = (args, native = false) =>
      spawnSync(native ? process.execPath : binary, args, {
        cwd: root,
        encoding: "utf8",
        timeout: 60000,
        maxBuffer: 16 * 1024 * 1024,
        env: environment,
      });
    const ok = (result) => {
      assert.equal(result.status, 0, result.stderr || result.stdout);
      if (result.stdout.trimStart().startsWith("{"))
        assert.ok(
          Buffer.byteLength(result.stdout) <= 65_536,
          "public JSON stays bounded",
        );
      return result;
    };
    const native = () =>
      exec(["--test", "--test-concurrency=1", "tests/core.test.mjs"], true);
    const nativeVitest = () =>
      exec(
        [
          "node_modules/vitest/vitest.mjs",
          "run",
          "tests/core.vitest.test.mjs",
          "--maxWorkers=1",
        ],
        true,
      );
    ok(native());
    ok(nativeVitest());
    // Both ordinary archive paths retain the same assertion bodies and probes.
    ok(
      exec([
        "--",
        "node",
        "node_modules/vitest/vitest.mjs",
        "run",
        "tests/core.vitest.test.mjs",
        "--maxWorkers=1",
      ]),
    );
    const [runId] = readdirSync(resolve(root, ".supercov/runs"));
    const args = ["runs", runId, "assertions", "--limit", "1000", "--json"];
    const query = () => {
      const envelope = JSON.parse(ok(exec(args)).stdout);
      assert.equal(envelope.command, "coverage.assertions");
      return envelope.data;
    };
    const result = query();
    const evidence = (id, pointer, analysisId, ...options) =>
      JSON.parse(
        ok(
          exec([
            "runs",
            id,
            "assertions",
            "--evidence",
            pointer,
            "--analysis",
            analysisId,
            "--json",
            ...options,
          ]),
        ).stdout,
      ).data;
    assert.equal(result.reportSchema, 2);
    assert.match(result.analysisId, /^[a-f0-9]{64}$/);
    assert.match(
      exec(["runs", runId, "assertions", "--analysis", "0".repeat(64), "--json"])
        .stdout,
      /analysis changed/,
    );
    assert.match(
      exec(["runs", runId, "assertions", "--evidence", "/absent", "--json"])
        .stdout,
      /pointer does not exist/,
    );
    const checkPragmas = (id) => {
      const readPragmas = (...options) =>
        JSON.parse(
          ok(exec(["runs", id, "assertions", "--pragmas", "--json", ...options]))
            .stdout,
        ).data;
      const all = readPragmas();
      assert.equal(all.view, "pragmas");
      assert.equal(all.pragmas.length, 12);
      assert.equal(all.summary.analyzerSupported, 2);
      const supported = all.pragmas.filter(
        (p) => p.validation === "analyzer-supported",
      );
      assert.deepEqual(supported.map((p) => p.strength).sort(), [
        "presence",
        "value",
      ]);
      for (const p of all.pragmas) {
        assert.equal(p.origin, "user-suggested");
        if (p.validation === "analyzer-supported") {
          assert.equal(p.hint.witness, "passed");
          assert.ok(
            p.observations.every(
              (o) => o.assertionSource === p.hint.assertionSource,
            ),
          );
        } else assert.equal(p.strength, undefined);
      }
      const reasons = all.pragmas.map((p) => p.reason);
      for (const reason of [
        "target-not-in-inventory",
        "ambiguous-target",
        "invalid-syntax",
        "unattached-assertion",
        "ambiguous-assertion",
        "call-not-recorded",
        "mixed-call-outcomes",
        "call-failed",
      ])
        assert.ok(reasons.includes(reason), reason);
      const discarded = all.pragmas.find(
        (p) => p.hint.target?.function === "discarded",
      );
      assert.equal(discarded.validation, "unresolved");
      const pages = Array.from(
        { length: 12 },
        (_, offset) =>
          readPragmas("--offset", String(offset), "--limit", "1").pragmas[0],
      );
      assert.deepEqual(pages, all.pragmas);
      assert.equal(
        readPragmas("--offset", "12", "--limit", "1").pragmas.length,
        0,
      );
      const only = readPragmas("--site", supported[0].hint.candidateSites[0]);
      assert.ok(
        only.pragmas.every((p) =>
          p.hint.candidateSites.includes(supported[0].hint.candidateSites[0]),
        ),
      );
      assert.match(
        ok(exec(["runs", id, "assertions", "--pragmas"])).stdout,
        /user-suggested; not formal proofs/,
      );
      return all;
    };
    checkPragmas(runId);
    const checkWitnessReasons = (result) => {
      for (const owner of ["unchecked", "sameLine"]) {
        const candidate = result.sites.find(
          (row) => row.site.owner === owner,
        ).candidate;
        assert.equal(candidate.reason.kind, "limit:assertion-witness", owner);
        assert.ok(
          candidate.witnessIssues.some(
            (issue) => issue.kind === "call-not-recorded",
          ),
        );
      }
      const caught = result.sites.find(
        (row) => row.site.owner === "caught",
      ).candidate;
      assert.equal(caught.reason.kind, "gap:not-asserted");
      assert.ok(
        caught.witnessIssues.some((issue) => issue.kind === "call-failed"),
      );
      // The new hint fixtures deliberately add two rejected calls to exact,
      // alongside its valid checks. Preserve their provenance without losing
      // the independent successful observations.
      assert.deepEqual(
        result.sites
          .find((row) => row.site.owner === "exact")
          .candidate.witnessIssues.map((issue) => issue.kind)
          .sort(),
        ["call-not-recorded", "mixed-call-outcomes"],
      );
    };
    checkWitnessReasons(result);
    assert.ok(result.sites.length >= 8);
    assert.equal(result.summary.sites, result.sites.length);
    assert.equal(result.assertionScore, null);
    assert.equal(Object.hasOwn(result, "experimental"), false);
    assert.equal(result.summary.semanticallyVerifiedSites, 0);
    assert.ok(
      result.evidence.executionLinks.count > 0,
      "existing assertion probes must be retained",
    );
    assert.ok(
      evidence(runId, "/diagnostics/linkedTests", result.analysisId).items[0]
        .value >= 7,
    );
    assert.equal(
      result.tests,
      undefined,
      "shared evidence is separately paged",
    );
    const testEvidence = evidence(
      runId,
      "/tests",
      result.analysisId,
      "--limit",
      "1",
    );
    assert.equal(testEvidence.items.length, 1);
    assert.equal(testEvidence.pagination.nextOffset, 1);
    for (const row of result.sites)
      assert.equal(row.analysisCertainty, "unverified");
    assert.deepEqual(
      query(),
      result,
      "query is deterministic and does not add tests/runs",
    );
    const exactSite = result.sites.find(
      (r) => r.site.owner === "exact" && r.site.category === "return",
    );
    assert.ok(exactSite);
    assert.equal(exactSite.candidate.status, "evident");
    const typedSite = result.sites.find(
      (r) => r.site.file === "src/typed.ts" && r.site.category === "return",
    );
    assert.equal(
      typedSite?.candidate.status,
      "evident",
      "ordinary TypeScript source is analyzed, not only JavaScript",
    );
    const page = JSON.parse(
      ok(
        exec([
          "runs",
          runId,
          "assertions",
          "--site",
          exactSite.site.id,
          "--json",
        ]),
      ).stdout,
    ).data;
    assert.equal(page.sites.length, 1);
    assert.equal(page.pagination.total, 1);
    assert.ok(result.sites.some((r) => r.site.kind === "decision"));
    // Deliberately enumerate specific changes rather than relabel them as all possible behavior.
    const coreFile = resolve(root, "src/core.mjs"),
      cliFile = resolve(root, "src/cli.mjs");
    const core = readFileSync(coreFile, "utf8"),
      cli = readFileSync(cliFile, "utf8");
    const variants = [
      ...[
        "overwritten",
        "ignoredCallback",
        "caught",
        "sameLine",
        "retainedAlias",
      ].map((owner) => [
        `${owner} return changes`,
        coreFile,
        core.replace(
          `function ${owner}() { return 4; }`,
          `function ${owner}() { return 9; }`,
        ),
        owner !== "retainedAlias",
      ]),
      [
        "exact return changes",
        coreFile,
        core.replace(
          "function exact() {\n  return 4;",
          "function exact() {\n  return 9;",
        ),
        false,
      ],
      [
        "discarded return changes",
        coreFile,
        core.replace(
          "function discarded() {\n  return 4;",
          "function discarded() {\n  return 9;",
        ),
        true,
      ],
      [
        "truthy return changes",
        coreFile,
        core.replace(
          "function coerced() {\n  return 4;",
          "function coerced() {\n  return 9;",
        ),
        true,
      ],
      [
        "coerced predicate changes",
        coreFile,
        core.replace(
          "function coerced() {\n  return 4;",
          "function coerced() {\n  return 0;",
        ),
        false,
      ],
      [
        "fake fetch does not read status",
        coreFile,
        core.replace("response.status(201)", "response.status(500)"),
        true,
      ],
      [
        "unexecuted assertion",
        coreFile,
        core.replace(
          "function unchecked() {\n  return 4;",
          "function unchecked() {\n  return 9;",
        ),
        true,
      ],
      [
        "diagnostic preserves substring",
        cliFile,
        cli.replace("alpha", "omega"),
        true,
      ],
      [
        "diagnostic removes substring",
        cliFile,
        cli.replace("token:", "other:"),
        false,
      ],
      [
        "exit status changes",
        cliFile,
        cli.replace("exitCode = 3", "exitCode = 0"),
        false,
      ],
    ];
    const oracle = [];
    for (const [label, file, source, survives] of variants) {
      writeFileSync(file, source);
      try {
        const run = native();
        assert.equal(run.error, undefined, label);
        assert.equal(
          run.status === 0,
          survives,
          `${label}\n${run.stderr}\n${run.stdout}`,
        );
        const sameRunner = nativeVitest();
        assert.equal(sameRunner.error, undefined, label);
        assert.equal(
          sameRunner.status === 0,
          survives,
          `Vitest oracle: ${label}\n${sameRunner.stderr}\n${sameRunner.stdout}`,
        );
        const stale = exec(args);
        assert.equal(stale.status, 2);
        assert.match(stale.stdout, /stale run/);
        oracle.push({
          label,
          survives,
          nativeStatus: run.status,
          nativeVitestStatus: sameRunner.status,
        });
      } finally {
        writeFileSync(coreFile, core);
        writeFileSync(cliFile, cli);
      }
    }
    const testPath = resolve(root, "tests/core.test.mjs"),
      tests = readFileSync(testPath, "utf8");
    writeFileSync(testPath, tests + "\n// changed test source\n");
    assert.match(exec(args).stdout, /tests changed/);
    assert.match(
      exec(["runs", runId, "assertions", "--pragmas", "--json"]).stdout,
      /tests changed/,
    );
    writeFileSync(testPath, tests);
    const packagePath = resolve(root, "package.json"),
      packageText = readFileSync(packagePath, "utf8");
    writeFileSync(
      packagePath,
      packageText.replace('"private":true', '"private":false'),
    );
    assert.match(exec(args).stdout, /stale run/);
    writeFileSync(packagePath, packageText);
    assert.deepEqual(query(), result);
    for (const [owner, bug] of [
      ["discarded", "JS-ASSERT-001"],
      ["sendStatus", "JS-ASSERT-002"],
      ["unchecked", "JS-ASSERT-003"],
      ["overwritten", "JS-ASSERT-001 overwrite"],
      ["ignoredCallback", "JS-ASSERT-001 ignored callback"],
      ["caught", "JS-ASSERT-003 caught failure"],
      ["sameLine", "JS-ASSERT-003 exact column"],
    ]) {
      await t.test(
        `${bug}: ${owner} must not receive value credit`,
        {
          todo: false,
        },
        () => {
          assert.notEqual(
            result.sites.find((r) => r.site.owner === owner).candidate.status,
            "evident",
          );
        },
      );
    }
    await t.test(
      "JS-ASSERT-004: ordinary node:test evidence accepts existing statement markers",
      {
        todo: false,
      },
      () => {
        ok(
          exec([
            "--",
            "node",
            "--test",
            "--test-concurrency=1",
            "tests/core.test.mjs",
          ]),
        );
        const nodeRun = readdirSync(resolve(root, ".supercov/runs")).find(
          (id) => id !== runId,
        );
        assert.ok(nodeRun);
        const nodeResult = JSON.parse(
          ok(exec(["runs", nodeRun, "assertions", "--limit", "1000", "--json"]))
            .stdout,
        ).data;
        checkWitnessReasons(nodeResult);
        checkPragmas(nodeRun);
        for (const owner of ["exact", "twice", "retainedAlias"]) {
          assert.equal(
            nodeResult.sites.find((r) => r.site.owner === owner)?.candidate
              .status,
            "evident",
          );
        }
        for (const owner of [
          "discarded",
          "sendStatus",
          "unchecked",
          "overwritten",
          "ignoredCallback",
          "caught",
          "sameLine",
        ]) {
          assert.notEqual(
            nodeResult.sites.find((r) => r.site.owner === owner)?.candidate
              .status,
            "evident",
          );
        }
        assert.ok(
          evidence(
            nodeRun,
            "/executionLinks",
            nodeResult.analysisId,
            "--limit",
            "1000",
          ).items.some((entry) => entry.value?.statement),
          "node:test statement ownership survives archive projection",
        );
      },
    );
    await t.test(
      "oversized pragma remains fully readable through public evidence pages",
      () => {
        const raw =
          "// observes: src/core.mjs#exact return 4 via " +
          "😀 proof ".repeat(11_000);
        writeFileSync(
          testPath,
          tests.replace("// observes: src/core.mjs#exact return 4", raw),
        );
        const before = new Set(readdirSync(resolve(root, ".supercov/runs")));
        ok(
          exec([
            "--",
            "node",
            "--test",
            "--test-concurrency=1",
            "tests/core.test.mjs",
          ]),
        );
        const largeRun = readdirSync(resolve(root, ".supercov/runs")).find(
          (id) => !before.has(id),
        );
        const response = ok(
          exec(["runs", largeRun, "assertions", "--pragmas", "--json"]),
        );
        assert.ok(Buffer.byteLength(response.stdout) <= 65_536);
        const large = JSON.parse(response.stdout).data;
        assert.equal(large.summary.hints, 12);
        const ref = large.pragmas.find((row) => row.detailOnly)?.evidence
          .pointer;
        assert.ok(ref);
        const members = evidence(largeRun, `${ref}/hint`, large.analysisId);
        const textRef = members.items.find((entry) => entry.key === "raw");
        assert.equal(textRef.detailOnly, true);
        let restored = "",
          offset = 0;
        do {
          const chunk = evidence(
            largeRun,
            textRef.pointer,
            large.analysisId,
            "--offset",
            String(offset),
            "--limit",
            "4000",
          );
          assert.equal(chunk.pagination.unit, "unicodeScalars");
          restored += chunk.text;
          offset = chunk.pagination.nextOffset;
        } while (offset !== null);
        assert.equal(restored, raw);
      },
    );
    // Saved only in the temporary fixture, never in the analyzed application or prototype.
    writeFileSync(
      resolve(root, "asserted-query-result.json"),
      JSON.stringify({ result, oracle }, null, 2),
    );
    t.diagnostic(
      JSON.stringify({
        root,
        sites: result.summary.sites,
        oracle,
        candidates: result.sites.map((r) => ({
          owner: r.site.owner,
          category: r.site.category,
          candidate: r.candidate.status,
        })),
      }),
    );
  },
);
