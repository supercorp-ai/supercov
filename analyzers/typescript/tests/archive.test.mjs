import test from "node:test";
import assert from "node:assert/strict";
import {
  mkdtempSync,
  cpSync,
  mkdirSync,
  readFileSync,
  writeFileSync,
  rmSync,
} from "node:fs";
import { resolve } from "node:path";
import { tmpdir } from "node:os";
import { spawnSync } from "node:child_process";
import ts from "typescript";
import { analyzeArchive, PROTOCOL } from "../dist/archive.js";
import { checkedIdentity } from "../bin/identity.mjs";

const projectRoot = resolve(import.meta.dirname, "fixtures/archive");
const empty = () => ({
  protocol: PROTOCOL,
  projectRoot,
  runId: "run_test",
  sourceFiles: ["src/core.mjs"],
  effects: [],
  manifest: { points: [], branches: [], decisions: [] },
  records: [],
  limitations: [],
});
test("archive facts require the exact schema, rules, ABI and capabilities", () => {
  // A previously compatible analyzer must not silently drop witness limits.
  for (const protocol of [
    {
      ...PROTOCOL,
      capabilities: PROTOCOL.capabilities.filter(
        (cap) => cap !== "primitive-decision-sensitivity-v1",
      ),
    },
    {
      ...PROTOCOL,
      capabilities: PROTOCOL.capabilities.filter(
        (cap) => cap !== "process-exit-consumer-v1",
      ),
    },
    {
      ...PROTOCOL,
      capabilities: PROTOCOL.capabilities.filter(
        (cap) => cap !== "process-exit-source-v1",
      ),
    },
    {
      ...PROTOCOL,
      capabilities: PROTOCOL.capabilities.map((cap) =>
        cap === "assertion-comparison-relations-v2"
          ? "assertion-comparison-relations-v1"
          : cap,
      ),
    },
    { ...PROTOCOL, rules: "source-linked-v2/archive-2" },
    { ...PROTOCOL, capabilities: ["requiresTotal-v1"] },
    {
      ...PROTOCOL,
      capabilities: PROTOCOL.capabilities.filter(
        (cap) => cap !== "mock-observation-projections-v1",
      ),
    },
    {
      ...PROTOCOL,
      capabilities: PROTOCOL.capabilities.filter(
        (cap) => cap !== "assertion-comparison-relations-v2",
      ),
    },
    {
      ...PROTOCOL,
      capabilities: PROTOCOL.capabilities.filter(
        (cap) => cap !== "mock-count-lifetimes-v1",
      ),
    },
    {
      ...PROTOCOL,
      capabilities: PROTOCOL.capabilities.filter(
        (cap) => cap !== "mock-count-factories-v1",
      ),
    },
    {
      ...PROTOCOL,
      capabilities: PROTOCOL.capabilities.filter(
        (cap) => cap !== "mock-count-rows-v1",
      ),
    },
    {
      ...PROTOCOL,
      capabilities: PROTOCOL.capabilities.filter(
        (cap) => cap !== "mock-count-array-projections-v1",
      ),
    },
  ])
    assert.throws(
      () => analyzeArchive({ ...empty(), protocol }, ts),
      /Unsupported assertion analyzer/,
    );
  for (const field of ["abi", "factsSchema", "rules", "capabilities"]) {
    const input = empty();
    input.protocol = {
      ...PROTOCOL,
      [field]: field === "capabilities" ? [] : "future-version",
    };
    assert.throws(
      () => analyzeArchive(input, ts),
      /Unsupported assertion analyzer/,
    );
  }
});
test("empty or setup-only archives cannot silently look like successfully analyzed suites", () => {
  assert.throws(() => analyzeArchive(empty(), ts), /No uniquely attributed/);
});
test("passed attempts without source provenance cannot masquerade as execution gaps", () => {
  const input = empty();
  const record = {
    test: "missing source",
    testId: "test",
    role: "test",
    status: "passed",
    scope: {
      version: 1,
      runId: input.runId,
      workerId: "w",
      testId: "test",
      testKey: "k",
      retry: 0,
      attemptId: "a",
    },
    runtime: [],
    browser: [],
    server: [],
    phases: [],
  };
  input.records.push(record);
  assert.throws(() => analyzeArchive(input, ts), /passed test.*source file/i);
  // A second usable record must not hide the missing attempt either. Ordinary
  // coverage remains available; assertion gap claims require complete provenance.
  input.records.push({
    ...record,
    testId: "other",
    test: "exact return",
    testFile: "tests/core.test.mjs",
    scope: {
      ...record.scope,
      testId: "other",
      testKey: "other",
      attemptId: "other",
    },
  });
  assert.throws(() => analyzeArchive(input, ts), /passed test.*source file/i);
});
test("server statement/phase records require the full matching attempt scope", () => {
  const input = empty();
  const scope = {
    version: 1,
    runId: input.runId,
    workerId: "worker",
    testId: "test",
    testKey: "key",
    retry: 0,
    attemptId: "attempt",
  };
  const point = {
    id: "entry",
    file: "src/core.mjs",
    line: 1,
    column: 8,
    source: "function exact()",
    kind: "function",
  };
  input.manifest.points.push(point);
  const event = {
    type: "hit",
    id: "entry",
    phaseId: "assertion",
    statementId: "tests/core.test.mjs:1:1",
    scope,
  };
  const record = {
    test: "exact return",
    testId: "test",
    testFile: "tests/core.test.mjs",
    role: "test",
    status: "passed",
    retry: 0,
    scope,
    runtime: [],
    browser: [],
    server: [event],
    phases: [
      {
        id: "assertion",
        kind: "assertion",
        source: "tests/core.test.mjs:1:1",
        operation: "node:assert/strict.equal",
        status: "passed",
      },
    ],
  };
  input.records.push(record);
  assert.equal(analyzeArchive(input, ts).executionLinks.length, 1);
  for (const key of Object.keys(scope)) {
    record.server = [
      {
        ...event,
        scope: {
          ...scope,
          [key]: typeof scope[key] === "number" ? 9 : "foreign",
        },
      },
    ];
    assert.equal(analyzeArchive(input, ts).executionLinks.length, 0, key);
  }
  record.server = [{ ...event, scope: undefined }];
  assert.equal(analyzeArchive(input, ts).executionLinks.length, 0);
});
test("decision positions must match actual source, not merely a similarly named function", () => {
  const input = empty();
  input.manifest.decisions.push({
    id: "bad",
    file: "src/core.mjs",
    line: 1,
    column: 1,
    source: "not the recorded source",
    conditions: ["x"],
  });
  assert.throws(() => analyzeArchive(input, ts), /no longer matches source/);
});
test("stale source and modified generated analyzers fail before producing facts", (t) => {
  assert.match(checkedIdentity().sourceSha256, /^[a-f0-9]{64}$/);
  const root = mkdtempSync(resolve(tmpdir(), "supercov-analyzer-identity-"));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  const packageRoot = resolve(import.meta.dirname, "..");
  for (const directory of ["bin", "dist", "src"])
    cpSync(resolve(packageRoot, directory), resolve(root, directory), {
      recursive: true,
    });
  for (const file of ["package.json", "package-lock.json", "tsconfig.json"])
    cpSync(resolve(packageRoot, file), resolve(root, file));
  const run = () =>
    spawnSync(process.execPath, [resolve(root, "bin/query.mjs")], {
      input: "{}",
      encoding: "utf8",
    });
  for (const file of [
    "src/analyze.ts",
    "dist/analyze.js",
    "src/mock-counts.ts",
    "dist/mock-counts.js",
  ]) {
    const path = resolve(root, file),
      before = readFileSync(path, "utf8");
    writeFileSync(path, before + "\n// modified\n");
    const result = run();
    assert.equal(result.status, 1);
    assert.match(result.stderr, /build is stale or modified/);
    assert.equal(result.stdout, "");
    writeFileSync(path, before);
  }
});
