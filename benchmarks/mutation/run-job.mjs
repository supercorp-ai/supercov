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
//   supercov-cold  Supercov: measure the project's test command (per-test coverage and weak
//                  mutation), then `mutants test` (the strong pass); with --shard K, only
//                  `--shard K/n` of the strong pass, n = the project's "supercovShards"
//   pr-supercov    the edit committed, then measure, then `mutants test --diff HEAD~1`
//
// Supercov runs through the published npm wrapper, installed outside the clone, with
// SUPERCOV_RUST_BINARY pointing at a binary built from the working tree (the workflow
// restores it and passes its path as SUPERCOV_BENCH_BINARY).
//
// Runs on a CI machine, never locally: it installs packages into the clone.
// Node only, no dependencies.

import { spawn, execSync } from 'node:child_process';
import { appendFileSync, copyFileSync, existsSync, globSync, mkdirSync, readFileSync, readdirSync, renameSync, rmSync, statSync, writeFileSync, createWriteStream } from 'node:fs';
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
    variant: { type: 'string' },
  },
});
const corpus = JSON.parse(readFileSync(join(here, 'corpus.json'), 'utf8'));
const project = corpus.projects.find((p) => p.name === args.project);
if (!project) throw new Error(`unknown project ${args.project}`);
const scenario = args.scenario;
const SCENARIOS = ['stryker-cold', 'mutator-cold', 'truth', 'pr-stryker', 'pr-mutator', 'supercov-cold', 'pr-supercov', 'supercov-diag', 'shell-diag'];
if (!SCENARIOS.includes(scenario)) throw new Error(`unknown scenario ${scenario}`);
const out = resolve(args.out ?? 'out');
const out_ = out;
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
  variant: args.variant || null,
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

/** The project's mutate set as files, for SUPERCOV_SOURCE_ROOTS. */
function mutateFiles() {
  const excluded = new Set(project.mutate.exclude.flatMap((g) => globSync(g, { cwd: clone })));
  return [...new Set(project.mutate.include.flatMap((g) => globSync(g, { cwd: clone })))].filter((f) => !excluded.has(f)).sort();
}

let supercov = null;
// first-wN: the first variant with Vitest's --maxWorkers=N.
const firstWorkers = args.variant?.match(/^first-w(\d+)$/)?.[1] ?? null;
// wN: the strong pass (not the measured run) with Vitest's --maxWorkers=N.
const strongWorkers = args.variant?.match(/^w(\d+)$/)?.[1] ?? null;
// first-prof: the first variant with a CPU profile of the strong pass's main process; prof: the
// plain job with one.
const firstProfile = args.variant === 'first-prof';
const mainProfile = firstProfile || args.variant === 'prof' || args.variant === 'prof-arena' || args.variant === 'prof-direct';
// prof-direct: prof with Vitest's workers forked by Vitest itself, not by Supercov's spawners.
// direct: the plain job with Vitest's workers forked by Vitest itself.
if (args.variant === 'prof-direct' || args.variant === 'direct') process.env.SUPERCOV_STRONG_DIRECT_FORK = '1';
// ahead0: no workers started ahead of Vitest's requests (each copy's worker starts when Vitest asks).
if (args.variant === 'ahead0') process.env.SUPERCOV_STRONG_WORKERS_AHEAD = '0';
// prof-arena: prof with glibc's malloc held to two arenas in every process of the strong pass.
if (args.variant === 'prof-arena') process.env.MALLOC_ARENA_MAX = '2';

async function installSupercov() {
  const binary = process.env.SUPERCOV_BENCH_BINARY;
  if (!binary || !existsSync(binary)) throw new Error(`SUPERCOV_BENCH_BINARY (${binary}) is not a file`);
  const prefix = join(workRoot, 'supercov-cli');
  mkdirSync(prefix, { recursive: true });
  await phase('tool-install', `npm i --prefix ${prefix} --no-audit --no-fund supercov@${corpus.tools.supercovWrapper} >/dev/null && ls ${prefix}/node_modules/.bin`, { cwd: workRoot });
  if (project.supercovVitest && args.variant !== 'own-vitest') await upgradeVitest(project.supercovVitest, project.supercovVitestReason);
  if (args.variant === 'own-vitest') note(`variant own-vitest: Supercov on the project's own Vitest ${project.vitest}`);
  if (firstWorkers) note(`variant first-w${firstWorkers}: the first variant with Vitest's --maxWorkers=${firstWorkers}`);
  if (args.variant === 'first') note('variant first: no measured run; `supercov mutants -- <test command>` measures what it needs, then tests the mutants');
  process.env.SUPERCOV_RUST_BINARY = binary;
  process.env.SUPERCOV_SOURCE_ROOTS = mutateFiles().join(',');
  supercov = `${prefix}/node_modules/.bin/supercov`;
  result.versions.supercovWrapper = corpus.tools.supercovWrapper;
  try {
    result.versions.supercov = execSync(`${supercov} --version`, { cwd: clone, encoding: 'utf8' }).trim();
  } catch (error) {
    result.versions.supercov = `--version failed: ${String(error.message).split('\n')[0]}`;
  }
  result.versions.supercovBinaryKey = process.env.SUPERCOV_BENCH_BINARY_KEY ?? null;
  note(`Supercov instruments only the mutate set: SUPERCOV_SOURCE_ROOTS lists its ${process.env.SUPERCOV_SOURCE_ROOTS.split(',').length} files`);
}

/** The latest run's directory under .supercov/runs. */
function latestRunDir() {
  const root = join(clone, '.supercov', 'runs');
  if (!existsSync(root)) return null;
  const dirs = readdirSync(root).map((d) => join(root, d)).filter((d) => statSync(d).isDirectory());
  dirs.sort((a, b) => statSync(b).mtimeMs - statSync(a).mtimeMs);
  return dirs[0] ?? null;
}

async function supercovMeasure() {
  const debug = process.env.SUPERCOV_BENCH_DEBUG_GLOBS;
  if (debug) process.env.SUPERCOV_KEEP_WORKSPACE = '1';
  const record = await phase('supercov-measure', `${supercov} -- node_modules/.bin/${project.test}`, { allowFail: true });
  if (debug) {
    // Diagnosis only: copy files of the kept workspace (globs relative to .supercov/) into the output.
    const base = join(clone, '.supercov');
    const files = debug.split(',').flatMap((g) => globSync(g, { cwd: base }));
    for (const file of files.slice(0, 50)) {
      const from = join(base, file);
      if (!statSync(from).isFile()) continue;
      const to = join(out_, 'debug', file);
      mkdirSync(dirname(to), { recursive: true });
      writeFileSync(to, readFileSync(from).subarray(0, 4 << 20));
    }
    record.debugFiles = files.length;
    mkdirSync(join(out_, 'debug'), { recursive: true });
    try {
      execSync(`find .supercov -maxdepth 8 \\( -name '*.jsonl' -o -type d -maxdepth 5 \\) | head -400 > ${join(out_, 'debug', 'listing.txt')}`, { cwd: clone, stdio: 'ignore' });
    } catch {}
  }
  const run = latestRunDir();
  record.runDir = run ? run.slice(clone.length + 1) : null;
  save();
  return record;
}

async function supercovStrong(name, flags, { first = false } = {}) {
  const out = join(out_, `${name}.result.json`);
  const command = first
    ? `${supercov} mutants --json ${flags.join(' ')} -- node_modules/.bin/${project.test}${firstWorkers ? ` --maxWorkers=${firstWorkers}` : ''}`
    : `${supercov} runs latest mutants test --json ${flags.join(' ')}${strongWorkers ? ` -- node_modules/.bin/${project.test} --maxWorkers=${strongWorkers}` : ''}`;
  const record = await phase(name, `${command} > ${out}`, { allowFail: true });
  record.result = existsSync(out) ? `${name}.result.json` : null;
  const run = latestRunDir();
  if (run && existsSync(join(run, 'strong-mutation.json'))) {
    copyFileSync(join(run, 'strong-mutation.json'), join(out_, `${name}.strong-mutation.json`));
    record.strongFile = `${name}.strong-mutation.json`;
  }
  // Every mutant with its strong verdict, for the join with the ground truth (not timed).
  const list = join(out_, `${name}.mutants.jsonl`);
  try {
    // `--json --limit N` stops at a reply-size cap (about 120 mutants here, with hasMore); jsonl writes every one.
    execSync(`${supercov} runs latest mutants --format jsonl --output ${list}`, { cwd: clone, stdio: ['ignore', 'ignore', 'inherit'], maxBuffer: 1 << 30 });
    record.mutants = `${name}.mutants.jsonl`;
  } catch (error) {
    record.mutantsError = String(error.message).split('\n')[0];
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
  const isSupercov = scenario.includes('supercov');
  if (isSupercov) {
    excludeFromGit(['.supercov/']);
    await installSupercov();
  } else if (scenario === 'shell-diag') {
    // nothing to install: DIAG_SCRIPT's commands use the project's own tools
  } else if (isStryker) await installStryker();
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
  } else if (scenario === 'supercov-cold') {
    if (!args.shard || args.shard === '1') await plainTests();
    // A shard is K (of the project's supercovShards) or K-N (K of N, for a re-split shard).
    const split = args.shard?.match(/^(\d+)-(\d+)$/);
    const shard = args.shard ? ['--shard', split ? `${split[1]}/${split[2]}` : `${args.shard}/${project.supercovShards}`] : [];
    if (mainProfile) process.env.SUPERCOV_STRONG_CPU_PROFILE = join(out_, 'main.cpuprofile');
    if (args.variant === 'first' || firstWorkers || firstProfile) {
      // What a user's first `supercov mutants` does: nothing measured before it.
      await supercovStrong('supercov-strong', shard, { first: true });
    } else {
      const measure = await supercovMeasure();
      if (measure.exitCode !== 0) throw new Error(`supercov measure failed (exit ${measure.exitCode})`);
      // The plan of this job's part (what would run, against which tests), for where time goes.
      await phase('supercov-plan', `${supercov} runs latest mutants test --plan --json ${shard.join(' ')} > ${join(out_, 'supercov-plan.json')}`, { allowFail: true });
      if (args.shard && !project.supercovShards) throw new Error(`${project.name} has no supercovShards`);
      await supercovStrong('supercov-strong', shard);
    }
  } else if (scenario === 'shell-diag') {
    // Diagnosis only: DIAG_SCRIPT holds commands separated by ';;', each run in the clone as a phase.
    const commands = (process.env.DIAG_SCRIPT ?? '').split(';;').map((c) => c.trim()).filter(Boolean);
    for (const [i, command] of commands.entries()) await phase(`diag-${i + 1}`, command, { allowFail: true });
  } else if (scenario === 'supercov-diag') {
    // Diagnosis of strong-pass verdicts: SUPERCOV_DIAG_FILES (comma-separated source files) and
    // SUPERCOV_DIAG_POINTS (comma-separated file:line) name what to look at. Keeps the workspace
    // and each strong stage's assignments and outcomes, and archives them with the plans.
    process.env.SUPERCOV_KEEP_WORKSPACE = '1';
    process.env.SUPERCOV_STRONG_KEEP_STAGES = '1';
    const files = (process.env.SUPERCOV_DIAG_FILES ?? '').split(',').filter(Boolean);
    const points = (process.env.SUPERCOV_DIAG_POINTS ?? '').split(',').filter(Boolean);
    const measure = await supercovMeasure();
    if (measure.exitCode !== 0) throw new Error(`supercov measure failed (exit ${measure.exitCode})`);
    for (const point of points) {
      const name = point.replace(/[^A-Za-z0-9]+/g, '_');
      await phase(`query-${name}`, `${supercov} runs latest mutants ${point} --json > ${join(out_, `query-${name}.json`)}`, { allowFail: true });
    }
    files.forEach((file, i) => {
      result.notes.push(`diag file ${i + 1}: ${file}`);
    });
    for (const [i, file] of files.entries()) {
      await phase(`plan-${i + 1}`, `${supercov} runs latest mutants test --plan --json --file ${file} > ${join(out_, `plan-${i + 1}.json`)}`, { allowFail: true });
    }
    for (const [i, file] of files.entries()) {
      await phase(`strong-${i + 1}`, `${supercov} runs latest mutants test --json --file ${file} > ${join(out_, `strong-${i + 1}.json`)}`, { allowFail: true });
      // Each command numbers its stages from 0 in the same directory: keep this command's, then clear them.
      await phase(`stages-${i + 1}`, `d=$(find .supercov -path '*node_modules*' -prune -o -type d -name strong-stages -print | head -1); [ -n "$d" ] && tar czf ${join(out_, `stages-${i + 1}.tgz`)} -C "$d" . && rm -rf "$d"`, { allowFail: true });
    }
    await phase('archive', `find .supercov \\( -path '*node_modules*' -prune \\) -o \\( -path '*strong-stages*' -o -path '.supercov/strong/*' -o -path '*frontend-evidence/mutants.json' -o -path '.supercov/runs/*/strong-mutation.json' \\) -type f -print | tar czf ${join(out_, 'diag.tgz')} -T -`, { allowFail: true });
  } else if (scenario === 'pr-supercov') {
    applyEdit();
    const measure = await supercovMeasure();
    if (measure.exitCode !== 0) throw new Error(`supercov measure failed (exit ${measure.exitCode})`);
    await supercovStrong('supercov-diff', ['--diff', 'HEAD~1']);
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
