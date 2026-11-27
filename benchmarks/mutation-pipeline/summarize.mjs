#!/usr/bin/env node
// Reads every `strong-mutation.json` under a directory of benchmark artifacts
// (one artifact directory per project, named `<language>-<project>`) and
// prints, per project and per language: the mutants, how many were decided
// without running a test, how many ran, the test executions and the test
// time they took. With --verdicts FILE also writes each mutant's verdict, one
// JSON line each, for `compare.mjs`.
//
//   node summarize.mjs <artifacts> [--verdicts FILE] [--json]

import { readdirSync, readFileSync, statSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';

const [root, ...rest] = process.argv.slice(2);
const verdictsOut = rest.includes('--verdicts') ? rest[rest.indexOf('--verdicts') + 1] : null;

function* files(directory) {
  for (const name of readdirSync(directory)) {
    const path = join(directory, name);
    if (statSync(path).isDirectory()) yield* files(path);
    else yield path;
  }
}

const rows = [];
const lines = [];
for (const artifact of readdirSync(root).sort()) {
  const directory = join(root, artifact);
  if (!statSync(directory).isDirectory()) continue;
  const [language, ...name] = artifact.split('-');
  const project = name.join('-');
  const mutants = new Map();
  let strongSeconds = null;
  for (const path of files(directory)) {
    if (path.endsWith('strong-mutation.json')) {
      const stored = JSON.parse(readFileSync(path, 'utf8'));
      for (const [id, verdict] of Object.entries(stored.mutants ?? {})) mutants.set(id, verdict);
    }
    if (path.endsWith('strong-seconds.txt')) strongSeconds = Number(readFileSync(path, 'utf8').trim());
  }
  if (!mutants.size) continue;
  const row = { language, project, mutants: mutants.size, decidedWithoutRunning: 0, ran: 0, testExecutions: 0, testMs: 0, strongSeconds, statuses: {}, reasons: {} };
  for (const [id, verdict] of mutants) {
    row.statuses[verdict.status] = (row.statuses[verdict.status] ?? 0) + 1;
    const tests = verdict.tests ?? 0;
    if (tests > 0) {
      row.ran += 1;
      row.testExecutions += tests;
      row.testMs += verdict.durationMs ?? 0;
    } else {
      row.decidedWithoutRunning += 1;
      const reason = String(verdict.reason ?? '-').replace(/:.*/, '');
      row.reasons[reason] = (row.reasons[reason] ?? 0) + 1;
    }
    lines.push(JSON.stringify({ language, project, id, status: verdict.status, killedBy: verdict.killedBy ?? null, tests, reason: verdict.reason ?? null }));
  }
  rows.push(row);
}
if (verdictsOut) writeFileSync(verdictsOut, lines.join('\n') + '\n');

const byLanguage = {};
for (const row of rows) {
  const total = (byLanguage[row.language] ??= { language: row.language, mutants: 0, decidedWithoutRunning: 0, ran: 0, testExecutions: 0, testMs: 0, strongSeconds: 0 });
  for (const key of ['mutants', 'decidedWithoutRunning', 'ran', 'testExecutions', 'testMs']) total[key] += row[key];
  total.strongSeconds += row.strongSeconds ?? 0;
}
const decided = (r) => r.mutants - (r.statuses?.error ?? 0);
if (rest.includes('--json')) {
  console.log(JSON.stringify({ projects: rows, languages: Object.values(byLanguage) }, null, 1));
} else {
  console.log('| language | project | mutants | decided without running | ran | test executions | per ran mutant | test s | strong pass s |');
  console.log('| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |');
  for (const r of [...rows, ...Object.values(byLanguage).map((l) => ({ ...l, project: '**all**' }))]) {
    console.log(`| ${r.language} | ${r.project} | ${r.mutants} | ${r.decidedWithoutRunning} | ${r.ran} | ${r.testExecutions} | ${(r.testExecutions / Math.max(r.ran, 1)).toFixed(2)} | ${(r.testMs / 1000).toFixed(1)} | ${r.strongSeconds?.toFixed?.(1) ?? '-'} |`);
  }
  for (const r of rows) console.log(`${r.language}/${r.project}: statuses ${JSON.stringify(r.statuses)} not-run reasons ${JSON.stringify(r.reasons)}`);
  void decided;
}
