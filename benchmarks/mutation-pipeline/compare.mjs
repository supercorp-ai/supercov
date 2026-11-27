#!/usr/bin/env node
// Joins two verdict files `summarize.mjs --verdicts` wrote (the base's and a
// change's) by language, project and mutant id, and prints every mutant
// whose verdict moved, grouped by the move. A change that only saves work
// moves nothing but `(none) -> x` or `x -> equivalent`; anything else (a kill
// turned survivor, a survivor turned timeout) is listed in full. Exits 1
// when a verdict a run decided moved to another a run decides.
//
//   node compare.mjs <base.jsonl> <new.jsonl>

import { readFileSync } from 'node:fs';

const read = (path) =>
  new Map(
    readFileSync(path, 'utf8')
      .split('\n')
      .filter(Boolean)
      .map((line) => JSON.parse(line))
      .map((v) => [`${v.language}/${v.project}/${v.id}`, v]),
  );
const [basePath, newPath] = process.argv.slice(2);
const base = read(basePath);
const next = read(newPath);
const decided = new Set(['killed', 'survived', 'timeout', 'no-coverage']);
const moves = new Map();
let violations = 0;
for (const key of new Set([...base.keys(), ...next.keys()])) {
  const from = base.get(key)?.status ?? '(none)';
  const to = next.get(key)?.status ?? '(none)';
  if (from === to) continue;
  const move = `${from} -> ${to}`;
  if (!moves.has(move)) moves.set(move, []);
  moves.get(move).push(key);
  if (decided.has(from) && decided.has(to)) violations += 1;
}
console.log(`mutants compared: base ${base.size}, new ${next.size}`);
const same = [...base.keys()].filter((key) => next.get(key)?.status === base.get(key).status).length;
console.log(`same verdict: ${same}`);
for (const [move, keys] of [...moves].sort()) {
  console.log(`${move}: ${keys.length}`);
  for (const key of keys.slice(0, 50)) console.log(`  ${key}`);
}
console.log(violations ? `VERDICT CHANGES: ${violations}` : 'no decided verdict moved');
process.exit(violations ? 1 : 0);
