#!/usr/bin/env node
// One of N groups of the scripts `npm run test:engine-integrations` runs,
// for CI to run the groups side by side:
//
//   node scripts/engine-integrations-group.mjs <group> <groups>
//
// The scripts are read from package.json, so the list stays one list. Each
// weighs the seconds its gate took (.github/gate-seconds.tsv, through the
// gate names in crates/supercov-cli/tests/e2e_gates.rs; a script no gate runs
// counts as a minute), and they are dealt longest first to the lightest
// group. Supercov is built first, as the npm script builds it.
import { spawnSync } from 'node:child_process';
import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';

const repository = resolve(import.meta.dirname, '..');
const [group, groups] = process.argv.slice(2).map(Number);
if (!(groups >= 1 && group >= 1 && group <= groups)) {
  console.error('usage: engine-integrations-group.mjs <group> <groups>');
  process.exit(2);
}

const scripts = JSON.parse(readFileSync(resolve(repository, 'package.json'), 'utf8')).scripts;
const commands = scripts['test:engine-integrations']
  .split(' && ')
  .filter((command) => command.startsWith('node scripts/'))
  .map((command) => command.slice('node '.length).split(' '));

const gates = new Map();
for (const [, name, script, args] of readFileSync(resolve(repository, 'crates/supercov-cli/tests/e2e_gates.rs'), 'utf8').matchAll(/^ {4}([a-z0-9_]+) => "([^"]+)"(?: \[([^\]]*)\])?/gmu)) {
  const words = (args ?? '').split(',').map((word) => word.trim().replace(/^"|"$/gu, '')).filter(Boolean);
  gates.set([`scripts/${script}`, ...words].join(' '), name);
}
const seconds = new Map(
  readFileSync(resolve(repository, '.github/gate-seconds.tsv'), 'utf8')
    .split('\n')
    .filter((line) => line && !line.startsWith('#'))
    .map((line) => line.split('\t'))
    .map(([name, value]) => [name, Number(value)]),
);
const weight = (command) => seconds.get(gates.get(command.join(' '))) ?? 60;

const loads = Array.from({ length: groups }, () => 0);
const mine = [];
for (const command of [...commands].sort((a, b) => weight(b) - weight(a) || a.join(' ').localeCompare(b.join(' ')))) {
  const lightest = loads.indexOf(Math.min(...loads));
  loads[lightest] += weight(command);
  if (lightest === group - 1) mine.push(command);
}

const run = (program, args) => {
  console.log(`\n[engine-integrations ${group}/${groups}] ${program} ${args.join(' ')}`);
  const result = spawnSync(program, args, { cwd: repository, stdio: 'inherit' });
  if (result.status !== 0) process.exit(result.status ?? 1);
};
run('cargo', ['build', '-p', 'supercov']);
// In the order package.json lists them, as the npm script runs them.
for (const command of commands.filter((command) => mine.includes(command))) run('node', command);
console.log(`\n[engine-integrations ${group}/${groups}] ${mine.length} of ${commands.length} scripts passed`);
