import { test } from "node:test";
import assert from "node:assert/strict";
import {
  cpSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { resolve, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import { spawnSync } from "node:child_process";
import { analyze } from "../dist/analyze.js";
import { isDeepStrictEqual } from "node:util";

const prototype = process.env.SUPERCOV_ASSERTED_PROTOTYPE;
const secondProject = process.env.SUPERCOV_ASSERTED_SECOND_PROJECT;
const repository = resolve(dirname(fileURLToPath(import.meta.url)), "../../..");
const read = (path) => JSON.parse(readFileSync(path, "utf8"));
const run = (command, args, options) => {
  const result = spawnSync(command, args, {
    encoding: "utf8",
    maxBuffer: 16 * 1024 * 1024,
    timeout: 300_000,
    ...options,
  });
  assert.equal(result.error, undefined, result.error?.message);
  assert.equal(result.status, 0, `${result.stdout}\n${result.stderr}`);
  return result;
};

for (const name of ["supergateway", "essential-seo"]) {
  test(
    `stable inventory, reviewed observation delta, and reference join parity: ${name}`,
    {
      skip:
        !prototype || (name === "essential-seo" && !secondProject)
          ? "set SUPERCOV_ASSERTED_PROTOTYPE and SUPERCOV_ASSERTED_SECOND_PROJECT to run external parity"
          : false,
    },
    (t) => {
      const work = mkdtempSync(resolve(tmpdir(), "supercov-ts-parity-"));
      t.after(() => {
        if (!process.env.SUPERCOV_KEEP_ASSERTED_PARITY)
          rmSync(work, { recursive: true, force: true });
      });
      const tools = resolve(prototype, "tools/asserted-coverage");
      const seo = name === "essential-seo";
      const input = resolve(tools, seo ? "out-essential-seo-trial" : "out");
      cpSync(resolve(input, "cov"), resolve(work, "cov"), { recursive: true });
      cpSync(resolve(input, "inventory.json"), resolve(work, "inventory.json"));
      const projectRoot = seo ? resolve(secondProject) : resolve(prototype);
      const extra = seo
        ? {
            sourceDir: "app",
            testDir: "tests/unit",
            tsconfig: "tsconfig.json",
            coverageRunner: "vitest",
          }
        : {};
      const env = {
        ...process.env,
        OUT_DIR: work,
        STRYKER_REPORT: resolve(
          tools,
          "fixtures",
          seo
            ? "stryker-essential-seo-all-2026-09-09.json"
            : "stryker-src-lib-step3-2026-09-09.json",
        ),
      };
      // Parent-shell prototype options must never silently alter the reference.
      for (const key of [
        "SRC_DIR",
        "TEST_DIR",
        "TSCONFIG",
        "COVERAGE_RUNNER",
        "RUNTIME_OBS",
        "FACTS",
        "TRIAGE",
        "PROBES",
        "BASELINE",
        "DEBUG_SITE",
      ])
        delete env[key];
      if (seo)
        Object.assign(env, {
          SRC_DIR: "app",
          TEST_DIR: "tests/unit",
          TSCONFIG: "tsconfig.json",
          COVERAGE_RUNNER: "vitest",
        });
      const reference = run(
        process.execPath,
        [
          resolve(prototype, "node_modules/tsx/dist/cli.mjs"),
          resolve(tools, "resolve-oracles.ts"),
          projectRoot,
        ],
        { cwd: prototype, env },
      );
      const expected = read(resolve(work, "facts.json"));
      const started = performance.now();
      const actual = analyze({ projectRoot, inputDirectory: work, ...extra });
      const elapsed = performance.now() - started;
      // v2 deliberately removes unsafe inference rules. Exact old facts/verdict
      // parity would preserve the false positives these regressions fix. Never
      // improve the report by deleting sites or changing their coverage instead.
      const inventory = (facts) =>
        facts.sites.map((site) =>
          Object.fromEntries(
            [
              "id",
              "file",
              "line",
              "kind",
              "category",
              "classification",
              "owner",
              "method",
              "coveredBy",
            ].map((key) => [key, site[key]]),
          ),
        );
      assert.deepEqual(inventory(actual.facts), inventory(expected));
      assert.deepEqual(
        actual.facts.tests.map(({ id, file }) => ({ id, file })),
        expected.tests.map(({ id, file }) => ({ id, file })),
      );
      const referenceTests = new Map(
        expected.tests.map((test) => [test.id, test]),
      );
      const normalizeObservation = ({
        assertionSource,
        assertionMethod,
        ...observation
      }) => JSON.parse(JSON.stringify(observation));
      const delta = {
        previousObservations: 0,
        currentObservations: 0,
        removedObservations: 0,
        addedObservations: 0,
        changedTests: 0,
        changedSites: 0,
        suppressedObservations:
          actual.diagnostics.suppressedObservations.length,
      };
      for (const test of actual.facts.tests) {
        const before = referenceTests.get(test.id).observations;
        const after = test.observations.map(normalizeObservation);
        delta.previousObservations += before.length;
        delta.currentObservations += after.length;
        delta.removedObservations += before.filter(
          (ob) => !after.some((other) => isDeepStrictEqual(ob, other)),
        ).length;
        delta.addedObservations += after.filter(
          (ob) => !before.some((other) => isDeepStrictEqual(ob, other)),
        ).length;
        if (!isDeepStrictEqual(before, after)) delta.changedTests++;
        for (const observation of test.observations) {
          assert.ok(
            observation.assertionSource,
            "no execution-only fallback may supply an observation",
          );
          const phases = read(
            resolve(work, `cov/${test.id}.phases.json`),
          ).filter(
            (phase) =>
              phase.source === observation.assertionSource &&
              phase.op.split(".").pop() === observation.assertionMethod,
          );
          assert.ok(
            phases.length && phases.every((phase) => phase.status === "passed"),
            `missing exact successful witness: ${test.id} ${observation.assertionSource}`,
          );
        }
      }
      delta.changedSites = actual.facts.sites.filter(
        (site, index) =>
          !isDeepStrictEqual(
            JSON.parse(JSON.stringify(site)),
            expected.sites[index],
          ),
      ).length;
      // Reviewed v2 calibration, not a soundness oracle. Supergateway's legacy
      // evidence has no phase files, so none of its assertions has an exact
      // runtime witness. Essential SEO does; removed facts are not new gaps.
      assert.deepEqual(
        delta,
        seo
          ? {
              previousObservations: 1697,
              currentObservations: 1019,
              removedObservations: 678,
              addedObservations: 0,
              changedTests: 289,
              changedSites: 175,
              suppressedObservations: 223,
            }
          : {
              previousObservations: 852,
              currentObservations: 0,
              removedObservations: 852,
              addedObservations: 0,
              changedTests: 80,
              changedSites: 2,
              suppressedObservations: 155,
            },
      );
      assert.equal(actual.facts.sites.length, seo ? 1813 : 661);
      assert.equal(actual.facts.tests.length, seo ? 527 : 81);
      const actualPath = resolve(work, "extracted-facts.json");
      writeFileSync(actualPath, JSON.stringify(actual.facts));
      writeFileSync(
        resolve(work, "comparison.json"),
        JSON.stringify({ name, ...delta }, null, 2),
      );
      const engine = run(
        "cargo",
        [
          "test",
          "--offline",
          "-p",
          "supercov-engine",
          "--test",
          "asserted_coverage_parity",
          "--",
          "--nocapture",
        ],
        {
          cwd: repository,
          env: {
            ...process.env,
            // This remains the exact engine-port gate: reference facts in,
            // reference resolutions out. It is NOT current-analyzer parity.
            SUPERCOV_ASSERTED_FACTS: resolve(work, "facts.json"),
            SUPERCOV_ASSERTED_RESOLUTION: resolve(work, "resolution.json"),
            SUPERCOV_ASSERTED_CURRENT_FACTS: actualPath,
            SUPERCOV_ASSERTED_COMPARISON_OUTPUT: resolve(
              work,
              "witness-comparison.json",
            ),
          },
        },
      );
      const witnessComparison = read(resolve(work, "witness-comparison.json"));
      assert.equal(witnessComparison.unchangedCreditAndCoverage, true);
      assert.deepEqual(
        witnessComparison.before,
        seo
          ? {
              contractual: 936,
              evident: 518,
              partial: 56,
              presence: 30,
              unresolved: 332,
              gaps: 357,
              limits: 61,
            }
          : {
              contractual: 364,
              evident: 0,
              partial: 0,
              presence: 0,
              unresolved: 364,
              gaps: 298,
              limits: 66,
            },
      );
      assert.deepEqual(witnessComparison.after, {
        ...witnessComparison.before,
        gaps: seo ? 334 : 13,
        limits: seo ? 84 : 351,
      });
      t.diagnostic(
        JSON.stringify({
          sites: actual.facts.sites.length,
          tests: actual.facts.tests.length,
          elapsedMs: Math.round(elapsed),
          ...delta,
          ...(process.env.SUPERCOV_KEEP_ASSERTED_PARITY ? { work } : {}),
        }),
      );
      t.diagnostic(
        reference.stdout
          .split("\n")
          .find((line) => line.startsWith("Agreement ")),
      );
      t.diagnostic(engine.stderr);
    },
  );
}
