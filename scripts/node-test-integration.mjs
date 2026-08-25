import { rmSync } from "node:fs";
import { resolve } from "node:path";
import {
  coverageQuery,
  latestRun,
  requireSupercov,
} from "./coverage-test-helpers.mjs";

const root = resolve("tests/fixtures/generic-node");
rmSync(resolve(root, ".supercov"), { recursive: true, force: true });
requireSupercov(root, ["--", "npm", "test"], { stdio: "inherit" });
const runId = latestRun(root);
const summary = coverageQuery(root, runId).data;
if (summary.tests !== 4 || summary.coverageByRunner?.[0]?.runner !== "node:test")
  throw new Error(`expected four attributed node:test tests, received ${JSON.stringify(summary)}`);
if (summary.coverage.conditionCoveragePct !== 100)
  throw new Error(`expected 100% MC/DC, received ${summary.coverage.conditionCoveragePct}%`);
if (summary.confidence.lines.asserted === 0)
  throw new Error("expected node:assert argument execution to be assertion-attributed");
if (summary.confidence.assertionCoveredMcdcConditions !== summary.coverage.conditions)
  throw new Error("expected every node:test MC/DC witness to be assertion-linked");
console.log(`[node:test] run ${runId}: four exact test scopes, 100% assertion-linked MC/DC`);
