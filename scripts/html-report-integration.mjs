#!/usr/bin/env node

import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import {
  mkdirSync,
  mkdtempSync,
  readFileSync,
  readdirSync,
  rmSync,
  statSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { dirname, resolve } from "node:path";
import { gunzipSync } from "node:zlib";
import { fileURLToPath } from "node:url";

const repository = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const binary = resolve(repository, "target/debug/supercov");
const project = mkdtempSync(resolve(tmpdir(), "supercov-html-report-"));

try {
  mkdirSync(resolve(project, "src"));
  writeFileSync(
    resolve(project, "package.json"),
    JSON.stringify({ name: "portable-report-fixture", type: "module" }),
  );
  writeFileSync(
    resolve(project, "src/access.js"),
    [
      'const hostile = "</script><script>alert(1)</script>";',
      "export function access(member, owner) {",
      '  if (member && owner) return `owner:${hostile.length}`;',
      '  return "visitor";',
      "}",
      "",
    ].join("\n"),
  );
  writeFileSync(
    resolve(project, "access.test.js"),
    [
      'import assert from "node:assert/strict";',
      'import { access } from "./src/access.js";',
      'assert.match(access(true, true), /^owner:/);',
      "",
    ].join("\n"),
  );

  execFileSync(binary, ["--", "node", "--test"], {
    cwd: project,
    encoding: "utf8",
    stdio: "pipe",
  });
  execFileSync(
    binary,
    ["report", "--runs", "1", "--no-open", "--output", "report.html"],
    { cwd: project, encoding: "utf8", stdio: "pipe" },
  );

  // A saved assessment, written by hand so this stays offline and free. Only the
  // source fingerprint decides whether it pairs with the run, so it is copied
  // from the run itself; a made-up one must stay unpaired.
  const runDirectory = resolve(project, ".supercov/runs");
  const runId = readdirSync(runDirectory)[0];
  const runRecord = JSON.parse(
    readFileSync(resolve(runDirectory, runId, "run.json"), "utf8"),
  );
  const sourceFingerprint = runRecord.integrity.fingerprint.source;
  const snapshot = resolve(project, ".supercov/quality/snapshots/q_00000000000000aa");
  mkdirSync(snapshot, { recursive: true });
  writeFileSync(
    resolve(snapshot, "manifest.json"),
    JSON.stringify({
      schema_version: 4,
      id: "q_00000000000000aa",
      created_at: "2026-09-18T00:00:00.000Z",
      instrument: "catalog",
      catalog_version: "properties-v1",
      model: "jev-1.13.0",
      source_fingerprint: sourceFingerprint,
      source_files: 1,
      health: 4.84,
      counts: { files: 1, scored: 1 },
      policy: { cutoffs: "none", fail_on_finding: false },
    }),
  );
  writeFileSync(
    resolve(snapshot, "files.json"),
    JSON.stringify({
      files: [
        {
          path: "src/access.js",
          bytes: 120,
          sha256: "a".repeat(64),
          status: "completed",
          health: 3.5,
          present: [{ check: "long_method", value: 0.95 }],
          checks: { long_method: 0.95 },
        },
      ],
      directories: [],
    }),
  );
  execFileSync(
    binary,
    ["report", "--runs", "1", "--no-open", "--output", "paired.html"],
    { cwd: project, encoding: "utf8", stdio: "pipe" },
  );
  const pairedHtml = readFileSync(resolve(project, "paired.html"), "utf8");
  const pairedScripts = [
    ...pairedHtml.matchAll(/<script(?: [^>]*)?>([\s\S]*?)<\/script>/g),
  ];
  const paired = JSON.parse(
    gunzipSync(Buffer.from(pairedScripts[0][1].trim(), "base64")).toString("utf8"),
  );
  assert.equal(paired.qualities.length, 1);
  assert.equal(paired.timeline.length, 1);
  assert.equal(paired.timeline[0].kind, "paired");
  assert.equal(paired.timeline[0].qualityId, "q_00000000000000aa");
  assert.equal(paired.qualities[0].files[0].path, "src/access.js");

  // A snapshot taken against different source must not be folded into the run.
  writeFileSync(
    resolve(snapshot, "manifest.json"),
    readFileSync(resolve(snapshot, "manifest.json"), "utf8").replace(
      sourceFingerprint,
      "b".repeat(64),
    ),
  );
  execFileSync(
    binary,
    ["report", "--runs", "1", "--no-open", "--output", "unpaired.html"],
    { cwd: project, encoding: "utf8", stdio: "pipe" },
  );
  const unpairedHtml = readFileSync(resolve(project, "unpaired.html"), "utf8");
  const unpairedScripts = [
    ...unpairedHtml.matchAll(/<script(?: [^>]*)?>([\s\S]*?)<\/script>/g),
  ];
  const unpaired = JSON.parse(
    gunzipSync(Buffer.from(unpairedScripts[0][1].trim(), "base64")).toString("utf8"),
  );
  assert.equal(unpaired.timeline.length, 2);
  assert.deepEqual(
    unpaired.timeline.map((item) => item.kind).sort(),
    ["quality", "run"],
  );

  const reportPath = resolve(project, "report.html");
  const html = readFileSync(reportPath, "utf8");
  assert(statSync(reportPath).size < 24 * 1024 * 1024);
  assert.match(html, /Content-Security-Policy/);
  assert.match(html, /DecompressionStream\("gzip"\)/);
  assert.doesNotMatch(html, /SUPERCOV_REPORT_PAYLOAD/);
  assert.doesNotMatch(html, /<script>alert\(1\)<\/script>/);
  assert.doesNotMatch(html, /Private · offline/);
  assert.doesNotMatch(html, /Historical .* snapshot/i);
  assert.doesNotMatch(html, /This saved run passed/i);
  assert.doesNotMatch(html, /needed coverage in this run/i);
  assert.doesNotMatch(html, />Unchanged</i);
  assert.doesNotMatch(html, /Coverage history/);
  assert.match(html, /review: "Overview"/);
  assert.doesNotMatch(html, /Included code/);
  assert.match(html, /const tabs = \["review", "explore", "tests"\]/);
  assert.match(html, /function testsCoveringFile/);
  assert.match(html, /testFileFilter/);
  assert.match(html, /testKind/);
  assert.match(html, /testSuite/);
  assert.match(html, /End-to-end/);
  assert.match(html, /function groupedTestFiles/);
  assert.match(html, /function groupedTestsInFile/);
  assert.match(html, /Find a test file or test/);
  assert.match(html, /Covered source files/);
  assert.match(html, /function closedGapsByFile/);
  assert.match(html, /file-mark\.improved/);
  assert.match(html, /Covered executable line/);
  assert.doesNotMatch(html, /Affected code/);
  assert.doesNotMatch(html, /Previous run/);

  // The judgments a report can carry, and the way it offers the ones it lacks.
  assert.match(html, /function renderQuality/);
  assert.match(html, /function renderAssertions/);
  assert.match(html, /function renderMissing/);
  assert.match(html, /function renderQualityOnly/);
  assert.match(html, /function promptFor/);
  // A file:// report has no clipboard API, so the selection path must survive.
  assert.match(html, /execCommand\("copy"\)/);
  assert.match(html, /function copyBySelection/);
  // Banding must match the command line, including rounding before comparing.
  assert.match(html, /rounded >= 8 \? "good" : rounded >= 5 \? "fair" : "weak"/);
  // Quality is a ranking, not a target, and the report has to say so.
  assert.match(html, /not a target/);
  // No price is quoted; a report outlives the rates.
  assert.doesNotMatch(html, /\$\d/);

  const scripts = [...html.matchAll(/<script(?: [^>]*)?>([\s\S]*?)<\/script>/g)];
  assert.equal(scripts.length, 2);
  new Function(scripts[1][1]);
  const bundle = JSON.parse(
    gunzipSync(Buffer.from(scripts[0][1].trim(), "base64")).toString("utf8"),
  );
  assert.equal(bundle.schemaVersion, 2);
  assert.equal(bundle.project, "portable-report-fixture");
  assert.equal(bundle.runs.length, 1);
  // Nothing has been assessed here, so the run stands alone on the timeline
  // and the report says quality is missing rather than implying a zero.
  assert.equal(bundle.qualities.length, 0);
  assert.equal(bundle.timeline.length, 1);
  assert.equal(bundle.timeline[0].kind, "run");
  assert.equal(bundle.timeline[0].runId, bundle.runs[0].id);
  assert.equal(bundle.timeline[0].qualityId, null);
  assert.equal(bundle.selectedId, bundle.runs[0].id);
  // A run records what source it measured, which is the only thing that pairs
  // it with an assessment.
  assert.match(bundle.runs[0].sourceFingerprint, /^[0-9a-f]{64}$/);
  // Every run writes a map, so the section is carried. Nothing explains a flow
  // yet, so it reports that status and no percentage rather than a zero.
  const asserted = bundle.runs[0].assertions;
  assert.equal(asserted.available, true);
  assert.equal(asserted.summary.status, "notAssessed");
  assert.equal(asserted.summary.statements.percentage, null);
  assert.match(asserted.basis, /agent-assessed/);
  // The query path carries the map's absolute path; a shareable report must not.
  assert.equal(asserted.map, undefined);
  // A report is attached to pull requests; it must not carry a home directory.
  assert.doesNotMatch(JSON.stringify(bundle), /\/(Users|home)\/[a-z]/i);
  assert.equal(bundle.runs[0].sourceMode, "exact");
  assert.match(bundle.runs[0].sources["src/access.js"].contents, /hostile/);
  assert(bundle.runs[0].fileDetails["src/access.js"].gapLines.length > 0);

  console.log(
    `[html-report] generated, decoded, and syntax-checked ${statSync(reportPath).size} byte portable report`,
  );
} finally {
  rmSync(project, { recursive: true, force: true });
}
