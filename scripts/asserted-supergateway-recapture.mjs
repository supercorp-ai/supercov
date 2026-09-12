/** Recalibrate the fixed Supergateway cohort with one fresh ordinary capture.
 * No mutations, test edits, analyzer ablations, or runtime changes. */
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import {
  cpSync,
  existsSync,
  mkdtempSync,
  readFileSync,
  readdirSync,
  writeFileSync,
} from "node:fs";
import { createRequire } from "node:module";
import { tmpdir } from "node:os";
import { relative, resolve } from "node:path";
import { spawnSync } from "node:child_process";
import { checkedIdentity } from "../analyzers/typescript/bin/identity.mjs";
import {
  metrics,
  availabilityPrediction,
} from "./asserted-stryker-metrics.mjs";

const [projectArg, runId, outputArg, check] = process.argv.slice(2);
assert.ok(
  projectArg && /^run_[a-f0-9]+$/.test(runId) && outputArg,
  "Usage: node scripts/asserted-supergateway-recapture.mjs <prototype-project> <run-id> <new-results.json> [--check]",
);
assert.ok(check === undefined || check === "--check");
assert.ok(process.argv.length <= 6);
const root = resolve(projectArg),
  output = resolve(outputArg),
  repository = resolve(import.meta.dirname, "..");
assert.ok(!existsSync(output), "Refusing to overwrite a prior result");
const read = (path) => JSON.parse(readFileSync(path, "utf8"));
const hash = (bytes) => createHash("sha256").update(bytes).digest("hex");
const execute = (cmd, args, options = {}) =>
  spawnSync(cmd, args, {
    cwd: root,
    encoding: "utf8",
    timeout: 300000,
    maxBuffer: 64 * 1024 * 1024,
    ...options,
  });
const ok = (result) => {
  assert.equal(result.error, undefined, result.error?.message);
  assert.equal(result.status, 0, result.stderr || result.stdout);
  return result.stdout;
};
const tools = resolve(root, "tools/asserted-coverage");
const frozen = read(
  resolve(repository, "docs/asserted-stryker-replay-2026-09-09.json"),
);
const baseline = frozen.samples.supergateway;
const fixturePath = resolve(
  tools,
  "fixtures/stryker-src-lib-step3-2026-09-09.json",
);
const inventoryPath = resolve(tools, "out/inventory.json");
assert.equal(hash(readFileSync(fixturePath)), baseline.fixtureSha256);
assert.equal(hash(readFileSync(inventoryPath)), baseline.inventorySha256);
function sourceHash() {
  const files = [];
  function walk(path) {
    for (const e of readdirSync(path, { withFileTypes: true }).sort((a, b) =>
      a.name.localeCompare(b.name),
    )) {
      const next = resolve(path, e.name);
      if (e.isDirectory()) walk(next);
      else if (e.isFile()) files.push(next);
    }
  }
  for (const dir of ["src", "tests"]) walk(resolve(root, dir));
  const h = createHash("sha256");
  for (const file of files)
    h.update(relative(root, file))
      .update("\0")
      .update(readFileSync(file))
      .update("\0");
  return { files: files.length, sha256: h.digest("hex") };
}
assert.deepEqual(
  sourceHash(),
  baseline.sourceAndTestFiles,
  "source/test snapshot differs from frozen replay",
);
const before = checkedIdentity();
const { analyze } = await import("../analyzers/typescript/dist/analyze.js");
const ts = createRequire(
  resolve(repository, "analyzers/typescript/package.json"),
)("typescript");
const prototypeSource = readFileSync(
  resolve(tools, "resolve-oracles.ts"),
  "utf8",
);
assert.equal(hash(prototypeSource), frozen.provenance.prototypeSourceSha256);
const ast = ts.createSourceFile(
  "prototype.ts",
  prototypeSource,
  ts.ScriptTarget.Latest,
  true,
);
const fn = (name) => {
  const matches = ast.statements.filter(
    (n) => ts.isFunctionDeclaration(n) && n.name?.text === name,
  );
  assert.equal(matches.length, 1);
  return matches[0].getText(ast);
};
const variable = (name) => {
  const matches = ast.statements
    .filter(ts.isVariableStatement)
    .flatMap((n) => [...n.declarationList.declarations])
    .filter((n) => n.name.getText(ast) === name);
  assert.equal(matches.length, 1);
  return `const ${matches[0].getText(ast)};`;
};
const predictorSource = `export function predictor(sites, rows) {const resolutions=new Map(rows.map(r=>[r.site,r]));
  ${variable("RANK")} ${variable("REMOVAL")} ${fn("predict")} ${fn("predictMutant")} return predictMutant;}`;
const compiled = ts.transpileModule(predictorSource, {
  compilerOptions: {
    module: ts.ModuleKind.ESNext,
    target: ts.ScriptTarget.ES2022,
  },
}).outputText;
const { predictor } = await import(
  "data:text/javascript;base64," + Buffer.from(compiled).toString("base64")
);
const work = mkdtempSync(resolve(tmpdir(), "supercov-supergateway-recapture-"));
const conversion = ok(
  execute(process.execPath, [
    resolve(tools, "supercov-to-cov.mjs"),
    root,
    runId,
    work,
  ]),
);
cpSync(inventoryPath, resolve(work, "inventory.json"));
const inventory = read(inventoryPath),
  fixture = read(fixturePath),
  index = read(resolve(work, "cov/index.json"));
const phases = index.flatMap((t) =>
  read(resolve(work, `cov/${t.id}.phases.json`)).map((p) => ({
    ...p,
    test: t.id,
  })),
);
const phaseCounts = {};
for (const p of phases)
  phaseCounts[p.status ?? "incomplete"] =
    (phaseCounts[p.status ?? "incomplete"] ?? 0) + 1;
const start = performance.now();
// Supercov positions are already original-source positions. "vitest" is the
// legacy option that disables V8/ts-node generated-line remapping for any runner.
const analyzed = analyze({
  projectRoot: root,
  inputDirectory: work,
  coverageRunner: "vitest",
});
const joined = JSON.parse(
  ok(
    execute(resolve(repository, "target/debug/examples/asserted_join"), [], {
      input: JSON.stringify(analyzed.facts),
    }),
  ),
);
const queryMs = performance.now() - start;
const predict = predictor(inventory.sites, joined.resolutions),
  byId = new Map(joined.resolutions.map((r) => [r.site, r]));
const previous = new Map(baseline.rows.map((r) => [r.id, r]));
const rows = [];
for (const [file, data] of Object.entries(fixture.files)) {
  const source = readFileSync(resolve(root, file), "utf8"),
    lines = [0];
  for (let i = 0; i < source.length; i++)
    if (source[i] === "\n") lines.push(i + 1);
  const offset = (p) => lines[p.line - 1] + p.column - 1;
  for (const m of data.mutants) {
    const id = `${file}#${m.id}`;
    if (!previous.has(id)) continue;
    const start = offset(m.location.start),
      end = offset(m.location.end);
    const cands = inventory.sites
      .filter((s) => s.file === file && s.pos < end && start < s.endPos)
      .sort((a, b) => a.endPos - a.pos - (b.endPos - b.pos));
    const containing = cands.filter((s) => s.pos <= start && s.endPos >= end);
    const site =
      containing.find((s) => s.classification !== "review") ??
      cands.find((s) => s.classification !== "review") ??
      containing[0] ??
      cands[0];
    assert.equal(
      site?.id ?? null,
      previous.get(id).site,
      "mutant-to-site mapping changed",
    );
    assert.equal(m.status, previous.get(id).status);
    const removal =
      ["BlockStatement", "ArrowFunction", "MethodExpression"].includes(
        m.mutatorName,
      ) ||
      (m.mutatorName === "CallExpression" && m.replacement.trim() === ";");
    const inside = removal
      ? inventory.sites.filter(
          (s) => s.file === file && s.pos >= start && s.endPos <= end,
        )
      : [];
    const relevant = [site, ...inside]
      .filter(Boolean)
      .map((s) => byId.get(s.id));
    const binary = predict(
      file,
      start,
      end,
      site,
      m.mutatorName,
      m.replacement,
    );
    rows.push({
      id,
      file,
      line: m.location.start.line,
      status: m.status,
      actualKilled: m.status !== "Survived",
      site: site?.id,
      mutator: m.mutatorName,
      replacement: m.replacement,
      binary: binary ?? null,
      availability: availabilityPrediction(binary, relevant) ?? null,
      candidate: byId.get(site?.id),
      previous: previous.get(id).predictions,
    });
  }
}
assert.deepEqual(
  rows.map((r) => r.id),
  baseline.rows.map((r) => r.id),
);
// Verify every positive source observation has an exact successful witness in
// its owning captured test; never manufacture a phase to improve the score.
let observations = 0;
for (const t of analyzed.facts.tests)
  for (const ob of t.observations) {
    const matches = phases.filter(
      (p) =>
        p.test === t.id &&
        p.source === ob.assertionSource &&
        p.op.split(".").pop() === ob.assertionMethod,
    );
    assert.ok(
      matches.length && matches.every((p) => p.status === "passed"),
      `missing witness ${t.id} ${ob.assertionSource}`,
    );
    observations++;
  }
writeFileSync(resolve(work, "facts.json"), JSON.stringify(analyzed.facts));
writeFileSync(resolve(work, "resolutions.json"), JSON.stringify(joined));
const publicQuery = execute(resolve(repository, "target/debug/supercov"), [
  "runs",
  runId,
  "asserted",
  "--limit",
  "1",
  "--json",
]);
const publicResult = publicQuery.stdout ? JSON.parse(publicQuery.stdout) : null;
assert.equal(publicQuery.error, undefined, publicQuery.error?.message);
assert.equal(publicQuery.status, 0, publicQuery.stderr || publicQuery.stdout);
assert.equal(publicResult.ok, true);
assert.equal(publicResult.data.reportSchema, 2);
assert.equal(publicResult.data.pagination.returned, 1);
assert.ok(Buffer.byteLength(publicQuery.stdout) <= 65_536);
const publicText = ok(
  execute(resolve(repository, "target/debug/supercov"), [
    "runs",
    runId,
    "asserted",
    "--file",
    "src/lib/headers.ts",
    "--limit",
    "3",
  ]),
);
const coverage = JSON.parse(
  ok(
    execute(resolve(repository, "target/debug/supercov"), [
      "runs",
      runId,
      "--json",
    ]),
  ),
).data;
const runDirectory = resolve(root, ".supercov/runs", runId);
const report = {
  schema: 1,
  runId,
  project: root,
  work,
  analyzer: before,
  sourceAndTestFiles: sourceHash(),
  archiveSha256: hash(readFileSync(resolve(runDirectory, "evidence.raw.gz"))),
  run: read(resolve(runDirectory, "run.json")),
  fixtureSha256: hash(readFileSync(fixturePath)),
  inventorySha256: hash(readFileSync(inventoryPath)),
  conversion,
  sites: inventory.sites.length,
  tests: analyzed.facts.tests.length,
  records: index.length,
  phases: phases.length,
  phaseCounts,
  observations,
  queryMs,
  summary: joined.summary,
  diagnostics: analyzed.diagnostics,
  binary: metrics(rows, "binary"),
  availability: metrics(rows, "availability"),
  binaryExcludingTimeouts: metrics(
    rows.filter((r) => r.status !== "Timeout"),
    "binary",
  ),
  byFile: Object.fromEntries(
    [...new Set(rows.map((r) => r.file))].map((file) => [
      file,
      metrics(
        rows.filter((r) => r.file === file),
        "binary",
      ),
    ]),
  ),
  publicQuery: {
    status: publicQuery.status,
    response: publicResult,
    stderr: publicQuery.stderr,
    headersText: publicText,
  },
  coverage,
  caveats: [
    "Historical frozen Stryker oracle, not new mutant executions or global assertion accuracy.",
    "Legacy conversion retains embedded server records but does not join separately archived child/server streams; public archive adapter does.",
    "Full suite is captured, but this fixed 100-mutant oracle covers src/lib only. Unknowns remain in its denominator.",
  ],
  rows,
};
assert.deepEqual(sourceHash(), baseline.sourceAndTestFiles);
assert.deepEqual(
  checkedIdentity(),
  before,
  "analyzer changed during calibration",
);
report.capabilityGate = {
  // Explicitly accepted by the user after the fresh full-suite calibration.
  // Keep the cohort, unknown accounting, and false-kill guard unchanged.
  minimumCorrect: 84,
  total: 100,
  maximumFalseKills: 4,
  minimumTrueKills: 78,
  correct: report.availability.correct,
  falseKills: report.availability.fp,
  trueKills: report.availability.tp,
  passed:
    report.availability.total === 100 &&
    report.availability.correct >= 84 &&
    report.availability.fp <= 4 &&
    report.availability.tp >= 78,
  meaning:
    "Historical cohort regression target; unknowns count as unresolved, not correct. Not a correctness proof.",
};
writeFileSync(output, JSON.stringify(report, null, 2), { flag: "wx" });
console.log(
  JSON.stringify(
    {
      output,
      work,
      records: report.records,
      phases: report.phaseCounts,
      observations,
      summary: joined.summary,
      binary: report.binary,
      availability: report.availability,
      capabilityGate: report.capabilityGate,
      publicStatus: publicQuery.status,
    },
    null,
    2,
  ),
);
if (check && !report.capabilityGate.passed) process.exitCode = 1;
