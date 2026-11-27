// DIAGNOSTIC ONLY: two answers files (scripts/speed/answers.mjs), compared
// part by part, and their evidence again with each record's identical
// browser snapshots counted once.
import { readFileSync } from 'node:fs';
// A child process's pid is the one value answers.mjs leaves as it is.
const [left, right] = process.argv.slice(2).map((file) => JSON.parse(readFileSync(file, 'utf8').replace(/"child": \d+/g, '"child": "PID"')));
const lines = (value) => JSON.stringify(value, null, 1).split('\n');
function differing(a, b) {
  const counts = new Map();
  for (const line of lines(a)) counts.set(line, (counts.get(line) ?? 0) + 1);
  let unmatched = 0;
  for (const line of lines(b)) { const c = counts.get(line) ?? 0; if (c > 0) counts.set(line, c - 1); else unmatched += 1; }
  return unmatched + [...counts.values()].reduce((s, c) => s + c, 0);
}
const report = {};
for (const key of new Set([...Object.keys(left), ...Object.keys(right)])) {
  if (JSON.stringify(left[key]) === JSON.stringify(right[key])) continue;
  report[key] = differing(left[key], right[key]);
}
const once = (evidence) => Object.fromEntries(Object.entries(evidence ?? {}).map(([name, records]) => [name, records.map((record) => {
  if (!record || !Array.isArray(record.browser)) return record;
  const seen = new Set();
  return { ...record, browser: record.browser.filter((snapshot) => { const text = JSON.stringify(snapshot); if (seen.has(text)) return false; seen.add(text); return true; }) };
})]));
const evidenceOnce = JSON.stringify(once(left.evidence)) === JSON.stringify(once(right.evidence));
console.log(JSON.stringify({ identical: Object.keys(report).length === 0, differingLinesByPart: report, evidenceEqualWithDuplicateSnapshotsOnce: evidenceOnce }));
