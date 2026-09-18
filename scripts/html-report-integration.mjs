#!/usr/bin/env node

import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import {
  mkdirSync,
  mkdtempSync,
  readFileSync,
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
