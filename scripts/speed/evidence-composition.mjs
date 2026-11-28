// What a run's evidence is made of: bytes of JSON by field of its test
// records (hits, decisions, events by column, phases, ...) and by entry,
// read from its evidence.raw.gz. What a measured run writes per test is most
// of what it adds after the tests, so this says where that goes.
//
//   node scripts/speed/evidence-composition.mjs .supercov/runs/<run>/evidence.raw.gz
import { readFileSync } from 'node:fs';
import { gunzipSync } from 'node:zlib';

const raw = gunzipSync(readFileSync(process.argv[2]));
let at = raw.indexOf('{"path"') - 4;
const sizes = new Map();
const counts = new Map();
const add = (key, bytes, n = 1) => {
  sizes.set(key, (sizes.get(key) ?? 0) + bytes);
  counts.set(key, (counts.get(key) ?? 0) + n);
};
const size = (value) => (value === undefined ? 0 : JSON.stringify(value).length);
let total = 0;
while (at < raw.length) {
  const headerSize = raw.readUInt32BE(at);
  const header = JSON.parse(raw.subarray(at + 4, at + 4 + headerSize).toString('utf8'));
  const start = at + 4 + headerSize;
  const contents = raw.subarray(start, start + header.bytes);
  at = start + header.bytes;
  total += header.bytes;
  if (!header.path.endsWith('.mcdc.jsonl')) {
    // Entries one per writing process count as one.
    add(`entry ${header.path.replace(/^execution\.host\..*\.jsonl$/, 'execution.host.*.jsonl').replace(/[0-9a-f-]{8,}/g, '*')}`, header.bytes);
    continue;
  }
  for (const line of contents.toString('utf8').split('\n')) {
    if (!line) continue;
    add('records', line.length);
    const record = JSON.parse(line);
    for (const [key, value] of Object.entries(record)) {
      if (key === 'runtime' || key === 'browser' || key === 'server') continue;
      add(`record.${key}`, size(value));
    }
    for (const kind of ['runtime', 'browser', 'server']) {
      for (const snapshot of record[kind] ?? []) {
        for (const [key, value] of Object.entries(snapshot)) {
          if (key === 'eventColumns') {
            for (const [column, data] of Object.entries(value)) add(`${kind}.eventColumns.${column}`, size(data), Array.isArray(data) ? data.length : 1);
          } else {
            add(`${kind}.${key}`, size(value), Array.isArray(value) ? value.length : 1);
          }
          if (key === 'decisions') for (const decision of value) add(`${kind}.decisions.vectors(n)`, 0, decision.vectors?.length ?? 0);
          if (key === 'comparisons') for (const comparison of value) add(`${kind}.comparisons.outcomes(n)`, 0, comparison.outcomes?.length ?? 0);
        }
      }
    }
  }
}
console.log(`uncompressed archive ${(total / 1e6).toFixed(1)} MB`);
for (const [key, bytes] of [...sizes].sort((a, b) => b[1] - a[1]))
  console.log(`${(bytes / 1e6).toFixed(2).padStart(9)} MB  ${String(counts.get(key)).padStart(10)}  ${key}`);
