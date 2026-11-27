// Experiment-only: what Supercov does after the tests finish, timed.
// BENCH_MODE=fixtures records Excalidraw's Vitest suite whole and in shards
// with one binary; BENCH_MODE=measure takes those runs and, for each binary,
// rebuilds the whole run's index, merges the shards, opens and reports the
// merged run, and diffs the two, timing each step with its peak memory, then
// checks every binary answered alike.
import assert from 'node:assert/strict';
import { spawn, spawnSync } from 'node:child_process';
import {
  appendFileSync,
  cpSync,
  existsSync,
  mkdirSync,
  readdirSync,
  readFileSync,
  rmSync,
  statSync,
  writeFileSync,
} from 'node:fs';
import { createHash } from 'node:crypto';
import { gunzipSync } from 'node:zlib';
import { join, resolve } from 'node:path';

const output = resolve(process.env.BENCH_OUTPUT ?? 'bench-out');
mkdirSync(output, { recursive: true });
const logs = join(output, 'logs');
mkdirSync(logs, { recursive: true });
const shardCount = Number(process.env.BENCH_SHARDS ?? 16);
const excalidraw = process.env.EXCALIDRAW ?? '701b901a0f1ba3d0b390d9cdf63d112e25abac4b';

function run(cwd, command, args, label, extra = {}, mayFail = false) {
  const started = process.hrtime.bigint();
  const timeFile = join(logs, `${label}.time`);
  const result = spawnSync('/usr/bin/time', ['-f', '%M %e %U %S', '-o', timeFile, command, ...args], {
    cwd,
    env: { ...process.env, CI: 'true', ...extra },
    encoding: 'utf8',
    timeout: 3_600_000,
    maxBuffer: 512 * 1024 * 1024,
  });
  const seconds = Number(process.hrtime.bigint() - started) / 1e9;
  writeFileSync(join(logs, `${label}.log`), `${result.stdout}\n--- stderr\n${result.stderr}`);
  assert.ifError(result.error);
  if (!mayFail) assert.equal(result.status, 0, `${label} failed: see ${label}.log\n${result.stderr.slice(-2000)}`);
  const [rssKb, , user, system] = readFileSync(timeFile, 'utf8').trim().split('\n').at(-1).split(' ').map(Number);
  return { seconds, rssMb: rssKb / 1024, cpu: user + system, stdout: result.stdout, status: result.status };
}

function runAsync(cwd, command, args, label, extra = {}) {
  return new Promise((done) => {
    const child = spawn(command, args, { cwd, env: { ...process.env, CI: 'true', ...extra } });
    let text = '';
    child.stdout.on('data', (chunk) => { text += chunk; });
    child.stderr.on('data', (chunk) => { text += chunk; });
    child.on('close', (status) => {
      writeFileSync(join(logs, `${label}.log`), text);
      done(status);
    });
  });
}

function checkout(directory) {
  if (existsSync(directory)) return;
  mkdirSync(directory, { recursive: true });
  run(directory, 'git', ['init', '-q'], 'git-init');
  run(directory, 'git', ['fetch', '-q', '--depth', '1', 'https://github.com/excalidraw/excalidraw.git', excalidraw], 'git-fetch');
  run(directory, 'git', ['checkout', '-q', '--detach', 'FETCH_HEAD'], 'git-checkout');
}

function sizes(directory) {
  const rows = [];
  const walk = (path, relative) => {
    for (const entry of readdirSync(path, { withFileTypes: true })) {
      const full = join(path, entry.name);
      const name = relative ? `${relative}/${entry.name}` : entry.name;
      if (entry.isDirectory()) walk(full, name);
      else rows.push([name, statSync(full).size]);
    }
  };
  walk(directory, '');
  return rows;
}

const mb = (bytes) => (bytes / 1024 / 1024).toFixed(1);

if (process.env.BENCH_MODE === 'fixtures') {
  const binary = resolve(process.env.BENCH_BINARY);
  const project = join(output, 'excalidraw');
  checkout(project);
  run(project, 'npx', ['-y', 'yarn@1.22.22', 'install', '--frozen-lockfile', '--non-interactive'], 'install');
  const plain = run(project, 'npx', ['vitest', 'run'], 'plain', {}, true);
  const idFile = (name) => join(output, `${name}.id`);
  const full = run(project, binary, ['--', 'npx', 'vitest', 'run'], 'fixture-full', { SUPERCOV_PHASE_TIMING: '1', SUPERCOV_RUN_ID_FILE: idFile('full') }, true);
  const fullId = readFileSync(idFile('full'), 'utf8').trim();
  const shardIds = [];
  const started = Date.now();
  // Four at a time: a CI that shards runs its jobs side by side as well.
  for (let first = 1; first <= shardCount; first += 4) {
    const batch = [];
    for (let shard = first; shard < first + 4 && shard <= shardCount; shard += 1) {
      batch.push(runAsync(project, binary, ['--', 'npx', 'vitest', 'run', `--shard=${shard}/${shardCount}`], `fixture-shard-${shard}`, { SUPERCOV_RUN_ID_FILE: idFile(`shard-${shard}`) }));
    }
    await Promise.all(batch);
  }
  for (let shard = 1; shard <= shardCount; shard += 1) shardIds.push(readFileSync(idFile(`shard-${shard}`), 'utf8').trim());
  const runs = join(project, '.supercov/runs');
  const listing = sizes(runs).sort((a, b) => b[1] - a[1]);
  writeFileSync(
    join(output, 'fixture-summary.txt'),
    [
      `plain vitest ${plain.seconds.toFixed(1)} s`,
      `full run ${full.seconds.toFixed(1)} s, peak ${full.rssMb.toFixed(0)} MB`,
      `shards ${((Date.now() - started) / 1000).toFixed(1)} s`,
      `full run.json timings: ${JSON.stringify(JSON.parse(readFileSync(join(runs, fullId, 'run.json'), 'utf8')).timings)}`,
      ...listing.slice(0, 40).map(([name, size]) => `${mb(size)} MB ${name}`),
    ].join('\n') + '\n',
  );
  writeFileSync(join(output, 'fixture-full-log.txt'), readFileSync(join(logs, 'fixture-full.log'), 'utf8').slice(-20000));
  // Indexes are caches the measuring job rebuilds; the rest is kept.
  rmSync(join(output, 'runs'), { recursive: true, force: true });
  cpSync(runs, join(output, 'runs'), { recursive: true });
  for (const [name] of sizes(join(output, 'runs'))) {
    if (name.endsWith('query-index.v1.bin')) rmSync(join(output, 'runs', name));
  }
  writeFileSync(join(output, 'fixture.json'), JSON.stringify({ full: fullId, shards: shardIds }, null, 2));
  process.exit(0);
}

assert.equal(process.env.BENCH_MODE, 'measure');
const binaries = process.env.BENCH_BINARIES.split(',').map((entry) => {
  const [name, path] = entry.split('=');
  return { name, path: resolve(path) };
});
const rounds = Number(process.env.BENCH_ROUNDS ?? 3);
const fixtures = join(output, 'fixtures');
mkdirSync(fixtures, { recursive: true });
run(fixtures, 'tar', ['-xf', resolve(process.env.BENCH_FIXTURES)], 'untar');
const { full, shards } = JSON.parse(readFileSync(join(fixtures, 'fixture.json'), 'utf8'));
const project = join(output, 'excalidraw');
checkout(project);
const samples = join(output, 'samples.jsonl');
const outputs = join(output, 'outputs');
mkdirSync(outputs, { recursive: true });

const steps = [];
function record(round, binary, step, result) {
  const sample = { round, binary, step, seconds: result.seconds, rssMb: result.rssMb, cpu: result.cpu };
  appendFileSync(samples, `${JSON.stringify(sample)}\n`);
  steps.push(sample);
}

// What each binary answered, with the merged run's id named alike.
function keep(binary, name, text, merged) {
  const normal = merged ? text.split(merged).join('<merged>') : text;
  writeFileSync(join(outputs, `${binary}-${name}`), normal);
}

const storeSizes = {};
for (let round = 1; round <= rounds; round += 1) {
  const order = round % 2 ? binaries : [...binaries].reverse();
  for (const { name, path } of order) {
    const label = `${round}-${name}`;
    rmSync(join(project, '.supercov'), { recursive: true, force: true });
    mkdirSync(join(project, '.supercov'), { recursive: true });
    cpSync(join(fixtures, 'runs'), join(project, '.supercov/runs'), { recursive: true });
    const q = (args, step, mayFail = false) => {
      const result = run(project, path, args, `${label}-${step}`, {}, mayFail);
      record(round, name, step, result);
      return result;
    };
    const fullFirst = q(['runs', full], 'full-index-build');
    q(['runs', full], 'full-reopen');
    const merge = q(['merge', ...shards], 'merge');
    const merged = /merged run (\S+)/.exec(merge.stdout)?.[1];
    assert.ok(merged, `no merged run id in ${label}-merge`);
    const mergedFirst = q(['runs', merged], 'merged-index-build');
    q(['runs', merged], 'merged-reopen');
    const gaps = q(['runs', merged, 'gaps', '--json'], 'merged-gaps-json');
    const files = q(['runs', merged, 'files', '--json'], 'merged-files-json', true);
    const reportDir = join(output, `reports-${label}`);
    mkdirSync(reportDir, { recursive: true });
    for (const format of ['lcov', 'istanbul', 'html']) {
      q(['runs', merged, 'report', '--format', format, '--output', join(reportDir, `merged.${format}`), '--force'], `report-${format}`);
    }
    const diff = q(['diff', full, merged, '--json'], 'diff-json', true);
    if (round === 1) {
      keep(name, 'full.txt', fullFirst.stdout);
      keep(name, 'merged.txt', mergedFirst.stdout, merged);
      keep(name, 'gaps.json', gaps.stdout, merged);
      keep(name, 'files.json', files.stdout, merged);
      keep(name, 'diff.json', diff.stdout, merged);
      for (const format of ['lcov', 'istanbul', 'html']) {
        keep(name, `report.${format}`, readFileSync(join(reportDir, `merged.${format}`), 'utf8'), merged);
      }
      // The merged evidence, entry for entry, as a digest.
      const evidence = gunzipSync(readFileSync(join(project, '.supercov/runs', merged, 'evidence.raw.gz')));
      keep(name, 'merged-evidence.sha256', createHash('sha256').update(evidence.toString('latin1').split(merged).join('<merged>'), 'latin1').digest('hex') + '\n');
      storeSizes[name] = {
        full: sizes(join(project, '.supercov/runs', full)),
        merged: sizes(join(project, '.supercov/runs', merged)),
      };
    }
    rmSync(reportDir, { recursive: true, force: true });
  }
}

// Agreement: every output of every binary as the first binary's.
const disagreements = [];
const reference = binaries[0].name;
for (const file of readdirSync(outputs).filter((file) => file.startsWith(`${reference}-`))) {
  const rest = file.slice(reference.length + 1);
  const expected = readFileSync(join(outputs, file));
  for (const { name } of binaries.slice(1)) {
    const actual = readFileSync(join(outputs, `${name}-${rest}`));
    if (!expected.equals(actual)) disagreements.push(`${name} ${rest}`);
  }
}

const median = (values) => {
  const sorted = [...values].sort((a, b) => a - b);
  return sorted[Math.floor(sorted.length / 2)];
};
const stepNames = [...new Set(steps.map((sample) => sample.step))];
const lines = [
  '| step | ' + binaries.map(({ name }) => `${name} s | ${name} MB`).join(' | ') + ' |',
  '|---|' + binaries.map(() => '---:|---:').join('|') + '|',
];
for (const step of stepNames) {
  const cells = binaries.map(({ name }) => {
    const of = steps.filter((sample) => sample.step === step && sample.binary === name);
    return `${median(of.map((s) => s.seconds)).toFixed(2)} | ${Math.max(...of.map((s) => s.rssMb)).toFixed(0)}`;
  });
  lines.push(`| ${step} | ${cells.join(' | ')} |`);
}
lines.push('', '## Store sizes (MB)', '');
for (const { name } of binaries) {
  for (const which of ['full', 'merged']) {
    const rows = storeSizes[name][which];
    const total = rows.reduce((sum, [, size]) => sum + size, 0);
    lines.push(`- ${name} ${which}: ${mb(total)} total; ${rows.sort((a, b) => b[1] - a[1]).slice(0, 6).map(([file, size]) => `${file} ${mb(size)}`).join(', ')}`);
  }
}
lines.push('', disagreements.length ? `## DISAGREE\n\n${disagreements.join('\n')}` : '## All outputs agree');
writeFileSync(join(output, 'summary.md'), lines.join('\n') + '\n');
console.log(lines.join('\n'));
if (process.env.GITHUB_STEP_SUMMARY) appendFileSync(process.env.GITHUB_STEP_SUMMARY, lines.join('\n') + '\n');
assert.equal(disagreements.length, 0, 'binaries disagree');
