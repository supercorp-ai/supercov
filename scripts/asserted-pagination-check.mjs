/** Read-only walk of a real project's public pages; writes only the requested new report. */
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { writeFileSync } from "node:fs";
import { resolve } from "node:path";

const [project, runId, output] = process.argv.slice(2);
assert.ok(
  project && runId && output,
  "Usage: node scripts/asserted-pagination-check.mjs <project> <run-id> <new-report.json>",
);
const repository = resolve(import.meta.dirname, "..");
let analysisId,
  requests = 0,
  maxBytes = 0;
const started = performance.now();
function read(args) {
  const result = spawnSync(
    resolve(repository, "target/debug/supercov"),
    [
      "runs",
      runId,
      "asserted",
      "--json",
      ...args,
      ...(analysisId ? ["--analysis", analysisId] : []),
    ],
    {
      cwd: project,
      env: { ...process.env, SUPERCOV_PACKAGE_ROOT: repository },
      encoding: "utf8",
      timeout: 60_000,
      maxBuffer: 1_048_576,
    },
  );
  assert.equal(result.error, undefined, result.error?.message);
  assert.equal(result.status, 0, result.stderr || result.stdout);
  const bytes = Buffer.byteLength(result.stdout);
  assert.ok(bytes <= 65_536);
  maxBytes = Math.max(bytes, maxBytes);
  requests++;
  const envelope = JSON.parse(result.stdout);
  assert.equal(envelope.ok, true);
  const data = envelope.data;
  analysisId ??= data.analysisId;
  assert.equal(data.analysisId, analysisId);
  assert.equal(data.reportSchema, 2);
  return data;
}
const first = read(["--limit", "1"]);
const walked = {};
for (const collection of [
  "sites",
  "tests",
  "attempts",
  "executionLinks",
  "pragmas",
]) {
  let offset = 0,
    pages = 0;
  const pointers = [];
  do {
    const page = read([
      ...(collection === "sites" ? [] : ["--evidence", `/${collection}`]),
      "--offset",
      String(offset),
      "--limit",
      "1000",
    ]);
    const rows = page[collection === "sites" ? "sites" : "items"];
    assert.equal(page.pagination.total, first.evidence[collection].count);
    assert.equal(rows.length, page.pagination.returned);
    assert.equal(page.pagination.hasMore, page.pagination.nextOffset !== null);
    for (const row of rows) pointers.push(row.evidence?.pointer ?? row.pointer);
    pages++;
    const next = page.pagination.nextOffset;
    if (next !== null) assert.ok(next > offset);
    offset = next;
  } while (offset !== null);
  assert.deepEqual(
    pointers,
    Array.from(
      { length: first.evidence[collection].count },
      (_, i) => `/${collection}/${i}`,
    ),
  );
  walked[collection] = {
    count: pointers.length,
    pages,
    noOmissionsOrDuplicates: true,
  };
}
const report = {
  runId,
  analysisId,
  reportSchema: 2,
  requests,
  maxBytes,
  elapsedMs: performance.now() - started,
  walked,
  summary: first.summary,
};
writeFileSync(resolve(output), JSON.stringify(report, null, 2) + "\n", {
  flag: "wx",
});
console.log(JSON.stringify(report, null, 2));
