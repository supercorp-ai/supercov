#!/usr/bin/env node
// Splits a project's mutate set into ground-truth shards that each fit a ten-minute job.
//
//   node benchmarks/mutation/plan-shards.mjs <project> <report.json[,report.json...] | checkout dir> <count>
//     [--checkout DIR] [--write] [--key shards|coldShards]
//
// Given a checkout of the project instead of a report (before any report exists), the cost of
// a file is its count of non-blank lines that are not comments or type declarations.
// The cost of a mutant under disableBail is the number of tests it runs: every covering
// test, or every test for a static mutant. Files are packed greedily by that cost; a file
// costing more than one shard's share is cut into line ranges. With --write the shards
// replace the project's "shards" in corpus.json. Every shard lists files or line ranges;
// together they cover the whole mutate set (checked against the report's files).

import { globSync, readFileSync, statSync, writeFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const here = dirname(fileURLToPath(import.meta.url));
const [name, reportFile, countText, ...rest] = process.argv.slice(2);
const write = rest.includes('--write') ? '--write' : null;
const key = rest.includes('--key') ? rest[rest.indexOf('--key') + 1] : 'shards';
const count = Number(countText);
const corpusFile = join(here, 'corpus.json');
const corpus = JSON.parse(readFileSync(corpusFile, 'utf8'));
const project = corpus.projects.find((p) => p.name === name);
if (!project || !reportFile || !count) throw new Error('usage: plan-shards.mjs <project> <report.json> <count> [--write]');

// Line-based cost: one unit per code line that is not blank, a comment or a type declaration.
function codeLines(root, file) {
  return readFileSync(join(root, file), 'utf8')
    .split('\n')
    .map((text, i) => ({ text: text.trim(), line: i + 1 }))
    .filter(({ text }) => text && !/^(\/\/|\*|\/\*|import |export type |type |interface |export interface |\}$|\);?$)/.test(text))
    .map(({ line }) => ({ line, cost: 1 }));
}

function mutateFiles(root) {
  const excluded = new Set(project.mutate.exclude.flatMap((g) => globSync(g, { cwd: root })));
  return [...new Set(project.mutate.include.flatMap((g) => globSync(g, { cwd: root })))].filter((f) => !excluded.has(f)).sort();
}

// Inputs: a checkout directory, one or more StrykerJS reports (comma-separated, e.g. the
// reports of a sharded cold run), or both (reports first, then --checkout DIR for the files
// no report covers, costed at the reports' mean cost per code line).
let files = [];
let allTests = 0;
const isDir = !reportFile.includes(',') && statSync(reportFile).isDirectory();
const checkout = rest.includes('--checkout') ? rest[rest.indexOf('--checkout') + 1] : isDir ? reportFile : null;
const reportFiles = isDir ? [] : reportFile.split(',');
for (const file of reportFiles) {
  const report = JSON.parse(readFileSync(file, 'utf8'));
  allTests = Math.max(allTests, Object.values(report.testFiles ?? {}).reduce((n, f) => n + f.tests.length, 0));
  const cost = (m) => (m.static ? allTests : (m.coveredBy ?? []).length) + 1;
  for (const [path, entry] of Object.entries(report.files)) {
    if (files.some((f) => f.file === path)) continue;
    files.push({ file: path, mutants: entry.mutants.map((m) => ({ line: m.location.start.line, cost: cost(m) })) });
  }
}
if (checkout) {
  const known = new Set(files.map((f) => f.file));
  let perLine = 1;
  if (files.length) {
    const knownCost = files.reduce((s, f) => s + f.mutants.reduce((a, m) => a + m.cost, 0), 0);
    const knownLines = files.reduce((s, f) => s + codeLines(checkout, f.file).length, 0);
    perLine = knownCost / Math.max(knownLines, 1);
  }
  for (const file of mutateFiles(checkout)) {
    if (known.has(file)) continue;
    files.push({ file, mutants: codeLines(checkout, file).map((m) => ({ ...m, cost: perLine })) });
  }
}
const total = Math.round(files.reduce((s, f) => s + f.mutants.reduce((a, m) => a + m.cost, 0), 0));
const share = total / count;

// Pieces: whole files, or line ranges of files costing more than a share.
const pieces = [];
for (const f of files) {
  const fileCost = f.mutants.reduce((a, m) => a + m.cost, 0);
  if (fileCost <= share) {
    pieces.push({ spec: f.file, cost: fileCost });
    continue;
  }
  const byLine = new Map();
  for (const m of f.mutants) byLine.set(m.line, (byLine.get(m.line) ?? 0) + m.cost);
  const lines = [...byLine.keys()].sort((a, b) => a - b);
  let start = 1;
  let acc = 0;
  for (const line of lines) {
    acc += byLine.get(line);
    if (acc >= share * 0.9) {
      pieces.push({ spec: `${f.file}:${start}-${line}`, cost: acc });
      start = line + 1;
      acc = 0;
    }
  }
  if (acc > 0 || start === 1) pieces.push({ spec: `${f.file}:${start}-99999`, cost: acc });
  else pieces.at(-1).spec = pieces.at(-1).spec.replace(/-\d+$/, '-99999');
}

pieces.sort((a, b) => b.cost - a.cost);
const bins = Array.from({ length: Math.max(count, 1) }, () => ({ cost: 0, include: [] }));
for (const piece of pieces) {
  const bin = bins.reduce((a, b) => (b.cost < a.cost ? b : a));
  bin.cost += piece.cost;
  bin.include.push(piece.spec);
}
const shards = bins
  .filter((b) => b.include.length)
  .map((b, i) => ({ name: `s${i + 1}`, estimatedTestRuns: Math.round(b.cost), include: b.include.sort() }));

console.log(`${name}: ${files.length} files, ${allTests} tests, cost ${total}; ${shards.length} shards`);
for (const s of shards) console.log(`  ${s.name}: cost ${s.estimatedTestRuns}, ${s.include.length} pieces`);
if (write === '--write') {
  project[key] = shards;
  project[`${key}Note`] = allTests
    ? `planned by plan-shards.mjs from a StrykerJS cold report: ${total} test runs under disableBail split ${shards.length} ways`
    : `planned by plan-shards.mjs from source lines (${total} code lines split ${shards.length} ways), before any report existed`;
  writeFileSync(corpusFile, `${JSON.stringify(corpus, null, 2)}\n`);
  console.log('corpus.json updated');
}
