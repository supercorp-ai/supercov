// Two answers files (answers.mjs) compared after the ids a non-JS run draws
// at random are named by what they stand for, and every list is sorted:
//   node scripts/speed/canonical-answers.mjs a.json b.json
// Exits 0 when equal; otherwise prints the first differences.
import { readFileSync, writeFileSync } from 'node:fs';

const RANDOM = /\b(owned-attempt|python-phase|ruby-phase|go-phase|jvm-phase|rust-phase|[a-z]+-phase|[a-z]+-attempt):[0-9a-f]{16,}\b/g;

function canonical(path) {
  const text = readFileSync(path, 'utf8');
  // Each random id becomes the one word for its kind: what it named is in
  // the record beside it (test, phase kind, operation).
  const value = JSON.parse(text.replace(RANDOM, (_, kind) => `${kind}:ID`));
  const sort = (node) => {
    if (Array.isArray(node)) {
      const items = node.map(sort);
      return items
        .map((item) => [JSON.stringify(item), item])
        .sort((a, b) => (a[0] < b[0] ? -1 : a[0] > b[0] ? 1 : 0))
        .map(([, item]) => item);
    }
    if (node && typeof node === 'object') {
      return Object.fromEntries(Object.keys(node).sort().map((key) => [key, sort(node[key])]));
    }
    return node;
  };
  return JSON.stringify(sort(value), null, 1);
}

const [a, b] = process.argv.slice(2);
const left = canonical(a);
const right = canonical(b);
if (left === right) {
  console.log(`IDENTICAL (canonical): ${a} ${b}`);
  process.exit(0);
}
writeFileSync(`${a}.canonical`, left);
writeFileSync(`${b}.canonical`, right);
const l = left.split('\n');
const r = right.split('\n');
let shown = 0;
for (let index = 0; index < Math.max(l.length, r.length) && shown < 40; index += 1) {
  if (l[index] !== r[index]) {
    console.log(`${index}: - ${l[index]}\n${index}: + ${r[index]}`);
    shown += 1;
  }
}
console.log(`DIFFER (canonical): ${l.length} vs ${r.length} lines`);
process.exit(1);
