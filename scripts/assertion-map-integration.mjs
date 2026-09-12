import assert from "node:assert/strict";
import { mkdtempSync, mkdirSync, readFileSync, writeFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { requireSupercov, executeSupercov, latestRun, coverageQuery } from "./coverage-test-helpers.mjs";

const root = mkdtempSync(join(tmpdir(), "supercov-assertion-maps-"));
const read = path => JSON.parse(readFileSync(path, "utf8"));
const write = (path, data) => writeFileSync(path, JSON.stringify(data, null, 2) + "\n");
function query(run, ...args) { return coverageQuery(root, run, "assertions", ...args).data; }
function run() { requireSupercov(root, ["--", "node", "--test"]); return latestRun(root); }
try {
  mkdirSync(join(root, "src")); mkdirSync(join(root, "tests"));
  write(join(root, "package.json"), { name: "map-pilot", private: true, type: "module" });
  writeFileSync(join(root, "src/core.js"), "export function value() {\n  return 1;\n}\n");
  writeFileSync(join(root, "tests/core.test.js"), "import assert from 'node:assert/strict';\nimport test from 'node:test';\nimport { value } from '../src/core.js';\ntest('value', () => {\n  assert.equal(value(), 1);\n});\n");
  const first = run();
  assert.equal(coverageQuery(root, first).data.confidence.lines.asserted, 0);
  assert.equal(coverageQuery(root, first).data.assertionCoverage, undefined);
  const initialized = query(first, "init");
  const map = read(initialized.map);
  assert.equal(map.assertions.length, 1);
  const a = map.assertions[0];
  assert.equal(a.at.line, 5);
  a.analysis = "mapped";
  a.observes = ["value returns one"];
  a.flows = [{ id: "return-value", explanation: "The returned integer is compared to one by this assertion.", nodes: [
    { id: "return", at: { file: "src/core.js", line: 2, column: 3, text: "return 1;" } }
  ], edges: [], countsAsAsserted: ["return"], watch: [{kind:"file", file:"src/core.js"}, {kind:"file", file:"tests/core.test.js"}] }];
  write(initialized.map, map);
  assert.equal(query(first).summary.lines.asserted, 0, "editing a map is not review acknowledgement");
  assert.match(requireSupercov(root, ["runs", first]).stdout, /Assertions 0\.00% \(0\/1\)/);
  query(first, "review", "--all");
  let report = query(first);
  assert.equal(report.summary.lines.asserted, 1, JSON.stringify(report));
  assert.equal(report.summary.statements.asserted, 1);
  assert.equal(report.summary.dirtyFlows, 0);
  assert.equal(query(first, "--view", "creditedLines").items[0].assertions[0], a.id);
  assert.equal(coverageQuery(root, first).data.assertionCoverage.summary.lines.asserted, 1);
  const regular = coverageQuery(root, first).data.assertionCoverage;
  assert.equal(regular.summary.statements.percentage, 100);
  assert.equal(regular.revision, report.revision);
  assert.match(requireSupercov(root, ["runs", first]).stdout, /Assertions 100\.00% \(1\/1\)/);
  // The regular report must refresh after map edits without rerunning tests.
  a.flows[0].explanation += " Reviewed again.";
  write(initialized.map, map);
  const edited = coverageQuery(root, first).data.assertionCoverage;
  assert.notEqual(edited.revision, regular.revision);
  assert.equal(edited.summary.statements.percentage, 0);
  assert.match(requireSupercov(root, ["runs", first]).stdout, /1 dirty flow\(s\)/);
  writeFileSync(initialized.map, "{broken JSON");
  assert.equal(coverageQuery(root, first).data.assertionCoverage.available, false);
  assert.match(requireSupercov(root, ["runs", first]).stdout, /Assertions unavailable: assertions.json/);
  write(initialized.map, map);
  query(first, "review", "--all");
  assert.equal(coverageQuery(root, first).data.assertionCoverage.summary.statements.percentage, 100);
  const frozen = query(first, "source", "--file", "src/core.js").items;
  assert.equal(frozen[1].text, "  return 1;");
  assert.notEqual(executeSupercov(root, ["runs",first,"assertions","init"]).status, 0, "init must preserve authored work");
  assert.notEqual(executeSupercov(root, ["runs",first,"asserted"]).status, 0, "legacy analyzer command is gone");
  const second = run();
  query(second, "init", "--from", first);
  assert.equal(query(second).summary.dirtyFlows, 0);
  assert.equal(query(second).summary.lines.asserted, 1);
  writeFileSync(join(root,"src/core.js"), "export function value() {\n  return 2 - 1;\n}\n");
  const third = run();
  const carried = query(third,"init","--from",second);
  report = query(third);
  assert.equal(report.summary.dirtyFlows,1);
  assert.equal(report.summary.lines.asserted,0);
  assert.equal(query(first,"source","--file","src/core.js").items[1].text,"  return 1;", "old input snapshot remains frozen");
  assert.equal(query(first).workingTree.stale,true);
  const updated=read(carried.map);assert.equal(updated.assertions[0].id,a.id);
  updated.assertions[0].flows[0].nodes[0].at.text="return 2 - 1;";
  write(carried.map,updated);
  query(third,"review","--flow",`${a.id}/return-value`);
  assert.equal(query(third).summary.lines.asserted,1);
  assert.equal(read(initialized.map).assertions[0].flows[0].nodes[0].at.text,"return 1;");
  console.log(JSON.stringify({pilot:"assertion-map-cli",runs:3,assertions:1,creditedLines:1,inheritance:"unchanged reused; edited flow dirty until review",archivedSources:"preserved"}));
} finally { rmSync(root,{recursive:true,force:true}); }

const rustRoot = mkdtempSync(join(tmpdir(), "supercov-assertion-map-rust-"));
try {
  mkdirSync(join(rustRoot,"src"));
  writeFileSync(join(rustRoot,"Cargo.toml"),'[package]\nname="map-pilot"\nversion="0.1.0"\nedition="2024"\n');
  writeFileSync(join(rustRoot,"src/lib.rs"),"pub fn value() -> i32 {\n  return 1;\n}\n#[cfg(test)] mod tests {\n  #[test] fn value_test() {\n    assert_eq!(super::value(), 1);\n    assert!(true);\n  }\n}\n");
  requireSupercov(rustRoot,["--","cargo","test"]);
  const run=latestRun(rustRoot);
  const queryRust=(...args)=>coverageQuery(rustRoot,run,"assertions",...args).data;
  const init=queryRust("init");
  const sites=queryRust("--view","assertions").items;
  assert.equal(sites.length,2);
  assert(sites.every(a=>a.observedPassingTests.length===1),JSON.stringify(sites));
  const map=read(init.map);
  const a=map.assertions[0];a.analysis="mapped";a.observes=["value returns one"];
  a.flows=[{id:"return-value",explanation:"This macro compares the returned integer with one.",nodes:[{id:"return",at:{file:"src/lib.rs",line:2,column:3,text:"return 1;"}}],edges:[],countsAsAsserted:["return"],watch:[{kind:"file",file:"src/lib.rs"}]}];
  write(init.map,map);queryRust("review","--all");
  assert.equal(queryRust().summary.statements.asserted,1);
  console.log(JSON.stringify({pilot:"assertion-map-rust",assertions:2,passingSites:2,creditedStatements:1}));
} finally { rmSync(rustRoot,{recursive:true,force:true}); }
