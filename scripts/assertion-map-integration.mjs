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
  const automatic = coverageQuery(root, first).data.assertionCoverage;
  assert.equal(automatic.summary.statements.percentage, 0);
  assert.equal(automatic.summary.unmappedAssertions, 1);
  assert.equal(automatic.inheritance.from, null);
  const initialized = query(first);
  assert.equal(initialized.map, automatic.map);
  const map = read(initialized.map);
  assert.equal(map.assertions.length, 1);
  assert.equal(initialized.items.length, 1);
  assert.equal(initialized.items[0].analysis, 'unmapped');
  assert.equal(initialized.items[0].inMap, true);
  const a = map.assertions[0];
  const source = coverageQuery(root, first, 'source', 'src/core.js', '--offset', '1', '--limit', '1');
  assert.equal(source.command, 'coverage.source');
  assert.deepEqual(source.data.items, [{ line: 2, text: '  return 1;' }]);
  const sourceText = requireSupercov(root, ['runs', first, 'source', 'src/core.js', '--offset', '1', '--limit', '1']).stdout;
  assert.match(sourceText, /2 │   return 1;/);
  assert(!sourceText.includes('"text"'));
  assert.match(sourceText, /--offset 2 --limit 1/);
  const listText = requireSupercov(root, ['runs', first, 'assertions']).stdout;
  assert(listText.includes(a.id));
  assert.match(listText, /unmapped · no flows · 1 passing test/);
  for (const resource of [['source'], ['source', 'missing.ts'], ['assertion'], ['assertion', 'unknown-id'], ['assertion', a.id, '--limit', '1'], ['assertions', 'inventory']]) {
    assert.equal(executeSupercov(root, ['runs', first, ...resource]).status, 2, resource.join(' '));
  }
  // Removing a map entry must not hide a recognized assertion from the list.
  write(initialized.map, { assertions: [] });
  const missing = query(first);
  assert.equal(missing.items.length, 1);
  assert.equal(missing.items[0].inMap, false);
  assert.equal(missing.items[0].id, a.id);
  assert.equal(missing.summary.missingInventoryAssertions, 1);
  assert.equal(coverageQuery(root, first, 'assertion', a.id).data.assertion.inMap, false);
  write(initialized.map, map);
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
  const detail = coverageQuery(root, first, 'assertion', a.id);
  assert.equal(detail.command, 'coverage.assertion');
  assert.deepEqual(detail.data.assertion.observes, a.observes);
  assert.deepEqual(detail.data.assertion.flows[0].nodes, a.flows[0].nodes);
  assert.deepEqual(detail.data.assertion.flows[0].watch, a.flows[0].watch);
  assert.equal(detail.data.assertion.flows[0].explanation, a.flows[0].explanation);
  assert.equal(detail.data.assertion.flows[0].current, true);
  assert.equal(detail.data.assertion.flows[0].eligible, true);
  assert.equal(detail.data.revision, report.revision);
  assert.equal(detail.data.tests.length, 1);
  const detailText = requireSupercov(root, ['runs', first, 'assertion', a.id]).stdout;
  assert(detailText.includes(a.flows[0].explanation));
  assert.match(detailText, /Node return — src\/core.js:2:3/);
  assert.match(detailText, /current, eligible for credit/);
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
  assert.equal(coverageQuery(root, first, 'source', 'src/core.js').data.items[1].text, '  return 1;', 'source remains readable with malformed map JSON');
  assert.match(requireSupercov(root, ["runs", first]).stdout, /Assertions unavailable: assertions.json/);
  write(initialized.map, map);
  query(first, "review", "--all");
  assert.equal(coverageQuery(root, first).data.assertionCoverage.summary.statements.percentage, 100);
  const firstMapBytes = readFileSync(initialized.map);
  const firstStatePath = join(root, '.supercov/runs', first, 'assertions.state.json');
  const firstStateBytes = readFileSync(firstStatePath);
  const frozen = coverageQuery(root, first, "source", "src/core.js").data.items;
  assert.equal(frozen[1].text, "  return 1;");
  assert.notEqual(executeSupercov(root, ["runs",first,"assertions","init"]).status, 0, "the init command is removed");
  assert.notEqual(executeSupercov(root, ["runs",first,"asserted"]).status, 0, "legacy analyzer command is gone");
  const second = run();
  assert.equal(query(second).inheritance.from, first);
  assert.equal(query(second).summary.dirtyFlows, 0);
  assert.equal(query(second).summary.lines.asserted, 1);
  writeFileSync(join(root,"src/core.js"), "export function value() {\n  return 2 - 1;\n}\n");
  const third = run();
  const carried = query(third);
  assert.equal(carried.inheritance.from, second);
  report = query(third);
  assert.equal(report.summary.dirtyFlows,1);
  assert.equal(report.summary.lines.asserted,0);
  assert.equal(coverageQuery(root, first, "source", "src/core.js").data.items[1].text,"  return 1;", "old input snapshot remains frozen");
  assert.equal(query(first).workingTree.stale,true);
  const updated=read(carried.map);assert.equal(updated.assertions[0].id,a.id);
  updated.assertions[0].flows[0].nodes[0].at.text="return 2 - 1;";
  write(carried.map,updated);
  query(third,"review","--flow",`${a.id}/return-value`);
  assert.equal(query(third).summary.lines.asserted,1);
  assert.equal(read(initialized.map).assertions[0].flows[0].nodes[0].at.text,"return 1;");
  // A different test command starts its own map and cannot eclipse this suite.
  requireSupercov(root, ["--", "node", "--test", "tests/core.test.js"]);
  const focused = latestRun(root);
  assert.equal(query(focused).inheritance.from, null);
  const fourth = run();
  assert.equal(query(fourth).inheritance.from, third);
  assert.equal(query(fourth).summary.statements.asserted, 1);

  // Newer malformed work stays untouched. An older fallback requires review.
  writeFileSync(query(fourth).map, "{broken JSON");
  const fallback = run();
  const recovered = query(fallback);
  assert.equal(recovered.inheritance.from, third);
  assert.equal(recovered.inheritance.skipped[0].run, fourth);
  assert.equal(recovered.summary.dirtyFlows, 1);
  assert.equal(recovered.summary.statements.asserted, 0);
  assert.match(requireSupercov(root, ["runs", fallback]).stdout, /Map reuse skipped 1 prior map/);
  assert.equal(readFileSync(join(root, '.supercov/runs', fourth, 'assertions.json'), 'utf8'), '{broken JSON');
  const stillDirty = run();
  assert.equal(query(stillDirty).inheritance.from, fallback);
  assert.equal(query(stillDirty).summary.dirtyFlows, 1, 'fallback review requirement persists');
  query(stillDirty, "review", "--all");
  assert.equal(query(stillDirty).summary.statements.asserted, 1);

  // Exact moves remap both assertion and statement anchors automatically.
  const testPath = join(root, 'tests/core.test.js');
  const corePath = join(root, 'src/core.js');
  writeFileSync(testPath, '\n' + readFileSync(testPath, 'utf8'));
  writeFileSync(corePath, '\n' + readFileSync(corePath, 'utf8'));
  const moved = run();
  const movedReport = query(moved);
  const movedMap = read(movedReport.map);
  assert.equal(movedMap.assertions[0].id, a.id);
  assert.equal(movedMap.assertions[0].at.line, 6);
  assert.equal(movedMap.assertions[0].flows[0].nodes[0].at.line, 3);
  assert.equal(movedReport.summary.statements.asserted, 1);

  // Failed runs still have maps, but cannot inherit passing execution credit.
  writeFileSync(corePath, readFileSync(corePath, 'utf8').replace('return 2 - 1;', 'return 2;'));
  assert.notEqual(executeSupercov(root, ["--", "node", "--test"]).status, 0);
  const failed = latestRun(root);
  assert.equal(query(failed).inheritance.from, moved);
  assert.equal(query(failed).summary.runPassed, false);
  assert.equal(query(failed).summary.statements.asserted, 0);
  assert.deepEqual(readFileSync(initialized.map), firstMapBytes);
  assert.deepEqual(readFileSync(firstStatePath), firstStateBytes);
  console.log(JSON.stringify({pilot:"assertion-map-cli",runs:9,assertions:1,creditedLines:1,inheritance:"unchanged reused; edited flow dirty until review",archivedSources:"preserved"}));
} finally { rmSync(root,{recursive:true,force:true}); }

const rustRoot = mkdtempSync(join(tmpdir(), "supercov-assertion-map-rust-"));
try {
  mkdirSync(join(rustRoot,"src"));
  writeFileSync(join(rustRoot,"Cargo.toml"),'[package]\nname="map-pilot"\nversion="0.1.0"\nedition="2024"\n');
  writeFileSync(join(rustRoot,"src/lib.rs"),"pub fn value() -> i32 {\n  return 1;\n}\n#[cfg(test)] mod tests {\n  #[test] fn value_test() {\n    assert_eq!(super::value(), 1);\n    assert!(true);\n  }\n}\n");
  requireSupercov(rustRoot,["--","cargo","test"]);
  const run=latestRun(rustRoot);
  const queryRust=(...args)=>coverageQuery(rustRoot,run,"assertions",...args).data;
  const init=queryRust();
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
