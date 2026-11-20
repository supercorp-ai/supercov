#!/usr/bin/env node
// One benchmark job: clone a corpus project at its pinned commit, install it and
// one mutation tool, run one scenario, and write what it measured to --out:
//
//   result.json      phases (wall time, CPU seconds, peak RSS, exit code), versions,
//                    machine, and the reports each phase wrote
//   <phase>.log      every output line of the phase, prefixed with seconds since it started
//   <phase>.json     the mutation-testing-elements report the phase wrote, if any
//   tests.json       Vitest's JSON report of the plain test run (cold scenarios)
//
// Scenarios (see README.md):
//   stryker-cold   StrykerJS, perTest coverage, concurrency = CPUs, no incremental, no checker
//   mutator-cold   mizchi/mutator with its defaults and the json reporter
//   truth          StrykerJS with disableBail (every covering test runs for every mutant);
//                  with --shard NAME only that shard's files of the mutate set ("shards")
//   (stryker-cold takes --shard NAME for projects with "coldShards": that shard's files;
//    mutator-cold takes --shard I for projects with "mutatorColdShards": its own --shard I/N partition)
//   pr-stryker     incremental base run, then the edit; Stryker on the changed lines (cold),
//                  then Stryker --incremental from the base run's file (warm)
//   pr-mutator     cold base run, then the edit committed; mutator --since HEAD~1 without
//                  a snapshot (cold), then with the base run's snapshot (warm)
//
// Runs on a CI machine, never locally: it installs packages into the clone.
// Node only, no dependencies.

import { spawn, execSync } from 'node:child_process';
import { appendFileSync, copyFileSync, existsSync, mkdirSync, readFileSync, renameSync, rmSync, writeFileSync, createWriteStream } from 'node:fs';
import { availableParallelism, cpus, totalmem, tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { parseArgs } from 'node:util';

const here = dirname(fileURLToPath(import.meta.url));
const { values: args } = parseArgs({
  options: {
    project: { type: 'string' },
    scenario: { type: 'string' },
    shard: { type: 'string' },
    out: { type: 'string' },
    work: { type: 'string' },
    budget: { type: 'string' },
  },
});
const corpus = JSON.parse(readFileSync(join(here, 'corpus.json'), 'utf8'));
const project = corpus.projects.find((p) => p.name === args.project);
if (!project) throw new Error(`unknown project ${args.project}`);
const scenario = args.scenario;
const SCENARIOS = ['stryker-cold', 'mutator-cold', 'truth', 'pr-stryker', 'pr-mutator'];
if (!SCENARIOS.includes(scenario)) throw new Error(`unknown scenario ${scenario}`);
const out = resolve(args.out ?? 'out');
mkdirSync(out, { recursive: true });
const workRoot = resolve(args.work ?? process.env.RUNNER_TEMP ?? tmpdir(), 'mutation-bench');
const clone = join(workRoot, project.name);
const started = Date.now();
// The whole job must end within ten minutes; the steps before this script take about a minute.
const deadline = started + Number(args.budget ?? 510) * 1000;
const CPUS = availableParallelism();

const result = {
  project: project.name,
  scenario,
  shard: args.shard ?? null,
  commit: project.commit,
  startedAt: new Date(started).toISOString(),
  machine: machine(),
  versions: {},
  notes: [],
  phases: [],
};
const note = (text) => { if (!result.notes.includes(text)) result.notes.push(text); };
const save = () => writeFileSync(join(out, 'result.json'), JSON.stringify(result, null, 2));

function machine() {
  let instanceType = null;
  try {
    const token = execSync(
      'curl -s -m 2 -X PUT http://169.254.169.254/latest/api/token -H "X-aws-ec2-metadata-token-ttl-seconds: 60"',
      { encoding: 'utf8' },
    );
    instanceType = execSync(
      `curl -s -m 2 -H "X-aws-ec2-metadata-token: ${token}" http://169.254.169.254/latest/meta-data/instance-type`,
      { encoding: 'utf8' },
    ).trim() || null;
  } catch {}
  return { instanceType, cpus: CPUS, cpuModel: cpus()[0]?.model ?? null, memGiB: +(totalmem() / 2 ** 30).toFixed(1) };
}

function sh(command, cwd = clone) {
  console.log(`$ ${command}`);
  return execSync(command, { cwd, stdio: ['ignore', 'pipe', 'inherit'], encoding: 'utf8', env: process.env });
}

const hasGnuTime = existsSync('/usr/bin/time');

/** Runs a shell command as one measured phase; its output goes to <name>.log with timestamps. */
function phase(name, command, { cwd = clone, allowFail = false } = {}) {
  return new Promise((resolvePhase, rejectPhase) => {
    const timeFile = join(out, `${name}.time`);
    const remaining = deadline - Date.now();
    const record = { name, command, startOffsetS: +((Date.now() - started) / 1000).toFixed(2) };
    if (remaining < 15_000) {
      record.skipped = 'no time left in the job budget';
      result.phases.push(record);
      save();
      if (allowFail) return resolvePhase(record);
      return rejectPhase(new Error(`${name}: ${record.skipped}`));
    }
    console.log(`\n::group::${name}: ${command}`);
    const argv = hasGnuTime ? ['/usr/bin/time', '-v', '-o', timeFile, 'bash', '-c', command] : ['bash', '-c', command];
    const t0 = Date.now();
    const child = spawn(argv[0], argv.slice(1), { cwd, detached: true, stdio: ['ignore', 'pipe', 'pipe'], env: process.env });
    const log = createWriteStream(join(out, `${name}.log`));
    let timedOut = false;
    const killer = setTimeout(() => {
      timedOut = true;
      try { process.kill(-child.pid, 'SIGTERM'); } catch {}
      setTimeout(() => { try { process.kill(-child.pid, 'SIGKILL'); } catch {} }, 10_000).unref();
    }, remaining);
    for (const stream of [child.stdout, child.stderr]) {
      let buffered = '';
      stream.setEncoding('utf8');
      stream.on('data', (chunk) => {
        buffered += chunk;
        const lines = buffered.split(/\r?\n|\r/);
        buffered = lines.pop();
        const stamp = ((Date.now() - t0) / 1000).toFixed(3);
        for (const line of lines) {
          log.write(`[${stamp}] ${line}\n`);
          process.stdout.write(`[${stamp}] ${line}\n`);
        }
      });
      stream.on('end', () => { if (buffered) log.write(`[${((Date.now() - t0) / 1000).toFixed(3)}] ${buffered}\n`); });
    }
    child.on('close', (code, signal) => {
      clearTimeout(killer);
      log.end();
      console.log('::endgroup::');
      record.wallS = +((Date.now() - t0) / 1000).toFixed(3);
      record.exitCode = code;
      if (signal) record.signal = signal;
      if (timedOut) record.timedOut = true;
      if (hasGnuTime && existsSync(timeFile)) record.rusage = parseGnuTime(readFileSync(timeFile, 'utf8'));
      result.phases.push(record);
      save();
      console.log(`${name}: ${record.wallS}s exit ${code}${record.rusage ? ` cpu ${record.rusage.cpuS}s` : ''}`);
      if ((code !== 0 || timedOut) && !allowFail) return rejectPhase(new Error(`${name} failed (exit ${code}${timedOut ? ', timed out' : ''})`));
      resolvePhase(record);
    });
  });
}

function parseGnuTime(text) {
  const num = (label) => {
    const m = text.match(new RegExp(`${label}: (.+)`));
    return m ? m[1].trim() : null;
  };
  const user = Number(num('User time \\(seconds\\)'));
  const sys = Number(num('System time \\(seconds\\)'));
  return {
    userS: user,
    sysS: sys,
    cpuS: +(user + sys).toFixed(2),
    cpuPercent: num('Percent of CPU this job got'),
    maxRssKiB: Number(num('Maximum resident set size \\(kbytes\\)')),
    note: 'GNU time: the process and every descendant it waited for',
  };
}

const isWorkspace = () => existsSync(join(clone, 'pnpm-workspace.yaml'));
const pnpmAdd = (specs) => `pnpm add -D ${isWorkspace() ? '-w ' : ''}${specs.join(' ')}`;

function packageJson() {
  return JSON.parse(readFileSync(join(clone, 'package.json'), 'utf8'));
}

function installedVersion(name) {
  const file = join(clone, 'node_modules', name, 'package.json');
  return existsSync(file) ? JSON.parse(readFileSync(file, 'utf8')).version : null;
}

/** Vitest at `version` in place of the project's, with the project's @vitest/* plugins dropped. */
async function upgradeVitest(version, reason) {
  const pkg = packageJson();
  const deps = { ...pkg.dependencies, ...pkg.devDependencies };
  const plugins = Object.keys(deps).filter((d) => d.startsWith('@vitest/'));
  if (plugins.length) await phase('vitest-plugins-remove', `pnpm remove ${isWorkspace() ? '-w ' : ''}${plugins.join(' ')}`);
  const also = project.vitestUpgradeAlso ?? [];
  await phase('vitest-upgrade', pnpmAdd([`vitest@${version}`, ...also]));
  note(`Vitest upgraded in this job from ${project.vitest} to ${version}${plugins.length ? ` (removed ${plugins.join(', ')})` : ''}: ${reason}`);
  if (also.length) note(`also added ${also.join(', ')}: ${project.vitestUpgradeAlsoReason}`);
}

function excludeFromGit(lines) {
  appendFileSync(join(clone, '.git/info/exclude'), `\n${lines.join('\n')}\n`);
}

function commitAll(message) {
  sh('git add -A');
  sh(`git -c user.name=bench -c user.email=bench@example.invalid commit -q --no-verify --allow-empty -m "${message}"`);
}

function applyEdit() {
  sh(`git apply --whitespace=nowarn ${join(here, project.patch)}`);
  commitAll('benchmark edit');
}

/** Stryker `mutate` entries for the lines the edit adds: file:start-end. */
function changedLineRanges() {
  const ranges = [];
  let file = null;
  let line = 0;
  for (const text of readFileSync(join(here, project.patch), 'utf8').split('\n')) {
    if (text.startsWith('+++ ')) { file = text.slice(4).replace(/^b\//, ''); continue; }
    const hunk = text.match(/^@@ -\d+(?:,\d+)? \+(\d+)/);
    if (hunk) { line = Number(hunk[1]); continue; }
    if (!file || text.startsWith('---')) continue;
    if (text.startsWith('+')) {
      const last = ranges.at(-1);
      if (last && last.file === file && last.end === line - 1) last.end = line;
      else ranges.push({ file, start: line, end: line });
      line++;
    } else if (!text.startsWith('-')) line++;
  }
  return ranges.map((r) => `${r.file}:${r.start}-${r.end}`);
}

/** The shard list a sharded scenario draws from: ground truth uses "shards", cold runs "coldShards". */
function shardList() {
  return scenario === 'truth' ? project.shards : project.coldShards;
}

function currentShard() {
  if (!args.shard) return null;
  const list = shardList() ?? [];
  const index = list.findIndex((s) => s.name === args.shard);
  if (index < 0) throw new Error(`no shard ${args.shard} for ${project.name} ${scenario}`);
  return { ...list[index], index: index + 1, count: list.length };
}

function mutateGlobs() {
  const shard = currentShard();
  if (shard) return { include: shard.include, exclude: project.mutate.exclude };
  return project.mutate;
}

function writeStrykerConfig(file, { mutate, incremental = false, disableBail = false, report }) {
  const config = {
    packageManager: 'pnpm',
    testRunner: 'vitest',
    plugins: ['@stryker-mutator/vitest-runner'],
    vitest: project.vitestConfig ? { configFile: project.vitestConfig } : {},
    coverageAnalysis: 'perTest',
    concurrency: CPUS,
    incremental,
    incrementalFile: 'reports/stryker-incremental.json',
    checkers: [],
    disableBail,
    mutate,
    ignorePatterns: project.strykerIgnorePatterns ?? [],
    reporters: ['json', 'progress-append-only'],
    jsonReporter: { fileName: report },
    tempDirName: '.stryker-tmp',
    cleanTempDir: true,
    ...project.strykerOptions,
  };
  if (project.strykerOptions) note(`StrykerJS options ${JSON.stringify(project.strykerOptions)}: ${project.strykerOptionsReason}`);
  writeFileSync(join(clone, file), JSON.stringify(config, null, 2));
  return config;
}

const strykerMutate = ({ include, exclude }) => [...include, ...exclude.map((e) => `!${e}`)];

async function stryker(name, options) {
  const report = `reports/mutation/${name}.json`;
  const configFile = `stryker.${name}.json`;
  const config = writeStrykerConfig(configFile, { ...options, report });
  const record = await phase(name, `node_modules/.bin/stryker run ${configFile}`, { allowFail: true });
  record.config = config;
  collectReport(name, join(clone, report), record);
}

async function mutator(name, extra = [], { includeGlobs = project.mutate } = {}) {
  const reportDir = `reports-mutator/${name}`;
  const flags = [
    ...includeGlobs.include.flatMap((g) => ['--include', `'${g}'`]),
    ...includeGlobs.exclude.flatMap((g) => ['--exclude', `'${g}'`]),
    ...(project.vitestConfig ? ['--config', project.vitestConfig] : []),
    '--reporter', 'json', '--reporter', 'text',
    '--report-dir', reportDir,
    ...(project.mutatorArgs ?? []),
    ...extra,
  ];
  if (project.mutatorArgs) note(`mutator flags ${project.mutatorArgs.join(' ')}: ${project.mutatorArgsReason}`);
  const record = await phase(name, `node_modules/.bin/mutator ${flags.join(' ')}`, { allowFail: true });
  collectReport(name, join(clone, reportDir, 'mutation.json'), record);
}

function collectReport(name, file, record) {
  if (existsSync(file)) {
    copyFileSync(file, join(out, `${name}.json`));
    record.report = `${name}.json`;
  } else {
    record.report = null;
  }
  save();
}

async function setup() {
  rmSync(clone, { recursive: true, force: true });
  mkdirSync(clone, { recursive: true });
  await phase('clone', `git init -q . && git fetch -q --depth 1 ${project.repo} ${project.commit} && git checkout -q FETCH_HEAD`);
  if (project.configFile) copyFileSync(join(here, project.configFile), join(clone, project.vitestConfig));
  excludeFromGit(['stryker.*.json', 'reports/', 'reports-mutator/', '.stryker-tmp/', '.mutator/', '.mutator-*/', 'node_modules/']);
  await phase('pnpm-setup', `npm i -g pnpm@${project.pnpm} >/dev/null && pnpm --version`, { cwd: workRoot });
  await phase('install', project.install);
}

async function installStryker() {
  if (project.vitestForStryker) await upgradeVitest(project.vitestForStryker, project.vitestForStrykerReason);
  await phase('tool-install', pnpmAdd([`@stryker-mutator/core@${corpus.tools.stryker}`, `@stryker-mutator/vitest-runner@${corpus.tools.stryker}`]));
  result.versions.stryker = installedVersion('@stryker-mutator/core');
}

async function installMutator() {
  const major = Number(String(installedVersion('vitest') ?? project.vitest).split('.')[0]);
  if (major < 5) await upgradeVitest(corpus.tools.vitestForMutator, 'mizchi/mutator needs Vitest 5');
  await phase('tool-install', pnpmAdd([`@mizchi/mutator@${corpus.tools.mutator}`]));
  result.versions.mutator = installedVersion('@mizchi/mutator');
}

async function plainTests() {
  if (args.shard && args.shard !== '1' && !(scenario === 'stryker-cold' && currentShard()?.index === 1)) return;
  const record = await phase('tests', `node_modules/.bin/${project.test} --reporter=json --outputFile=${join(out, 'tests.json')}`, { allowFail: true });
  if (existsSync(join(out, 'tests.json'))) {
    const t = JSON.parse(readFileSync(join(out, 'tests.json'), 'utf8'));
    record.tests = { total: t.numTotalTests, passed: t.numPassedTests, failed: t.numFailedTests, files: t.numTotalTestSuites };
    save();
  }
}

async function main() {
  console.log(`${project.name} / ${scenario}${args.shard ? ` / ${args.shard}` : ''} on ${CPUS} CPUs; clone at ${clone}`);
  save();
  await setup();
  const isStryker = scenario.includes('stryker') || scenario === 'truth';
  if (isStryker) await installStryker();
  else await installMutator();
  result.versions.node = process.version;
  result.versions.pnpm = project.pnpm;
  result.versions.vitest = installedVersion('vitest');
  result.versions.vitestProject = project.vitest;
  commitAll('benchmark setup');
  save();

  const prMutate = project.prMutate ?? project.mutate;
  if (scenario === 'stryker-cold') {
    await plainTests();
    await stryker('stryker-cold', { mutate: strykerMutate(mutateGlobs()) });
  } else if (scenario === 'truth') {
    await stryker('truth', { mutate: strykerMutate(mutateGlobs()), disableBail: true });
  } else if (scenario === 'mutator-cold') {
    await plainTests();
    const shard = args.shard ? ['--shard', `${args.shard}/${project.mutatorColdShards}`] : [];
    if (args.shard && !project.mutatorColdShards) throw new Error(`${project.name} has no mutatorColdShards`);
    await mutator('mutator-cold', shard);
  } else if (scenario === 'pr-stryker') {
    if (project.prMutate) note(`PR scenario base runs mutate ${JSON.stringify(project.prMutate.include)}: ${project.prMutateNote}`);
    await stryker('stryker-incremental-base', { mutate: strykerMutate(prMutate), incremental: true });
    applyEdit();
    const lines = changedLineRanges();
    result.changedLines = lines;
    await stryker('stryker-lines', { mutate: lines });
    await stryker('stryker-incremental', { mutate: strykerMutate(prMutate), incremental: true });
  } else if (scenario === 'pr-mutator') {
    if (project.prMutate) note(`PR scenario base runs mutate ${JSON.stringify(project.prMutate.include)}: ${project.prMutateNote}`);
    await mutator('mutator-base', [], { includeGlobs: prMutate });
    if (existsSync(join(clone, '.mutator'))) renameSync(join(clone, '.mutator'), join(clone, '.mutator-base'));
    applyEdit();
    await mutator('mutator-since-cold', ['--since', 'HEAD~1'], { includeGlobs: prMutate });
    rmSync(join(clone, '.mutator'), { recursive: true, force: true });
    if (existsSync(join(clone, '.mutator-base'))) renameSync(join(clone, '.mutator-base'), join(clone, '.mutator'));
    else result.notes.push('the base run left no .mutator directory: the warm run is cold');
    await mutator('mutator-since-warm', ['--since', 'HEAD~1'], { includeGlobs: prMutate });
  }
  result.totalS = +((Date.now() - started) / 1000).toFixed(2);
  const failed = result.phases.filter((p) => p.exitCode !== 0 || p.timedOut || p.skipped);
  if (failed.length) result.error = `phases that failed: ${failed.map((p) => p.name).join(', ')}`;
  save();
  if (failed.length) {
    console.error(result.error);
    process.exit(1);
  }
}

main().catch((error) => {
  result.error = String(error?.stack ?? error);
  result.totalS = +((Date.now() - started) / 1000).toFixed(2);
  save();
  console.error(error);
  process.exit(1);
});
